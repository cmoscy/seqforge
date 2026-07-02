//! Phase 10 save round-trip: load → save → reload must preserve the
//! sequence and (for GenBank) the feature model, including flag-style
//! qualifiers and provenance.

use seqforge_bio::{load, save};
use seqforge_core::{Annotations, Buffer, Document, Feature, Primer, Provenance, Strand, Topology};
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
    (buf, Annotations::from_parts(doc.features, doc.primers))
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

// ── Primer round-trip (Phase 0.3: primer_bind ↔ Primer) ─────────────────────────

fn assert_primers_eq(a: &[Primer], b: &[Primer]) {
    assert_eq!(a.len(), b.len(), "primer count differs");
    for (x, y) in a.iter().zip(b) {
        assert_eq!(x.binding, y.binding, "binding");
        assert_eq!(x.strand, y.strand, "strand");
        assert_eq!(x.sequence, y.sequence, "sequence");
        assert_eq!(x.name, y.name, "name");
        assert_eq!(x.qualifiers, y.qualifiers, "qualifiers");
    }
}

#[test]
fn puc19_primer_binds_load_as_primers_not_features() {
    let doc = load(&fixture("pUC19.gbk")).expect("load pUC19");
    assert!(
        !doc.primers.is_empty(),
        "pUC19's primer_bind records should become primers"
    );
    // The diversion is total: no feature keeps the primer_bind kind.
    assert!(
        doc.features.iter().all(|f| f.raw_kind != "primer_bind"),
        "primer_bind must not remain a Feature"
    );
    // Each primer carries a footprint and a directional strand.
    for p in &doc.primers {
        assert!(p.binding.is_some(), "loaded primer should be attached");
        assert!(matches!(p.strand, Strand::Forward | Strand::Reverse));
        assert!(
            !p.sequence.is_empty(),
            "best-effort oligo should be derived"
        );
    }
}

#[test]
fn roundtrip_puc19_preserves_primers() {
    let doc1 = load(&fixture("pUC19.gbk")).expect("load pUC19");
    let (buf, ann) = shell(doc1);
    let out = TempOut::new("pUC19", "gb");
    save(&buf, &ann, out.path()).expect("save gb");

    let doc2 = load(out.path()).expect("reload gb");
    assert_eq!(buf.text, doc2.sequence, "sequence changed on round-trip");
    assert_primers_eq(&ann.primers().cloned().collect::<Vec<_>>(), &doc2.primers);
    // Primers are emitted from `primers` only — no primer_bind leaked into features.
    assert!(doc2.features.iter().all(|f| f.raw_kind != "primer_bind"));
}

#[test]
fn authored_primer_with_five_prime_tail_round_trips_losslessly() {
    // The 5' tail ("GGGGG") has no template counterpart, so it survives only via
    // the /seqforge_primer note — the reason a primer can't be a Feature.
    let buf = Buffer::new(
        "tail_test".into(),
        None,
        b"AAAACGTACGTAAAA".to_vec(),
        Topology::Linear,
    );
    let mut ann = Annotations::new(vec![]);
    ann.add_primer(Primer {
        id: Default::default(),
        name: "tailed_fwd".into(),
        sequence: "GGGGGCGTACGT".into(), // tail + footprint
        binding: Some(4..10),
        strand: Strand::Forward,
        qualifiers: std::collections::BTreeMap::new(),
    });

    let out = TempOut::new("tail", "gb");
    save(&buf, &ann, out.path()).expect("save gb");
    let doc2 = load(out.path()).expect("reload gb");

    assert_eq!(doc2.primers.len(), 1);
    let p = &doc2.primers[0];
    assert_eq!(p.sequence, "GGGGGCGTACGT", "5' tail must survive verbatim");
    assert_eq!(p.binding, Some(4..10));
    assert_eq!(p.strand, Strand::Forward);
    assert_eq!(p.name, "tailed_fwd");
}

#[test]
fn detached_primer_is_skipped_on_write() {
    // A detached primer (binding = None) has no primer_bind location to write; it
    // is skipped rather than crashing. Attached primers still round-trip.
    let buf = Buffer::new(
        "det".into(),
        None,
        b"ACGTACGTACGT".to_vec(),
        Topology::Linear,
    );
    let mut ann = Annotations::new(vec![]);
    ann.add_primer(Primer {
        id: Default::default(),
        name: "floating".into(),
        sequence: "TTTTTT".into(),
        binding: None,
        strand: Strand::Forward,
        qualifiers: std::collections::BTreeMap::new(),
    });
    ann.add_primer(Primer {
        id: Default::default(),
        name: "attached".into(),
        sequence: "ACGT".into(),
        binding: Some(0..4),
        strand: Strand::Forward,
        qualifiers: std::collections::BTreeMap::new(),
    });

    let out = TempOut::new("detached", "gb");
    save(&buf, &ann, out.path()).expect("save gb");
    let doc2 = load(out.path()).expect("reload gb");

    assert_eq!(
        doc2.primers.len(),
        1,
        "only the attached primer is written back"
    );
    assert_eq!(doc2.primers[0].name, "attached");
}
