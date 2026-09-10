//! Phase 10 save round-trip: load → save → reload must preserve the
//! sequence and (for GenBank) the feature model, including flag-style
//! qualifiers and provenance.

use seqforge_bio::{load, save};
use seqforge_core::{
    Annotations, Buffer, Document, Feature, Lineage, LineageOp, Location, Primer, Strand, Topology,
};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

/// A unique temp path with the given extension, cleaned up by `Drop`.
struct TempOut(PathBuf);

impl TempOut {
    /// A temp path unique to this call.
    ///
    /// The clock is **not** a unique id — `SystemTime::now()` can return the
    /// same value for consecutive calls — and two tests can share a `tag`
    /// (`pUC19` is used twice), so a timestamped name let parallel tests collide
    /// on one path. A monotonic counter is unique by construction.
    fn new(tag: &str, ext: &str) -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let mut p = std::env::temp_dir();
        p.push(format!(
            "seqforge_rt_{tag}_{}_{n}.{ext}",
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

/// Two `TempOut`s must never share a path, even with the same tag — `pUC19` is
/// used by two tests that run in parallel.
#[test]
fn temp_out_paths_are_unique_per_call() {
    let a = TempOut::new("pUC19", "gb");
    let b = TempOut::new("pUC19", "gb");
    assert_ne!(a.path(), b.path());
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

/// A tailed primer must survive a trip through a *foreign* tool too, so the
/// interoperable note is written alongside `/seqforge_primer` — and appended to
/// the record's own description rather than replacing it.
#[test]
fn tailed_primer_gains_an_interoperable_sequence_note() {
    let buf = Buffer::new(
        "interop".into(),
        None,
        b"AAAACGTACGTAAAA".to_vec(),
        Topology::Linear,
    );
    let mut ann = Annotations::new(vec![]);
    let mut qualifiers = BTreeMap::new();
    qualifiers.insert("note".to_string(), Some("cloning primer".to_string()));
    ann.add_primer(Primer {
        id: Default::default(),
        name: "tailed".into(),
        sequence: "GGGGGCGTACGT".into(),
        binding: Some(seqforge_core::Span::from_range(4..10)),
        strand: Strand::Forward,
        qualifiers,
    });

    let out = TempOut::new("interop", "gb");
    save(&buf, &ann, out.path()).expect("save");
    let text = std::fs::read_to_string(out.path()).unwrap();
    let flat: String = text
        .split(['\n', '\r'])
        .map(str::trim)
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        flat.contains("sequence: GGGGGCGTACGT"),
        "no interoperable note written:\n{text}"
    );
    assert!(
        flat.contains("cloning primer"),
        "the record's own note must not be replaced:\n{text}"
    );

    // And it still reloads as the same primer, twice over (idempotent).
    let doc2 = load(out.path()).expect("reload");
    assert_eq!(doc2.primers[0].sequence, "GGGGGCGTACGT");
    let (buf2, ann2) = shell(doc2);
    let out2 = TempOut::new("interop2", "gb");
    save(&buf2, &ann2, out2.path()).expect("save again");
    let doc3 = load(out2.path()).expect("reload again");
    assert_eq!(doc3.primers[0].sequence, "GGGGGCGTACGT");
    assert_eq!(
        doc3.primers[0].qualifiers,
        ann2.primers().next().unwrap().qualifiers,
        "a second round trip must not keep appending"
    );
}

/// An untailed primer needs no note — reconstruction recovers it exactly, so the
/// file's own wording is left alone.
#[test]
fn untailed_primer_gets_no_extra_note() {
    let buf = Buffer::new(
        "plain".into(),
        None,
        b"AAAACGTACGTAAAA".to_vec(),
        Topology::Linear,
    );
    let mut ann = Annotations::new(vec![]);
    ann.add_primer(Primer {
        id: Default::default(),
        name: "plain".into(),
        sequence: "CGTACG".into(),
        binding: Some(seqforge_core::Span::from_range(4..10)),
        strand: Strand::Forward,
        qualifiers: BTreeMap::new(),
    });
    let out = TempOut::new("plain", "gb");
    save(&buf, &ann, out.path()).expect("save");
    let text = std::fs::read_to_string(out.path()).unwrap();
    assert!(!text.contains("sequence:"), "unexpected note:\n{text}");
}

// ── Foreign (SnapGene / Benchling) primer notes ────────────────────────────────

/// Write a minimal GenBank with one `primer_bind` carrying a foreign-style note.
fn foreign_primer_file(tag: &str, seq: &str, location: &str, note: &str) -> TempOut {
    let out = TempOut::new(tag, "gb");
    let wrapped: String = seq
        .as_bytes()
        .chunks(60)
        .enumerate()
        .map(|(i, chunk)| {
            format!(
                "{:>9} {}\n",
                i * 60 + 1,
                String::from_utf8_lossy(chunk).to_lowercase()
            )
        })
        .collect();
    std::fs::write(
        out.path(),
        format!(
            "LOCUS       foreign  {} bp    DNA     linear   UNK 01-JAN-1980\n\
             FEATURES             Location/Qualifiers\n\
             \x20    primer_bind     {location}\n\
             \x20                    /label=P1\n\
             \x20                    /note=\"{note}\"\n\
             ORIGIN\n{wrapped}//\n",
            seq.len()
        ),
    )
    .unwrap();
    out
}

/// A cloning primer's 5' tail has no template coordinates, so reconstructing the
/// oligo from the footprint truncates it. SnapGene records the full oligo in the
/// note; honouring it recovers the reagent instead of a truncation.
#[test]
fn snapgene_sequence_note_recovers_a_tailed_primer() {
    // template[5..25] is the annealed region; the oligo adds an 8 nt BsaI tail.
    let template = "TTTTTGGCATTACGCAGGATCCAAGTTTTT";
    let anneal = &template[5..25];
    let oligo = format!("GGTCTCAG{anneal}");
    let out = foreign_primer_file(
        "snapgene_tail",
        template,
        "6..25",
        &format!("color: black; sequence: {oligo}; added: 2020-11-14"),
    );

    let doc = load(out.path()).expect("load");
    assert_eq!(doc.primers.len(), 1);
    let p = &doc.primers[0];
    assert_eq!(p.sequence, oligo, "the full tailed oligo must be recovered");
    assert_eq!(
        p.binding.unwrap(),
        seqforge_core::Span::from_range(5..25),
        "the binding stays the annealed footprint"
    );
    assert_eq!(p.sequence.len() - p.binding.unwrap().len, 8, "tail length");
}

/// A reverse record's note holds the oligo as authored (bottom-strand sense), so
/// the consistency check compares against the footprint's reverse complement.
#[test]
fn snapgene_sequence_note_recovers_a_tailed_reverse_primer() {
    let template = "TTTTTGGCATTACGCAGGATCCAAGTTTTT";
    // revcomp(template[5..25]) plus an 8 nt tail.
    let anneal = "CTTGGATCCTGCGTAATGCC";
    let oligo = format!("GGTCTCAG{anneal}");
    let out = foreign_primer_file(
        "snapgene_tail_rev",
        template,
        "complement(6..25)",
        &format!("sequence: {oligo}"),
    );

    let doc = load(out.path()).expect("load");
    let p = &doc.primers[0];
    assert_eq!(p.sequence, oligo, "reverse oligo recovered verbatim");
    assert_eq!(p.strand, Strand::Reverse);
    assert_eq!(p.sequence.len() - p.binding.unwrap().len, 8);
}

/// A note that does not agree with what actually anneals is stale, or belongs to
/// another record. Adopting it would swap a correct reagent for a wrong one, so
/// the best-effort reconstruction wins instead.
#[test]
fn inconsistent_sequence_note_is_rejected() {
    let template = "TTTTTGGCATTACGCAGGATCCAAGTTTTT";
    let out = foreign_primer_file(
        "snapgene_stale",
        template,
        "6..25",
        "color: black; sequence: GGTCTCAGACGTACGTACGTACGTACGT",
    );

    let doc = load(out.path()).expect("load");
    let p = &doc.primers[0];
    assert_eq!(
        p.sequence, "GGCATTACGCAGGATCCAAG",
        "a note that doesn't match the footprint must not be trusted"
    );
}

/// No sequence note at all — the pre-existing behaviour is unchanged.
#[test]
fn primer_without_a_sequence_note_still_falls_back() {
    let template = "TTTTTGGCATTACGCAGGATCCAAGTTTTT";
    let out = foreign_primer_file(
        "snapgene_none",
        template,
        "6..25",
        "color: #75c6a9; direction: RIGHT",
    );

    let doc = load(out.path()).expect("load");
    assert_eq!(doc.primers[0].sequence, "GGCATTACGCAGGATCCAAG");
}
