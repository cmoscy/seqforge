//! Assembly-recipe workbench commands: author bins (`EditRecipe`), run the
//! recipe into product buffers (`RunRecipe`), and save/load the recipe document.
//! Recipe edits route through one `EditRecipe { op }` command so `apply` stays
//! the single mutation site (decision 5) without a per-field command explosion.

use std::path::PathBuf;

use egui_file_dialog::FileDialog;
use seqforge_core::{
    Bin, Boundary, DispatchError, PrepareKind, Recipe, RecipeId, SourceRef, SpanEnds,
    TopologyIntent, ViewerResponse, default_role,
};
use seqforge_session::Executed;

use super::{layout, snapshot_focus_for_overlay};
use crate::app::AppState;
use crate::focus::FocusScope;
use crate::overlay::Overlay;

/// An in-place edit to a recipe's authored state (decision 26 — batch-first).
#[derive(Debug, Clone)]
pub enum RecipeOp {
    AddBin,
    RemoveBin(usize),
    SetRole {
        bin: usize,
        role: String,
    },
    /// Bulk-add sources (multi-file pick / glob / drag / open buffer).
    AddSources {
        bin: usize,
        sources: Vec<SourceRef>,
    },
    RemoveSource {
        bin: usize,
        index: usize,
    },
    /// Switch / set the prepare op (Digest 5′→3′ / PCR / As-is).
    SetPrepare {
        bin: usize,
        prepare: PrepareKind,
    },
    /// Set a Digest 5′→3′ span (convenience over [`RecipeOp::SetPrepare`]).
    SetPrepareEnds {
        bin: usize,
        five_prime: Boundary,
        three_prime: Boundary,
    },
    /// Per-source 5′→3′ override (exception path for `@pos`).
    SetSourceSpan {
        bin: usize,
        index: usize,
        span: Option<SpanEnds>,
    },
    SetIntent(TopologyIntent),
    SetExpand(seqforge_core::Expand),
    /// Join verb (+ GG enzyme when Golden Gate).
    SetJoin(seqforge_core::JoinKind),
}

/// What a pending file dialog should do with its pick (workbench flows).
#[derive(Debug, Clone)]
pub enum RecipeDialog {
    AddSource {
        recipe: RecipeId,
        bin: usize,
    },
    SaveRecipe(RecipeId),
    LoadRecipe(RecipeId),
    /// Run the recipe and write every product into the picked directory.
    RunToDir(RecipeId),
}

pub(super) fn apply_new_recipe(
    state: &mut AppState,
) -> Result<Option<ViewerResponse>, DispatchError> {
    let id = state.workspace.add_recipe(Recipe::with_bins(2));
    layout::place_recipe_tab(state, id);
    layout::ensure_welcome_invariant(state);
    layout::dock_activate_recipe(state, id);
    state.focus.set_scope(FocusScope::Recipe(id));
    Ok(None)
}

pub(super) fn apply_close_recipe(
    state: &mut AppState,
    id: RecipeId,
) -> Result<Option<ViewerResponse>, DispatchError> {
    layout::remove_recipe_tab(state, id);
    state.workspace.recipes.remove(&id);
    state.recipe_combo_selection.remove(&id);
    state.recipe_fidelity.remove(&id);
    layout::ensure_welcome_invariant(state);
    Ok(None)
}

pub(super) fn apply_edit_recipe(
    state: &mut AppState,
    id: RecipeId,
    op: RecipeOp,
) -> Result<Option<ViewerResponse>, DispatchError> {
    let recipe = state
        .workspace
        .recipe_mut(id)
        .ok_or_else(|| DispatchError::InvalidInput(format!("no recipe {id}")))?;
    match op {
        RecipeOp::AddBin => {
            let i = recipe.bins.len();
            recipe.bins.push(Bin::empty(i));
        }
        RecipeOp::RemoveBin(i) => {
            if i < recipe.bins.len() && recipe.bins.len() > 1 {
                recipe.bins.remove(i);
            }
        }
        RecipeOp::SetRole { bin, role } => {
            if let Some(b) = recipe.bins.get_mut(bin) {
                b.role = if role.trim().is_empty() {
                    default_role(bin)
                } else {
                    role
                };
            }
        }
        RecipeOp::AddSources { bin, sources } => {
            if let Some(b) = recipe.bins.get_mut(bin) {
                for source in sources {
                    b.sources.push(seqforge_core::Source {
                        ref_: source,
                        pin: None,
                        span: None,
                    });
                }
            }
        }
        RecipeOp::RemoveSource { bin, index } => {
            if let Some(b) = recipe.bins.get_mut(bin) {
                if index < b.sources.len() {
                    b.sources.remove(index);
                }
            }
        }
        RecipeOp::SetPrepare { bin, prepare } => {
            if let Some(b) = recipe.bins.get_mut(bin) {
                b.prepare = prepare;
            }
        }
        RecipeOp::SetPrepareEnds {
            bin,
            five_prime,
            three_prime,
        } => {
            if let Some(b) = recipe.bins.get_mut(bin) {
                b.prepare = PrepareKind::Digest {
                    five_prime,
                    three_prime,
                };
            }
        }
        RecipeOp::SetSourceSpan { bin, index, span } => {
            if let Some(s) = recipe
                .bins
                .get_mut(bin)
                .and_then(|b| b.sources.get_mut(index))
            {
                s.span = span;
            }
        }
        RecipeOp::SetIntent(intent) => recipe.intent = intent,
        RecipeOp::SetExpand(expand) => recipe.expand = expand,
        RecipeOp::SetJoin(join) => recipe.join = join,
    }
    state.recipe_combo_selection.remove(&id);
    Ok(None)
}

pub(super) fn apply_run_recipe(
    state: &mut AppState,
    id: RecipeId,
) -> Result<Option<ViewerResponse>, DispatchError> {
    let recipe = state
        .workspace
        .recipe(id)
        .cloned()
        .ok_or_else(|| DispatchError::InvalidInput(format!("no recipe {id}")))?;

    // A workbench Run honours the combo checkboxes; with none ticked it runs
    // every compatible combo.
    let selection = state.recipe_combo_selection.get(&id).map(|s| {
        let mut v: Vec<usize> = s.iter().copied().collect();
        v.sort_unstable();
        v
    });
    present_assemble(
        state,
        seqforge_session::AssembleOpts {
            recipe,
            dry_run: false,
            fidelity_dataset: None,
            fidelity_matrix: false,
            emit_recipe: None,
            out: None,
            format: "genbank".into(),
            combos: seqforge_session::ComboSelection::Indices(selection),
            origin: None,
        },
    )
}

/// Present an assembly run: `session::assemble` computes, we dock `opened`.
fn present_assemble(
    state: &mut AppState,
    opts: seqforge_session::AssembleOpts,
) -> Result<Option<ViewerResponse>, DispatchError> {
    let Executed { response, opened } = {
        let (ws, mut host) = state.session();
        seqforge_session::assemble(ws, &mut host, opts)?
    };
    let first = opened.first().copied();
    for vid in opened {
        layout::place_view_tab(state, vid);
    }
    if let Some(vid) = first {
        layout::ensure_welcome_invariant(state);
        layout::dock_activate_view(state, vid);
        state.focus.set_scope(FocusScope::View(vid));
    }
    Ok(Some(response))
}

/// Workbench Run with an export directory — the GUI half of the parity pair
/// with `seqforge assemble --out`.
pub(super) fn apply_run_recipe_to_dir(
    state: &mut AppState,
    id: RecipeId,
    dir: PathBuf,
) -> Result<Option<ViewerResponse>, DispatchError> {
    let recipe = state
        .workspace
        .recipe(id)
        .cloned()
        .ok_or_else(|| DispatchError::InvalidInput(format!("no recipe {id}")))?;
    let selection = state.recipe_combo_selection.get(&id).map(|s| {
        let mut v: Vec<usize> = s.iter().copied().collect();
        v.sort_unstable();
        v
    });
    present_assemble(
        state,
        seqforge_session::AssembleOpts {
            recipe,
            dry_run: false,
            fidelity_dataset: None,
            fidelity_matrix: false,
            emit_recipe: None,
            out: Some(dir),
            format: "genbank".into(),
            combos: seqforge_session::ComboSelection::Indices(selection),
            origin: None,
        },
    )
}

pub(super) fn apply_set_combo_selection(
    state: &mut AppState,
    id: RecipeId,
    selected: std::collections::HashSet<usize>,
) -> Result<Option<ViewerResponse>, DispatchError> {
    state.recipe_combo_selection.insert(id, selected);
    Ok(None)
}

pub(super) fn apply_set_fidelity(
    state: &mut AppState,
    id: RecipeId,
    dataset: Option<seqforge_bio::FidelityDataset>,
) -> Result<Option<ViewerResponse>, DispatchError> {
    state.recipe_fidelity.insert(id, dataset);
    Ok(None)
}

pub(super) fn apply_save_recipe(
    state: &mut AppState,
    id: RecipeId,
    path: PathBuf,
    force: bool,
) -> Result<Option<ViewerResponse>, DispatchError> {
    let path = ensure_json_extension(path);
    let recipe = state
        .workspace
        .recipe(id)
        .ok_or_else(|| DispatchError::InvalidInput(format!("no recipe {id}")))?
        .clone();

    if !force {
        match preflight_recipe_export(&recipe, &state.workspace) {
            Preflight::Scratch(msg) => {
                state.toasts.warning(msg);
                return Ok(None);
            }
            Preflight::Dirty { names, buffers } => {
                snapshot_focus_for_overlay(state);
                if let Some(tag) = state.overlays.push_unique(Overlay::DirtyRecipeSaveConfirm {
                    recipe: id,
                    path,
                    dirty_names: names,
                    dirty_buffers: buffers,
                }) {
                    state
                        .events
                        .emit(crate::event::AppEvent::OverlayPushed(tag));
                }
                return Ok(None);
            }
            Preflight::Clean => {}
        }
    }

    let normalized = match normalize_recipe_for_export(&recipe, &state.workspace) {
        Ok(r) => r,
        Err(msg) => {
            state.toasts.warning(msg);
            return Ok(None);
        }
    };
    let json = serde_json::to_string_pretty(&normalized)
        .map_err(|e| DispatchError::InvalidInput(e.to_string()))?;
    std::fs::write(&path, json).map_err(|e| DispatchError::InvalidInput(e.to_string()))?;
    state
        .toasts
        .info(format!("Saved recipe → {}", path.display()));
    Ok(None)
}

pub(super) fn apply_save_recipe_saving_buffers_first(
    state: &mut AppState,
    id: RecipeId,
    path: PathBuf,
    dirty_buffers: Vec<seqforge_core::BufferId>,
) -> Result<Option<ViewerResponse>, DispatchError> {
    for bid in dirty_buffers {
        super::file::save_buffer_id(state, bid, false)?;
    }
    apply_save_recipe(state, id, path, true)
}

/// Append `.json` when the chosen path has no (or a non-json) extension.
fn ensure_json_extension(path: PathBuf) -> PathBuf {
    match path.extension().and_then(|e| e.to_str()) {
        Some(ext) if ext.eq_ignore_ascii_case("json") => path,
        _ => path.with_extension("json"),
    }
}

enum Preflight {
    Clean,
    Scratch(String),
    Dirty {
        names: Vec<String>,
        buffers: Vec<seqforge_core::BufferId>,
    },
}

fn preflight_recipe_export(recipe: &Recipe, workspace: &seqforge_session::Workspace) -> Preflight {
    let mut dirty_names = Vec::new();
    let mut dirty_buffers = Vec::new();
    for bin in &recipe.bins {
        for source in &bin.sources {
            let SourceRef::Buffer(bid) = &source.ref_ else {
                continue;
            };
            let Some(arc) = workspace.buffers.get(*bid) else {
                return Preflight::Scratch(format!("buffer {bid} is no longer open"));
            };
            let Ok(buf) = arc.read() else {
                return Preflight::Scratch("buffer lock poisoned".into());
            };
            if buf.source_path.is_none() {
                return Preflight::Scratch(format!(
                    "Cannot save recipe: \"{}\" is unsaved — save the sequence first",
                    seqforge_session::display_name(&buf)
                ));
            }
            if buf.dirty {
                dirty_names.push(seqforge_session::display_name(&buf));
                dirty_buffers.push(*bid);
            }
        }
    }
    if dirty_buffers.is_empty() {
        Preflight::Clean
    } else {
        Preflight::Dirty {
            names: dirty_names,
            buffers: dirty_buffers,
        }
    }
}

pub(super) fn apply_load_recipe(
    state: &mut AppState,
    id: RecipeId,
    path: PathBuf,
) -> Result<Option<ViewerResponse>, DispatchError> {
    let text =
        std::fs::read_to_string(&path).map_err(|e| DispatchError::InvalidInput(e.to_string()))?;
    let loaded: Recipe =
        serde_json::from_str(&text).map_err(|e| DispatchError::InvalidInput(e.to_string()))?;
    let rematerialized = rematerialize_recipe_buffers(loaded, &state.workspace);
    let recipe = state
        .workspace
        .recipe_mut(id)
        .ok_or_else(|| DispatchError::InvalidInput(format!("no recipe {id}")))?;
    *recipe = rematerialized;
    state.recipe_combo_selection.remove(&id);
    state.recipe_fidelity.remove(&id);
    state.toasts.info(format!(
        "Loaded recipe ← {}",
        path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("recipe.json")
    ));
    Ok(None)
}

/// Rewrite live `Buffer` sources to durable `Path`s for recipe.json export.
/// Aborts if any buffer is unsaved (no `source_path`).
fn normalize_recipe_for_export(
    recipe: &Recipe,
    workspace: &seqforge_session::Workspace,
) -> Result<Recipe, String> {
    use std::hash::{Hash, Hasher};

    let mut out = recipe.clone();
    for bin in &mut out.bins {
        for source in &mut bin.sources {
            match &source.ref_ {
                SourceRef::Buffer(bid) => {
                    let arc = workspace
                        .buffers
                        .get(*bid)
                        .ok_or_else(|| format!("buffer {bid} is no longer open"))?;
                    let buf = arc.read().map_err(|_| "buffer lock poisoned".to_string())?;
                    let Some(path) = buf.source_path.clone() else {
                        return Err(format!(
                            "Cannot save recipe: \"{}\" is unsaved — save the sequence first",
                            seqforge_session::display_name(&buf)
                        ));
                    };
                    if source.pin.is_none() {
                        let mut hasher = std::collections::hash_map::DefaultHasher::new();
                        buf.text.hash(&mut hasher);
                        source.pin = Some(hasher.finish());
                    }
                    source.ref_ = SourceRef::Path(path);
                }
                SourceRef::Path(p) => {
                    if source.pin.is_none() {
                        source.pin = seqforge_session::hash_file_bytes(p);
                    }
                }
            }
        }
    }
    Ok(out)
}

/// Prefer live `Buffer` handles when a path source is already open in the workspace.
fn rematerialize_recipe_buffers(
    mut recipe: Recipe,
    workspace: &seqforge_session::Workspace,
) -> Recipe {
    for bin in &mut recipe.bins {
        for source in &mut bin.sources {
            if let SourceRef::Path(p) = &source.ref_ {
                if let Some(bid) = workspace.buffers.id_for_path(p) {
                    source.ref_ = SourceRef::Buffer(bid);
                }
            }
        }
    }
    recipe
}

// ── File-dialog prompts (workbench flows) ─────────────────────────────────────

pub(super) fn apply_prompt_add_source_file(
    state: &mut AppState,
    recipe: RecipeId,
    bin: usize,
) -> Result<Option<ViewerResponse>, DispatchError> {
    open_dialog(
        state,
        RecipeDialog::AddSource { recipe, bin },
        DialogMode::PickMultiple,
    );
    Ok(None)
}

pub(super) fn apply_prompt_save_recipe(
    state: &mut AppState,
    id: RecipeId,
) -> Result<Option<ViewerResponse>, DispatchError> {
    open_dialog(state, RecipeDialog::SaveRecipe(id), DialogMode::SaveFile);
    Ok(None)
}

/// Workbench "Run → folder…" — pick an output directory, then run.
pub(super) fn apply_prompt_run_to_dir(
    state: &mut AppState,
    id: RecipeId,
) -> Result<Option<ViewerResponse>, DispatchError> {
    open_dialog(state, RecipeDialog::RunToDir(id), DialogMode::PickDirectory);
    Ok(None)
}

pub(super) fn apply_prompt_load_recipe(
    state: &mut AppState,
    id: RecipeId,
) -> Result<Option<ViewerResponse>, DispatchError> {
    open_dialog(state, RecipeDialog::LoadRecipe(id), DialogMode::PickFile);
    Ok(None)
}

/// How a workbench file dialog should behave.
enum DialogMode {
    PickFile,
    PickMultiple,
    SaveFile,
    PickDirectory,
}

fn open_dialog(state: &mut AppState, intent: RecipeDialog, mode: DialogMode) {
    let mut dialog = FileDialog::new();
    match mode {
        DialogMode::SaveFile => {
            dialog = dialog
                .default_file_name("recipe.json")
                .add_file_filter(
                    "Recipe JSON",
                    std::sync::Arc::new(|p: &std::path::Path| {
                        p.extension()
                            .and_then(|e| e.to_str())
                            .is_some_and(|e| e.eq_ignore_ascii_case("json"))
                    }),
                )
                .default_file_filter("Recipe JSON");
            dialog.save_file();
        }
        DialogMode::PickFile => {
            dialog = dialog
                .add_file_filter(
                    "Recipe JSON",
                    std::sync::Arc::new(|p: &std::path::Path| {
                        p.extension()
                            .and_then(|e| e.to_str())
                            .is_some_and(|e| e.eq_ignore_ascii_case("json"))
                    }),
                )
                .default_file_filter("Recipe JSON");
            dialog.pick_file();
        }
        DialogMode::PickMultiple => dialog.pick_multiple(),
        DialogMode::PickDirectory => dialog.pick_directory(),
    }
    snapshot_focus_for_overlay(state);
    state.pending_recipe_dialog = Some(intent);
    if let Some(tag) = state
        .overlays
        .push_unique(Overlay::FileDialog(Box::new(dialog)))
    {
        state
            .events
            .emit(crate::event::AppEvent::OverlayPushed(tag));
    }
}
