//! Produce verbs: mint buffers or panes.
//!
//! Digestion's fragment *list* is a [`crate::query`]; this module opens the
//! Fragments pane. PCR and assemble mint new buffers (and optionally write
//! products to disk).

use std::path::PathBuf;

use seqforge_core::{
    Annotations, AssemblyBinPreview, AssemblyComboPreview, AssemblyPartPreview, DispatchError,
    Expand, FidelityMatrixPreview, JoinKind, Orient, PartialPolicy, ProductInfo, Recipe, Topology,
    TopologyIntent, ViewId, ViewKind, ViewerResponse, transport,
};

use crate::edit::resolve_target;
use crate::execute::{Executed, MAX_MATERIALIZED};
use crate::host::{Host, Level};
use crate::query;
use crate::resolver::WorkspaceResolver;
use crate::workspace::Workspace;

fn done(response: ViewerResponse) -> Result<Executed, DispatchError> {
    Ok(Executed {
        response,
        opened: Vec::new(),
    })
}

fn done_opened(response: ViewerResponse, opened: Vec<ViewId>) -> Result<Executed, DispatchError> {
    Ok(Executed { response, opened })
}

/// Digest: query the fragment set, then open a Fragments pane.
pub fn digest(
    ws: &mut Workspace,
    host: &mut dyn Host,
    vid: ViewId,
    enzymes: &[String],
    circular: bool,
) -> Result<Executed, DispatchError> {
    let (source_buffer, name, canonical, infos, warnings) =
        ws.with_buffer(vid, |v, buf, ann| {
            match query::digest_fragments(v, buf, ann, enzymes, circular) {
                ViewerResponse::Fragments {
                    name,
                    enzymes: canonical,
                    fragments: infos,
                    warnings,
                    ..
                } => (v.buffer_id, name, canonical, infos, warnings),
                other => panic!("digest_fragments returned {other:?}"),
            }
        })?;

    for w in &warnings {
        host.notify(Level::Warning, format!("Digest: {w}"));
    }
    let frag_vid = ws.add_view(source_buffer, ViewKind::Fragments);
    if let Some(v) = ws.view_mut(frag_vid) {
        v.fragments_query = Some(canonical.clone());
    }
    done_opened(
        ViewerResponse::Fragments {
            name,
            enzymes: canonical,
            count: infos.len(),
            fragments: infos,
            warnings,
        },
        vec![frag_vid],
    )
}

/// PCR: mint a linear amplicon buffer.
pub fn pcr(
    ws: &mut Workspace,
    host: &mut dyn Host,
    view: Option<ViewId>,
    fwd: seqforge_core::PrimerId,
    rev: seqforge_core::PrimerId,
    name: Option<String>,
) -> Result<Executed, DispatchError> {
    let vid = resolve_target(ws, view)?;

    struct Built {
        bytes: Vec<u8>,
        ann: Annotations,
        name: String,
        warnings: Vec<String>,
    }

    let built = ws.with_buffer(vid, |_, buf, ann| {
        let fwd_p = ann
            .primer(fwd)
            .ok_or_else(|| DispatchError::InvalidInput(format!("no primer with id {fwd}")))?;
        let rev_p = ann
            .primer(rev)
            .ok_or_else(|| DispatchError::InvalidInput(format!("no primer with id {rev}")))?;

        let prod = seqforge_bio::pcr(&buf.text, fwd_p, rev_p, buf.is_circular())
            .map_err(|e| DispatchError::InvalidInput(e.to_string()))?;

        let mut slice = transport::extract(
            &buf.text,
            ann,
            prod.amplicon,
            PartialPolicy::TruncatePartials,
            &buf.name,
        );
        slice.primers.retain(|p| p.binding.is_some());

        let mut prod_ann = Annotations::default();
        transport::place(
            &mut prod_ann,
            &slice,
            prod.tail_f_len,
            Orient::Identity,
            false,
            prod.bytes.len(),
        );
        prod.reanchor_primers(&mut prod_ann);

        let name = name
            .clone()
            .unwrap_or_else(|| format!("{} amplicon", buf.name));
        Ok::<Built, DispatchError>(Built {
            bytes: prod.bytes,
            ann: prod_ann,
            name,
            warnings: prod.warnings,
        })
    })??;

    let len = built.bytes.len();
    for w in &built.warnings {
        host.notify(Level::Warning, format!("PCR: {w}"));
    }
    let view_id = ws.new_buffer_annotated(built.name, built.bytes, Topology::Linear, built.ann);
    done_opened(ViewerResponse::Edited { len, changed: true }, vec![view_id])
}

/// How the caller chose which combos to run.
pub enum ComboSelection {
    /// Already-resolved indices (`None` = every compatible combo).
    Indices(Option<Vec<usize>>),
    /// An unparsed `--combos` selector (`None` = every compatible combo).
    Spec(Option<String>),
}

/// Inputs to [`assemble`] after a [`Recipe`] exists.
pub struct AssembleOpts {
    pub recipe: Recipe,
    pub dry_run: bool,
    pub fidelity_dataset: Option<String>,
    pub fidelity_matrix: bool,
    pub emit_recipe: Option<PathBuf>,
    pub out: Option<PathBuf>,
    pub format: String,
    pub combos: ComboSelection,
    pub origin: Option<String>,
}

/// Build a [`Recipe`] from the clap/serde fields of [`ViewerRequest::Assemble`].
pub fn recipe_from_flags(
    inputs: &[String],
    method: &str,
    topology: &str,
    enzymes: Option<&str>,
    expand: &str,
) -> Result<Recipe, DispatchError> {
    let bad = DispatchError::InvalidInput;
    if inputs.len() == 1 && inputs[0].ends_with(".json") {
        let text = std::fs::read_to_string(&inputs[0])
            .map_err(|e| bad(format!("read recipe {}: {e}", inputs[0])))?;
        return serde_json::from_str(&text)
            .map_err(|e| bad(format!("parse recipe {}: {e}", inputs[0])));
    }
    if inputs.is_empty() {
        return Err(bad(
            "no inputs — pass a recipe.json or bin tokens (SOURCE[@FROM..TO])".into(),
        ));
    }
    let bins = inputs
        .iter()
        .map(|t| seqforge_bio::parse_bin_token(t, enzymes))
        .collect::<Result<Vec<_>, String>>()
        .map_err(bad)?;
    let join = match method {
        "ligate" => JoinKind::Ligate,
        "golden-gate" | "golden_gate" | "gg" => {
            let enzyme = enzymes
                .map(seqforge_bio::normalize_enzymes)
                .and_then(|e| e.split_whitespace().next().map(str::to_string))
                .ok_or_else(|| bad("--method golden-gate needs --enzymes (e.g. BsaI)".into()))?;
            JoinKind::GoldenGate { enzyme }
        }
        other => {
            return Err(bad(format!(
                "unknown method {other:?} (supports: ligate, golden-gate)"
            )));
        }
    };
    Ok(Recipe {
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
    })
}

/// Run (or dry-run) an already-built recipe against the workspace.
///
/// Workbench Run and `ViewerRequest::Assemble` both call this.
pub fn assemble(
    ws: &mut Workspace,
    host: &mut dyn Host,
    opts: AssembleOpts,
) -> Result<Executed, DispatchError> {
    let AssembleOpts {
        recipe,
        dry_run,
        fidelity_dataset,
        fidelity_matrix,
        emit_recipe,
        out,
        format,
        combos,
        origin,
    } = opts;

    if let Some(path) = &emit_recipe {
        let json = serde_json::to_string_pretty(&recipe)
            .map_err(|e| DispatchError::InvalidInput(format!("serialize recipe: {e}")))?;
        std::fs::write(path, json).map_err(|e| {
            DispatchError::InvalidInput(format!("write recipe {}: {e}", path.display()))
        })?;
    }

    if dry_run {
        return assemble_dry_run(ws, &recipe, fidelity_dataset.as_deref(), fidelity_matrix);
    }
    if fidelity_dataset.is_some() {
        return Err(DispatchError::InvalidInput(
            "--fidelity-dataset only applies with --dry-run (scores are not persisted)".into(),
        ));
    }
    if fidelity_matrix {
        return Err(DispatchError::InvalidInput(
            "--fidelity-matrix only applies with --dry-run --fidelity-dataset".into(),
        ));
    }

    let format = seqforge_bio::ProductFormat::parse(&format).ok_or_else(|| {
        DispatchError::InvalidInput(format!(
            "unknown product format {format:?} (supports: genbank, fasta)"
        ))
    })?;
    let origin = origin.map(|spec| spec.parse().unwrap_or_else(|e| match e {}));
    assemble_materialize(ws, host, &recipe, combos, out.map(|d| (d, format)), origin)
}

fn assemble_dry_run(
    ws: &Workspace,
    recipe: &Recipe,
    fidelity_dataset: Option<&str>,
    fidelity_matrix: bool,
) -> Result<Executed, DispatchError> {
    if fidelity_matrix && fidelity_dataset.is_none() {
        return Err(DispatchError::InvalidInput(
            "--fidelity-matrix requires --fidelity-dataset".into(),
        ));
    }
    let dataset = match fidelity_dataset {
        None => None,
        Some(name) => Some(seqforge_bio::FidelityDataset::parse(name).ok_or_else(|| {
            DispatchError::InvalidInput(format!(
                "unknown --fidelity-dataset {name:?} (try t4_25c_18h, bsai, sapi, …)"
            ))
        })?),
    };

    let resolver = WorkspaceResolver { ws };
    let bins: Vec<AssemblyBinPreview> = recipe
        .bins
        .iter()
        .map(|b| {
            let (infos, warnings) = seqforge_bio::preview_bin(b, &resolver);
            AssemblyBinPreview {
                role: b.role.clone(),
                fragments: infos.len(),
                warnings,
            }
        })
        .collect();
    let (summaries, warnings) = seqforge_bio::enumerate_combos(recipe, &resolver, dataset);
    let compatible = summaries.iter().filter(|c| c.ok).count();
    let combo_list: Vec<AssemblyComboPreview> = summaries
        .iter()
        .map(|c| AssemblyComboPreview {
            index: c.index,
            ok: c.ok,
            parts: c
                .parts
                .iter()
                .map(|p| AssemblyPartPreview {
                    source: p.source_name.clone(),
                    length: p.length,
                })
                .collect(),
            detail: c.detail.clone(),
            fidelity: if dataset.is_some() { c.fidelity } else { None },
            fidelity_three_prime: dataset.is_some() && c.fidelity_three_prime,
        })
        .collect();

    let fidelity_dataset_id = dataset.map(|d| d.id().to_string());
    let fidelity_matrix_preview = if fidelity_matrix {
        if let Some(ds) = dataset {
            seqforge_bio::first_combo_fidelity_matrix(recipe, &resolver, ds).map(
                |(combo_index, matrix)| {
                    let n = matrix.dim();
                    let labels: Vec<String> = matrix
                        .labels
                        .iter()
                        .map(|l| String::from_utf8_lossy(l).into_owned())
                        .collect();
                    let counts: Vec<Vec<u32>> = (0..n)
                        .map(|i| (0..n).map(|j| matrix.get(i, j)).collect())
                        .collect();
                    FidelityMatrixPreview {
                        combo_index,
                        labels,
                        counts,
                    }
                },
            )
        } else {
            None
        }
    } else {
        None
    };

    done(ViewerResponse::AssemblyDryRun {
        bins,
        combos: summaries.len(),
        compatible_combos: compatible,
        combo_list,
        warnings,
        fidelity_dataset: fidelity_dataset_id,
        fidelity_matrix: fidelity_matrix_preview,
    })
}

fn assemble_materialize(
    ws: &mut Workspace,
    host: &mut dyn Host,
    recipe: &Recipe,
    selection: ComboSelection,
    export: Option<(PathBuf, seqforge_bio::ProductFormat)>,
    origin: Option<seqforge_bio::OriginSpec>,
) -> Result<Executed, DispatchError> {
    let result = {
        let resolver = WorkspaceResolver { ws };
        let compatible = |resolver: &WorkspaceResolver| -> Vec<usize> {
            let (summaries, _) = seqforge_bio::enumerate_combos(recipe, resolver, None);
            summaries
                .into_iter()
                .filter(|c| c.ok)
                .map(|c| c.index)
                .collect()
        };
        let indices: Vec<usize> = match &selection {
            ComboSelection::Indices(Some(v)) => v.clone(),
            ComboSelection::Indices(None) => compatible(&resolver),
            ComboSelection::Spec(None) => compatible(&resolver),
            ComboSelection::Spec(Some(spec)) => {
                let (summaries, _) = seqforge_bio::enumerate_combos(recipe, &resolver, None);
                seqforge_bio::parse_combo_spec(spec, summaries.len())
                    .map_err(|e| DispatchError::InvalidInput(format!("combos: {e}")))?
            }
        };
        seqforge_bio::run_indices(recipe, &resolver, &indices)
    };

    let mut result = result;
    if let Some(spec) = &origin {
        seqforge_bio::set_origins(&mut result.products, spec)
            .map_err(DispatchError::InvalidInput)?;
    }
    let result = result;

    for w in &result.warnings {
        host.notify(Level::Warning, format!("Assemble: {w}"));
    }

    let total = result.products.len();
    if total == 0 {
        host.notify(Level::Warning, "Assemble: no product produced".into());
        return done(ViewerResponse::Products {
            count: 0,
            products: Vec::new(),
            warnings: result.warnings,
        });
    }

    let paths: Vec<Option<PathBuf>> = match &export {
        Some((dir, format)) => seqforge_bio::write_products(&result.products, dir, *format)
            .map_err(|e| DispatchError::InvalidInput(format!("write products: {e}")))?
            .into_iter()
            .map(Some)
            .collect(),
        None => vec![None; total],
    };

    let infos: Vec<ProductInfo> = result
        .products
        .iter()
        .zip(&paths)
        .map(|(p, path)| ProductInfo {
            name: p.name.clone(),
            length: p.fragment.len(),
            topology: p.fragment.topology,
            combo_index: p.combo_index,
            parts: p.parts.iter().map(|c| c.source_name.clone()).collect(),
            path: path.clone(),
        })
        .collect();

    let capped = total > MAX_MATERIALIZED;
    let mut opened = Vec::new();
    for prod in result.products.into_iter().take(MAX_MATERIALIZED) {
        let ann =
            Annotations::from_parts(prod.fragment.slice.features, prod.fragment.slice.primers);
        let vid = ws.new_buffer_annotated(
            prod.name,
            prod.fragment.slice.bytes,
            prod.fragment.topology,
            ann,
        );
        opened.push(vid);
    }
    if capped {
        let written = if export.is_some() {
            " (all were written to disk)"
        } else {
            ""
        };
        host.notify(
            Level::Warning,
            format!("Assemble: {total} products, opened the first {MAX_MATERIALIZED}{written}"),
        );
    }
    if let Some((dir, _)) = &export {
        host.notify(
            Level::Info,
            format!("Assemble: wrote {total} product(s) to {}", dir.display()),
        );
    }
    done_opened(
        ViewerResponse::Products {
            count: infos.len(),
            products: infos,
            warnings: result.warnings,
        },
        opened,
    )
}
