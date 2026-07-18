//! Per-buffer undo/redo history.
//!
//! Each entry is a **text splice delta** (`{ start, old_bytes, new_bytes }` —
//! O(edit size), the operands `apply_splice` already has) plus a **full clone
//! of the annotations as they were before the edit** (the un-invertible part:
//! a destroyed or clamped feature can't be inverse-reconstructed, so it is
//! snapshotted). See `plans/editor.md` §3.
//!
//! Storing both `old_bytes` (removed) and `new_bytes` (inserted) makes undo
//! and redo symmetric and lossless:
//! - **undo**: `splice(start..start+new_bytes.len(), old_bytes)`; swap annotations.
//! - **redo**: `splice(start..start+old_bytes.len(), new_bytes)`; swap annotations.
//!
//! Correctness relies on the **single execution path**: the live buffer is
//! always exactly the post-last-edit state, so undo/redo walk the stack from
//! the current state without a base snapshot.
//!
//! Bounded by a **per-buffer byte budget** (deltas are variable-size, so a
//! count cap is a false guard); eviction is silent and oldest-first. Budget
//! usage is **recomputed** from entry `size_bytes()` — not a running counter —
//! because undo/redo swap annotation snapshots and can change an entry's size.

use std::collections::VecDeque;

use crate::{Annotations, Buffer, Topology};

/// Default per-buffer history budget in bytes. Deltas are tiny for normal
/// edits, so ~16 MB is effectively unlimited undo depth; configurable.
pub const DEFAULT_BUDGET_BYTES: usize = 16 * 1024 * 1024;

/// Backstop on entry count, purely for data-structure hygiene; the byte
/// budget is the real governor.
pub const MAX_ENTRIES: usize = 2000;

/// Coalescing window: consecutive contiguous `Insert`s within this span merge
/// into the last entry rather than pushing a new one.
pub const COALESCE_WINDOW: std::time::Duration = std::time::Duration::from_millis(500);

/// Classifies an edit for coalescing. Only consecutive contiguous `Insert`s
/// coalesce.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditKind {
    Insert,
    Delete,
    Other,
}

/// One reversible edit: the splice delta for the text plus a snapshot of the
/// annotations as they were *before* the edit.
#[derive(Debug, Clone)]
pub struct HistoryEntry {
    /// Splice start (same index in both directions).
    pub start: usize,
    /// Bytes that were removed — restored on undo.
    pub old_bytes: Vec<u8>,
    /// Bytes that were inserted — re-applied on redo.
    pub new_bytes: Vec<u8>,
    /// Annotations as they were before this edit (restored on undo).
    /// After undo, this field holds the post-edit snapshot (stashed for redo).
    pub annotations: Annotations,
    /// Buffer topology as it was before this edit, when the edit changed it
    /// (Linearize / Circularize). `None` for edits that leave topology alone —
    /// the common case — so undo/redo skip the swap. Stamped via
    /// [`History::stamp_topology`] after `record`.
    pub topology_before: Option<Topology>,
}

impl HistoryEntry {
    /// Heap bytes this entry holds, for budget accounting.
    fn size_bytes(&self) -> usize {
        self.old_bytes.len() + self.new_bytes.len() + annotations_size(&self.annotations)
    }
}

/// Rough heap estimate for an `Annotations` clone — enough for budgeting, not
/// exact.
fn annotations_size(ann: &Annotations) -> usize {
    let features: usize = ann
        .features
        .iter()
        .map(|f| {
            let quals: usize = f
                .qualifiers
                .iter()
                .map(|(k, v)| k.len() + v.as_ref().map_or(0, String::len))
                .sum();
            64 + f.label.len() + f.raw_kind.len() + quals
        })
        .sum();
    // Primers ride the same `Annotations` snapshot on undo (decision 14);
    // count them too so the budget estimate doesn't undercount buffers with
    // many primers. Benign if it drifts — this is an estimate, not exact.
    let primers: usize = ann
        .primers
        .iter()
        .map(|p| {
            let quals: usize = p
                .qualifiers
                .iter()
                .map(|(k, v)| k.len() + v.as_ref().map_or(0, String::len))
                .sum();
            64 + p.name.len() + p.sequence.len() + quals
        })
        .sum();
    features + primers
}

/// Per-buffer undo/redo stacks. Shared across all views into a buffer.
#[derive(Debug)]
pub struct History {
    past: VecDeque<HistoryEntry>,
    future: Vec<HistoryEntry>,
    budget: usize,
    last_edit_kind: Option<EditKind>,
    last_edit_at: Option<std::time::Instant>,
}

impl Default for History {
    fn default() -> Self {
        Self::with_budget(DEFAULT_BUDGET_BYTES)
    }
}

impl History {
    pub fn with_budget(budget: usize) -> Self {
        Self {
            past: VecDeque::new(),
            future: Vec::new(),
            budget,
            last_edit_kind: None,
            last_edit_at: None,
        }
    }

    pub fn can_undo(&self) -> bool {
        !self.past.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.future.is_empty()
    }

    pub fn len(&self) -> usize {
        self.past.len()
    }

    pub fn is_empty(&self) -> bool {
        self.past.is_empty()
    }

    /// Sum of `size_bytes()` over `past` and `future`. Recomputed on demand so
    /// undo/redo annotation swaps cannot desync a running counter.
    fn total_bytes(&self) -> usize {
        self.past
            .iter()
            .chain(self.future.iter())
            .map(HistoryEntry::size_bytes)
            .sum()
    }

    /// Record an edit as a splice delta. `old_bytes` is the slice removed at
    /// `start`; `new_bytes` is what was inserted there; `ann_before` is the
    /// annotation state prior to the edit. Clears the redo stack.
    ///
    /// Consecutive, contiguous `Insert`s within [`COALESCE_WINDOW`] merge into
    /// the last entry (so a run of keystrokes is one undo unit). Returns `true`
    /// if a new entry was pushed, `false` if coalesced.
    pub fn record(
        &mut self,
        start: usize,
        old_bytes: Vec<u8>,
        new_bytes: Vec<u8>,
        ann_before: &Annotations,
        kind: EditKind,
    ) -> bool {
        // Any new edit invalidates the redo stack. Drop entries outright —
        // sizes are recomputed in `enforce_bounds`, not subtracted here.
        self.future.clear();

        let now = std::time::Instant::now();
        let pushed = if self.can_coalesce(start, &old_bytes, kind, now) {
            // Extend the last entry's inserted bytes; its `old_bytes` (empty,
            // an insert removes nothing) and `ann_before` (the pre-run state)
            // stay as-is.
            if let Some(last) = self.past.back_mut() {
                last.new_bytes.extend_from_slice(&new_bytes);
            }
            false
        } else {
            self.past.push_back(HistoryEntry {
                start,
                old_bytes,
                new_bytes,
                annotations: ann_before.clone(),
                topology_before: None,
            });
            true
        };

        self.last_edit_kind = Some(kind);
        self.last_edit_at = Some(now);
        self.enforce_bounds();
        pushed
    }

    /// Whether this edit should merge into the last `past` entry: an `Insert`
    /// (removes nothing) immediately following the previous insert, within the
    /// coalesce window.
    fn can_coalesce(
        &self,
        start: usize,
        old_bytes: &[u8],
        kind: EditKind,
        now: std::time::Instant,
    ) -> bool {
        if kind != EditKind::Insert
            || !old_bytes.is_empty()
            || self.last_edit_kind != Some(EditKind::Insert)
        {
            return false;
        }
        if self
            .last_edit_at
            .is_none_or(|t| now.duration_since(t) > COALESCE_WINDOW)
        {
            return false;
        }
        // Only merge if this insert is contiguous with the end of the last one.
        self.past
            .back()
            .is_some_and(|last| start == last.start + last.new_bytes.len())
    }

    /// Record that the just-pushed entry changed the buffer topology from
    /// `before`, so undo/redo restore it. Call right after [`History::record`]
    /// for a topology-changing edit (Linearize / Circularize). No-op if the last
    /// edit coalesced (topology ops use `EditKind::Other`, which never does).
    pub fn stamp_topology(&mut self, before: Topology) {
        if let Some(last) = self.past.back_mut() {
            last.topology_before = Some(before);
        }
    }

    /// Undo the most recent edit. Returns `false` if there's nothing to undo.
    pub fn undo(&mut self, buf: &mut Buffer, ann: &mut Annotations) -> bool {
        let Some(mut entry) = self.past.pop_back() else {
            return false;
        };
        // Reverse the splice: remove the inserted span, restore old bytes.
        buf.text.splice(
            entry.start..entry.start + entry.new_bytes.len(),
            entry.old_bytes.iter().copied(),
        );
        // The entry holds the pre-edit annotations; stash the current
        // (post-edit) ones into the entry so redo can restore them.
        std::mem::swap(ann, &mut entry.annotations);
        // Same swap for topology when this edit changed it.
        if let Some(tb) = entry.topology_before {
            entry.topology_before = Some(buf.topology);
            buf.topology = tb;
        }
        buf.version += 1;
        buf.dirty = true;
        self.future.push(entry);
        // Coalescing must not bridge across an undo.
        self.last_edit_kind = None;
        self.last_edit_at = None;
        true
    }

    /// Redo the most recently undone edit. Returns `false` if nothing to redo.
    pub fn redo(&mut self, buf: &mut Buffer, ann: &mut Annotations) -> bool {
        let Some(mut entry) = self.future.pop() else {
            return false;
        };
        // Re-apply the forward splice: remove old bytes, insert new bytes.
        buf.text.splice(
            entry.start..entry.start + entry.old_bytes.len(),
            entry.new_bytes.iter().copied(),
        );
        // `entry.annotations` currently holds the post-edit state (stashed by
        // undo); restore it and stash the pre-edit state back for a later undo.
        std::mem::swap(ann, &mut entry.annotations);
        if let Some(tb) = entry.topology_before {
            entry.topology_before = Some(buf.topology);
            buf.topology = tb;
        }
        buf.version += 1;
        buf.dirty = true;
        self.past.push_back(entry);
        self.last_edit_kind = None;
        self.last_edit_at = None;
        true
    }

    /// Enforce the byte budget and entry-count backstop by dropping oldest
    /// `past` entries (silent, FIFO). Keeps at least one undo step. Budget
    /// usage is recomputed from current entry sizes each check.
    fn enforce_bounds(&mut self) {
        while self.past.len() > MAX_ENTRIES
            || (self.total_bytes() > self.budget && self.past.len() > 1)
        {
            if self.past.pop_front().is_none() {
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Feature, Location, Strand, Topology};
    use std::collections::BTreeMap;

    fn buf(bytes: &[u8]) -> Buffer {
        Buffer::new("t".into(), None, bytes.to_vec(), Topology::Linear)
    }

    fn feat(start: usize, end: usize, label: &str) -> Feature {
        Feature {
            id: Default::default(),
            location: Location::simple(start..end),
            raw_kind: "misc_feature".into(),
            label: label.into(),
            strand: Strand::Forward,
            qualifiers: BTreeMap::new(),
            provenance: None,
        }
    }

    /// Apply an edit through the same capture-then-splice flow the workspace
    /// helper will use, recording it in `h`.
    fn edit(
        h: &mut History,
        buf: &mut Buffer,
        ann: &mut Annotations,
        start: usize,
        end: usize,
        new: &[u8],
        kind: EditKind,
    ) {
        let old_bytes = buf.text[start..end].to_vec();
        h.record(start, old_bytes, new.to_vec(), ann, kind);
        // mirror apply_splice's effect on text + the feature shift is exercised
        // by mutations.rs tests; here annotations are managed by the caller.
        crate::mutations::apply_splice(buf, ann, start..end, new);
    }

    #[test]
    fn five_contiguous_inserts_coalesce_to_one_entry() {
        let mut h = History::default();
        let mut b = buf(b"AAAA");
        let mut a = Annotations::default();
        // type "GGGGG" one char at a time at the advancing cursor (pos 2,3,4,5,6)
        for (i, ch) in b"GGGGG".iter().enumerate() {
            edit(
                &mut h,
                &mut b,
                &mut a,
                2 + i,
                2 + i,
                &[*ch],
                EditKind::Insert,
            );
        }
        assert_eq!(h.len(), 1, "contiguous typing run = one undo unit");
        assert_eq!(b.text, b"AAGGGGGAA");
        // one undo reverts the whole run
        h.undo(&mut b, &mut a);
        assert_eq!(b.text, b"AAAA");
    }

    #[test]
    fn insert_then_delete_are_two_entries() {
        let mut h = History::default();
        let mut b = buf(b"AAAA");
        let mut a = Annotations::default();
        edit(&mut h, &mut b, &mut a, 4, 4, b"CC", EditKind::Insert);
        edit(&mut h, &mut b, &mut a, 0, 2, b"", EditKind::Delete);
        assert_eq!(h.len(), 2);
        assert_eq!(b.text, b"AACC");
    }

    #[test]
    fn non_contiguous_inserts_do_not_coalesce() {
        let mut h = History::default();
        let mut b = buf(b"AAAA");
        let mut a = Annotations::default();
        edit(&mut h, &mut b, &mut a, 0, 0, b"X", EditKind::Insert);
        // jump elsewhere — not adjacent to the previous insert
        edit(&mut h, &mut b, &mut a, 4, 4, b"Y", EditKind::Insert);
        assert_eq!(h.len(), 2, "non-adjacent inserts are distinct undo units");
    }

    #[test]
    fn undo_redo_restores_bytes_and_features() {
        let mut h = History::default();
        let mut b = buf(b"AAAACCCCGGGG"); // len 12
        let mut a = Annotations::new(vec![feat(4, 8, "mid")]);
        // delete [4,8) — destroys the feature fully inside it
        edit(&mut h, &mut b, &mut a, 4, 8, b"", EditKind::Delete);
        assert_eq!(b.text, b"AAAAGGGG");
        assert!(a.features.is_empty(), "feature destroyed by the delete");

        // undo restores both the bytes and the destroyed feature
        assert!(h.undo(&mut b, &mut a));
        assert_eq!(b.text, b"AAAACCCCGGGG");
        assert_eq!(a.features.len(), 1);
        assert_eq!(a.features[0].bounds(b.text.len()), 4..8);
        assert_eq!(a.features[0].label, "mid");

        // redo re-applies and re-destroys
        assert!(h.redo(&mut b, &mut a));
        assert_eq!(b.text, b"AAAAGGGG");
        assert!(a.features.is_empty());
    }

    #[test]
    fn new_edit_after_undo_clears_redo() {
        let mut h = History::default();
        let mut b = buf(b"AAAA");
        let mut a = Annotations::default();
        edit(&mut h, &mut b, &mut a, 4, 4, b"C", EditKind::Insert);
        h.undo(&mut b, &mut a);
        assert!(h.can_redo());
        edit(&mut h, &mut b, &mut a, 0, 0, b"T", EditKind::Other);
        assert!(!h.can_redo(), "a new edit after undo drops the redo stack");
    }

    #[test]
    fn byte_budget_evicts_oldest_keeps_recent() {
        // Tiny budget so a few edits force eviction.
        let mut h = History::with_budget(256);
        let mut b = buf(&[b'A'; 100]);
        let mut a = Annotations::default();
        // Each delete stores ~50 old_bytes; several will exceed 256 bytes.
        for _ in 0..10 {
            // delete 50 then re-insert 50 to keep length stable, distinct entries
            edit(&mut h, &mut b, &mut a, 0, 50, &[b'C'; 50], EditKind::Other);
        }
        assert!(!h.is_empty(), "keeps at least one undo step");
        assert!(
            h.len() < 10,
            "older entries evicted under the byte budget (kept {})",
            h.len()
        );
        // The most recent edit is still undoable.
        assert!(h.can_undo());
    }

    #[test]
    fn undo_ann_swap_then_new_edit_does_not_underflow_budget() {
        // Mirrors paste→undo→type: record with small ann_before, then grow live
        // annotations (as place would), undo (swaps larger anns into the future
        // entry), then record a fresh edit that clears redo. Must not panic on
        // a desynced running byte counter.
        let mut h = History::default();
        let mut b = buf(b"");
        let mut a = Annotations::default();
        let pasted = b"ATGCATGC";
        h.record(0, Vec::new(), pasted.to_vec(), &a, EditKind::Other);
        b.text.extend_from_slice(pasted);
        // Post-record annotation growth (carried features from place).
        a.features.push(feat(0, 8, "carried"));
        assert!(h.undo(&mut b, &mut a));
        assert_eq!(b.text, b"");
        assert!(a.features.is_empty(), "undo restores pre-paste annotations");
        assert!(h.can_redo());

        // New edit after undo clears future — previously underflowed `bytes`.
        edit(&mut h, &mut b, &mut a, 0, 0, b"GG", EditKind::Insert);
        assert_eq!(b.text, b"GG");
        assert!(!h.can_redo());
        assert!(h.can_undo());
        assert_eq!(
            h.total_bytes(),
            h.past.iter().map(HistoryEntry::size_bytes).sum::<usize>(),
            "budget total matches entry sizes after redo clear"
        );
    }
}
