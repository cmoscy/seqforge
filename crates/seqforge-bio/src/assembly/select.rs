//! Fragment selection — narrow a digest pool to the 5′→3′ fragment (decision 26).
//!
//! Boundary matching is **directional** — a fragment's left boundary must match
//! `five_prime` and its right boundary `three_prime`, in that order — so the two
//! arcs of a 2-cut molecule (backbone vs stuffer) are distinguishable.
//!
//! Same enzyme twice (`BsaI..BsaI`) with exactly two sites and no `@pos`: bind
//! 5′ → lowest cut coordinate and 3′ → the other (min→max). Flipping the span
//! (GUI ⇄ → `max..min` pins) selects the other arc. Right-end `@pos` compares
//! modulo the source length so wrapping circular fragments match.

use seqforge_core::{Boundary, Fragment, SpanEnds};

/// Keep the fragment whose left matches `span.five_prime` and right matches
/// `span.three_prime` (after same-enzyme defaults).
pub(super) fn by_span(
    span: &SpanEnds,
    frags: Vec<Fragment>,
    ctx: &str,
    warnings: &mut Vec<String>,
) -> Vec<Fragment> {
    let (five, three) = resolve_same_enzyme_defaults(span, &frags, ctx, warnings);
    directional(frags, &five, &three, ctx, warnings)
}

/// For `E..E` with both `at` unset and exactly two cut positions in the pool,
/// pin 5′ → `min(pos)` and 3′ → `max(pos)`. More than two sites without `@pos`
/// warns and leaves the span unbound (directional will then fail).
fn resolve_same_enzyme_defaults(
    span: &SpanEnds,
    frags: &[Fragment],
    ctx: &str,
    warnings: &mut Vec<String>,
) -> (Boundary, Boundary) {
    match (&span.five_prime, &span.three_prime) {
        (
            Boundary::EnzymeSite {
                enzyme: a,
                at: None,
            },
            Boundary::EnzymeSite {
                enzyme: b,
                at: None,
            },
        ) if a == b && !a.is_empty() => {
            // Each cut appears as exactly one fragment's left boundary.
            let mut positions: Vec<usize> = frags
                .iter()
                .filter(|f| f.left_cut_by() == Some(a.as_str()))
                .map(|f| f.lineage.source_range.start)
                .collect();
            positions.sort_unstable();
            positions.dedup();
            match positions.as_slice() {
                [lo, hi] => (
                    Boundary::enzyme_at(a.clone(), *lo),
                    Boundary::enzyme_at(a.clone(), *hi),
                ),
                positions if positions.len() > 2 => {
                    warnings.push(format!(
                        "{ctx}: {a} cuts {}× — pin @pos on the ambiguous end(s)",
                        positions.len()
                    ));
                    (span.five_prime.clone(), span.three_prime.clone())
                }
                _ => (span.five_prime.clone(), span.three_prime.clone()),
            }
        }
        _ => (span.five_prime.clone(), span.three_prime.clone()),
    }
}

/// The fragment whose **left** boundary matches `a` and **right** boundary
/// matches `b` (directional — order distinguishes the two arcs).
pub(super) fn directional(
    frags: Vec<Fragment>,
    a: &Boundary,
    b: &Boundary,
    ctx: &str,
    warnings: &mut Vec<String>,
) -> Vec<Fragment> {
    // Full digest of a molecule: Σ fragment lengths = source length (used so
    // wrapping circular right ends compare equal to `@pos` cut coordinates).
    let mol_len: usize = frags.iter().map(Fragment::len).sum();
    match frags.into_iter().find(|f| {
        matches_side(f, a, Side::Left, mol_len) && matches_side(f, b, Side::Right, mol_len)
    }) {
        Some(f) => vec![f],
        None => {
            warnings.push(format!("{ctx}: no fragment bounded by {a}..{b}"));
            Vec::new()
        }
    }
}

#[derive(Clone, Copy)]
enum Side {
    Left,
    Right,
}

/// Whether one side of a fragment satisfies a [`Boundary`]. The side's source
/// coordinate is the corresponding end of `lineage.source_range` (which the
/// digest partitions at top-strand cut points, so it equals the enzyme's cut
/// position — the `at` occurrence tiebreaker). Right ends of wrapping circular
/// fragments may be `≥ mol_len`; compare `@pos` modulo `mol_len`.
fn matches_side(f: &Fragment, b: &Boundary, side: Side, mol_len: usize) -> bool {
    let (cut_by, pos) = match side {
        Side::Left => (f.left_cut_by(), f.lineage.source_range.start),
        Side::Right => (f.right_cut_by(), f.lineage.source_range.end),
    };
    match b {
        Boundary::EnzymeSite { enzyme, at } => {
            cut_by == Some(enzyme.as_str()) && at.is_none_or(|p| cut_pos_eq(pos, p, mol_len))
        }
        Boundary::Coordinate(n) => cut_pos_eq(pos, *n, mol_len),
        Boundary::Terminus => cut_by.is_none(),
        Boundary::FeatureEdge {
            feature,
            side: fside,
        } => feature_edge_at(f, feature, *fside, side),
    }
}

fn cut_pos_eq(raw: usize, want: usize, mol_len: usize) -> bool {
    if raw == want {
        return true;
    }
    mol_len > 0 && raw % mol_len == want % mol_len
}

/// A feature edge coincides with a fragment boundary when the named feature's
/// 5′/3′ edge sits at the fragment's local start (left) or end (right).
fn feature_edge_at(
    f: &Fragment,
    name: &str,
    fside: seqforge_core::FeatureSide,
    side: Side,
) -> bool {
    let target = match side {
        Side::Left => 0,
        Side::Right => f.len(),
    };
    f.slice.features.iter().any(|ft| {
        if ft.label != name {
            return false;
        }
        let bounds = ft.location.bounds(f.len());
        let edge = match fside {
            seqforge_core::FeatureSide::Five => bounds.start,
            seqforge_core::FeatureSide::Three => bounds.end,
        };
        edge == target
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use seqforge_core::document::{Feature, Lineage, LineageOp, Location, Topology};
    use seqforge_core::{Boundary, End, SeqSlice, Strand};
    use std::collections::BTreeMap;

    fn frag(
        len: usize,
        source_range: std::ops::Range<usize>,
        left: Option<&str>,
        right: Option<&str>,
        feature: Option<&str>,
    ) -> Fragment {
        let features = feature
            .map(|label| {
                vec![Feature {
                    id: Default::default(),
                    location: Location::simple(0..len),
                    raw_kind: "misc_feature".into(),
                    label: label.into(),
                    strand: Strand::Forward,
                    qualifiers: BTreeMap::new(),
                    lineage: None,
                }]
            })
            .unwrap_or_default();
        Fragment {
            slice: SeqSlice {
                bytes: vec![b'A'; len],
                features,
                primers: Vec::new(),
            },
            left: End::Blunt,
            right: End::Blunt,
            topology: Topology::Linear,
            lineage: Lineage {
                source_doc: "src".into(),
                source_range,
                op: LineageOp::Digest {
                    left: left.map(str::to_string),
                    right: right.map(str::to_string),
                },
            },
        }
    }

    fn enzyme(name: &str) -> Boundary {
        Boundary::enzyme(name)
    }

    /// The backbone-vs-stuffer pool: a 2-cut molecule's two arcs share the enzyme
    /// *set* {EcoRI, PstI} but differ in **order**.
    fn two_arcs() -> Vec<Fragment> {
        vec![
            frag(12, 0..12, Some("EcoRI"), Some("PstI"), Some("ori")),
            frag(8, 12..20, Some("PstI"), Some("EcoRI"), Some("mcs")),
        ]
    }

    /// Same enzyme twice on a 52-bp circle: cuts at 10 and 40.
    /// Non-wrapping arc 10→40 (30 bp); wrapping arc 40→10 (22 bp, end=62).
    fn same_enzyme_arcs() -> Vec<Fragment> {
        vec![
            frag(30, 10..40, Some("BsaI"), Some("BsaI"), None),
            frag(22, 40..62, Some("BsaI"), Some("BsaI"), None),
        ]
    }

    #[test]
    fn directional_distinguishes_the_two_arcs() {
        let mut w = Vec::new();
        let bb = directional(two_arcs(), &enzyme("EcoRI"), &enzyme("PstI"), "ctx", &mut w);
        assert_eq!(bb.len(), 1);
        assert_eq!(bb[0].len(), 12, "EcoRI→PstI is the backbone arc");

        let st = directional(two_arcs(), &enzyme("PstI"), &enzyme("EcoRI"), "ctx", &mut w);
        assert_eq!(st.len(), 1);
        assert_eq!(st[0].len(), 8, "PstI→EcoRI is the stuffer arc");
        assert!(w.is_empty());
    }

    #[test]
    fn at_occurrence_tiebreaker_matches_the_cut_position() {
        let mut w = Vec::new();
        let hit = directional(
            two_arcs(),
            &Boundary::enzyme_at("EcoRI", 0),
            &Boundary::enzyme_at("PstI", 12),
            "ctx",
            &mut w,
        );
        assert_eq!(hit.len(), 1);
        let miss = directional(
            two_arcs(),
            &Boundary::enzyme_at("EcoRI", 999),
            &enzyme("PstI"),
            "ctx",
            &mut w,
        );
        assert!(miss.is_empty());
        assert_eq!(w.len(), 1);
    }

    #[test]
    fn by_span_picks_the_directional_arc() {
        let mut w = Vec::new();
        let span = SpanEnds::new(enzyme("EcoRI"), enzyme("PstI"));
        let kept = by_span(&span, two_arcs(), "ctx", &mut w);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].len(), 12);
    }

    #[test]
    fn unique_ecori_psti_needs_no_at_pin() {
        let mut w = Vec::new();
        let span = SpanEnds::new(enzyme("EcoRI"), enzyme("PstI"));
        let kept = by_span(&span, two_arcs(), "ctx", &mut w);
        assert_eq!(kept.len(), 1);
        assert!(w.is_empty());
    }

    #[test]
    fn same_enzyme_two_sites_defaults_to_min_max_walk() {
        let mut w = Vec::new();
        let span = SpanEnds::new(enzyme("BsaI"), enzyme("BsaI"));
        let kept = by_span(&span, same_enzyme_arcs(), "ctx", &mut w);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].len(), 30, "default min→max is the 10..40 arc");
        assert!(w.is_empty());
    }

    #[test]
    fn same_enzyme_flip_selects_the_other_arc() {
        let mut w = Vec::new();
        // Explicit max..min (GUI ⇄ after defaulting) selects the wrapping arc.
        let span = SpanEnds::new(
            Boundary::enzyme_at("BsaI", 40),
            Boundary::enzyme_at("BsaI", 10),
        );
        let kept = by_span(&span, same_enzyme_arcs(), "ctx", &mut w);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].len(), 22, "flipped ends pick the wrapping arc");
        assert!(w.is_empty());
    }
}
