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

use crate::span::Span;
use crate::{Annotations, Buffer, Location, Strand};

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
    shift_primers(ann, start, removed, inserted);

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
///
/// The policy applies **per segment** of a feature's [`Location`]: each leaf
/// range is shifted/clamped independently and dropped if it collapses; a
/// feature is dropped only when *every* leaf collapses. For the common
/// single-`Simple` feature this is exactly the pre-`Location` behavior.
fn shift_features(ann: &mut Annotations, start: usize, removed: usize, inserted: usize) {
    ann.features.retain_mut(
        |f| match shift_location(&f.location, start, removed, inserted) {
            Some(loc) => {
                f.location = loc;
                true
            }
            None => false,
        },
    );
}

/// Apply the splice-shift policy to one range. Returns the shifted range, or
/// `None` if it collapses (fully inside the removed region, or a straddle whose
/// body is entirely cut) — the caller drops it.
///
/// This is the leaf op behind [`shift_location`] (via `Location::map_spans`); the
/// same left/right/straddle classification is shared by [`shift_primers`] (which
/// detaches instead of dropping).
pub(crate) fn shift_range(
    r: &Range<usize>,
    start: usize,
    removed: usize,
    inserted: usize,
) -> Option<Range<usize>> {
    let end = start + removed; // end of the removed region
    let delta = inserted as isize - removed as isize;
    let (fs, fe) = (r.start, r.end);

    // Fully left of the edit — untouched.
    if fe <= start {
        return Some(fs..fe);
    }
    // Fully right of the removed region — shift both ends by delta.
    if fs >= end {
        return Some((fs as isize + delta) as usize..(fe as isize + delta) as usize);
    }
    // Fully inside the removed region — destroyed.
    if fs >= start && fe <= end {
        return None;
    }
    // Straddles: clamp the overlap to the edit point, apply delta at/after `end`.
    let new_start = fs.min(start);
    let new_end = if fe > end {
        (fe as isize + delta) as usize
    } else {
        start // tail cut — clamp to the edit point
    };
    (new_start < new_end).then_some(new_start..new_end)
}

/// Apply the splice-shift policy over a [`Location`] via `Location::map_spans`
/// (the shared Simple/Complement/Join recursion); the leaf is [`shift_range`],
/// which drops a segment that collapses. Returns `None` if every segment drops.
///
/// P1: no `Simple` wraps yet, so a leaf span is treated as the plain linear range
/// `Span::range` for the splice-clamp policy.
fn shift_location(
    loc: &Location,
    start: usize,
    removed: usize,
    inserted: usize,
) -> Option<Location> {
    loc.map_spans(&|span| {
        shift_range(&span.range(), start, removed, inserted).map(Span::from_range)
    })
}

/// Apply the **primer**-specific shift policy for a splice (ROADMAP decision 14;
/// consistency note #1 in `plans/primers.md`).
///
/// Shares the offset math with [`shift_features`] but with one load-bearing
/// difference: a primer is a *reagent*, not a sub-range, so it is **never
/// dropped**. When an edit destroys the primer's **3' terminus** — the anchor
/// where priming/extension begins — the primer *detaches* (`binding = None`, the
/// `Detached` state) but the reagent survives. The 3' terminus is `binding.end`
/// for a `Forward` primer and `binding.start` for a `Reverse` one; an insertion
/// (empty removed region) can never destroy it.
fn shift_primers(ann: &mut Annotations, start: usize, removed: usize, inserted: usize) {
    let end = start + removed; // end of the removed region
    let delta = inserted as isize - removed as isize;

    for p in &mut ann.primers {
        let (bs, be) = match &p.binding {
            // Linear footprint end (`start + len`); primers don't wrap yet.
            Some(s) => (s.start, s.start + s.len),
            None => continue, // already detached — nothing to track
        };

        // Fully left of the edit — untouched.
        if be <= start {
            continue;
        }

        // Fully right of the removed region — translate the whole footprint.
        if bs >= end {
            p.binding = p.binding.map(|s| s.shift(delta));
            continue;
        }

        // From here the footprint overlaps the removed region `[start, end)`.
        // Detach if the 3' anchor base was removed; the removed indices are
        // `[start, end)`.
        let anchor_destroyed = match p.strand {
            // 3' base at index be-1; removed iff be-1 ∈ [start, end).
            Strand::Forward => start < be && be <= end,
            // 3' base at index bs; removed iff bs ∈ [start, end).
            Strand::Reverse => start <= bs && bs < end,
            // A directionless primer is unusual; treat any overlap with the
            // removed region conservatively as a detach (either terminus may be
            // load-bearing).
            Strand::Both | Strand::None => true,
        };
        if anchor_destroyed {
            p.binding = None; // Detached — but the primer is kept.
            continue;
        }

        // Anchor survived: clamp the overlap to the edit point, shifting whatever
        // lies at/after `end` by delta (mirrors the `shift_features` straddle).
        let new_start = bs.min(start);
        let new_end = if be > end {
            (be as isize + delta) as usize
        } else {
            start
        };
        // The surviving-anchor cases never collapse (the anchor lies outside the
        // removed region), but guard defensively: a collapse detaches, never drops.
        p.binding = (new_end > new_start).then_some(Span::from_range(new_start..new_end));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Feature, Location, Primer, Strand, Topology};
    use std::collections::BTreeMap;

    fn buf(len: usize) -> Buffer {
        Buffer::new("t".into(), None, vec![b'A'; len], Topology::Linear)
    }

    fn feat(start: usize, end: usize) -> Feature {
        Feature {
            id: Default::default(),
            location: Location::simple(start..end),
            raw_kind: "misc_feature".into(),
            label: "f".into(),
            strand: Strand::Forward,
            qualifiers: BTreeMap::new(),
            lineage: None,
        }
    }

    fn ann(features: Vec<Feature>) -> Annotations {
        Annotations::new(features)
    }

    fn primer(binding: Option<Range<usize>>, strand: Strand) -> Primer {
        Primer {
            id: Default::default(),
            name: "p".into(),
            sequence: "ACGT".into(),
            binding: binding.map(Span::from_range),
            strand,
            qualifiers: BTreeMap::new(),
        }
    }

    /// One-primer annotations for shift tests.
    fn ann_primer(binding: Option<Range<usize>>, strand: Strand) -> Annotations {
        let mut a = Annotations::new(vec![]);
        a.add_primer(primer(binding, strand));
        a
    }

    /// The (only) primer's current binding, as a linear range for comparison.
    fn binding0(a: &Annotations) -> Option<Range<usize>> {
        a.primers()
            .next()
            .unwrap()
            .binding
            .map(|s| s.start..s.start + s.len)
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
        assert_eq!(a.features[0].bounds(b.text.len()), 7..10);
    }

    #[test]
    fn insert_right_of_feature_untouched() {
        // feature [2,5); insert at pos 8 → unchanged
        let mut b = buf(10);
        let mut a = ann(vec![feat(2, 5)]);
        apply_insert(&mut b, &mut a, 8, b"CC");
        assert_eq!(a.features[0].bounds(b.text.len()), 2..5);
    }

    #[test]
    fn insert_inside_feature_extends_end() {
        // feature [2,8); insert 3 at pos 5 → [2,11)
        let mut b = buf(10);
        let mut a = ann(vec![feat(2, 8)]);
        apply_insert(&mut b, &mut a, 5, b"CCC");
        assert_eq!(a.features[0].bounds(b.text.len()), 2..11);
    }

    #[test]
    fn insert_at_feature_start_shifts() {
        // feature [5,8); insert at pos 5 (== start) → right-shift to [7,10)
        let mut b = buf(10);
        let mut a = ann(vec![feat(5, 8)]);
        apply_insert(&mut b, &mut a, 5, b"CC");
        assert_eq!(a.features[0].bounds(b.text.len()), 7..10);
    }

    // ── delete cases (§2) ──────────────────────────────────────────────────────

    #[test]
    fn delete_left_of_feature_untouched() {
        // feature [5,8); delete [0,2) → fully right, shift by -2 → [3,6)
        let mut b = buf(10);
        let mut a = ann(vec![feat(5, 8)]);
        apply_delete(&mut b, &mut a, 0, 2);
        assert_eq!(a.features[0].bounds(b.text.len()), 3..6);
    }

    #[test]
    fn delete_fully_right_is_untouched() {
        // feature [2,5); delete [7,9) → left of edit, untouched
        let mut b = buf(10);
        let mut a = ann(vec![feat(2, 5)]);
        apply_delete(&mut b, &mut a, 7, 9);
        assert_eq!(a.features[0].bounds(b.text.len()), 2..5);
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
        assert_eq!(a.features[0].bounds(b.text.len()), 2..4);
    }

    #[test]
    fn delete_straddles_end_pulls_start() {
        // feature [4,8); delete [2,6) → start<=feat.start<end, feat.end>end
        // → start=start(2), end -= n(4) → [2,4)
        let mut b = buf(10);
        let mut a = ann(vec![feat(4, 8)]);
        apply_delete(&mut b, &mut a, 2, 6);
        assert_eq!(a.features[0].bounds(b.text.len()), 2..4);
    }

    #[test]
    fn delete_spanned_by_feature_contracts() {
        // feature [2,9); delete [4,6) (n=2) → feat.start<start && feat.end>end
        // → end -= 2 → [2,7)
        let mut b = buf(10);
        let mut a = ann(vec![feat(2, 9)]);
        apply_delete(&mut b, &mut a, 4, 6);
        assert_eq!(a.features[0].bounds(b.text.len()), 2..7);
    }

    // ── replace (§2: one op, delta shift) ──────────────────────────────────────

    #[test]
    fn replace_shifts_right_feature_by_delta() {
        // feature [6,9); replace [2,4) (len 2) with 5 bases → delta +3 → [9,12)
        let mut b = buf(10);
        let mut a = ann(vec![feat(6, 9)]);
        apply_replace(&mut b, &mut a, 2, 4, b"CCCCC");
        assert_eq!(a.features[0].bounds(b.text.len()), 9..12);
        assert_eq!(b.text.len(), 13);
    }

    #[test]
    fn replace_negative_delta_shifts_left() {
        // feature [6,9); replace [2,5) (len 3) with 1 base → delta -2 → [4,7)
        let mut b = buf(10);
        let mut a = ann(vec![feat(6, 9)]);
        apply_replace(&mut b, &mut a, 2, 5, b"C");
        assert_eq!(a.features[0].bounds(b.text.len()), 4..7);
    }

    // ── primer shift policy (decision 14 / consistency #1) ──────────────────────
    //
    // Load-bearing difference from features: a primer is NEVER dropped. An edit
    // destroying its 3' anchor detaches it (binding = None); everything else
    // tracks position. 3' anchor = binding.end for Forward, binding.start for
    // Reverse.

    #[test]
    fn primer_insert_left_shifts_right() {
        // Forward [4,8); insert 2 at pos 1 → fully right of edit → [6,10).
        let mut b = buf(10);
        let mut a = ann_primer(Some(4..8), Strand::Forward);
        apply_insert(&mut b, &mut a, 1, b"CC");
        assert_eq!(binding0(&a), Some(6..10));
    }

    #[test]
    fn primer_delete_right_untouched() {
        // Forward [2,5); delete [7,9) → left of edit → untouched.
        let mut b = buf(10);
        let mut a = ann_primer(Some(2..5), Strand::Forward);
        apply_delete(&mut b, &mut a, 7, 9);
        assert_eq!(binding0(&a), Some(2..5));
    }

    #[test]
    fn primer_forward_delete_of_three_prime_anchor_detaches_but_keeps() {
        // Forward [4,8); 3' anchor at index 7. delete [7,9) removes it → detach.
        let mut b = buf(10);
        let mut a = ann_primer(Some(4..8), Strand::Forward);
        apply_delete(&mut b, &mut a, 7, 9);
        assert_eq!(binding0(&a), None, "3' anchor destroyed → Detached");
        assert_eq!(a.primers_len(), 1, "the reagent is never dropped");
    }

    #[test]
    fn primer_forward_delete_of_five_prime_keeps_anchor_attached() {
        // Forward [4,8); 3' anchor at index 7. delete [2,6) cuts the 5' side but
        // spares the anchor → stays attached, footprint clamped/shifted to [2,4)
        // (the surviving anchor base, was idx 7, is now idx 3 ∈ [2,4)).
        let mut b = buf(10);
        let mut a = ann_primer(Some(4..8), Strand::Forward);
        apply_delete(&mut b, &mut a, 2, 6);
        assert_eq!(binding0(&a), Some(2..4));
    }

    #[test]
    fn primer_reverse_delete_of_three_prime_anchor_detaches() {
        // Reverse [4,8); 3' anchor at index 4 (start). delete [4,6) removes it.
        let mut b = buf(10);
        let mut a = ann_primer(Some(4..8), Strand::Reverse);
        apply_delete(&mut b, &mut a, 4, 6);
        assert_eq!(binding0(&a), None);
        assert_eq!(a.primers_len(), 1);
    }

    #[test]
    fn primer_reverse_delete_of_high_end_keeps_anchor_attached() {
        // Reverse [4,8); 3' anchor at index 4. delete [6,8) trims the far (5') end
        // but spares the anchor → stays attached, [4,6).
        let mut b = buf(10);
        let mut a = ann_primer(Some(4..8), Strand::Reverse);
        apply_delete(&mut b, &mut a, 6, 8);
        assert_eq!(binding0(&a), Some(4..6));
    }

    #[test]
    fn primer_delete_fully_inside_detaches_either_strand() {
        // Footprint entirely within the removed region → both termini gone.
        for strand in [Strand::Forward, Strand::Reverse] {
            let mut b = buf(12);
            let mut a = ann_primer(Some(4..8), strand);
            apply_delete(&mut b, &mut a, 2, 10);
            assert_eq!(binding0(&a), None, "{strand:?}");
            assert_eq!(a.primers_len(), 1, "{strand:?}");
        }
    }

    #[test]
    fn primer_insertion_never_detaches() {
        // An insertion (empty removed region) can't destroy an anchor, whatever
        // the strand or position — the primer stays attached.
        for strand in [Strand::Forward, Strand::Reverse] {
            for pos in [0, 4, 6, 8, 10] {
                let mut b = buf(10);
                let mut a = ann_primer(Some(4..8), strand);
                apply_insert(&mut b, &mut a, pos, b"CC");
                assert!(
                    binding0(&a).is_some(),
                    "insert at {pos} on {strand:?} must not detach"
                );
            }
        }
    }

    #[test]
    fn primer_already_detached_stays_detached() {
        // binding = None is left untouched by any edit.
        let mut b = buf(10);
        let mut a = ann_primer(None, Strand::Forward);
        apply_delete(&mut b, &mut a, 2, 6);
        assert_eq!(binding0(&a), None);
        assert_eq!(a.primers_len(), 1);
    }

    #[test]
    fn primer_replace_shifts_fully_right_footprint() {
        // Forward [6,9); replace [2,4) (len 2) with 5 bases → delta +3 → [9,12).
        let mut b = buf(10);
        let mut a = ann_primer(Some(6..9), Strand::Forward);
        apply_replace(&mut b, &mut a, 2, 4, b"CCCCC");
        assert_eq!(binding0(&a), Some(9..12));
    }
}
