//! Integration: the full assembly pipeline (`run`) over a test `SourceResolver`.

use std::collections::HashMap;

use seqforge_bio::{ResolvedSource, SourceResolver, run};
use seqforge_core::{
    Annotations, Bin, Boundary, Expand, Fragment, JoinKind, PrepareKind, Recipe, Source, SourceRef,
    Topology, TopologyIntent,
};

/// Resolves `SourceRef::Path("<name>")` against an in-memory table.
struct TestResolver {
    table: HashMap<String, (Vec<u8>, Topology)>,
}

impl SourceResolver for TestResolver {
    fn resolve(&self, r: &SourceRef) -> Result<ResolvedSource, String> {
        match r {
            SourceRef::Path(p) => {
                let key = p.to_string_lossy().to_string();
                let (bytes, topology) = self
                    .table
                    .get(&key)
                    .ok_or_else(|| format!("unknown source {key}"))?;
                Ok(ResolvedSource {
                    name: key,
                    bytes: bytes.clone(),
                    topology: *topology,
                    ann: Annotations::default(),
                })
            }
            SourceRef::Buffer(_) => Err("buffer refs unsupported in this test".into()),
        }
    }
}

fn path_source(name: &str) -> Source {
    Source {
        ref_: SourceRef::Path(name.into()),
        pin: None,
        span: None,
    }
}

fn digest_bin(role: &str, source: &str, five: &str, three: &str) -> Bin {
    Bin {
        role: role.into(),
        sources: vec![path_source(source)],
        prepare: PrepareKind::Digest {
            five_prime: Boundary::enzyme(five),
            three_prime: Boundary::enzyme(three),
        },
    }
}

fn canonical_circular(bytes: &[u8]) -> Vec<u8> {
    let rc: Vec<u8> = bytes
        .iter()
        .rev()
        .map(|b| match b.to_ascii_uppercase() {
            b'A' => b'T',
            b'T' => b'A',
            b'C' => b'G',
            b'G' => b'C',
            o => o,
        })
        .collect();
    let min_rot = |s: &[u8]| {
        (0..s.len())
            .map(|i| -> Vec<u8> { s[i..].iter().chain(s[..i].iter()).copied().collect() })
            .min()
            .unwrap_or_default()
    };
    min_rot(bytes).min(min_rot(&rc))
}

#[test]
fn two_bin_religation_reconstructs_the_plasmid() {
    // A circular plasmid with one EcoRI + one BamHI site (DISTINCT overhangs, so
    // religation is directional → exactly one circular product). Each bin picks
    // one arc via 5′→3′; Ligate/Circular fuses them back into the original.
    let plasmid = b"GAATTCAAAAGGATCCAAAA".to_vec();
    let resolver = TestResolver {
        table: HashMap::from([("P".to_string(), (plasmid.clone(), Topology::Circular))]),
    };
    let recipe = Recipe {
        bins: vec![
            digest_bin("A", "P", "EcoRI", "BamHI"),
            digest_bin("B", "P", "BamHI", "EcoRI"),
        ],
        join: JoinKind::Ligate,
        intent: TopologyIntent::Circular,
        expand: Expand::AllToAll,
        name_template: None,
    };

    let result = run(&recipe, &resolver);
    assert!(
        result.products.iter().any(|p| {
            p.fragment.topology == Topology::Circular
                && canonical_circular(p.fragment.bytes()) == canonical_circular(&plasmid)
        }),
        "expected a circular product equal to the input plasmid; warnings={:?}",
        result.warnings
    );
    assert_eq!(result.products.len(), 1);
    assert_eq!(result.products[0].name, "A+B");
}

#[test]
fn all_to_all_library_from_two_sources_in_a_bin() {
    // Combinatorial width is sources-in-bin (not "keep all digest bands").
    let plasmid = b"GAATTCAAAAGGATCCAAAA".to_vec();
    let resolver = TestResolver {
        table: HashMap::from([
            ("P".to_string(), (plasmid.clone(), Topology::Circular)),
            ("Q".to_string(), (plasmid, Topology::Circular)),
        ]),
    };
    let recipe = Recipe {
        bins: vec![
            Bin {
                role: "A".into(),
                sources: vec![path_source("P"), path_source("Q")],
                prepare: PrepareKind::Digest {
                    five_prime: Boundary::enzyme("EcoRI"),
                    three_prime: Boundary::enzyme("BamHI"),
                },
            },
            digest_bin("B", "P", "BamHI", "EcoRI"),
        ],
        join: JoinKind::Ligate,
        intent: TopologyIntent::Any,
        expand: Expand::AllToAll,
        name_template: None,
    };
    let result = run(&recipe, &resolver);
    assert!(
        result.products.len() >= 2,
        "2 sources in bin A × 1 in B → ≥2 products; got {} warnings={:?}",
        result.products.len(),
        result.warnings
    );
}

#[test]
fn file_resolver_religates_puc19_halves_to_the_plasmid() {
    use seqforge_bio::FileResolver;

    let bin = |five: &str, three: &str| Bin {
        role: "pUC19".into(),
        sources: vec![path_source("tests/fixtures/pUC19.gbk")],
        prepare: PrepareKind::Digest {
            five_prime: Boundary::enzyme(five),
            three_prime: Boundary::enzyme(three),
        },
    };
    let recipe = Recipe {
        bins: vec![bin("EcoRI", "PstI"), bin("PstI", "EcoRI")],
        join: JoinKind::Ligate,
        intent: TopologyIntent::Circular,
        expand: Expand::AllToAll,
        name_template: None,
    };
    let result = run(&recipe, &FileResolver);
    let product = result
        .products
        .iter()
        .find(|p| p.fragment.topology == Topology::Circular)
        .expect("a circular product");
    assert_eq!(product.fragment.len(), 2686, "religated pUC19 is 2686 bp");

    // Wrap-aware extract must carry ori through the wrapping digest arc so ligate
    // place/merge can restore it (decision 23 — one transport path).
    let ori = product
        .fragment
        .slice
        .features
        .iter()
        .find(|f| f.raw_kind == "rep_origin" || f.label == "ori")
        .expect("rep_origin/ori must survive EcoRI↔PstI religation");
    let ori_len = ori.location.as_span().map(|s| s.len).unwrap_or_else(|| {
        ori.location
            .pieces(product.fragment.len())
            .iter()
            .map(|r| r.end - r.start)
            .sum()
    });
    assert!(
        ori_len >= 500,
        "ori should remain ~589 bp, got {ori_len} loc={:?}",
        ori.location
    );
}

#[test]
fn same_walk_twice_does_not_assemble_circular() {
    use seqforge_bio::{FileResolver, probe_recipe};

    let bin = |five: &str, three: &str| Bin {
        role: format!("{five}..{three}"),
        sources: vec![path_source("tests/fixtures/pUC19.gbk")],
        prepare: PrepareKind::Digest {
            five_prime: Boundary::enzyme(five),
            three_prime: Boundary::enzyme(three),
        },
    };
    let recipe = Recipe {
        bins: vec![bin("EcoRI", "PstI"), bin("EcoRI", "PstI")],
        join: JoinKind::Ligate,
        intent: TopologyIntent::Circular,
        expand: Expand::AllToAll,
        name_template: None,
    };
    let probe = probe_recipe(&recipe, &FileResolver);
    assert_eq!(
        probe.compatible_combos, 0,
        "same walk ×2 must not probe as compatible"
    );
    let result = run(&recipe, &FileResolver);
    assert!(
        result.products.is_empty(),
        "strict join must not flip to rescue same-walk ×2; warnings={:?}",
        result.warnings
    );
    assert!(
        result
            .warnings
            .iter()
            .any(|w| w.contains("authored 5′/3′") || w.contains("do not match"))
    );
}

#[test]
fn both_orients_via_complementary_bins_assembles() {
    // Documented pattern: complementary walks across bins (not join flips).
    use seqforge_bio::FileResolver;

    let bin = |role: &str, five: &str, three: &str| Bin {
        role: role.into(),
        sources: vec![path_source("tests/fixtures/pUC19.gbk")],
        prepare: PrepareKind::Digest {
            five_prime: Boundary::enzyme(five),
            three_prime: Boundary::enzyme(three),
        },
    };
    let recipe = Recipe {
        bins: vec![bin("A", "EcoRI", "BamHI"), bin("B", "BamHI", "EcoRI")],
        join: JoinKind::Ligate,
        intent: TopologyIntent::Circular,
        expand: Expand::AllToAll,
        name_template: None,
    };
    let result = run(&recipe, &FileResolver);
    assert!(
        result
            .products
            .iter()
            .any(|p| p.fragment.topology == Topology::Circular),
        "complementary walks must assemble; warnings={:?}",
        result.warnings
    );
}

#[test]
fn empty_bin_is_reported_as_a_warning_not_a_panic() {
    let resolver = TestResolver {
        table: HashMap::new(),
    };
    let recipe = Recipe {
        bins: vec![digest_bin("A", "missing", "EcoRI", "EcoRI")],
        join: JoinKind::Ligate,
        intent: TopologyIntent::Circular,
        expand: Expand::AllToAll,
        name_template: None,
    };
    let result = run(&recipe, &resolver);
    assert!(result.products.is_empty());
    assert!(result.warnings.iter().any(|w| w.contains("missing")));
    let _ = std::mem::size_of::<Fragment>();
}

#[test]
fn run_indices_joins_only_selected_combos() {
    use seqforge_bio::{enumerate_combos, run_indices};

    let plasmid = b"GAATTCAAAAGGATCCAAAA".to_vec();
    let resolver = TestResolver {
        table: HashMap::from([
            ("P".to_string(), (plasmid.clone(), Topology::Circular)),
            ("Q".to_string(), (plasmid, Topology::Circular)),
        ]),
    };
    let recipe = Recipe {
        bins: vec![
            Bin {
                role: "A".into(),
                sources: vec![path_source("P"), path_source("Q")],
                prepare: PrepareKind::Digest {
                    five_prime: Boundary::enzyme("EcoRI"),
                    three_prime: Boundary::enzyme("BamHI"),
                },
            },
            digest_bin("B", "P", "BamHI", "EcoRI"),
        ],
        join: JoinKind::Ligate,
        intent: TopologyIntent::Any,
        expand: Expand::AllToAll,
        name_template: None,
    };
    let (summaries, _) = enumerate_combos(&recipe, &resolver, None);
    assert!(summaries.len() >= 2);
    let first = summaries[0].index;
    let subset = run_indices(&recipe, &resolver, &[first]);
    let full = run(&recipe, &resolver);
    assert!(
        !subset.products.is_empty(),
        "selected combo should assemble; warnings={:?}",
        subset.warnings
    );
    assert!(
        full.products.len() > subset.products.len(),
        "running all combos should yield more products than one index ({} vs {})",
        full.products.len(),
        subset.products.len()
    );
}

#[test]
fn run_indices_empty_selection_warns() {
    use seqforge_bio::run_indices;

    let plasmid = b"GAATTCAAAAGGATCCAAAA".to_vec();
    let resolver = TestResolver {
        table: HashMap::from([("P".to_string(), (plasmid, Topology::Circular))]),
    };
    let recipe = Recipe {
        bins: vec![
            digest_bin("A", "P", "EcoRI", "BamHI"),
            digest_bin("B", "P", "BamHI", "EcoRI"),
        ],
        join: JoinKind::Ligate,
        intent: TopologyIntent::Circular,
        expand: Expand::AllToAll,
        name_template: None,
    };
    let result = run_indices(&recipe, &resolver, &[]);
    assert!(result.products.is_empty());
    assert!(
        result
            .warnings
            .iter()
            .any(|w| w.contains("no combos selected"))
    );
}

/// A product carries the combo it came from and that combo's per-bin parts, so
/// a combinatorial run can be joined back to its inputs without re-expanding.
#[test]
fn products_carry_combo_index_and_parts() {
    let plasmid = b"GAATTCAAAAGGATCCAAAA".to_vec();
    let resolver = TestResolver {
        table: HashMap::from([
            ("P".to_string(), (plasmid.clone(), Topology::Circular)),
            ("Q".to_string(), (plasmid, Topology::Circular)),
        ]),
    };
    let recipe = Recipe {
        bins: vec![
            Bin {
                role: "A".into(),
                sources: vec![path_source("P"), path_source("Q")],
                prepare: PrepareKind::Digest {
                    five_prime: Boundary::enzyme("EcoRI"),
                    three_prime: Boundary::enzyme("BamHI"),
                },
            },
            digest_bin("B", "P", "BamHI", "EcoRI"),
        ],
        join: JoinKind::Ligate,
        intent: TopologyIntent::Any,
        expand: Expand::AllToAll,
        name_template: None,
    };
    let result = run(&recipe, &resolver);
    assert!(!result.products.is_empty());
    for p in &result.products {
        assert_eq!(
            p.parts.len(),
            recipe.bins.len(),
            "one part per bin, in bin order"
        );
        assert!(
            p.parts
                .iter()
                .all(|c| c.source_name == "P" || c.source_name == "Q"),
            "parts name their source: {:?}",
            p.parts.iter().map(|c| &c.source_name).collect::<Vec<_>>()
        );
    }
    // Bin A holds two sources → the run spans more than one combo.
    let distinct: std::collections::HashSet<usize> =
        result.products.iter().map(|p| p.combo_index).collect();
    assert!(distinct.len() > 1, "expected products from several combos");
}

/// `--out` / "Run → folder…": a product written as GenBank reloads with the
/// same bases and topology. This is the write path both shells call.
#[test]
fn products_round_trip_through_genbank_export() {
    use seqforge_bio::{ProductFormat, write_products};

    let plasmid = b"GAATTCAAAAGGATCCAAAA".to_vec();
    let resolver = TestResolver {
        table: HashMap::from([("P".to_string(), (plasmid, Topology::Circular))]),
    };
    let recipe = Recipe {
        bins: vec![
            digest_bin("A", "P", "EcoRI", "BamHI"),
            digest_bin("B", "P", "BamHI", "EcoRI"),
        ],
        join: JoinKind::Ligate,
        intent: TopologyIntent::Circular,
        expand: Expand::AllToAll,
        name_template: Some("export-{bin0}-{n}".into()),
    };
    let result = run(&recipe, &resolver);
    assert!(!result.products.is_empty(), "{:?}", result.warnings);

    let dir = std::env::temp_dir().join("seqforge-export-roundtrip");
    let _ = std::fs::remove_dir_all(&dir);
    let paths = write_products(&result.products, &dir, ProductFormat::GenBank).unwrap();
    assert_eq!(paths.len(), result.products.len());

    for (product, path) in result.products.iter().zip(&paths) {
        assert!(path.exists(), "{} was not written", path.display());
        let reloaded = seqforge_bio::load(path).unwrap();
        assert_eq!(
            reloaded.sequence, product.fragment.slice.bytes,
            "bases survive the round trip"
        );
        assert_eq!(reloaded.topology, product.fragment.topology);
    }
    // The name template drove the filename, not `role+role`.
    assert!(
        paths[0]
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("export-P-"),
        "unexpected filename {:?}",
        paths[0]
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// `--origin Start`: the product is rotated to a named landmark, its features
/// travel with it, and the molecule is unchanged as a circle.
#[test]
fn origin_rotates_the_product_to_a_named_feature() {
    use seqforge_bio::{OriginSpec, set_origins};
    use seqforge_core::{Feature, Location};

    let plasmid = b"GAATTCAAAAGGATCCAAAA".to_vec();
    let resolver = TestResolver {
        table: HashMap::from([("P".to_string(), (plasmid, Topology::Circular))]),
    };
    let recipe = Recipe {
        bins: vec![
            digest_bin("A", "P", "EcoRI", "BamHI"),
            digest_bin("B", "P", "BamHI", "EcoRI"),
        ],
        join: JoinKind::Ligate,
        intent: TopologyIntent::Circular,
        expand: Expand::AllToAll,
        name_template: None,
    };
    let mut result = run(&recipe, &resolver);
    assert!(!result.products.is_empty(), "{:?}", result.warnings);

    // Plant a landmark part-way round, then rotate to it.
    let mark = 7usize;
    let before = result.products[0].fragment.slice.bytes.clone();
    let len = before.len();
    result.products[0].fragment.slice.features.push(Feature {
        id: seqforge_core::FeatureId(0),
        raw_kind: "misc_feature".into(),
        label: "Start".into(),
        location: Location::simple(mark..mark + 1),
        strand: seqforge_core::Strand::Forward,
        qualifiers: Default::default(),
        lineage: None,
    });

    set_origins(&mut result.products, &OriginSpec::Feature("Start".into())).unwrap();
    let after = &result.products[0].fragment.slice.bytes;

    // Same circle, read from a different starting base.
    assert_eq!(after.len(), len);
    assert_eq!(*after, [&before[mark..], &before[..mark]].concat());
    // The landmark now sits at position 0.
    let start = result.products[0]
        .fragment
        .slice
        .features
        .iter()
        .find(|f| f.label == "Start")
        .expect("landmark survived");
    assert_eq!(start.location.as_span().unwrap().start, 0);
}

/// A label that names no feature, or several, is an error — never a guess.
#[test]
fn ambiguous_or_missing_origin_label_is_rejected() {
    use seqforge_bio::{OriginSpec, resolve_origin};
    use seqforge_core::{Feature, FeatureId, Location, Strand};

    let feature = |label: &str, at: usize| Feature {
        id: FeatureId(0),
        raw_kind: "misc_feature".into(),
        label: label.into(),
        location: Location::simple(at..at + 1),
        strand: Strand::Forward,
        qualifiers: Default::default(),
        lineage: None,
    };

    let one = vec![feature("Start", 5)];
    assert_eq!(
        resolve_origin(&OriginSpec::Feature("Start".into()), &one, 20).unwrap(),
        5
    );
    let two = vec![feature("Start", 5), feature("Start", 11)];
    let err = resolve_origin(&OriginSpec::Feature("Start".into()), &two, 20).unwrap_err();
    assert!(err.contains("ambiguous"), "unexpected: {err}");
    let err = resolve_origin(&OriginSpec::Feature("Nope".into()), &one, 20).unwrap_err();
    assert!(err.contains("no feature labelled"), "unexpected: {err}");
    let err = resolve_origin(&OriginSpec::Index(99), &one, 20).unwrap_err();
    assert!(err.contains("past the end"), "unexpected: {err}");
}
