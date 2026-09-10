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
            // The footprint is the annealed region, so anything the oligo has
            // 5' of it is a tail — a restriction site, an overhang, a homology
            // arm. Report it explicitly: for a cloning primer the tail is the
            // functional part, and it is invisible from the span alone.
            // The footprint is the annealed span, so the tail is exactly what
            // the oligo has left over — read it straight off, not from a
            // decomposition, which would clamp an origin-crossing span to the
            // sequence end and over-report the tail.
            let tail_len = oligo.len().saturating_sub(s.span.len);
            let tail = oligo[..tail_len].to_string();

            // Tm needs a contiguous template region; extend past the origin for
            // a wrapping site so the duplex is the real one.
            let end = s.span.start + s.span.len;
            let extended;
            let (tm_template, tm_range) = if end > doc.sequence.len() {
                let overhang = end - doc.sequence.len();
                extended = doc
                    .sequence
                    .iter()
                    .chain(&doc.sequence[..overhang.min(doc.sequence.len())])
                    .copied()
                    .collect::<Vec<_>>();
                (&extended[..], s.span.start..end)
            } else {
                (&doc.sequence[..], s.span.start..end)
            };
            let tm = seqforge_bio::anneal_tm(oligo, &tm_range, s.strand, tm_template)
                .ok()
                .map(|t| (t * 10.0).round() / 10.0);
            serde_json::json!({
                // Wrap-aware footprint as {start, len} (P5b: a site crossing the
                // origin is one wrapping span, not an end > len overflow).
                "start": s.span.start,
                "len": s.span.len,
                "strand": s.strand,
                "mismatches": s.mismatches,
                "three_prime_match": s.three_prime_match,
                "anneal_len": s.span.len,
                "tail": tail,
                "tail_len": tail_len,
                "anneal_tm": tm,
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

    // `--enzymes` is repeatable, so join the occurrences; commas inside one
    // occurrence are `parse_enzyme_query`'s job, not ours (it normalizes them
    // for both presets and name lists).
    let query = enzymes.join(" ");

    // One implementation, shared with the viewer — see `digest_resolved`.
    let (infos, warnings, names) = seqforge_bio::digest_projection(
        &doc.sequence,
        &doc.name,
        circular,
        &ann,
        &query,
        &seqforge_core::MethylContext::default(),
    );
    let names: Vec<String> = names.split_whitespace().map(str::to_string).collect();

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
/// Everything `assemble` was invoked with. A struct rather than a dozen
/// positional arguments so adding a flag stays a one-line change at each end.
pub struct AssembleOpts<'a> {
    pub inputs: &'a [String],
    pub method: &'a str,
    pub topology: &'a str,
    pub default_enzymes: Option<&'a str>,
    pub expand: &'a str,
    pub emit_recipe: Option<&'a Path>,
    pub dry_run: bool,
    pub fidelity_dataset: Option<&'a str>,
    pub fidelity_matrix: bool,
    /// Directory to write products into (`--out`).
    pub out: Option<&'a Path>,
    /// Product format when `out` is set.
    pub format: &'a str,
    /// Product-name template (see `seqforge_bio::assembly::naming`).
    pub name_template: Option<&'a str>,
    /// Combo selector (see [`seqforge_bio::parse_combo_spec`]).
    pub combos: Option<&'a str>,
    /// Rotate each circular product to this feature label or index.
    pub origin: Option<&'a str>,
}

/// `assemble`. Accepts either a single `recipe.json` or inline bin tokens
/// (`SOURCE[@FROM..TO]`), runs the shared `seqforge_bio` engine over the
/// filesystem, and prints the products — writing them to `--out` when asked.
/// Both faces build the same `Recipe` (parity with the GUI, which runs the
/// identical `seqforge_bio::run`) and both write through
/// `seqforge_bio::write_products`.
pub fn run_assemble(opts: AssembleOpts<'_>) -> anyhow::Result<()> {
    use seqforge_core::{Expand, JoinKind, Recipe, TopologyIntent};

    let AssembleOpts {
        inputs,
        method,
        topology,
        default_enzymes,
        expand,
        emit_recipe,
        dry_run,
        fidelity_dataset,
        fidelity_matrix,
        out,
        format,
        name_template,
        combos,
        origin,
    } = opts;

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
            .map(|t| {
                seqforge_bio::parse_bin_token(t, default_enzymes).map_err(|e| anyhow::anyhow!(e))
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        let join = match method {
            "ligate" => JoinKind::Ligate,
            "golden-gate" | "golden_gate" | "gg" => {
                let enzyme = default_enzymes
                    .map(seqforge_bio::normalize_enzymes)
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
    let mut recipe = recipe;
    if let Some(t) = name_template {
        recipe.name_template = Some(t.to_string());
    }
    let recipe = recipe;

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
    let format = seqforge_bio::ProductFormat::parse(format)
        .ok_or_else(|| anyhow::anyhow!("unknown --format {format:?} (supports: genbank, fasta)"))?;

    // `--combos` narrows the run to selected indices. Resolving the selector
    // needs the combo count, which only the expansion knows — so enumerate
    // first, then run just the chosen ones.
    let result = match combos {
        None => seqforge_bio::run(&recipe, &seqforge_bio::FileResolver),
        Some(spec) => {
            let (summaries, _) =
                seqforge_bio::enumerate_combos(&recipe, &seqforge_bio::FileResolver, None);
            let indices = seqforge_bio::parse_combo_spec(spec, summaries.len())
                .map_err(|e| anyhow::anyhow!("--combos: {e}"))?;
            seqforge_bio::run_indices(&recipe, &seqforge_bio::FileResolver, &indices)
        }
    };

    // Rotate before naming/export so the buffer, the file, and every reported
    // coordinate agree on where position 0 is.
    let mut result = result;
    if let Some(spec) = origin {
        let spec: seqforge_bio::OriginSpec = spec.parse().unwrap_or_else(|e| match e {});
        seqforge_bio::set_origins(&mut result.products, &spec)
            .map_err(|e| anyhow::anyhow!("--origin: {e}"))?;
    }
    let result = result;

    let paths = match out {
        Some(dir) => seqforge_bio::write_products(&result.products, dir, format)
            .with_context(|| format!("write products to {}", dir.display()))?
            .into_iter()
            .map(Some)
            .collect(),
        None => vec![None; result.products.len()],
    };

    let products: Vec<_> = result
        .products
        .iter()
        .zip(&paths)
        .map(|(p, path)| {
            let info = p.fragment.to_info(0);
            serde_json::json!({
                "name": p.name,
                "length": info.length,
                "topology": info.topology,
                "left": info.left,
                "right": info.right,
                "combo_index": p.combo_index,
                "parts": p.parts.iter().map(|c| serde_json::json!({
                    "source": c.source_name,
                    "length": c.length,
                })).collect::<Vec<_>>(),
                "path": path,
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

// ── Routing: where does this command's document come from? ────────────────────

/// Where a command's document comes from, and therefore who can run it
/// (ROADMAP decision 27).
///
/// This is what replaced the old two-tier command split. "Needs a GUI" is not a
/// property of the *verb* — every verb is `open a document, do something, maybe
/// write a document` — it is a property of where the document lives. A request
/// naming only file paths can run in this process; one naming a session buffer
/// or the active view has to reach the session that owns it.
#[derive(Debug, PartialEq, Eq)]
pub enum DocSource {
    /// Every input is a path on disk — runnable here, no socket required.
    Paths,
    /// At least one input names live session state (`buffer:<n>`, or an
    /// implicit "the active view"). Must be forwarded.
    Session,
}

impl DocSource {
    /// Classify a request. Only `Assemble` can currently go either way; every
    /// other `ViewerRequest` variant is view-scoped and therefore `Session`.
    pub fn of(req: &ViewerRequest) -> Self {
        match req {
            ViewerRequest::Assemble { inputs, .. } => {
                // No inputs at all means "the recipe open in the workbench".
                if inputs.is_empty() || inputs.iter().any(|t| t.starts_with("buffer:")) {
                    DocSource::Session
                } else {
                    DocSource::Paths
                }
            }
            // Every other verb addresses one document, so its `Target` decides.
            // `--in` names a file we can open here; `--view` and the default
            // (the active view) name session state we cannot see.
            other => match other.target() {
                Some(t) if t.is_path() => DocSource::Paths,
                _ => DocSource::Session,
            },
        }
    }
}

/// Run a file-targeted request in this process, against a throwaway workspace.
///
/// The whole path is three steps because nothing here is new: `open_path` mints
/// the buffer and a view, `core::dispatch` is already session-free (it takes
/// `(&mut View, &Buffer, &mut Annotations, &B, req)`), and the response
/// serializes to the same JSON the socket returns. The workspace — and with it
/// the buffer's undo history — is dropped when the process exits, which is why
/// `undo`/`redo` stay session verbs: there is no previous command to reverse.
fn run_on_file(req: ViewerRequest) -> anyhow::Result<()> {
    let path = req
        .target()
        .and_then(|t| t.path.clone())
        .ok_or_else(|| anyhow::anyhow!("internal: routed a request with no file target"))?;

    let bio = seqforge_session::Bio;
    let mut ws = seqforge_session::Workspace::default();
    let vid = ws
        .open_path(&path, &bio)
        .map_err(|e| anyhow::anyhow!("open {}: {e}", path.display()))?;

    let resp = ws
        .with_buffer(vid, |view, buf, ann| {
            seqforge_core::dispatch(view, buf, ann, &bio, req)
        })
        .map_err(|e| anyhow::anyhow!("{e}"))?
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    println!("{}", serde_json::to_string_pretty(&resp)?);
    Ok(())
}

/// Run a request here if its document is on disk; otherwise forward it to the
/// running SeqForge over the socket.
///
/// One entry point for the whole verb surface — the CLI no longer decides
/// local-vs-remote by which enum a command was declared in.
pub fn dispatch_cmd(req: ViewerRequest) -> anyhow::Result<()> {
    match (DocSource::of(&req), req) {
        (
            DocSource::Paths,
            ViewerRequest::Assemble {
                inputs,
                method,
                topology,
                enzymes,
                expand,
                emit_recipe,
                dry_run,
                fidelity_dataset,
                fidelity_matrix,
                out,
                format,
                name_template,
                combos,
                origin,
            },
        ) => run_assemble(AssembleOpts {
            inputs: &inputs,
            method: &method,
            topology: &topology,
            default_enzymes: enzymes.as_deref(),
            expand: &expand,
            emit_recipe: emit_recipe.as_deref(),
            dry_run,
            fidelity_dataset: fidelity_dataset.as_deref(),
            fidelity_matrix,
            out: out.as_deref(),
            format: &format,
            name_template: name_template.as_deref(),
            combos: combos.as_deref(),
            origin: origin.as_deref(),
        }),
        (DocSource::Paths, req) => run_on_file(req),
        (_, req) => dispatch_viewer_cmd(req),
    }
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

#[cfg(test)]
mod routing_tests {
    use super::DocSource;
    use seqforge_core::{Target, ViewerRequest};

    fn assemble(inputs: &[&str]) -> ViewerRequest {
        ViewerRequest::Assemble {
            inputs: inputs.iter().map(|s| s.to_string()).collect(),
            method: "ligate".into(),
            topology: "circular".into(),
            enzymes: None,
            expand: "all-to-all".into(),
            emit_recipe: None,
            dry_run: false,
            fidelity_dataset: None,
            fidelity_matrix: false,
            out: None,
            format: "genbank".into(),
            name_template: None,
            combos: None,
            origin: None,
        }
    }

    /// The rule that replaced the two-tier command split: routing follows the
    /// document, not the verb (ROADMAP decision 27).
    #[test]
    fn path_inputs_run_locally() {
        assert_eq!(
            DocSource::of(&assemble(&["a.gb", "parts/*.gb@BsaI..BsaI"])),
            DocSource::Paths,
            "nothing here needs a session, so no socket is required"
        );
    }

    #[test]
    fn a_buffer_input_needs_the_session_that_owns_it() {
        assert_eq!(
            DocSource::of(&assemble(&["a.gb", "buffer:3@BsaI..BsaI"])),
            DocSource::Session,
            "one live source is enough to make the whole request session-bound"
        );
    }

    #[test]
    fn no_inputs_means_the_open_workbench_recipe() {
        assert_eq!(DocSource::of(&assemble(&[])), DocSource::Session);
    }

    #[test]
    fn view_scoped_verbs_are_always_session_bound() {
        assert_eq!(
            DocSource::of(&ViewerRequest::GoTo {
                position: 10,
                target: Target::active()
            }),
            DocSource::Session
        );
    }
}
