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

use seqforge_core::commands::FragmentInfo;
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

/// Resolve an enzyme `query` against `seq`, then digest. Returns the fragments,
/// any methylation warnings, and the **canonical** enzyme-name string (stored on
/// a Fragments view so a re-run is identical).
///
/// This is the shared middle of every digest path. Three callers had copied it
/// inline — the viewer, `seqforge digest`, and fragment export — and they drifted
/// on methylation, on the `circular` override, and on enzyme-query
/// normalization (ROADMAP decision 27).
///
/// `circular` is a parameter rather than read off a `Buffer`, because the
/// callers legitimately disagree: the viewer takes the document's topology,
/// while `seqforge digest --circular` overrides it. Passing it in is what lets
/// one function serve both without a mode flag.
pub fn digest_resolved(
    seq: &[u8],
    name: &str,
    circular: bool,
    ann: &Annotations,
    query: &str,
    methyl: &MethylContext,
) -> (Vec<Fragment>, Vec<String>, String) {
    let parsed = crate::parse_enzyme_query(query);
    let names = crate::resolve_query_names(&parsed, seq, circular);
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let (frags, warnings) = digest_fragments(seq, ann, &refs, circular, name, methyl);
    (frags, warnings, names.join(" "))
}

/// [`digest_resolved`] projected to the serializable [`FragmentInfo`] shape that
/// both the Fragments view and the CLI/socket response render.
pub fn digest_projection(
    seq: &[u8],
    name: &str,
    circular: bool,
    ann: &Annotations,
    query: &str,
    methyl: &MethylContext,
) -> (Vec<FragmentInfo>, Vec<String>, String) {
    let (frags, warnings, canonical) = digest_resolved(seq, name, circular, ann, query, methyl);
    let infos = frags
        .iter()
        .enumerate()
        .map(|(i, f)| f.to_info(i))
        .collect();
    (infos, warnings, canonical)
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

    // ── The shared projection (ROADMAP decision 27) ───────────────────────
    //
    // These pin the two arguments that used to be hardcoded differently by each
    // of the three inline copies. They are the properties that make one
    // implementation safe to share.

    /// The viewer passed the view's authored methylation; `seqforge digest`
    /// passed `MethylContext::default()` unconditionally. Same file, two
    /// answers, no signal to the user. The context is now a parameter that
    /// demonstrably reaches the digest.
    #[test]
    fn projection_honours_the_methylation_argument() {
        let text = b"AAAAGATCAAAA";
        let ann = Annotations::default();

        let (blocked, warns, _) =
            digest_projection(text, "src", false, &ann, "MboI", &MethylContext::default());
        let (cut, _, _) = digest_projection(text, "src", false, &ann, "MboI", &MethylContext::NONE);

        assert!(
            blocked.len() < cut.len(),
            "Dam-blocked MboI must yield fewer fragments through the projection"
        );
        assert!(warns.iter().any(|w| w.contains("blocked")));
    }

    /// `circular` is the caller's decision, not a property read off a buffer:
    /// the viewer takes the document's topology, `seqforge digest --circular`
    /// overrides it. The override was inexpressible over the socket.
    #[test]
    fn circular_is_the_callers_decision() {
        let text = b"AAAGAATTCTTTGAATTCAAA";
        let ann = Annotations::default();

        let (linear, _, _) =
            digest_projection(text, "src", false, &ann, "EcoRI", &MethylContext::NONE);
        let (circular, _, _) =
            digest_projection(text, "src", true, &ann, "EcoRI", &MethylContext::NONE);

        // Two cuts: linear gives 3 pieces, circular gives 2.
        assert_eq!(linear.len(), 3);
        assert_eq!(circular.len(), 2);
    }

    /// The projection is exactly `digest_resolved` plus the `FragmentInfo`
    /// mapping — so the fragment-export path (which needs `Fragment`, not
    /// `FragmentInfo`) and the list path cannot diverge.
    #[test]
    fn projection_is_resolved_plus_the_info_mapping() {
        let text = b"AAAGAATTCTTTGAATTCAAA";
        let ann = Annotations::default();

        let (frags, warns_a, canon_a) =
            digest_resolved(text, "src", false, &ann, "EcoRI", &MethylContext::NONE);
        let (infos, warns_b, canon_b) =
            digest_projection(text, "src", false, &ann, "EcoRI", &MethylContext::NONE);

        assert_eq!(frags.len(), infos.len());
        assert_eq!(warns_a, warns_b);
        assert_eq!(canon_a, canon_b);
        assert_eq!(
            canon_a, "EcoRI",
            "canonical name string is what a re-run replays"
        );
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
