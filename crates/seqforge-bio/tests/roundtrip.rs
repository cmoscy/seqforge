//! Phase 10 save round-trip: load → save → reload must preserve the
//! sequence and (for GenBank) the feature model, including flag-style
//! qualifiers and provenance.

use seqforge_bio::{load, save};
use seqforge_core::{
    Annotations, Buffer, Document, Feature, Lineage, LineageOp, Location, Primer, Strand, Topology,
};
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
        assert_eq!(x.location, y.location, "location");
        assert_eq!(x.raw_kind, y.raw_kind, "raw_kind");
        assert_eq!(x.strand, y.strand, "strand");
        assert_eq!(x.label, y.label, "label");
        assert_eq!(x.qualifiers, y.qualifiers, "qualifiers");
        assert_eq!(x.lineage, y.lineage, "lineage");
    }
}

/// Collapse internal whitespace runs (incl. newlines) in a qualifier value.
fn normalize_ws(v: &Option<String>) -> Option<String> {
    v.as_ref()
        .map(|s| s.split_whitespace().collect::<Vec<_>>().join(" "))
}

/// Like [`assert_features_eq`] but compares free-text qualifier *values* with
/// whitespace normalized. Real GenBank files (e.g. NEB pUC19) hand-wrap `/note`
/// text across lines; gb-io reflows that on write, so byte-exact value equality
/// is a third-party formatting artifact, not a model change. Keys, ranges,
/// kinds, strands, labels, and provenance are still asserted exactly.
fn assert_features_eq_reflow_tolerant(a: &[Feature], b: &[Feature]) {
    assert_eq!(a.len(), b.len(), "feature count differs");
    for (x, y) in a.iter().zip(b) {
        assert_eq!(x.location, y.location, "location");
        assert_eq!(x.raw_kind, y.raw_kind, "raw_kind");
        assert_eq!(x.strand, y.strand, "strand");
        assert_eq!(x.label, y.label, "label");
        assert_eq!(x.lineage, y.lineage, "lineage");
        let keys_a: Vec<&String> = x.qualifiers.keys().collect();
        let keys_b: Vec<&String> = y.qualifiers.keys().collect();
        assert_eq!(keys_a, keys_b, "qualifier keys");
        for (k, va) in &x.qualifiers {
            assert_eq!(
                normalize_ws(va),
                normalize_ws(&y.qualifiers[k]),
                "qualifier `{k}` value (whitespace-normalized)"
            );
        }
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
fn roundtrip_puc19() {
    // Real NEB pUC19 — circular, feature-rich; the strongest fidelity anchor.
    // Uses the reflow-tolerant comparison: gb-io re-wraps hand-wrapped /note
    // text on write, so free-text values round-trip whitespace-normalized, not
    // byte-exact (a known GenBank limitation — see B follow-up in the roadmap).
    let doc1 = load(&fixture("pUC19.gbk")).expect("load pUC19");
    let (buf, ann) = shell(doc1);
    let out = TempOut::new("pUC19", "gb");
    save(&buf, &ann, out.path()).expect("save gb");
    let doc2 = load(out.path()).expect("reload gb");
    assert_eq!(buf.text, doc2.sequence, "sequence changed on round-trip");
    assert_eq!(buf.topology, doc2.topology, "topology changed");
    assert_features_eq_reflow_tolerant(&ann.iter().cloned().collect::<Vec<_>>(), &doc2.features);
}

#[test]
fn puc19_origin_join_normalizes_to_wrapping_simple_and_round_trips() {
    // pUC19's rep_origin is `join(2315..2686,1..217)` — a single ~589 bp region
    // that crosses the origin. On a circular molecule it must ingest as ONE
    // wrapping `Simple`, not a `Join` (plans/span.md decision 1); export inverts
    // to the original `join(...)` bytes for a fixed-point round-trip.
    use seqforge_core::Span;

    let doc1 = load(&fixture("pUC19.gbk")).expect("load pUC19");
    let len = doc1.sequence.len();
    assert_eq!(len, 2686);

    let ori = doc1
        .features
        .iter()
        .find(|f| f.raw_kind == "rep_origin")
        .expect("pUC19 has a rep_origin");
    // One wrapping Simple: start 2314 (0-based of 2315), len 372 + 217 = 589.
    assert_eq!(
        ori.location,
        Location::from_span(Span::new(2314, 589)),
        "origin-crossing join must fold to a single wrapping Simple"
    );
    assert!(ori.location.contains(2500, len), "covers the head arm");
    assert!(
        ori.location.contains(100, len),
        "covers the wrapped tail arm"
    );
    assert!(!ori.location.contains(1000, len), "excludes the interior");
    assert_eq!(ori.location.pieces(len), vec![2314..2686, 0..217]);
    let ori_location = ori.location.clone();

    // Export: the wrapping Simple must emit the original join(...) bytes.
    let (buf, ann) = shell(doc1);
    let out = TempOut::new("puc19_ori", "gb");
    save(&buf, &ann, out.path()).expect("save gb");
    let written = std::fs::read_to_string(out.path()).expect("read back");
    assert!(
        written.contains("join(2315..2686,1..217)"),
        "wrapping Simple must export as the origin join(...); rep_origin lines:\n{}",
        written
            .lines()
            .filter(|l| l.contains("2315") || l.contains("rep_origin"))
            .collect::<Vec<_>>()
            .join("\n")
    );

    // Fixed point: reload → identical wrapping Simple (load→save→load stable).
    let doc2 = load(out.path()).expect("reload gb");
    let ori2 = doc2
        .features
        .iter()
        .find(|f| f.raw_kind == "rep_origin")
        .expect("rep_origin survives round-trip");
    assert_eq!(
        ori_location, ori2.location,
        "origin geometry not fixed-point across load→save→load"
    );
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
        location: seqforge_core::Location::simple(10..40),
        raw_kind: "CDS".to_string(),
        label: "myCDS".to_string(),
        strand: Strand::Reverse,
        qualifiers,
        lineage: Some(Lineage {
            source_doc: "pUC19".to_string(),
            source_range: 100..130,
            op: LineageOp::Extract,
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
    assert_eq!(reloaded.lineage.as_ref().unwrap().op, LineageOp::Extract);
    assert_eq!(reloaded.qualifiers.get("pseudo"), Some(&None));
    assert_eq!(reloaded.strand, Strand::Reverse);
}

// ── Location round-trip (F0: no flattening of join/fuzzy/complement) ────────────

/// Save a single feature carrying `location`/`strand` on a 60 bp molecule, then
/// reload it — returning the reloaded feature so its geometry can be asserted.
fn roundtrip_location(tag: &str, location: Location, strand: Strand) -> Feature {
    let feature = Feature {
        id: Default::default(),
        location,
        raw_kind: "CDS".to_string(),
        label: "geneA".to_string(),
        strand,
        qualifiers: {
            let mut q = BTreeMap::new();
            q.insert("label".to_string(), Some("geneA".to_string()));
            q
        },
        lineage: None,
    };
    let buf = Buffer::new(
        "loc_test".to_string(),
        None,
        vec![b'A'; 60],
        Topology::Linear,
    );
    let ann = Annotations::new(vec![feature]);
    let out = TempOut::new(tag, "gb");
    save(&buf, &ann, out.path()).expect("save gb");
    let doc2 = load(out.path()).expect("reload gb");
    doc2.features.into_iter().next().expect("one feature")
}

#[test]
fn roundtrip_join_preserves_segments() {
    // A spliced CDS: join(11..20, 30..40) must NOT collapse to the 11..40 hull.
    let loc = Location::Join(vec![Location::simple(11..20), Location::simple(30..40)]);
    let r = roundtrip_location("loc_join", loc.clone(), Strand::Forward);
    assert_eq!(r.location, loc, "join segments preserved (no flatten)");
    assert_eq!(r.bounds(60), 11..40, "hull spans all segments");
    assert_eq!(r.strand, Strand::Forward);
}

#[test]
fn roundtrip_before_fuzzy_preserved() {
    // `<10..40` — a 5'-truncated feature.
    let loc = Location::Simple {
        span: seqforge_core::Span::from_range(10..40),
        before: true,
        after: false,
    };
    let r = roundtrip_location("loc_before", loc.clone(), Strand::Forward);
    assert_eq!(r.location, loc, "before (<) fuzzy preserved");
}

#[test]
fn roundtrip_after_fuzzy_preserved() {
    // `10..>40` — a 3'-truncated feature.
    let loc = Location::Simple {
        span: seqforge_core::Span::from_range(10..40),
        before: false,
        after: true,
    };
    let r = roundtrip_location("loc_after", loc.clone(), Strand::Forward);
    assert_eq!(r.location, loc, "after (>) fuzzy preserved");
}

#[test]
fn roundtrip_complement_join_is_reverse_with_segments() {
    // `complement(join(11..20,30..40))`: overall strand normalizes into
    // `Feature.strand = Reverse`; the geometry stays a strand-free Join.
    let geom = Location::Join(vec![Location::simple(11..20), Location::simple(30..40)]);
    let r = roundtrip_location("loc_comp", geom.clone(), Strand::Reverse);
    assert_eq!(
        r.strand,
        Strand::Reverse,
        "outer complement → reverse strand"
    );
    assert_eq!(r.location, geom, "join geometry preserved under complement");
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
        binding: Some(seqforge_core::Span::from_range(4..10)),
        strand: Strand::Forward,
        qualifiers: std::collections::BTreeMap::new(),
    });

    let out = TempOut::new("tail", "gb");
    save(&buf, &ann, out.path()).expect("save gb");
    let doc2 = load(out.path()).expect("reload gb");

    assert_eq!(doc2.primers.len(), 1);
    let p = &doc2.primers[0];
    assert_eq!(p.sequence, "GGGGGCGTACGT", "5' tail must survive verbatim");
    assert_eq!(p.binding, Some(seqforge_core::Span::from_range(4..10)));
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
        binding: Some(seqforge_core::Span::from_range(0..4)),
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
