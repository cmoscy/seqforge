use seqforge_bio::load;
use seqforge_core::Topology;
use std::path::PathBuf;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

#[test]
fn loads_fasta_small_linear() {
    let doc = load(&fixture("small_linear.fasta")).expect("should load");
    assert_eq!(doc.name, "small_linear_test");
    assert_eq!(doc.topology, Topology::Linear);
    assert!(!doc.is_empty(), "sequence should not be empty");
    assert!(
        doc.sequence.iter().all(|&b| b.is_ascii_uppercase()),
        "all bases should be uppercase"
    );
}

#[test]
fn loads_genbank_circular() {
    let doc = load(&fixture("circular_plasmid.gb")).expect("should load");
    assert_eq!(doc.topology, Topology::Circular);
    assert_eq!(doc.len(), 200, "sequence length should match LOCUS line");
    let non_source = doc
        .features
        .iter()
        .filter(|f| !matches!(f.kind, seqforge_core::FeatureKind::Source))
        .count();
    assert!(
        non_source >= 2,
        "should have at least 2 non-source features"
    );
}

#[test]
fn loads_genbank_multi_feature() {
    let doc = load(&fixture("multi_feature.gb")).expect("should load");
    assert_eq!(doc.topology, Topology::Linear);
    assert_eq!(doc.len(), 300);
    // source + promoter + 2 genes + 2 CDS + terminator = 7
    assert!(doc.features.len() >= 5, "should have at least 5 features");
}

#[test]
fn linear_vs_circular_topology_preserved() {
    let linear = load(&fixture("multi_feature.gb")).expect("should load");
    let circular = load(&fixture("circular_plasmid.gb")).expect("should load");
    assert_eq!(linear.topology, Topology::Linear);
    assert_eq!(circular.topology, Topology::Circular);
}

#[test]
fn unsupported_format_returns_error() {
    let result = load(std::path::Path::new("test.xyz"));
    assert!(result.is_err());
    let msg = result.unwrap_err().to_string();
    assert!(msg.contains("xyz") || msg.contains("Unsupported"));
}
