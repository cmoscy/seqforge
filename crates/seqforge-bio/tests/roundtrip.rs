//! Phase 10 save round-trip: load → save → reload must preserve the
//! sequence and (for GenBank) the feature model, including flag-style
//! qualifiers and provenance.

use seqforge_bio::{load, save};
use seqforge_core::{Annotations, Buffer, Document, Feature, Provenance, Strand, Topology};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

/// A unique temp path with the given extension, cleaned up by `Drop`.
struct TempOut(PathBuf);

impl TempOut {
    fn new(tag: &str, ext: &str) -> Self {
        let mut p = std::env::temp_dir();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        p.push(format!(
            "seqforge_rt_{tag}_{}_{nanos}.{ext}",
            std::process::id()
        ));
        TempOut(p)
    }
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempOut {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn shell(doc: Document) -> (Buffer, Annotations) {
    let buf = Buffer::new(doc.name, doc.source_path, doc.sequence, doc.topology);
    (buf, Annotations::new(doc.features))
}

fn assert_features_eq(a: &[Feature], b: &[Feature]) {
    assert_eq!(a.len(), b.len(), "feature count differs");
    for (x, y) in a.iter().zip(b) {
        assert_eq!(x.range, y.range, "range");
        assert_eq!(x.raw_kind, y.raw_kind, "raw_kind");
        assert_eq!(x.strand, y.strand, "strand");
        assert_eq!(x.label, y.label, "label");
        assert_eq!(x.qualifiers, y.qualifiers, "qualifiers");
        assert_eq!(x.provenance, y.provenance, "provenance");
    }
}

/// load fixture → save .gb → reload; sequence + features must be stable.
fn roundtrip_gb(name: &str) {
    let doc1 = load(&fixture(name)).expect("load fixture");
    let (buf, ann) = shell(doc1);
    let out = TempOut::new(name, "gb");
    save(&buf, &ann, out.path()).expect("save gb");

    let doc2 = load(out.path()).expect("reload gb");
    assert_eq!(buf.text, doc2.sequence, "sequence changed on round-trip");
    assert_eq!(buf.topology, doc2.topology, "topology changed");
    assert_features_eq(&ann.iter().cloned().collect::<Vec<_>>(), &doc2.features);
}

#[test]
fn roundtrip_circular_plasmid() {
    roundtrip_gb("circular_plasmid.gb");
}

#[test]
fn roundtrip_multi_feature() {
    roundtrip_gb("multi_feature.gb");
}

#[test]
fn roundtrip_small_linear_fasta() {
    let doc1 = load(&fixture("small_linear.fasta")).expect("load fasta");
    let (buf, ann) = shell(doc1);
    let out = TempOut::new("small_linear", "fasta");
    save(&buf, &ann, out.path()).expect("save fasta");

    let doc2 = load(out.path()).expect("reload fasta");
    assert_eq!(buf.text, doc2.sequence, "fasta sequence changed");
    assert_eq!(doc2.topology, Topology::Linear);
}

#[test]
fn roundtrip_preserves_provenance_and_flag_qualifiers() {
    let mut qualifiers = BTreeMap::new();
    qualifiers.insert("label".to_string(), Some("myCDS".to_string()));
    // Flag-style qualifier: no value. Must survive as `None`.
    qualifiers.insert("pseudo".to_string(), None);

    let feature = Feature {
        id: Default::default(),
        range: 10..40,
        raw_kind: "CDS".to_string(),
        label: "myCDS".to_string(),
        strand: Strand::Reverse,
        qualifiers,
        provenance: Some(Provenance {
            source_doc: "pUC19".to_string(),
            source_range: 100..130,
            operation: "GoldenGate(BsaI)".to_string(),
        }),
    };

    let buf = Buffer::new(
        "prov_test".to_string(),
        None,
        b"ATGCATGCATGCATGCATGCATGCATGCATGCATGCATGCATGC".to_vec(),
        Topology::Circular,
    );
    let ann = Annotations::new(vec![feature.clone()]);

    let out = TempOut::new("provenance", "gb");
    save(&buf, &ann, out.path()).expect("save gb");
    let doc2 = load(out.path()).expect("reload gb");

    assert_features_eq(&ann.iter().cloned().collect::<Vec<_>>(), &doc2.features);
    let reloaded = &doc2.features[0];
    assert_eq!(
        reloaded.provenance.as_ref().unwrap().operation,
        "GoldenGate(BsaI)"
    );
    assert_eq!(reloaded.qualifiers.get("pseudo"), Some(&None));
    assert_eq!(reloaded.strand, Strand::Reverse);
}
