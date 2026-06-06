//! Smoke test: end-to-end query resolution against the pUC19 fixture.

use std::path::Path;

fn load_puc19() -> (Vec<u8>, bool) {
    let path = Path::new("tests/fixtures/pUC19.gbk");
    let doc = seqforge_bio::load(path).expect("load pUC19");
    let circular = matches!(doc.topology, seqforge_core::Topology::Circular);
    (doc.sequence, circular)
}

#[test]
fn puc19_loads_and_is_canonical_size() {
    let (seq, circular) = load_puc19();
    assert_eq!(seq.len(), 2686, "pUC19 is canonically 2686 bp");
    assert!(circular, "pUC19 is a circular plasmid");
}

#[test]
fn puc19_has_well_known_unique_cutters() {
    // pUC19 is famous for its MCS unique-cutter set; among them EcoRI,
    // BamHI, HindIII, PstI, SalI, KpnI are all single-cutters. We don't
    // need to verify all of them — confirming a few well-known ones is
    // enough to prove the pipeline works end-to-end on real DNA.
    let (seq, circular) = load_puc19();
    let (names, sites) = seqforge_bio::resolve_query(
        &seqforge_bio::parse_enzyme_query("unique"),
        &seq,
        circular,
    );

    assert!(!names.is_empty(), "pUC19 has many unique cutters");
    for expected in ["EcoRI", "BamHI", "HindIII", "PstI"] {
        assert!(
            names.iter().any(|n| n == expected),
            "{expected} should be a unique cutter in pUC19. got {} names",
            names.len()
        );
        assert_eq!(
            sites.iter().filter(|s| s.enzyme == expected).count(),
            1,
            "{expected} should appear exactly once"
        );
    }
}

#[test]
fn puc19_unique_or_dual_is_superset_of_unique() {
    let (seq, circular) = load_puc19();
    let (unique_names, _) = seqforge_bio::resolve_query(
        &seqforge_bio::parse_enzyme_query("unique"),
        &seq,
        circular,
    );
    let (dual_names, _) = seqforge_bio::resolve_query(
        &seqforge_bio::parse_enzyme_query("unique and dual"),
        &seq,
        circular,
    );
    assert!(dual_names.len() >= unique_names.len());
    for n in &unique_names {
        assert!(
            dual_names.contains(n),
            "{n} appears in unique but not unique-or-dual"
        );
    }
}

#[test]
fn puc19_type_iis_query_runs_without_panic() {
    // We don't assert specific Type IIs hits — pUC19 may or may not happen
    // to have BsaI/BsmBI sites. What matters here is that the Type IIs
    // preset resolves cleanly on a real plasmid (no panic, sensible result).
    let (seq, circular) = load_puc19();
    let (names, sites) = seqforge_bio::resolve_query(
        &seqforge_bio::parse_enzyme_query("type iis"),
        &seq,
        circular,
    );
    // If the plasmid has any IIs sites, every site's enzyme must appear in
    // names; if it has none, both are empty.
    assert_eq!(sites.is_empty(), names.is_empty());
}
