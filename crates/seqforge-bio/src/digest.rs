//! Digest bridge — `restriction::digest` geometry → `core::Fragment`.
//!
//! `seqforge-bio` is the only crate that names `seqforge-restriction`, so the
//! zero-dep geometry (`RestrictionFragment`/`EndGeom`) is bridged to the rich
//! `core::Fragment` here — the same `Site → CutSite` pattern as `search.rs`.
//! Per fragment we run [`transport::extract`] to inherit the source's features
//! (re-homed, straddlers clamped + fuzzy-marked) and stamp a `LineageOp::Digest`
//! recording the two boundary enzymes; the per-end `cut_by` is then read off
//! that op (`Fragment::left_cut_by`), never duplicated onto the `End`.
//!
//! Fragments are **virtual values** — nothing is materialized to a buffer here
//! (ROADMAP decision 25).

use seqforge_core::document::{Lineage, LineageOp};
use seqforge_core::{
    Annotations, End, Fragment, MethylContext, OverhangSide, PartialPolicy, Span, Topology,
    transport,
};
use seqforge_restriction::{
    EndGeom, Enzyme, FragmentTopology, OverhangKind, digest as restriction_digest,
};

/// Digest `text` with the named enzymes, yielding virtual [`Fragment`]s over the
/// source plus any methylation warnings. Unknown enzyme names are dropped.
/// Methylation-blocked sites are excluded under `methylation` (decision 18).
pub fn digest_fragments(
    text: &[u8],
    ann: &Annotations,
    enzyme_names: &[&str],
    circular: bool,
    source_doc: &str,
    methylation: &MethylContext,
) -> (Vec<Fragment>, Vec<String>) {
    let enzymes: Vec<&'static Enzyme> = enzyme_names
        .iter()
        .filter_map(|n| seqforge_restriction::enzyme_by_name(n))
        .collect();

    let rs_methyl = seqforge_restriction::MethylContext {
        dam: methylation.dam,
        dcm: methylation.dcm,
        cpg: methylation.cpg,
    };

    let result = restriction_digest(text, &enzymes, circular, &rs_methyl);

    let fragments = result
        .fragments
        .into_iter()
        .map(|rf| {
            let span = Span::new(rf.span_start, rf.span_len);

            // Inherit annotations across the fragment. Straddling features are
            // clamped + fuzzy-marked; straddling primers detach → dropped.
            let mut slice =
                transport::extract(text, ann, span, PartialPolicy::TruncatePartials, source_doc);
            slice.primers.retain(|p| p.binding.is_some());

            Fragment {
                left: to_end(&rf.left),
                right: to_end(&rf.right),
                topology: to_topology(rf.topology),
                lineage: Lineage {
                    source_doc: source_doc.to_string(),
                    source_range: rf.span_start..rf.span_start + rf.span_len,
                    op: LineageOp::Digest {
                        left: rf.left.enzyme.map(str::to_string),
                        right: rf.right.enzyme.map(str::to_string),
                    },
                },
                slice,
            }
        })
        .collect();

    (fragments, result.warnings)
}

fn to_end(g: &EndGeom) -> End {
    match g.kind {
        OverhangKind::Blunt => End::Blunt,
        OverhangKind::FivePrime(_) => End::Overhang {
            side: OverhangSide::FivePrime,
            seq: g.seq.clone(),
        },
        OverhangKind::ThreePrime(_) => End::Overhang {
            side: OverhangSide::ThreePrime,
            seq: g.seq.clone(),
        },
    }
}

fn to_topology(t: FragmentTopology) -> Topology {
    match t {
        FragmentTopology::Linear => Topology::Linear,
        FragmentTopology::Circular => Topology::Circular,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use seqforge_core::Strand;
    use seqforge_core::document::{Feature, Location};
    use std::collections::BTreeMap;

    fn feature(start: usize, end: usize) -> Feature {
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

    #[test]
    fn ecori_digest_yields_two_fragments_with_cut_by() {
        let text = b"AAAGAATTCTTT";
        let ann = Annotations::default();
        let (frags, warnings) =
            digest_fragments(text, &ann, &["EcoRI"], false, "src", &MethylContext::NONE);
        assert_eq!(frags.len(), 2);
        assert!(warnings.is_empty());

        // Left fragment: native 5' terminus, EcoRI-cut 3' end.
        assert_eq!(frags[0].left, End::Blunt);
        assert!(matches!(
            frags[0].right,
            End::Overhang {
                side: OverhangSide::FivePrime,
                ..
            }
        ));
        assert_eq!(frags[0].left_cut_by(), None);
        assert_eq!(frags[0].right_cut_by(), Some("EcoRI"));
        // Right fragment: EcoRI-cut 5' end, native 3' terminus.
        assert_eq!(frags[1].left_cut_by(), Some("EcoRI"));
        assert_eq!(frags[1].right_cut_by(), None);

        // Top-strand bytes partition the input.
        let mut joined = Vec::new();
        for f in &frags {
            joined.extend_from_slice(f.bytes());
        }
        assert_eq!(joined, text);
    }

    #[test]
    fn features_rehome_into_fragment_local_coords() {
        // A feature fully inside the second fragment re-homes to local coords.
        let text = b"AAAGAATTCTTTTTT"; // cut after position 4 (G^AATTC)
        let mut ann = Annotations::default();
        ann.add(feature(9, 12)); // in the right fragment
        let (frags, _) =
            digest_fragments(text, &ann, &["EcoRI"], false, "src", &MethylContext::NONE);
        assert_eq!(frags.len(), 2);
        // The feature should land on the right fragment, shifted left.
        let right = &frags[1];
        assert_eq!(right.slice.features.len(), 1);
        let f = &right.slice.features[0];
        // Right fragment starts at top_cut = 4, so the feature at 9 → local 5.
        assert_eq!(f.location.bounds(right.len()).start, 5);
        // Inherited features carry extract lineage (the fragment op is on the fragment).
        assert!(f.lineage.is_some());
    }

    #[test]
    fn methylation_blocked_site_drops_a_boundary() {
        let text = b"AAAAGATCAAAA";
        let ann = Annotations::default();
        let (blocked, warns) = digest_fragments(
            text,
            &ann,
            &["MboI"],
            false,
            "src",
            &MethylContext::default(),
        );
        let (cut, _) = digest_fragments(text, &ann, &["MboI"], false, "src", &MethylContext::NONE);
        assert!(blocked.len() < cut.len());
        assert!(warns.iter().any(|w| w.contains("blocked")));
    }

    #[test]
    fn product_is_fragment_closure_smoke() {
        // A product IS a fragment: its slice can be re-digested.
        let text = b"AAAGAATTCTTTGAATTCAAA";
        let ann = Annotations::default();
        let (frags, _) =
            digest_fragments(text, &ann, &["EcoRI"], false, "src", &MethylContext::NONE);
        let mid = &frags[1];
        let (re, _) = digest_fragments(
            mid.bytes(),
            &Annotations::default(),
            &["EcoRI"],
            false,
            "frag",
            &MethylContext::NONE,
        );
        assert!(!re.is_empty());
    }
}
