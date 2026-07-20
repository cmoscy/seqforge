//! Integration: multi-enzyme digest of the pUC19 fixture (Restriction Tier 2).

use seqforge_core::{Annotations, End, MethylContext, Topology};
use std::path::Path;

fn load_puc19() -> (Vec<u8>, bool) {
    let doc = seqforge_bio::load(Path::new("tests/fixtures/pUC19.gbk")).expect("load pUC19");
    let circular = matches!(doc.topology, Topology::Circular);
    (doc.sequence, circular)
}

#[test]
fn ecori_single_cut_circular_gives_one_full_length_linear_fragment() {
    let (seq, circular) = load_puc19();
    let (frags, warnings) = seqforge_bio::digest_fragments(
        &seq,
        &Annotations::default(),
        &["EcoRI"],
        circular,
        "pUC19",
        &MethylContext::NONE,
    );
    assert_eq!(
        frags.len(),
        1,
        "one EcoRI site on a circle → one linear piece"
    );
    assert_eq!(frags[0].topology, Topology::Linear);
    assert_eq!(frags[0].len(), 2686);
    // Both ends are the same EcoRI cut (5' AATT).
    assert_eq!(frags[0].left_cut_by(), Some("EcoRI"));
    assert_eq!(frags[0].right_cut_by(), Some("EcoRI"));
    assert!(matches!(&frags[0].left, End::Overhang { seq, .. } if seq == b"AATT"));
    assert!(warnings.is_empty());
}

#[test]
fn ecori_pst_i_double_digest_gives_two_fragments_covering_the_plasmid() {
    let (seq, circular) = load_puc19();
    let (frags, _) = seqforge_bio::digest_fragments(
        &seq,
        &Annotations::default(),
        &["EcoRI", "PstI"],
        circular,
        "pUC19",
        &MethylContext::NONE,
    );
    assert_eq!(
        frags.len(),
        2,
        "two single-cutters on a circle → two fragments"
    );
    let total: usize = frags.iter().map(|f| f.len()).sum();
    assert_eq!(total, 2686, "fragments partition the whole plasmid");
    for f in &frags {
        assert_eq!(f.topology, Topology::Linear);
    }
}

#[test]
fn digest_top_strands_reassemble_to_a_rotation_of_the_input() {
    let (seq, circular) = load_puc19();
    let (frags, _) = seqforge_bio::digest_fragments(
        &seq,
        &Annotations::default(),
        &["EcoRI", "PstI", "HindIII"],
        circular,
        "pUC19",
        &MethylContext::NONE,
    );
    let mut joined = Vec::new();
    for f in &frags {
        joined.extend_from_slice(f.bytes());
    }
    let up: Vec<u8> = seq.iter().map(|b| b.to_ascii_uppercase()).collect();
    assert_eq!(joined.len(), up.len());
    // A circular digest reconstructs a rotation: `joined` is a substring of `up+up`.
    let mut doubled = up.clone();
    doubled.extend_from_slice(&up);
    assert!(
        doubled
            .windows(joined.len())
            .any(|w| w == joined.as_slice()),
        "reassembled top strand must be a rotation of the input"
    );
}
