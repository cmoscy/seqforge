//! Molecule-topology ops: origin rotation and whole-molecule reverse-complement
//! of the **annotation** layer (`plans/*` buffer-lifecycle mini-phase).
//!
//! These are pure geometry over `(text, Annotations)` — no `seqforge-bio`
//! dependency. Byte *reverse-complement* needs a complement table (a `bio`
//! concern), so the caller supplies the RC'd bytes and calls
//! [`reverse_complement_circular`] to mirror the annotations to match; byte
//! *rotation* is a plain slice rotate and lives here in full.
//!
//! Linearize / Circularize are **not** here: linearizing at `at` is exactly
//! `transport::extract` of the whole circle rooted at `at` (the partial policy
//! resolves the feature crossing the seam), and circularizing is a topology-flag
//! flip (optionally preceded by [`rotate_origin`]). Only rotation and the
//! annotation-mirror are genuinely new primitives.

use crate::document::Strand;
use crate::model::Annotations;

/// Rotate a **circular** molecule of length `L` so base `n` becomes position 0
/// ("Set Origin"). Rotates the bytes (`text[n..] ++ text[..n]`) and re-homes
/// every feature/primer by `-n (mod L)`, wrap-aware: a feature that contained
/// base `n` becomes a single origin-wrapping [`Span`] (no split — topology is
/// preserved). No-op for `n % L == 0` or an empty molecule.
pub fn rotate_origin(text: &mut [u8], ann: &mut Annotations, n: usize) {
    let l = text.len();
    if l == 0 {
        return;
    }
    let n = n % l;
    if n == 0 {
        return;
    }
    text.rotate_left(n);
    for f in &mut ann.features {
        // Total transform (no leaf drops), so `map_spans` always yields `Some`.
        f.location = f
            .location
            .map_spans(&|s| Some(s.rotated(n, l)))
            .expect("rotate is total");
    }
    for p in &mut ann.primers {
        if let Some(b) = p.binding {
            p.binding = Some(b.rotated(n, l));
        }
    }
}

/// Mirror the annotation layer for a **whole-molecule** reverse-complement of a
/// molecule of length `l`: every feature/primer footprint is mirrored across
/// `[0, l)` (keeping its id) and its strand flipped. Bijective and
/// wrap-preserving (an origin-spanning feature stays one wrapping span on the
/// reverse strand). The caller reverse-complements the bytes separately (via
/// `bio`), then calls this to keep the annotations consistent.
pub fn reverse_complement_circular(ann: &mut Annotations, l: usize) {
    for f in &mut ann.features {
        f.location = f.location.mirrored(l);
        f.strand = flip_strand(f.strand);
    }
    for p in &mut ann.primers {
        if let Some(b) = p.binding {
            p.binding = Some(b.mirrored(l));
        }
        p.strand = flip_strand(p.strand);
    }
}

fn flip_strand(s: Strand) -> Strand {
    match s {
        Strand::Forward => Strand::Reverse,
        Strand::Reverse => Strand::Forward,
        other => other, // Both / None are their own mirror
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::{Feature, FeatureId, Location, Primer, PrimerId};
    use crate::span::Span;
    use std::collections::BTreeMap;

    fn feat(loc: Location, strand: Strand) -> Feature {
        Feature {
            id: FeatureId::default(),
            location: loc,
            raw_kind: "misc_feature".into(),
            label: "f".into(),
            strand,
            qualifiers: BTreeMap::new(),
            provenance: None,
        }
    }

    fn primer(binding: Option<Span>, strand: Strand) -> Primer {
        Primer {
            id: PrimerId::default(),
            name: "p".into(),
            sequence: "ACGT".into(),
            binding,
            strand,
            qualifiers: BTreeMap::new(),
        }
    }

    #[test]
    fn rotate_moves_bytes_and_features() {
        let mut text = b"AAAACCCCGGGGTTTT".to_vec(); // L=16
        let mut ann = Annotations::new(vec![feat(Location::simple(4..8), Strand::Forward)]);
        rotate_origin(&mut text, &mut ann, 4);
        assert_eq!(text, b"CCCCGGGGTTTTAAAA");
        // feature 4..8 -> 0..4
        assert_eq!(ann.features[0].location.as_span(), Some(Span::new(0, 4)));
    }

    #[test]
    fn rotate_makes_a_seam_crossing_feature_wrap() {
        let mut text = b"AAAACCCCGGGGTTTT".to_vec(); // L=16
        // feature 2..6 straddles the new origin at n=4.
        let mut ann = Annotations::new(vec![feat(Location::simple(2..6), Strand::Forward)]);
        rotate_origin(&mut text, &mut ann, 4);
        // 2->14, 6->2: new span starts at 14, len 4 -> wraps [14..16) ∪ [0..2).
        let s = ann.features[0].location.as_span().unwrap();
        assert_eq!(s, Span::new(14, 4));
        assert!(s.wraps(16));
    }

    #[test]
    fn rotate_round_trips() {
        let mut text = b"AAAACCCCGGGGTTTT".to_vec();
        let orig = text.clone();
        let mut ann = Annotations::new(vec![feat(Location::simple(5..11), Strand::Forward)]);
        let orig_span = ann.features[0].location.as_span();
        rotate_origin(&mut text, &mut ann, 6);
        rotate_origin(&mut text, &mut ann, 16 - 6);
        assert_eq!(text, orig);
        assert_eq!(ann.features[0].location.as_span(), orig_span);
    }

    #[test]
    fn rotate_shifts_primer_binding() {
        let mut text = b"AAAACCCCGGGGTTTT".to_vec();
        let mut ann = Annotations::new(vec![]);
        ann.primers
            .push(primer(Some(Span::new(4, 4)), Strand::Forward));
        rotate_origin(&mut text, &mut ann, 4);
        assert_eq!(ann.primers[0].binding, Some(Span::new(0, 4)));
    }

    #[test]
    fn rc_mirrors_and_flips_strand() {
        // feature 5..10 on L=20 -> mirror [20-10, 20-5) = [10, 15); strand flips.
        let mut ann = Annotations::new(vec![feat(Location::simple(5..10), Strand::Forward)]);
        reverse_complement_circular(&mut ann, 20);
        assert_eq!(ann.features[0].location.as_span(), Some(Span::new(10, 5)));
        assert_eq!(ann.features[0].strand, Strand::Reverse);
    }

    #[test]
    fn rc_is_its_own_inverse() {
        let mut ann = Annotations::new(vec![feat(Location::simple(3..9), Strand::Forward)]);
        ann.primers
            .push(primer(Some(Span::new(12, 4)), Strand::Reverse));
        let f0 = ann.features[0].clone();
        let p0 = ann.primers[0].clone();
        reverse_complement_circular(&mut ann, 20);
        reverse_complement_circular(&mut ann, 20);
        assert_eq!(ann.features[0].location, f0.location);
        assert_eq!(ann.features[0].strand, f0.strand);
        assert_eq!(ann.primers[0].binding, p0.binding);
        assert_eq!(ann.primers[0].strand, p0.strand);
    }

    #[test]
    fn rc_preserves_a_wrapping_feature_arc() {
        // Origin-spanning feature [16..20) ∪ [0..4) — symmetric mirror keeps the arc.
        let mut ann = Annotations::new(vec![feat(
            Location::from_span(Span::new(16, 8)),
            Strand::Forward,
        )]);
        reverse_complement_circular(&mut ann, 20);
        let s = ann.features[0].location.as_span().unwrap();
        assert!(s.wraps(20), "mirrored arc still wraps: {s:?}");
        assert_eq!(s.len, 8);
    }
}
