//! The single forward mutation primitive — `apply_splice` — and its
//! content-given reductions (`insert` / `delete` / `replace`).
//!
//! Every sequence edit is a splice: replace `text[range]` with `new_bytes`.
//! Insert is the empty-range case, delete the empty-replacement case,
//! replace the general case. The feature-shift policy lives here, in
//! *one* place, and is the only thing that adjusts `Annotations` to track
//! the edit.
//!
//! **Contract (decided):** `apply_splice` trusts a well-formed range
//! (`start <= end <= buf.len()`). Bounds validation is *policy* and lives
//! at the command layer (Phase 12 returns `DispatchError::OutOfRange`); the
//! primitive only `debug_assert!`s the precondition. See
//! `docs/architecture.md` "Edit operations".
//!
//! These ops are content-given — they need no biology, so they live in
//! `core` with the model whose invariants they maintain. Edits whose bytes
//! are *derived* (reverse-complement, cloning) compose at the command layer
//! over this primitive.

use std::ops::Range;

use crate::{Annotations, Buffer};

/// Replace `buf.text[range]` with `new_bytes`, shift features per the §2
/// policy, bump `version`, and mark the buffer dirty.
///
/// Precondition: `range.start <= range.end <= buf.len()`.
pub fn apply_splice(
    buf: &mut Buffer,
    ann: &mut Annotations,
    range: Range<usize>,
    new_bytes: &[u8],
) {
    debug_assert!(
        range.start <= range.end && range.end <= buf.text.len(),
        "apply_splice precondition violated: range {:?} on len {}",
        range,
        buf.text.len()
    );

    let start = range.start;
    let removed = range.end - range.start;
    let inserted = new_bytes.len();

    buf.text.splice(range, new_bytes.iter().copied());

    shift_features(ann, start, removed, inserted);

    buf.version += 1;
    buf.dirty = true;
}

/// Insert `bases` at `pos` (the empty-range reduction of splice).
pub fn apply_insert(buf: &mut Buffer, ann: &mut Annotations, pos: usize, bases: &[u8]) {
    apply_splice(buf, ann, pos..pos, bases);
}

/// Delete `text[start..end]` (the empty-replacement reduction of splice).
pub fn apply_delete(buf: &mut Buffer, ann: &mut Annotations, start: usize, end: usize) {
    apply_splice(buf, ann, start..end, &[]);
}

/// Replace `text[start..end]` with `bases` (the general splice). A single
/// operation so the feature shift fires once.
pub fn apply_replace(
    buf: &mut Buffer,
    ann: &mut Annotations,
    start: usize,
    end: usize,
    bases: &[u8],
) {
    apply_splice(buf, ann, start..end, bases);
}

/// Apply the feature-shift policy for a splice that removed
/// `removed` bytes at `start` and inserted `inserted` bytes there.
///
/// Modelled as delete-then-insert at the same point, so the case analysis
/// is the §2 delete policy on `[start, start+removed)` followed by a
/// right-shift of everything at/after `start` by the net delta. Features
/// fully inside the removed region are dropped.
fn shift_features(ann: &mut Annotations, start: usize, removed: usize, inserted: usize) {
    let end = start + removed; // end of the removed region
    let delta = inserted as isize - removed as isize;

    ann.features.retain_mut(|f| {
        let (fs, fe) = (f.range.start, f.range.end);

        // Fully left of the edit — untouched.
        if fe <= start {
            return true;
        }

        // Fully right of the removed region — shift both ends by delta.
        if fs >= end {
            f.range.start = (fs as isize + delta) as usize;
            f.range.end = (fe as isize + delta) as usize;
            return true;
        }

        // From here the feature overlaps the removed region `[start, end)`.

        // Fully inside the removed region — destroyed.
        if fs >= start && fe <= end {
            return false;
        }

        // Spans / straddles the removed region. Clamp the overlap to the
        // edit point, then apply delta to whatever lies at/after `end`.
        let new_start = if fs < start { fs } else { start };
        // Portion of the feature at/after the removed region keeps its
        // length and moves by delta; the removed-overlap collapses to `start`.
        let new_end = if fe > end {
            (fe as isize + delta) as usize
        } else {
            // fe is within (start, end] — the tail was cut; clamp to start.
            start
        };

        f.range.start = new_start;
        f.range.end = new_end.max(new_start);
        // A straddle that collapses to an empty range (e.g. the whole
        // feature body was removed) is dropped.
        f.range.start < f.range.end
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Feature, Strand, Topology};
    use std::collections::BTreeMap;

    fn buf(len: usize) -> Buffer {
        Buffer::new("t".into(), None, vec![b'A'; len], Topology::Linear)
    }

    fn feat(start: usize, end: usize) -> Feature {
        Feature {
            id: Default::default(),
            range: start..end,
            raw_kind: "misc_feature".into(),
            label: "f".into(),
            strand: Strand::Forward,
            qualifiers: BTreeMap::new(),
            provenance: None,
        }
    }

    fn ann(features: Vec<Feature>) -> Annotations {
        Annotations::new(features)
    }

    // ── version / dirty ───────────────────────────────────────────────────────

    #[test]
    fn splice_bumps_version_and_dirty() {
        let mut b = buf(10);
        let mut a = ann(vec![]);
        assert_eq!(b.version, 0);
        assert!(!b.dirty);
        apply_insert(&mut b, &mut a, 5, b"GG");
        assert_eq!(b.version, 1);
        assert!(b.dirty);
        assert_eq!(b.text.len(), 12);
    }

    // ── insert cases (§2) ──────────────────────────────────────────────────────

    #[test]
    fn insert_left_of_feature_shifts_right() {
        // feature [5,8); insert 2 at pos 2 → [7,10)
        let mut b = buf(10);
        let mut a = ann(vec![feat(5, 8)]);
        apply_insert(&mut b, &mut a, 2, b"CC");
        assert_eq!(a.features[0].range, 7..10);
    }

    #[test]
    fn insert_right_of_feature_untouched() {
        // feature [2,5); insert at pos 8 → unchanged
        let mut b = buf(10);
        let mut a = ann(vec![feat(2, 5)]);
        apply_insert(&mut b, &mut a, 8, b"CC");
        assert_eq!(a.features[0].range, 2..5);
    }

    #[test]
    fn insert_inside_feature_extends_end() {
        // feature [2,8); insert 3 at pos 5 → [2,11)
        let mut b = buf(10);
        let mut a = ann(vec![feat(2, 8)]);
        apply_insert(&mut b, &mut a, 5, b"CCC");
        assert_eq!(a.features[0].range, 2..11);
    }

    #[test]
    fn insert_at_feature_start_shifts() {
        // feature [5,8); insert at pos 5 (== start) → right-shift to [7,10)
        let mut b = buf(10);
        let mut a = ann(vec![feat(5, 8)]);
        apply_insert(&mut b, &mut a, 5, b"CC");
        assert_eq!(a.features[0].range, 7..10);
    }

    // ── delete cases (§2) ──────────────────────────────────────────────────────

    #[test]
    fn delete_left_of_feature_untouched() {
        // feature [5,8); delete [0,2) → fully right, shift by -2 → [3,6)
        let mut b = buf(10);
        let mut a = ann(vec![feat(5, 8)]);
        apply_delete(&mut b, &mut a, 0, 2);
        assert_eq!(a.features[0].range, 3..6);
    }

    #[test]
    fn delete_fully_right_is_untouched() {
        // feature [2,5); delete [7,9) → left of edit, untouched
        let mut b = buf(10);
        let mut a = ann(vec![feat(2, 5)]);
        apply_delete(&mut b, &mut a, 7, 9);
        assert_eq!(a.features[0].range, 2..5);
    }

    #[test]
    fn delete_fully_inside_removes_feature() {
        // feature [4,6); delete [2,8) → feature destroyed
        let mut b = buf(10);
        let mut a = ann(vec![feat(4, 6)]);
        apply_delete(&mut b, &mut a, 2, 8);
        assert!(a.features.is_empty());
    }

    #[test]
    fn delete_straddles_start_clamps_end() {
        // feature [2,6); delete [4,8) → feat.start<start<feat.end<=end
        // → clamp end to start of cut: [2,4)
        let mut b = buf(10);
        let mut a = ann(vec![feat(2, 6)]);
        apply_delete(&mut b, &mut a, 4, 8);
        assert_eq!(a.features[0].range, 2..4);
    }

    #[test]
    fn delete_straddles_end_pulls_start() {
        // feature [4,8); delete [2,6) → start<=feat.start<end, feat.end>end
        // → start=start(2), end -= n(4) → [2,4)
        let mut b = buf(10);
        let mut a = ann(vec![feat(4, 8)]);
        apply_delete(&mut b, &mut a, 2, 6);
        assert_eq!(a.features[0].range, 2..4);
    }

    #[test]
    fn delete_spanned_by_feature_contracts() {
        // feature [2,9); delete [4,6) (n=2) → feat.start<start && feat.end>end
        // → end -= 2 → [2,7)
        let mut b = buf(10);
        let mut a = ann(vec![feat(2, 9)]);
        apply_delete(&mut b, &mut a, 4, 6);
        assert_eq!(a.features[0].range, 2..7);
    }

    // ── replace (§2: one op, delta shift) ──────────────────────────────────────

    #[test]
    fn replace_shifts_right_feature_by_delta() {
        // feature [6,9); replace [2,4) (len 2) with 5 bases → delta +3 → [9,12)
        let mut b = buf(10);
        let mut a = ann(vec![feat(6, 9)]);
        apply_replace(&mut b, &mut a, 2, 4, b"CCCCC");
        assert_eq!(a.features[0].range, 9..12);
        assert_eq!(b.text.len(), 13);
    }

    #[test]
    fn replace_negative_delta_shifts_left() {
        // feature [6,9); replace [2,5) (len 3) with 1 base → delta -2 → [4,7)
        let mut b = buf(10);
        let mut a = ann(vec![feat(6, 9)]);
        apply_replace(&mut b, &mut a, 2, 5, b"C");
        assert_eq!(a.features[0].range, 4..7);
    }
}
