//! The assembly engine — the shared pipeline over the closed `Fragment` IR
//! (ROADMAP decision 21). A recipe of **bins** is run as
//! `resolve · prepare(5′..3′) · expand · join · name`; only the **join** is
//! method-specific (`fn(Vec<Fragment>) -> Vec<Product>`), so a new method is one
//! file. Pure and headless — the app and CLI both drive it through a
//! [`SourceResolver`], differing only in how they resolve a [`SourceRef`].
//!
//! Ops are organized as **plugin cards** (`prepare/*`, `join/*` — each a pure fn
//! dispatched by a closed `match`). The `Box<dyn Op>` registry is deferred until
//! ≥2 real ops prove the shape (`docs/extensibility.md`).

mod expand;
mod naming;
mod prepare;
mod select;

pub mod discover;
pub mod join;

use std::path::{Path, PathBuf};

use seqforge_core::commands::FragmentInfo;
use seqforge_core::{
    Annotations, Bin, Boundary, Fragment, PrepareKind, Recipe, SourceRef, SpanEnds, Topology,
    TopologyIntent,
};

/// Expand a possibly-glob path into concrete paths (decision 26 bulk sources).
///
/// Supports `*` wildcards in the **final** path component only (`parts/*.gb`,
/// `dir/pre*`, `dir/*mid*`); the directory prefix must be literal. A pattern with
/// no `*` passes through unchanged (even if missing — the caller resolves and
/// reports). Matches are returned sorted; a glob that matches nothing yields an
/// empty vec. No new dependency — a tiny classic wildcard matcher.
pub fn expand_glob(pattern: &str) -> Vec<PathBuf> {
    if !pattern.contains('*') {
        return vec![PathBuf::from(pattern)];
    }
    let path = Path::new(pattern);
    let (dir, name_pat) = match path.file_name().and_then(|n| n.to_str()) {
        Some(name) => (path.parent().unwrap_or_else(|| Path::new(".")), name),
        None => return Vec::new(),
    };
    let dir = if dir.as_os_str().is_empty() {
        Path::new(".")
    } else {
        dir
    };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = entries
        .flatten()
        .filter(|e| {
            e.file_name()
                .to_str()
                .is_some_and(|n| wildcard_match(name_pat, n))
        })
        .map(|e| e.path())
        .collect();
    out.sort();
    out
}

/// Classic `*`-only wildcard match (each `*` matches any run, including empty).
fn wildcard_match(pattern: &str, name: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        return pattern == name;
    }
    let mut pos = 0usize;
    // First literal must anchor the start; last must anchor the end.
    if let Some(first) = parts.first() {
        if !name[pos..].starts_with(first) {
            return false;
        }
        pos += first.len();
    }
    for mid in &parts[1..parts.len() - 1] {
        if mid.is_empty() {
            continue;
        }
        match name[pos..].find(mid) {
            Some(i) => pos += i + mid.len(),
            None => return false,
        }
    }
    let last = parts[parts.len() - 1];
    name[pos..].ends_with(last) && name.len() - pos >= last.len()
}

/// A resolved source: the bytes + annotations + topology behind a [`SourceRef`].
/// The app resolves a `Buffer` handle against the live workspace; the CLI loads a
/// `Path`.
pub struct ResolvedSource {
    pub name: String,
    pub bytes: Vec<u8>,
    pub topology: Topology,
    pub ann: Annotations,
}

/// Resolves a recipe's [`SourceRef`]s to bytes. Implemented by the app (over the
/// workspace) and the CLI (over the filesystem).
pub trait SourceResolver {
    fn resolve(&self, r: &SourceRef) -> Result<ResolvedSource, String>;
}

/// A filesystem-only resolver (headless CLI). `Buffer` handles have no meaning
/// without a running workspace, so they error.
pub struct FileResolver;

impl SourceResolver for FileResolver {
    fn resolve(&self, r: &SourceRef) -> Result<ResolvedSource, String> {
        match r {
            SourceRef::Path(p) => {
                let doc = crate::load(p).map_err(|e| format!("{}: {e}", p.display()))?;
                let name = p
                    .file_name()
                    .and_then(|n| n.to_str())
                    .map(str::to_owned)
                    .unwrap_or(doc.name);
                Ok(ResolvedSource {
                    name,
                    bytes: doc.sequence,
                    topology: doc.topology,
                    ann: Annotations::from_parts(doc.features, doc.primers),
                })
            }
            SourceRef::Buffer(_) => {
                Err("buffer sources require a running SeqForge (use a file path)".into())
            }
        }
    }
}

/// A named product (`Product = Fragment`, closure) ready to materialize.
pub struct NamedProduct {
    pub name: String,
    pub fragment: Fragment,
}

/// The outcome of running a recipe.
pub struct AssemblyResult {
    pub products: Vec<NamedProduct>,
    /// Non-fatal advisories (a bin produced nothing, a source failed to resolve,
    /// a combo didn't assemble, intent unachievable, …).
    pub warnings: Vec<String>,
}

/// Resolve one bin to its candidate pool: **per source**, apply the bin's
/// prepare op, then for Digest narrow by the source's span override or the
/// bin's 5′→3′ ends (decision 26). Warnings accumulate; a failed source is skipped.
fn resolve_bin(
    bin: &Bin,
    resolver: &dyn SourceResolver,
    warnings: &mut Vec<String>,
) -> Vec<Fragment> {
    let mut pool = Vec::new();
    for source in &bin.sources {
        let resolved = match resolver.resolve(&source.ref_) {
            Ok(r) => r,
            Err(e) => {
                warnings.push(format!("bin \"{}\": {e}", bin.role));
                continue;
            }
        };
        let frags = match prepare::prepare_source(&bin.prepare, &resolved) {
            Ok(f) => f,
            Err(e) => {
                warnings.push(format!("bin \"{}\" · {}: {e}", bin.role, resolved.name));
                continue;
            }
        };
        let ctx = format!("bin \"{}\" · {}", bin.role, resolved.name);
        let kept = match &bin.prepare {
            PrepareKind::Digest {
                five_prime,
                three_prime,
            } => {
                let span = source
                    .span
                    .clone()
                    .unwrap_or_else(|| SpanEnds::new(five_prime.clone(), three_prime.clone()));
                let incomplete = matches!(
                    (&span.five_prime, &span.three_prime),
                    (Boundary::EnzymeSite { enzyme: a, .. }, Boundary::EnzymeSite { enzyme: b, .. })
                        if a.is_empty() || b.is_empty()
                );
                if incomplete {
                    warnings.push(format!("{ctx}: digest 5′→3′ ends not set"));
                    Vec::new()
                } else {
                    select::by_span(&span, frags, &ctx, warnings)
                }
            }
            // PCR / AsIs already yield the intended piece(s).
            _ => frags,
        };
        pool.extend(kept);
    }
    pool
}

/// Run a recipe: bins → prepared/narrowed candidate pools → expanded combos →
/// joined + intent-filtered products, each named from provenance.
pub fn run(recipe: &Recipe, resolver: &dyn SourceResolver) -> AssemblyResult {
    let mut warnings = Vec::new();
    let pools = resolve_pools(recipe, resolver, &mut warnings);
    let combos = expand::expand(&pools, recipe.expand);
    let indices: Vec<usize> = (0..combos.len()).collect();
    finish_run(recipe, &combos, &indices, warnings)
}

/// Run only the expanded combos at `indices` (order preserved). Empty selection
/// yields no products and a warning.
pub fn run_indices(
    recipe: &Recipe,
    resolver: &dyn SourceResolver,
    indices: &[usize],
) -> AssemblyResult {
    let mut warnings = Vec::new();
    if indices.is_empty() {
        warnings.push("no combos selected".to_string());
        return AssemblyResult {
            products: Vec::new(),
            warnings,
        };
    }
    let pools = resolve_pools(recipe, resolver, &mut warnings);
    let combos = expand::expand(&pools, recipe.expand);
    finish_run(recipe, &combos, indices, warnings)
}

fn resolve_pools(
    recipe: &Recipe,
    resolver: &dyn SourceResolver,
    warnings: &mut Vec<String>,
) -> Vec<Vec<Fragment>> {
    let mut pools: Vec<Vec<Fragment>> = Vec::with_capacity(recipe.bins.len());
    for bin in &recipe.bins {
        let pool = resolve_bin(bin, resolver, warnings);
        if pool.is_empty() {
            warnings.push(format!("bin \"{}\" produced no fragments", bin.role));
        }
        pools.push(pool);
    }
    pools
}

fn finish_run(
    recipe: &Recipe,
    combos: &[Vec<Fragment>],
    indices: &[usize],
    mut warnings: Vec<String>,
) -> AssemblyResult {
    let mut products = Vec::new();
    for &i in indices {
        let Some(combo) = combos.get(i).cloned() else {
            warnings.push(format!("combo index {i} out of range"));
            continue;
        };
        for fragment in join::join(&recipe.join, combo, recipe.intent) {
            products.push(fragment);
        }
    }
    if products.is_empty()
        && !indices.is_empty()
        && indices
            .iter()
            .any(|&i| combos.get(i).is_some_and(|c| !c.is_empty()))
    {
        warnings.push(
            "no product assembled (authored 5′/3′ ends do not match in bin order)".to_string(),
        );
    }

    let named = naming::name_products(recipe, products);
    AssemblyResult {
        products: named,
        warnings,
    }
}

/// One fragment contribution inside a combo (one bin).
#[derive(Debug, Clone)]
pub struct ComboPart {
    pub source_name: String,
    pub length: usize,
}

/// Compact per-combo summary for GUI tables and CLI dry-run.
#[derive(Debug, Clone)]
pub struct ComboSummary {
    pub index: usize,
    pub ok: bool,
    pub parts: Vec<ComboPart>,
    pub detail: Option<String>,
    /// Informational set fidelity (0..1) when a dataset was requested and the
    /// combo's overhangs were fully covered. Never gates Run.
    pub fidelity: Option<f64>,
    /// True when `fidelity` is `Some` and at least one scored overhang was
    /// 3′-chemistry (5′-assay data applied; UI shows `*`).
    pub fidelity_three_prime: bool,
}

/// Expand + probe every combo (same order as [`run`] / [`run_indices`]).
///
/// When `dataset` is `Some`, each combo is scored with
/// [`seqforge_fidelity::junction_fidelity`] over harvested sticky overhang
/// sequences (NEB-style: letters only; 5′-assay matrices).
/// Fidelity is **informational only** — it does not affect `ok` or Run.
pub fn enumerate_combos(
    recipe: &Recipe,
    resolver: &dyn SourceResolver,
    dataset: Option<seqforge_fidelity::Dataset>,
) -> (Vec<ComboSummary>, Vec<String>) {
    let mut warnings = Vec::new();
    let pools = resolve_pools(recipe, resolver, &mut warnings);
    let bin_roles: Vec<String> = recipe.bins.iter().map(|b| b.role.clone()).collect();
    let combos = expand::expand(&pools, recipe.expand);
    let circular = matches!(recipe.intent, TopologyIntent::Circular);
    let mut out = Vec::with_capacity(combos.len());
    for (index, combo) in combos.into_iter().enumerate() {
        let probe = join::probe_join(&combo);
        let ok = probe.compatible_for(recipe.intent);
        let annotated = annotate_probe(probe, &bin_roles);
        let detail = if ok {
            None
        } else {
            annotated
                .junctions
                .iter()
                .find(|j| !j.ok)
                .map(|j| j.detail.clone())
                .or_else(|| {
                    (!annotated.closes && matches!(recipe.intent, TopologyIntent::Circular))
                        .then(|| "circular ends do not close".to_string())
                })
                .or_else(|| Some("ends do not match".into()))
        };
        let (fidelity, fidelity_three_prime) = match dataset {
            Some(ds) => score_combo_fidelity(&combo, circular, ds),
            None => (None, false),
        };
        let parts = combo
            .iter()
            .map(|f| ComboPart {
                source_name: f.lineage.source_doc.clone(),
                length: f.len(),
            })
            .collect();
        out.push(ComboSummary {
            index,
            ok,
            parts,
            detail,
            fidelity,
            fidelity_three_prime,
        });
    }
    (out, warnings)
}

/// Returns `(set_fidelity, used_three_prime_chemistry)`.
fn score_combo_fidelity(
    combo: &[seqforge_core::Fragment],
    circular: bool,
    dataset: seqforge_fidelity::Dataset,
) -> (Option<f64>, bool) {
    let harvested = join::harvest_junction_overhangs(combo, circular);
    if harvested.is_empty() || harvested.iter().any(|h| h.is_none()) {
        return (None, false);
    }
    let used_three_prime = harvested.iter().flatten().any(|h| h.three_prime);
    match score_harvested(&harvested, dataset).and_then(|r| r.set_fidelity) {
        Some(f) => (Some(f), used_three_prime),
        None => (None, false),
    }
}

fn score_harvested(
    harvested: &[Option<join::HarvestedOverhang>],
    dataset: seqforge_fidelity::Dataset,
) -> Option<seqforge_fidelity::FidelityReport> {
    if harvested.is_empty() || harvested.iter().any(|h| h.is_none()) {
        return None;
    }
    let owned: Vec<Vec<u8>> = harvested.iter().flatten().map(|h| h.seq.clone()).collect();
    let refs: Vec<&[u8]> = owned.iter().map(|s| s.as_slice()).collect();
    if !dataset.covers(&refs) {
        return None;
    }
    Some(seqforge_fidelity::junction_fidelity(&refs, dataset))
}

/// Subset ligation-frequency matrix for the first compatible combo (else the
/// first combo). CLI `--fidelity-matrix` overlay; does not affect Run.
pub fn first_combo_fidelity_matrix(
    recipe: &Recipe,
    resolver: &dyn SourceResolver,
    dataset: seqforge_fidelity::Dataset,
) -> Option<(usize, seqforge_fidelity::SubsetMatrix)> {
    let mut warnings = Vec::new();
    let pools = resolve_pools(recipe, resolver, &mut warnings);
    let combos = expand::expand(&pools, recipe.expand);
    if combos.is_empty() {
        return None;
    }
    let circular = matches!(recipe.intent, TopologyIntent::Circular);
    let pick = combos
        .iter()
        .enumerate()
        .find(|(_, combo)| join::probe_join(combo).compatible_for(recipe.intent))
        .map(|(i, _)| i)
        .unwrap_or(0);
    let harvested = join::harvest_junction_overhangs(&combos[pick], circular);
    let report = score_harvested(&harvested, dataset)?;
    Some((pick, report.matrix?))
}

/// Preview one bin's candidate fragments (resolve · prepare · span) as the
/// serializable projection — drives the workbench's per-bin fragment list.
pub fn preview_bin(bin: &Bin, resolver: &dyn SourceResolver) -> (Vec<FragmentInfo>, Vec<String>) {
    let mut warnings = Vec::new();
    let pool = resolve_bin(bin, resolver, &mut warnings);
    let infos = pool.iter().enumerate().map(|(i, f)| f.to_info(i)).collect();
    (infos, warnings)
}

/// Dry-run join validity: resolve/prepare pools, expand combos, probe ends
/// **without** building products. Same identity-only predicate as `run`.
pub fn probe_recipe(recipe: &Recipe, resolver: &dyn SourceResolver) -> RecipeJoinProbe {
    let mut warnings = Vec::new();
    let pools = resolve_pools(recipe, resolver, &mut warnings);
    let bin_roles: Vec<String> = recipe.bins.iter().map(|b| b.role.clone()).collect();
    let combos = expand::expand(&pools, recipe.expand);
    let total = combos.len();
    let mut compatible = 0usize;
    let mut first: Option<join::JoinProbe> = None;
    let mut sample: Option<join::JoinProbe> = None;
    let mut sample_ok = true;
    const DETAIL_CAP: usize = 32;
    let mut detailed: Vec<join::JoinProbe> = Vec::new();

    for (ci, combo) in combos.into_iter().enumerate() {
        let probe = join::probe_join(&combo);
        let ok = probe.compatible_for(recipe.intent);
        if ok {
            compatible += 1;
        }
        let annotated = annotate_probe(probe, &bin_roles);
        if ci == 0 {
            first = Some(annotated.clone());
        }
        if sample.is_none() || (sample_ok && !ok) {
            sample = Some(annotated.clone());
            sample_ok = ok;
        }
        if detailed.len() < DETAIL_CAP {
            detailed.push(annotated);
        }
    }

    RecipeJoinProbe {
        combos: total,
        compatible_combos: compatible,
        first,
        sample,
        detailed: if total <= DETAIL_CAP {
            detailed
        } else {
            Vec::new()
        },
        warnings,
        bin_roles,
    }
}

fn annotate_probe(mut probe: join::JoinProbe, roles: &[String]) -> join::JoinProbe {
    for j in &mut probe.junctions {
        let from = roles.get(j.from).map(|s| s.as_str()).unwrap_or("?");
        let to = roles.get(j.to).map(|s| s.as_str()).unwrap_or("?");
        j.detail = format!("{from} → {to}: {}", j.detail);
    }
    probe
}

/// Result of [`probe_recipe`] — join validity without materializing products.
#[derive(Debug, Clone)]
pub struct RecipeJoinProbe {
    pub combos: usize,
    pub compatible_combos: usize,
    /// First expanded combo (batch-first default: first source per bin).
    pub first: Option<join::JoinProbe>,
    /// Representative probe (prefers a failing combo when any fail).
    pub sample: Option<join::JoinProbe>,
    /// Per-combo probes when `combos ≤ 32`; otherwise empty (use counts + sample).
    pub detailed: Vec<join::JoinProbe>,
    pub warnings: Vec<String>,
    pub bin_roles: Vec<String>,
}

/// Digest a source with the enzymes implied by the 5′/3′ ends and return **all**
/// candidate bands (no span pick) — internal helper; not a UX band list.
pub fn preview_digest_candidates(
    resolved: &ResolvedSource,
    five_prime: &Boundary,
    three_prime: &Boundary,
) -> Vec<FragmentInfo> {
    let prepare = PrepareKind::Digest {
        five_prime: five_prime.clone(),
        three_prime: three_prime.clone(),
    };
    let Ok(frags) = prepare::prepare_source(&prepare, resolved) else {
        return Vec::new();
    };
    frags
        .iter()
        .enumerate()
        .map(|(i, f)| f.to_info(i))
        .collect()
}
