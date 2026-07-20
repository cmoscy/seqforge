use std::path::Path;

use anyhow::Context;
use seqforge_core::{Annotations, Strand, Topology, ViewerRequest};

#[cfg(unix)]
use std::io::{BufRead, BufReader, Write};
#[cfg(unix)]
use std::os::unix::net::UnixStream;

// ── File commands ─────────────────────────────────────────────────────────────

pub fn run_info(path: &Path) -> anyhow::Result<()> {
    let doc =
        seqforge_bio::load(path).with_context(|| format!("Failed to load {}", path.display()))?;
    let info = serde_json::json!({
        "kind": "document_info",
        "name": doc.name,
        "length": doc.len(),
        "topology": format!("{:?}", doc.topology).to_lowercase(),
        "features": doc.features.len(),
        "primers": doc.primers.len(),
        "path": path,
    });
    println!("{}", serde_json::to_string_pretty(&info)?);
    Ok(())
}

/// Translate a (sub)range of a sequence file to protein — a local, read-only
/// derivation that needs no running GUI. `start`/`end` are 0-based half-open
/// (default: whole sequence); `strand` is `+`/`-`; `frame` is the GenBank
/// codon_start convention (1, 2, or 3).
pub fn run_translate(
    path: &Path,
    start: Option<usize>,
    end: Option<usize>,
    strand: &str,
    frame: usize,
) -> anyhow::Result<()> {
    let doc =
        seqforge_bio::load(path).with_context(|| format!("Failed to load {}", path.display()))?;
    let len = doc.len();
    let start = start.unwrap_or(0);
    let end = end.unwrap_or(len);
    if start >= end || end > len {
        anyhow::bail!("range {start}..{end} is invalid for a sequence of length {len}");
    }
    let strand = match strand.trim() {
        "-" | "reverse" | "Reverse" => Strand::Reverse,
        _ => Strand::Forward,
    };
    let protein = seqforge_bio::translate(&doc.sequence[start..end], strand, frame);
    let out = serde_json::json!({
        "kind": "translation",
        "name": doc.name,
        "start": start,
        "end": end,
        "strand": format!("{strand:?}").to_lowercase(),
        "frame": frame,
        "protein": protein,
        "length": protein.chars().count(),
    });
    println!("{}", serde_json::to_string_pretty(&out)?);
    Ok(())
}

/// Find open reading frames in a sequence file — a local analysis (no GUI).
/// `min_aa` filters by protein length; forward + reverse frames by default.
pub fn run_orfs(
    path: &Path,
    min_aa: usize,
    stop_to_stop: bool,
    forward_only: bool,
) -> anyhow::Result<()> {
    let doc =
        seqforge_bio::load(path).with_context(|| format!("Failed to load {}", path.display()))?;
    let orfs = seqforge_bio::find_orfs(&doc.sequence, min_aa, !stop_to_stop, !forward_only);
    let items: Vec<_> = orfs
        .iter()
        .map(|o| {
            serde_json::json!({
                "start": o.start,
                "end": o.end,
                "strand": format!("{:?}", o.strand).to_lowercase(),
                "frame": o.frame,
                "aa_len": o.aa_len,
            })
        })
        .collect();
    let out = serde_json::json!({
        "kind": "orfs",
        "name": doc.name,
        "count": orfs.len(),
        "orfs": items,
    });
    println!("{}", serde_json::to_string_pretty(&out)?);
    Ok(())
}

/// Melting temperature + GC of an oligo — a pure, local derivation (no running
/// GUI, no file). Reaches the vendored seqfold engine through `seqforge-bio`'s
/// thin `tm`/`gc` surface (`bio → thermo`; `core` never sees thermo). Tm is the
/// nearest-neighbour model (SantaLucia NN + Owczarzy-2008 salt), in °C.
pub fn run_tm(oligo: &str) -> anyhow::Result<()> {
    let tm = seqforge_bio::tm(oligo)
        .map_err(|e| anyhow::anyhow!("cannot compute Tm for {oligo:?}: {}", e.0))?;
    let hairpin = seqforge_bio::hairpin_dg(oligo, seqforge_bio::DEFAULT_FOLD_TEMP_C);
    let dimer = seqforge_bio::self_dimer_dg(oligo, seqforge_bio::DEFAULT_FOLD_TEMP_C);
    let out = serde_json::json!({
        "kind": "oligo_tm",
        "oligo": oligo.to_uppercase(),
        "length": oligo.len(),
        "tm": tm,
        "gc": seqforge_bio::gc(oligo),
        "hairpin_dg": hairpin.ok(),
        "self_dimer_dg": dimer.ok(),
    });
    println!("{}", serde_json::to_string_pretty(&out)?);
    Ok(())
}

/// List the primers in a sequence file with derived attachment state + QC — the
/// CLI face of the Inspector's `ListPrimers` projection (both go through the one
/// `seqforge_bio::primer_infos`, so GUI and agent can't drift). No GUI needed.
///
/// Ids are session-scoped (minted here via `Annotations::from_parts`, exactly as
/// on GUI load): stable within this invocation, not across runs.
pub fn run_primers_list(path: &Path) -> anyhow::Result<()> {
    let doc =
        seqforge_bio::load(path).with_context(|| format!("Failed to load {}", path.display()))?;
    let circular = matches!(doc.topology, Topology::Circular);
    // Mint ids + default names exactly like a GUI load (decision 9).
    let ann = Annotations::from_parts(doc.features, doc.primers);
    let primers: Vec<&seqforge_core::Primer> = ann.primers().collect();
    let infos = seqforge_bio::primer_infos(&doc.sequence, &primers, circular);
    let out = serde_json::json!({
        "kind": "primers_list",
        "count": infos.len(),
        "primers": infos,
    });
    println!("{}", serde_json::to_string_pretty(&out)?);
    Ok(())
}

/// Find binding sites for `oligo` on a sequence file (seed-and-extend, both
/// strands, circular-aware). Ranges are 0-based half-open on the top strand,
/// matching the `PrimerInfo.binding` projection. No GUI needed.
pub fn run_primers_find(path: &Path, oligo: &str) -> anyhow::Result<()> {
    let doc =
        seqforge_bio::load(path).with_context(|| format!("Failed to load {}", path.display()))?;
    let circular = matches!(doc.topology, Topology::Circular);
    let settings = seqforge_bio::AnnealSettings::default();
    let sites = seqforge_bio::find_primer_binding_sites(oligo, &doc.sequence, circular, settings);
    // `PrimerBinding` isn't `Serialize`; project each site to explicit JSON.
    let sites_json: Vec<_> = sites
        .iter()
        .map(|s| {
            serde_json::json!({
                // Wrap-aware footprint as {start, len} (P5b: a site crossing the
                // origin is one wrapping span, not an end > len overflow).
                "start": s.span.start,
                "len": s.span.len,
                "strand": s.strand,
                "mismatches": s.mismatches,
                "three_prime_match": s.three_prime_match,
            })
        })
        .collect();
    let out = serde_json::json!({
        "kind": "primers_find",
        "oligo": oligo.to_uppercase(),
        "count": sites_json.len(),
        "sites": sites_json,
    });
    println!("{}", serde_json::to_string_pretty(&out)?);
    Ok(())
}

/// Digest a sequence file with restriction enzymes (Restriction Tier 2) — the
/// **local** CLI face of `digest`. Loads the file, resolves the enzyme query
/// (same grammar as the GUI: names or presets like `golden gate` / `type IIs`),
/// and prints the virtual `FragmentInfo` set. Nothing is written — fragments are
/// virtual (decision 25); the molecule's methylation defaults apply (Dam⁺ Dcm⁺).
/// `--circular` overrides the file's topology.
pub fn run_digest(path: &Path, enzymes: &[String], circular_override: bool) -> anyhow::Result<()> {
    let doc =
        seqforge_bio::load(path).with_context(|| format!("Failed to load {}", path.display()))?;
    let circular = circular_override || matches!(doc.topology, Topology::Circular);
    // Mint ids exactly like a GUI load so inherited features project consistently.
    let ann = Annotations::from_parts(doc.features, doc.primers);

    // Accept comma- or space-separated enzyme lists (`--enzymes EcoRI,BamHI`).
    let query = enzymes.join(" ").replace(',', " ");
    let parsed = seqforge_bio::parse_enzyme_query(&query);
    let names = seqforge_bio::resolve_query_names(&parsed, &doc.sequence, circular);
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();

    let (frags, warnings) = seqforge_bio::digest_fragments(
        &doc.sequence,
        &ann,
        &refs,
        circular,
        &doc.name,
        &seqforge_core::MethylContext::default(),
    );
    let infos: Vec<_> = frags
        .iter()
        .enumerate()
        .map(|(i, f)| f.to_info(i))
        .collect();

    let out = serde_json::json!({
        "kind": "digest",
        "name": doc.name,
        "enzymes": names,
        "count": infos.len(),
        "fragments": infos,
        "warnings": warnings,
    });
    println!("{}", serde_json::to_string_pretty(&out)?);
    Ok(())
}

/// Assemble a product from a recipe (Assembly A1) — the **local** CLI face of
/// `assemble`. Accepts either a single `recipe.json` or inline bin tokens
/// (`SOURCE[@FROM..TO]`), runs the shared `seqforge_bio` engine over the
/// filesystem, and prints the products. Both faces build the same `Recipe`
/// (parity with the GUI, which runs the identical `seqforge_bio::run`).
#[allow(clippy::too_many_arguments)]
pub fn run_assemble(
    inputs: &[String],
    method: &str,
    topology: &str,
    default_enzymes: Option<&str>,
    expand: &str,
    emit_recipe: Option<&Path>,
    dry_run: bool,
    fidelity_dataset: Option<&str>,
    fidelity_matrix: bool,
) -> anyhow::Result<()> {
    use seqforge_core::{Expand, JoinKind, Recipe, TopologyIntent};

    // Build the recipe: a lone `*.json` loads; otherwise each input is a bin.
    let recipe = if inputs.len() == 1 && inputs[0].ends_with(".json") {
        let text = std::fs::read_to_string(&inputs[0])
            .with_context(|| format!("read recipe {}", inputs[0]))?;
        serde_json::from_str::<Recipe>(&text).with_context(|| "parse recipe json")?
    } else if inputs.is_empty() {
        anyhow::bail!("no inputs — pass a recipe.json or bin tokens (SOURCE[@FROM..TO])");
    } else {
        let bins = inputs
            .iter()
            .map(|t| parse_bin_token(t, default_enzymes))
            .collect::<anyhow::Result<Vec<_>>>()?;
        let join = match method {
            "ligate" => JoinKind::Ligate,
            "golden-gate" | "golden_gate" | "gg" => {
                let enzyme = default_enzymes
                    .map(normalize_enzymes)
                    .and_then(|e| e.split_whitespace().next().map(str::to_string))
                    .ok_or_else(|| {
                        anyhow::anyhow!("--method golden-gate needs --enzymes (e.g. BsaI)")
                    })?;
                JoinKind::GoldenGate { enzyme }
            }
            other => {
                anyhow::bail!("unknown --method {other:?} (supports: ligate, golden-gate)")
            }
        };
        Recipe {
            bins,
            join,
            intent: match topology {
                "linear" => TopologyIntent::Linear,
                "any" => TopologyIntent::Any,
                _ => TopologyIntent::Circular,
            },
            expand: if expand == "zip" {
                Expand::Zip
            } else {
                Expand::AllToAll
            },
            name_template: None,
        }
    };

    if let Some(path) = emit_recipe {
        std::fs::write(path, serde_json::to_string_pretty(&recipe)?)
            .with_context(|| format!("write recipe {}", path.display()))?;
    }

    if dry_run {
        if fidelity_matrix && fidelity_dataset.is_none() {
            anyhow::bail!("--fidelity-matrix requires --fidelity-dataset");
        }
        let dataset = match fidelity_dataset {
            None => None,
            Some(name) => Some(seqforge_bio::FidelityDataset::parse(name).ok_or_else(|| {
                anyhow::anyhow!(
                    "unknown --fidelity-dataset {name:?} (try t4_25c_18h, bsai, sapi, …)"
                )
            })?),
        };
        // Report the plan without materializing products: per-bin fragment
        // counts + per-combo summaries (identity-only join probe).
        let bins: Vec<_> = recipe
            .bins
            .iter()
            .map(|b| {
                let (infos, warnings) = seqforge_bio::preview_bin(b, &seqforge_bio::FileResolver);
                serde_json::json!({ "role": b.role, "fragments": infos.len(), "warnings": warnings })
            })
            .collect();
        let (summaries, warnings) =
            seqforge_bio::enumerate_combos(&recipe, &seqforge_bio::FileResolver, dataset);
        let compatible = summaries.iter().filter(|c| c.ok).count();
        let combos_json: Vec<_> = summaries
            .iter()
            .map(|c| {
                let mut obj = serde_json::json!({
                    "index": c.index,
                    "ok": c.ok,
                    "parts": c.parts.iter().map(|p| serde_json::json!({
                        "source": p.source_name,
                        "length": p.length,
                    })).collect::<Vec<_>>(),
                    "detail": c.detail,
                });
                if dataset.is_some() {
                    let obj = obj.as_object_mut().unwrap();
                    obj.insert(
                        "fidelity".into(),
                        match c.fidelity {
                            Some(f) => serde_json::json!(f),
                            None => serde_json::Value::Null,
                        },
                    );
                    obj.insert(
                        "fidelity_three_prime".into(),
                        serde_json::json!(c.fidelity_three_prime),
                    );
                }
                obj
            })
            .collect();
        let mut out = serde_json::json!({
            "kind": "assembly_dry_run",
            "bins": bins,
            "combos": summaries.len(),
            "compatible_combos": compatible,
            "combo_list": combos_json,
            "warnings": warnings,
        });
        if let Some(ds) = dataset {
            let root = out.as_object_mut().unwrap();
            root.insert("fidelity_dataset".into(), serde_json::json!(ds.id()));
            if fidelity_matrix {
                if let Some((combo_index, matrix)) = seqforge_bio::first_combo_fidelity_matrix(
                    &recipe,
                    &seqforge_bio::FileResolver,
                    ds,
                ) {
                    let n = matrix.dim();
                    let labels: Vec<String> = matrix
                        .labels
                        .iter()
                        .map(|l| String::from_utf8_lossy(l).into_owned())
                        .collect();
                    let counts: Vec<Vec<u32>> = (0..n)
                        .map(|i| (0..n).map(|j| matrix.get(i, j)).collect())
                        .collect();
                    root.insert(
                        "fidelity_matrix".into(),
                        serde_json::json!({
                            "combo_index": combo_index,
                            "labels": labels,
                            "counts": counts,
                        }),
                    );
                } else {
                    root.insert("fidelity_matrix".into(), serde_json::Value::Null);
                }
            }
        }
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }

    if fidelity_dataset.is_some() {
        anyhow::bail!("--fidelity-dataset only applies with --dry-run (scores are not persisted)");
    }
    if fidelity_matrix {
        anyhow::bail!("--fidelity-matrix only applies with --dry-run --fidelity-dataset");
    }

    let result = seqforge_bio::run(&recipe, &seqforge_bio::FileResolver);
    let products: Vec<_> = result
        .products
        .iter()
        .map(|p| {
            let info = p.fragment.to_info(0);
            serde_json::json!({
                "name": p.name,
                "length": info.length,
                "topology": info.topology,
                "left": info.left,
                "right": info.right,
            })
        })
        .collect();
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "kind": "assembly",
            "method": method,
            "count": products.len(),
            "products": products,
            "warnings": result.warnings,
        }))?
    );
    Ok(())
}

/// Parse a bin token `SOURCE[@FROM..TO]` into a [`Bin`] (decision 26).
///
/// - `SOURCE` may be a **glob** (`parts/*.gb`) → every match becomes a source in
///   the **same** bin (bulk, shared 5′→3′ prepare).
/// - `@E1..E2` is the digest 5′→3′ walk (`EcoRI..PstI`, `BsaI..BsaI`, `EcoRI@410..BamHI`).
/// - `@pcr:fwd..rev` / `@as-is` for PCR and pass-through.
/// - Trailing `[5′..3′]` is a per-source span override (rare `@pos` exception).
///
/// Without `@…`, defaults to `Digest(E..E)` from `--enzymes` (GG sugar:
/// bare path + `--enzymes BsaI` → `BsaI..BsaI`), else `AsIs`.
fn parse_bin_token(
    token: &str,
    default_enzymes: Option<&str>,
) -> anyhow::Result<seqforge_core::Bin> {
    use seqforge_core::{Bin, Boundary, PrepareKind, Source, SourceRef, SpanEnds};

    // Split off a trailing [span] (per-input override).
    let (rest, span_override) = match (token.find('['), token.ends_with(']')) {
        (Some(open), true) => {
            let inner = &token[open + 1..token.len() - 1];
            let span = inner
                .parse::<SpanEnds>()
                .map_err(|e| anyhow::anyhow!("bad span override in {token:?}: {e}"))?;
            (&token[..open], Some(span))
        }
        _ => (token, None),
    };

    // Split off @prepare / @5′..3′.
    let (source, prepare) = match rest.split_once('@') {
        Some((src, spec)) => (src, parse_prepare(spec)?),
        None => {
            let prep = match default_enzymes {
                Some(e) => {
                    let names: Vec<String> = normalize_enzymes(e)
                        .split_whitespace()
                        .map(str::to_string)
                        .collect();
                    match names.as_slice() {
                        [] => PrepareKind::AsIs,
                        [one] => PrepareKind::Digest {
                            five_prime: Boundary::enzyme(one.clone()),
                            three_prime: Boundary::enzyme(one.clone()),
                        },
                        [a, b, ..] => PrepareKind::Digest {
                            five_prime: Boundary::enzyme(a.clone()),
                            three_prime: Boundary::enzyme(b.clone()),
                        },
                    }
                }
                None => PrepareKind::AsIs,
            };
            (rest, prep)
        }
    };
    if source.is_empty() {
        anyhow::bail!("empty source in bin token {token:?}");
    }

    let paths = seqforge_bio::expand_glob(source);
    if paths.is_empty() {
        anyhow::bail!("no files match {source:?}");
    }
    let sources: Vec<Source> = paths
        .into_iter()
        .map(|p| Source {
            ref_: SourceRef::Path(p),
            pin: None,
            span: span_override.clone(),
        })
        .collect();

    Ok(Bin {
        role: bin_role(source),
        sources,
        prepare,
    })
}

/// A bin role from the source token: a glob → its parent directory name; a plain
/// path → its file stem.
fn bin_role(source: &str) -> String {
    if source.contains('*') {
        Path::new(source)
            .parent()
            .and_then(|p| p.file_name())
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "bin".to_string())
    } else {
        Path::new(source)
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| source.to_string())
    }
}

fn parse_prepare(spec: &str) -> anyhow::Result<seqforge_core::PrepareKind> {
    use seqforge_core::{Boundary, PrepareKind, SpanEnds};
    let spec = spec.trim();
    if spec.eq_ignore_ascii_case("as-is") || spec.eq_ignore_ascii_case("asis") {
        return Ok(PrepareKind::AsIs);
    }
    if let Some(pair) = spec.strip_prefix("pcr:") {
        let span: SpanEnds = pair
            .parse()
            .map_err(|e| anyhow::anyhow!("pcr prepare needs fwd..rev, got {spec:?}: {e}"))?;
        let (fwd, rev) = match (&span.five_prime, &span.three_prime) {
            (
                Boundary::EnzymeSite {
                    enzyme: f,
                    at: None,
                },
                Boundary::EnzymeSite {
                    enzyme: r,
                    at: None,
                },
            ) => (f.clone(), r.clone()),
            _ => anyhow::bail!("pcr prepare needs primer names (fwd..rev), got {spec:?}"),
        };
        return Ok(PrepareKind::Pcr { fwd, rev });
    }
    // Optional legacy `digest:` prefix, then 5′..3′.
    let span_text = spec.strip_prefix("digest:").unwrap_or(spec);
    let span: SpanEnds = span_text
        .parse()
        .map_err(|e| anyhow::anyhow!("bad prepare {spec:?}: {e}"))?;
    Ok(PrepareKind::Digest {
        five_prime: span.five_prime,
        three_prime: span.three_prime,
    })
}

/// Enzyme lists accept `,`, `+`, or `/` separators; the query grammar wants whitespace.
fn normalize_enzymes(list: &str) -> String {
    list.replace([',', '+', '/'], " ")
}

// ── Viewer command socket dispatch ────────────────────────────────────────────

/// Send a `ViewerRequest` to a running SeqForge GUI via the Unix domain socket
/// using the JSON-RPC 2.0 wire format.
///
/// Reads `SEQFORGE_SOCKET` from the environment. If unset, the command cannot
/// be delivered and an error is returned.
///
/// On non-Unix platforms (Windows), returns an error explaining that the
/// agent-IPC transport isn't supported in v0.1 (Tier 1 #5). File commands
/// (`info`, `digest`, `annotate`) work everywhere; viewer commands are
/// Unix-only until/unless we adopt `interprocess` for cross-platform sockets.
#[cfg(not(unix))]
pub fn dispatch_viewer_cmd(_req: ViewerRequest) -> anyhow::Result<()> {
    anyhow::bail!(
        "viewer commands (open/close/goto/find/enzymes) require a Unix \
         domain socket; not supported on this platform"
    )
}

#[cfg(unix)]
pub fn dispatch_viewer_cmd(req: ViewerRequest) -> anyhow::Result<()> {
    let socket_path = std::env::var("SEQFORGE_SOCKET").map_err(|_| {
        anyhow::anyhow!("no SeqForge instance running (SEQFORGE_SOCKET is not set)")
    })?;

    let mut stream = UnixStream::connect(&socket_path)
        .with_context(|| format!("could not connect to SeqForge socket at {socket_path}"))?;

    // Serialize the ViewerRequest as a JSON-RPC 2.0 request.
    // ViewerRequest uses serde tag="method", so we merge method into params.
    let req_value = serde_json::to_value(&req)?;
    let method = req_value
        .get("method")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_owned();
    let mut params = match req_value {
        serde_json::Value::Object(mut m) => {
            m.remove("method");
            serde_json::Value::Object(m)
        }
        _ => serde_json::Value::Null,
    };
    if matches!(params, serde_json::Value::Object(ref m) if m.is_empty()) {
        params = serde_json::Value::Null;
    }

    let rpc = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": method,
        "params": params,
    });
    let json = serde_json::to_string(&rpc)?;
    stream.write_all(format!("{json}\n").as_bytes())?;
    stream.flush()?;

    // Read and parse the JSON-RPC response.
    let mut response_line = String::new();
    BufReader::new(&stream).read_line(&mut response_line)?;
    let response: serde_json::Value = serde_json::from_str(response_line.trim())
        .context("invalid JSON-RPC response from SeqForge")?;

    if let Some(err) = response.get("error") {
        let msg = err
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown error");
        anyhow::bail!("SeqForge rejected command: {msg}");
    }

    // Pretty-print the result so agents can consume it.
    if let Some(result) = response.get("result") {
        println!("{}", serde_json::to_string_pretty(result)?);
    }

    Ok(())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(all(test, unix))]
mod tests {
    use seqforge_core::ViewerRequest;

    #[test]
    fn viewer_cmd_fails_without_socket_env() {
        // Safety: test binary is single-threaded by default; no concurrent env reads.
        unsafe { std::env::remove_var("SEQFORGE_SOCKET") };
        let err = super::dispatch_viewer_cmd(ViewerRequest::Close).unwrap_err();
        assert!(err.to_string().contains("SEQFORGE_SOCKET"));
    }
}

#[cfg(test)]
mod assemble_tests {
    use seqforge_core::{Bin, Boundary, PrepareKind, Source, SourceRef, SpanEnds};

    /// Parity: the bin a CLI token parses to is byte-identical to the bin a GUI
    /// would author, and it survives serde + the 5′→3′ Display/FromStr round-trip.
    #[test]
    fn cli_token_equals_gui_authored_bin() {
        let bin = super::parse_bin_token("pUC19.gb@BamHI..EcoRI", None).unwrap();

        let expected = Bin {
            role: "pUC19".into(),
            sources: vec![Source {
                ref_: SourceRef::Path("pUC19.gb".into()),
                pin: None,
                span: None,
            }],
            prepare: PrepareKind::Digest {
                five_prime: Boundary::enzyme("BamHI"),
                three_prime: Boundary::enzyme("EcoRI"),
            },
        };
        assert_eq!(bin, expected, "CLI token must equal the GUI-authored bin");

        let json = serde_json::to_string(&bin).unwrap();
        assert_eq!(serde_json::from_str::<Bin>(&json).unwrap(), bin);

        let span = SpanEnds::new(Boundary::enzyme("BamHI"), Boundary::enzyme("EcoRI"));
        assert_eq!(span.to_string(), "BamHI..EcoRI");
        assert_eq!("BamHI..EcoRI".parse::<SpanEnds>().unwrap(), span);
    }

    /// A per-input `[5′..3′]` with an `@pos` occurrence rides each source.
    #[test]
    fn per_input_span_override_is_carried_on_the_source() {
        let bin = super::parse_bin_token("geneC.gb@EcoRI..BamHI[EcoRI@410..BamHI]", None).unwrap();
        let span = bin.sources[0].span.as_ref().expect("span override");
        assert_eq!(span.to_string(), "EcoRI@410..BamHI");
    }

    /// A glob source expands to N sources in **one** bin (bulk).
    #[test]
    fn glob_expands_to_n_sources_in_one_bin() {
        let dir = std::env::temp_dir().join(format!("sf_glob_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["a.gb", "b.gb", "c.gb", "skip.txt"] {
            std::fs::write(dir.join(name), b">x\nACGT\n").unwrap();
        }
        let pattern = format!("{}/*.gb", dir.display());
        let bin = super::parse_bin_token(&format!("{pattern}@EcoRI..EcoRI"), None).unwrap();
        assert_eq!(bin.sources.len(), 3, "3 .gb files, not the .txt");
        let combos: usize = [bin.sources.len()].iter().product();
        assert_eq!(combos, 3);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Bare path + `--enzymes BsaI` sugars to `Digest { BsaI..BsaI }`.
    #[test]
    fn golden_gate_bare_path_sugars_to_bsai_span() {
        let bin = super::parse_bin_token("vector.gb", Some("BsaI")).unwrap();
        assert_eq!(
            bin.prepare,
            PrepareKind::Digest {
                five_prime: Boundary::enzyme("BsaI"),
                three_prime: Boundary::enzyme("BsaI"),
            }
        );
    }
}

#[cfg(test)]
mod primer_tests {
    use std::path::PathBuf;

    fn puc19() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../seqforge-bio/tests/fixtures/pUC19.gbk")
    }

    // Smoke tests: exercise load → project → serialize end-to-end (projection
    // correctness is covered by seqforge-bio/-core unit tests).
    #[test]
    fn primers_list_runs_on_fixture() {
        assert!(super::run_primers_list(&puc19()).is_ok());
    }

    #[test]
    fn primers_find_runs_on_fixture() {
        assert!(super::run_primers_find(&puc19(), "GGGAAACGCCTGGTATCTTT").is_ok());
    }
}
