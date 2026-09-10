//! Typed application commands and the single mutation site (`apply`).
//!
//! See [`docs/focus-refactor.md`](../../../docs/focus-refactor.md) §2.2.
//!
//! Every user-, menu-, hotkey-, bar-, and socket-initiated action is an
//! [`AppCommand`]. The frame loop in [`crate::app`] drains
//! `pending_commands` and routes each through [`apply`] — the *only*
//! function that mutates [`AppState`] in response to a command.
//!
//! ## Module layout (Stage 2.5e)
//!
//! - **`mod.rs`** (this file) — the closed enum, the public `apply`
//!   dispatcher, shared helpers (selection diffing, overlay focus
//!   snapshot/restore, dispatch pass-through, dock walking).
//! - **`file.rs`** — Open / Close / recent files / CLI install.
//! - **`nav.rs`** — Find / GoTo / selection / feature highlight.
//! - **`layout.rs`** — focus / tab cycling / center-dock invariants
//!   (Welcome, place-view-tab, activate-tab) + `buffers`/`focus` (CLI parity).
//!
//! Splitting by domain keeps `apply` short and each file under ~250
//! lines as edit, multi-cursor, plugin variants land in Tier 3+.

use std::path::PathBuf;
use std::sync::mpsc;

use seqforge_core::{
    BioOps, DispatchError, FeatureId, Selection, ViewId, ViewSelection, ViewerRequest,
    ViewerResponse, dispatch,
};

use crate::app::AppState;
use crate::event::AppEvent;
use crate::focus::FocusScope;
use seqforge_session::edit as sedit;

pub(crate) mod assembly;
mod edit;
pub(crate) mod file;
mod layout;
mod nav;
mod stage;

/// A queued command plus the optional one-shot channel that returns
/// the dispatch result. `None` for menu/hotkey/bar-originated commands;
/// `Some(tx)` for socket-originated commands.
pub type PendingCommand = (
    AppCommand,
    Option<mpsc::SyncSender<Result<ViewerResponse, DispatchError>>>,
);

/// Every user-, agent-, or code-initiated action.
#[derive(Debug, Clone)]
pub enum AppCommand {
    // ── File / document ──────────────────────────────────────────────
    PromptOpenFile,
    OpenFile(PathBuf),
    ClearRecent,
    /// Close the active view. ⌘W. Routes through `CloseTab` for the
    /// active view id.
    CloseDoc,
    /// Quit the app (`File → Quit`, ⌘Q). Sets `quit_requested`; the update loop
    /// routes it through the same dirty-buffer intercept as an OS window close,
    /// so unsaved work raises the confirm modal instead of exiting silently.
    Quit,
    /// Open the confirm modal for `File → Revert to Saved`.
    OpenRevertConfirm {
        view: Option<ViewId>,
    },
    /// Reload the target view's buffer from disk (`File → Revert`), discarding
    /// in-memory edits + annotations + undo history. GUI-only (needs `bio.load`).
    RevertBuffer {
        view: Option<ViewId>,
    },

    // ── Tabs ─────────────────────────────────────────────────────────
    CloseTab {
        view: ViewId,
    },
    NextTab,
    PrevTab,

    // ── Overlays ─────────────────────────────────────────────────────
    OpenFind,
    OpenGoTo,
    OpenEnzymes,
    DismissOverlay,
    SubmitFind {
        pattern: String,
        mismatches: u8,
    },
    SubmitGoTo {
        position: usize,
    },
    /// Replace the active enzyme set with the query's result (`EnzymeOp::Set`).
    SubmitEnzymes {
        query: String,
    },
    /// Union the query's enzymes into the active set (`EnzymeOp::Add`).
    AddEnzymes {
        query: String,
    },
    /// Remove a single enzyme from the active set by name (`EnzymeOp::Remove`).
    RemoveEnzyme {
        name: String,
    },
    /// Update the active view's methylation context. Verdicts derive at read time.
    SetMethylation {
        dam: bool,
        dcm: bool,
        cpg: bool,
    },
    DismissCliStatus,

    // ── Focus / layout ───────────────────────────────────────────────
    FocusPane(FocusScope),
    FocusPaneByIndex(usize),
    ResetLayout,
    /// Show/hide the Inspector region (right — active-document surface).
    ToggleInspector,
    /// Show/hide the Files region (left — workspace navigation).
    ToggleFiles,
    /// Show/hide the minimap overview (sub-region of the Inspector pane).
    ToggleMinimap,

    // ── Selection ────────────────────────────────────────────────────
    /// Set the active view's one selection (range / cursor / feature / primer /
    /// cut-site object). Replaces the former `SetSelection`/`SelectFeature`/
    /// `SelectPrimer` triple — the mutual exclusion is now structural in
    /// [`ViewSelection`], so producers set the full intent atomically instead of
    /// choreographing a range-set plus an object-set that each clear the others.
    Select(ViewSelection),
    /// Select a primer by id (Inspector row-click): sets `View.selected_primer`,
    /// and — when attached — selects + reveals its binding footprint.
    RevealPrimer {
        id: seqforge_core::PrimerId,
    },
    /// Cmd-click a primer (Inspector row or map arrow) to build / edit a PCR
    /// `PrimerPair` selection (Phase 3.1b). Bounded, ordered — not multi-select.
    PromotePrimerPair {
        id: seqforge_core::PrimerId,
    },
    /// Select a feature by id (Inspector row-click): sets `View.selected_feature`
    /// and selects + reveals its range.
    RevealFeature {
        id: FeatureId,
    },
    /// Select a cut site by key (Inspector Cut-sites row-click): sets the
    /// `CutSite` object selection (single-site granularity) + reveals its
    /// recognition span. The map↔panel counterpart of a map cut-site click.
    RevealCutSite {
        key: seqforge_core::CutSiteKey,
        start: usize,
        end: usize,
    },

    // ── Feature editing (Phase 14) ───────────────────────────────────
    /// Open the unified add/edit feature modal. `id` is `None` for a new
    /// feature (pre-filled from the selection) or `Some` to edit an existing one.
    OpenFeatureForm {
        id: Option<FeatureId>,
        label: String,
        kind: String,
        strand: String,
        start: usize,
        end: usize,
    },
    /// Commit the feature modal → `AddFeature` (`id` = `None`) or `UpdateFeature`
    /// (`id` = `Some`), then dismiss.
    SubmitFeatureForm {
        id: Option<FeatureId>,
        label: String,
        kind: String,
        strand: String,
        start: usize,
        end: usize,
    },
    /// Route a feature into the Inspector's inline editor (decision 15,
    /// tab-exclusive editing): dock/focus the pane, select the feature, enter
    /// edit mode. `arm_delete` opens with the two-step delete pre-armed (Delete
    /// gesture / context-menu Delete). Replaces the canvas edit modal.
    EditFeatureInInspector {
        id: FeatureId,
        arm_delete: bool,
    },
    /// Route a primer into the Inspector's inline editor (Phase 2.1, sibling of
    /// `EditFeatureInInspector`): dock/focus the pane, select the primer, enter
    /// edit mode. `arm_delete` opens with the two-step delete pre-armed (Delete
    /// gesture / canvas Delete-on-selected-primer).
    EditPrimerInInspector {
        id: seqforge_core::PrimerId,
        arm_delete: bool,
    },
    /// Open the Rename modal for a feature (right-click → Rename…).
    OpenRenameFeature {
        id: FeatureId,
        label: String,
    },
    /// Commit the Rename modal → one `RenameFeature`, then dismiss.
    SubmitRenameFeature {
        id: FeatureId,
        label: String,
    },
    /// Set which in-canvas translation lanes the active view shows
    /// (View → Translation). Carries the full new state; the menu toggles a
    /// field and sends the whole struct.
    SetTranslationDisplay(crate::viewer::TranslationDisplay),
    /// Set the active view's primer map-overlay display (Inspector Primers-tab
    /// header toggles: show/hide on map, arrows-vs-bases).
    SetPrimerDisplay(crate::viewer::PrimerDisplay),
    /// Set the active view's feature map visibility (Inspector Features-tab
    /// header toggles: show all on map, show the `source` metadata feature).
    SetFeatureVisibility(crate::viewer::FeatureVisibility),
    /// Toggle inline translation for a single feature (right-click → Show/Hide
    /// translation), anchored to that feature's start + strand.
    ToggleFeatureTranslation(FeatureId),
    /// Open the read-only translation window for a range/strand/frame.
    OpenTranslation {
        title: String,
        start: usize,
        end: usize,
        strand: seqforge_core::Strand,
        frame: usize,
    },

    // ── Tools ────────────────────────────────────────────────────────
    InstallCli,

    // ── Editor save side-effects ─────────────────────────────────────
    /// Write a buffer to disk (IO). Emitted by `edit::apply_save` (path
    /// known) and by the Save-As dialog on pick. Clears `dirty` + toasts.
    SaveDocument {
        view: Option<ViewId>,
        path: PathBuf,
    },
    /// Open the Save-As file dialog for `view` (GUI-only). On pick, enqueues
    /// `SaveDocument`.
    OpenSaveAs {
        view: Option<ViewId>,
    },
    /// Materialize one digest fragment as its own buffer — the **opt-in
    /// single-fragment export** seam (ROADMAP decision 25; the only per-fragment
    /// `new_buffer_annotated` call). `source_view` is the Fragments view; `index`
    /// is the fragment's position in the digest result.
    ExportFragment {
        source_view: ViewId,
        index: usize,
    },

    // ── Assembly workbench (A1) ──────────────────────────────────────
    /// Open a new empty assembly-recipe workbench tab (2 bins).
    NewRecipe,
    /// Close a recipe tab and drop its document.
    CloseRecipe {
        id: seqforge_core::RecipeId,
    },
    /// Author a recipe in place (one command, enum payload — the single mutation
    /// path without a per-field command explosion).
    EditRecipe {
        id: seqforge_core::RecipeId,
        op: assembly::RecipeOp,
    },
    /// Run a recipe → materialize its product(s) as buffers.
    RunRecipe {
        id: seqforge_core::RecipeId,
    },
    /// Serialize a recipe to a `.json` path.
    /// When `force` is false, dirty path-backed sources open a confirm modal.
    SaveRecipe {
        id: seqforge_core::RecipeId,
        path: PathBuf,
        force: bool,
    },
    /// Persist dirty recipe sources to disk, then export the recipe (`force`).
    SaveRecipeSavingBuffersFirst {
        id: seqforge_core::RecipeId,
        path: PathBuf,
        dirty_buffers: Vec<seqforge_core::BufferId>,
    },
    /// Replace this tab's recipe from a `.json` path (interchange / agent stage).
    LoadRecipe {
        id: seqforge_core::RecipeId,
        path: PathBuf,
    },
    /// Open a file dialog to add a file-at-rest source to a bin.
    PromptAddSourceFile {
        recipe: seqforge_core::RecipeId,
        bin: usize,
    },
    /// Open a save dialog for a recipe.
    PromptSaveRecipe {
        id: seqforge_core::RecipeId,
    },
    /// Open a pick dialog to load a recipe.json into this assembly tab.
    PromptLoadRecipe {
        id: seqforge_core::RecipeId,
    },
    /// Open a directory picker, then Run writing every product into it.
    PromptRunToDir {
        id: seqforge_core::RecipeId,
    },
    /// Run a recipe → product buffers **and** GenBank files under `dir`.
    RunRecipeToDir {
        id: seqforge_core::RecipeId,
        dir: PathBuf,
    },
    /// Session-only combo checkbox set for Run subset (not part of recipe IR).
    SetRecipeComboSelection {
        id: seqforge_core::RecipeId,
        selected: std::collections::HashSet<usize>,
    },
    /// Session-only fidelity dataset for combo preview (not part of recipe IR).
    SetRecipeFidelity {
        id: seqforge_core::RecipeId,
        dataset: Option<seqforge_bio::FidelityDataset>,
    },

    // ── Config ───────────────────────────────────────────────────────
    /// Re-read settings / theme / keybindings from disk.
    ReloadConfig,
    /// Seed `settings.toml` if missing and open it in the user's editor.
    OpenSettingsFile,
    /// Seed `keybindings.toml` if missing and open it in the user's editor.
    OpenKeybindingsFile,
    /// Seed `themes/<active>.toml` if missing and open it in the user's editor.
    OpenThemeFile,
    /// Open the config directory in the platform file manager.
    OpenConfigDir,

    // ── In-canvas staging (menu → arm a previewed edit) ──────────────
    /// Arm a staged, destructive edit on the active view's canvas instead of
    /// applying it immediately, so a menu Cut/Delete/Paste previews exactly
    /// like the in-canvas keystroke would (ROADMAP decision 10, revised: all
    /// interactive GUI surfaces stage; only CLI/terminal/agent post directly).
    /// The applier also focuses the view so the stage survives and `Enter`
    /// commits it — at which point it rides the *same* commit path the keyboard
    /// uses (`PendingEdit::to_request` → `ViewerRequest`).
    StageEdit(StagedEdit),

    // ── Pass-through ─────────────────────────────────────────────────
    Viewer(ViewerRequest),
}

/// A destructive edit armed for preview from the menu — the operand-bearing
/// mirror of the in-canvas `PendingEdit` (Cut/Delete need a range, Paste a
/// position). GUI-only: it never crosses the socket/CLI wire (those post the
/// `ViewerRequest` directly and immediately).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StagedEdit {
    Cut { start: usize, end: usize },
    Delete { start: usize, end: usize },
    Paste { pos: usize },
}

/// Predicate: is this command currently runnable?
pub fn is_enabled(cmd: &AppCommand, state: &AppState) -> bool {
    use AppCommand::*;
    match cmd {
        OpenFind
        | OpenGoTo
        | OpenEnzymes
        | SubmitFind { .. }
        | SubmitGoTo { .. }
        | SubmitEnzymes { .. }
        | AddEnzymes { .. }
        | RemoveEnzyme { .. }
        | SetMethylation { .. }
        | CloseDoc => state.workspace.active_view().is_some(),
        NextTab | PrevTab => count_view_tabs(state) >= 2,
        CloseTab { .. } | Quit => true,
        // Revert only makes sense for a file-backed buffer.
        RevertBuffer { .. } | OpenRevertConfirm { .. } => active_has_source_path(state),
        // Editor write-ops carry their own enablement so menus / keymap grey
        // correctly (Phase 12f). Read-scoped variants and the position-explicit
        // ops just need an open view.
        Viewer(req) => match req {
            ViewerRequest::Undo { .. } => active_can_undo(state),
            ViewerRequest::Redo { .. } => active_can_redo(state),
            ViewerRequest::Cut { .. }
            | ViewerRequest::Delete { .. }
            | ViewerRequest::ReverseComplement { .. } => has_range_selection(state),
            ViewerRequest::Copy { .. } => has_copy_operand(state),
            ViewerRequest::Paste { .. } => {
                !state.clipboard.is_empty() && state.workspace.active_view().is_some()
            }
            ViewerRequest::Save { .. } => active_dirty(state),
            ViewerRequest::Open { .. } => true,
            // Workspace-scoped queries — valid regardless of the active view.
            ViewerRequest::Buffers | ViewerRequest::Focus { .. } => true,
            // SaveAs, GoTo/Find/Enzymes, Insert/Replace/feature ops, Close.
            _ => state.workspace.active_view().is_some(),
        },
        // Mirror the immediate-op gating: Cut/Delete need a range, Paste a
        // non-empty clipboard (commit lowers to the same ViewerRequest).
        StageEdit(StagedEdit::Cut { .. }) | StageEdit(StagedEdit::Delete { .. }) => {
            has_range_selection(state)
        }
        StageEdit(StagedEdit::Paste { .. }) => {
            !state.clipboard.is_empty() && state.workspace.active_view().is_some()
        }
        Select(_) => true,
        // New Feature (create form from the menu) needs a range selection;
        // an edit form is opened with a concrete feature so it's always valid.
        OpenFeatureForm { id, .. } => id.is_some() || has_range_selection(state),
        OpenTranslation { .. }
        | SetTranslationDisplay(_)
        | ToggleFeatureTranslation(_)
        | SetPrimerDisplay(_)
        | SetFeatureVisibility(_) => state.workspace.active_view().is_some(),
        SubmitFeatureForm { .. } | OpenRenameFeature { .. } | SubmitRenameFeature { .. } => {
            state.workspace.active_view().is_some()
        }
        RevealPrimer { .. } | RevealFeature { .. } | RevealCutSite { .. } => {
            state.workspace.active_view().is_some()
        }
        PromotePrimerPair { .. } => state.workspace.active_view().is_some(),
        EditFeatureInInspector { .. } | EditPrimerInInspector { .. } => {
            state.workspace.active_view().is_some()
        }
        SaveDocument { .. } | OpenSaveAs { .. } => state.workspace.active_view().is_some(),
        ExportFragment { .. } => true,
        NewRecipe
        | CloseRecipe { .. }
        | EditRecipe { .. }
        | RunRecipe { .. }
        | SaveRecipe { .. }
        | SaveRecipeSavingBuffersFirst { .. }
        | LoadRecipe { .. }
        | PromptAddSourceFile { .. }
        | PromptSaveRecipe { .. }
        | PromptLoadRecipe { .. }
        | PromptRunToDir { .. }
        | RunRecipeToDir { .. }
        | SetRecipeComboSelection { .. }
        | SetRecipeFidelity { .. } => true,
        PromptOpenFile | OpenFile(_) | ClearRecent | DismissOverlay | DismissCliStatus
        | FocusPane(_) | FocusPaneByIndex(_) | ResetLayout | ToggleInspector | ToggleFiles
        | ToggleMinimap | InstallCli | ReloadConfig | OpenSettingsFile | OpenKeybindingsFile
        | OpenThemeFile | OpenConfigDir => true,
    }
}

// ── Shared helpers (used by submodules) ──────────────────────────────────────

pub(super) fn count_view_tabs(state: &AppState) -> usize {
    let mut n = 0;
    for surface in state.dock_state.iter_surfaces() {
        for node in surface.iter_nodes() {
            if let Some(tabs) = node.tabs() {
                for t in tabs {
                    if matches!(t, crate::tabs::Tab::View(_)) {
                        n += 1;
                    }
                }
            }
        }
    }
    n
}

/// Walk every view tab in the dock in traversal order.
pub(super) fn view_tab_order(state: &AppState) -> Vec<ViewId> {
    let mut out = Vec::new();
    for surface in state.dock_state.iter_surfaces() {
        for node in surface.iter_nodes() {
            if let Some(tabs) = node.tabs() {
                for t in tabs {
                    if let crate::tabs::Tab::View(vid) = t {
                        out.push(*vid);
                    }
                }
            }
        }
    }
    out
}

/// The active view's **text range** (the editable range/cursor), if any. Object
/// selections (feature/cut-site) surface their derived range; a selected primer
/// is object-only and yields `None`. Drives Cut/Copy/Delete gating + diff events.
pub(super) fn active_selection(state: &AppState) -> Option<Selection> {
    state
        .workspace
        .active_view()
        .and_then(|v| v.selection.text_range())
}

// ── Editor-op enablement predicates (Phase 12f) ──────────────────────────────--

/// True when the active view holds a non-empty range selection (not a bare
/// cursor) — gates Cut/Copy/Delete/Reverse-Complement.
pub(super) fn has_range_selection(state: &AppState) -> bool {
    active_selection(state).is_some_and(|s| !s.is_cursor())
}

/// True when Copy has an operand: a range selection **or** a selected primer
/// (keyboard ⌘C already copies the oligo; menu Copy must match).
pub(super) fn has_copy_operand(state: &AppState) -> bool {
    has_range_selection(state)
        || state
            .workspace
            .active_view()
            .is_some_and(|v| v.selection.selected_primer().is_some())
}

/// `(can_undo, can_redo)` for the active buffer's history, `(false, false)` if
/// no view or no history yet. Reads through the buffer store with a shared
/// borrow (no write lock needed).
fn active_history_flags(state: &AppState) -> (bool, bool) {
    state
        .workspace
        .active_view()
        .and_then(|v| state.workspace.buffers.history(v.buffer_id))
        .map_or((false, false), |h| (h.can_undo(), h.can_redo()))
}

fn active_can_undo(state: &AppState) -> bool {
    active_history_flags(state).0
}

fn active_can_redo(state: &AppState) -> bool {
    active_history_flags(state).1
}

/// True when the active buffer has unsaved changes — gates Save.
fn active_dirty(state: &AppState) -> bool {
    state
        .workspace
        .active_view()
        .and_then(|v| state.workspace.buffers.get(v.buffer_id))
        .and_then(|arc| arc.read().ok().map(|b| b.dirty))
        .unwrap_or(false)
}

fn active_has_source_path(state: &AppState) -> bool {
    state
        .workspace
        .active_view()
        .and_then(|v| state.workspace.buffers.get(v.buffer_id))
        .and_then(|arc| arc.read().ok().map(|b| b.source_path.is_some()))
        .unwrap_or(false)
}

pub(super) fn emit_selection_diff(state: &AppState, before: Option<Selection>) {
    let after = active_selection(state);
    if after != before {
        state
            .events
            .emit(AppEvent::SelectionChanged { selection: after });
    }
}

/// Snapshot focus on empty → non-empty overlay transitions. Call
/// *before* pushing any overlay. Idempotent within a single non-empty
/// run.
pub(super) fn snapshot_focus_for_overlay(state: &mut AppState) {
    if state.overlays.is_empty() {
        state.focus_before_overlay = Some(state.focus.scope);
    }
}

/// Restore focus on non-empty → empty overlay transitions. Call
/// *after* popping. Only restores when the stack is now empty.
pub(super) fn restore_focus_after_overlay(state: &mut AppState) {
    if !state.overlays.is_empty() {
        return;
    }
    if let Some(scope) = state.focus_before_overlay.take() {
        if state.focus.scope != scope {
            if let FocusScope::View(vid) = scope {
                state.workspace.focus_view(vid);
            }
            state.focus.set_scope(scope);
            state.events.emit(AppEvent::FocusChanged(scope));
        }
    }
}

/// Dispatch a view-scoped `ViewerRequest`. Routing rules:
///   - If `req.target_view()` is `Some(vid)`, the request operates on
///     that view explicitly (Stage 2.5d socket-protocol targeting).
///     `ViewNotFound` if the view has been closed.
///   - Otherwise it operates on `workspace.active_view`. `NoActiveView`
///     if no view is open.
///
/// View-scoped requests that target a non-active view still mutate
/// that view's state (selection, scroll, search results); callers
/// downstream of the response (status bar, agent reply) should treat
/// the response as authoritative for the *target* view, not the
/// current active view.
pub(super) fn dispatch_active<B: BioOps>(
    state: &mut AppState,
    bio: &B,
    req: ViewerRequest,
) -> Result<ViewerResponse, DispatchError> {
    if let Some(vid) = req.target().and_then(|t| t.view) {
        return state
            .workspace
            .with_buffer(vid, |view, buf, ann| dispatch(view, buf, ann, bio, req))
            .and_then(|inner| inner);
    }
    state
        .workspace
        .with_active_buffer(|view, buf, ann| dispatch(view, buf, ann, bio, req))
        .and_then(|inner| inner)
}

/// Seed `path` from `template` if it doesn't exist, then launch it in
/// the user's editor. Errors surface as toasts; the command never
/// fails (returns `Ok(None)` either way).
fn open_config_file(
    state: &mut AppState,
    path: std::path::PathBuf,
    template: &str,
) -> Result<Option<ViewerResponse>, DispatchError> {
    if let Err(e) = crate::config::ensure_file_exists(&path, template) {
        state.toasts.error(format!("seed config: {e}"));
        return Ok(None);
    }
    if let Err(e) = crate::config::open_in_editor(&path) {
        state.toasts.error(format!("open config: {e}"));
    }
    Ok(None)
}

// ── Public dispatcher ────────────────────────────────────────────────────────

/// Reject a `path` target before dispatch.
///
/// `--in` means *headless*: it resolves in the calling process against a
/// workspace that lives for one request, and any view state the verb sets is
/// scoped to that request. That is the rule the CLI already implements —
/// `DocSource::of` routes every path-targeted request to `run_on_file` and
/// never forwards it — so a `path` arriving here came from a hand-written
/// JSON-RPC payload and is a category error: it asks the session to act on a
/// document the session does not have.
///
/// Resolving it *into* the session was worse than refusing it. The previous
/// implementation called `Workspace::open_path`, which dedupes the buffer but
/// calls `add_view` unconditionally, so every such request minted a `View` that
/// no dock tab ever referenced — never rendered, never cached, never closed —
/// and stole `active_view` until the dock-focus mirror reverted it a frame
/// later. Refusing the target removes that path rather than teaching it to
/// place tabs, and keeps the invariant exact: the session only ever sees
/// `Active` or `View`.
///
/// To act on a file in the session, `open` it first, then target the view.
fn reject_path_target(cmd: &mut AppCommand) -> Result<(), DispatchError> {
    let AppCommand::Viewer(req) = cmd else {
        return Ok(());
    };
    let Some(target) = req.target_mut() else {
        return Ok(());
    };
    // `kind()` also rejects an ambiguous target (both `--view` and `--in`) here
    // rather than downstream, where silently preferring one would be a wrong
    // answer.
    match target.kind()? {
        seqforge_core::TargetKind::Active | seqforge_core::TargetKind::View(_) => Ok(()),
        seqforge_core::TargetKind::Path(_) => Err(DispatchError::InvalidInput(
            "a file target (`--in`) runs locally and cannot address this session; \
             use `--view`, or `open` the file first"
                .to_string(),
        )),
    }
}

pub fn apply<B: BioOps>(
    cmd: AppCommand,
    state: &mut AppState,
    bio: &B,
) -> Result<Option<ViewerResponse>, DispatchError> {
    // A `Path` target cannot address this session — refuse it once, before
    // dispatch, so every arm below reads `target.view` and cannot be handed a
    // document the workspace does not own (ROADMAP decision 27).
    let mut cmd = cmd;
    reject_path_target(&mut cmd)?;

    use AppCommand::*;
    match cmd {
        // ── File / document ─────────────────────────────────────────
        PromptOpenFile => file::apply_prompt_open(state),
        OpenFile(path) => file::apply_open_file(state, bio, path),
        ClearRecent => file::apply_clear_recent(state),
        CloseDoc => file::apply_close_doc(state),
        Quit => file::apply_quit(state),
        OpenRevertConfirm { view } => file::apply_open_revert_confirm(state, view),
        RevertBuffer { view } => file::apply_revert(state, bio, view),

        // ── Tabs ────────────────────────────────────────────────────
        CloseTab { view } => file::apply_close_view(state, view),
        NextTab => layout::apply_cycle_tab(state, 1),
        PrevTab => layout::apply_cycle_tab(state, -1),

        // ── Overlays ────────────────────────────────────────────────
        OpenFind => nav::apply_open_find(state),
        OpenGoTo => nav::apply_open_goto(state),
        OpenEnzymes => nav::apply_open_enzymes(state),
        DismissOverlay => nav::apply_dismiss_overlay(state),
        SubmitFind {
            pattern,
            mismatches,
        } => nav::apply_submit_find(state, bio, pattern, mismatches),
        SubmitGoTo { position } => nav::apply_submit_goto(state, bio, position),
        SubmitEnzymes { query } => {
            nav::apply_enzyme_op(state, bio, query, seqforge_core::EnzymeOp::Set)
        }
        AddEnzymes { query } => {
            nav::apply_enzyme_op(state, bio, query, seqforge_core::EnzymeOp::Add)
        }
        RemoveEnzyme { name } => {
            nav::apply_enzyme_op(state, bio, name, seqforge_core::EnzymeOp::Remove)
        }
        SetMethylation { dam, dcm, cpg } => nav::apply_set_methylation(state, bio, dam, dcm, cpg),
        DismissCliStatus => file::apply_dismiss_cli_status(state),

        // ── Focus / layout ──────────────────────────────────────────
        FocusPane(scope) => layout::apply_focus_pane(state, scope),
        FocusPaneByIndex(n) => layout::apply_focus_pane_by_index(state, n),
        ResetLayout => layout::apply_reset_layout(state),
        ToggleInspector => layout::apply_toggle_inspector(state),
        ToggleFiles => layout::apply_toggle_files(state),
        ToggleMinimap => layout::apply_toggle_minimap(state),

        // ── Selection ───────────────────────────────────────────────
        Select(sel) => nav::apply_select(state, sel),
        RevealPrimer { id } => nav::apply_reveal_primer(state, id),
        PromotePrimerPair { id } => nav::apply_promote_primer_pair(state, id),
        RevealFeature { id } => nav::apply_reveal_feature(state, id),
        RevealCutSite { key, start, end } => nav::apply_reveal_cut_site(state, key, start, end),
        EditFeatureInInspector { id, arm_delete } => {
            nav::apply_edit_feature_in_inspector(state, id, arm_delete)
        }
        EditPrimerInInspector { id, arm_delete } => {
            nav::apply_edit_primer_in_inspector(state, id, arm_delete)
        }

        // ── Feature editing (Phase 14) ──────────────────────────────
        OpenFeatureForm {
            id,
            label,
            kind,
            strand,
            start,
            end,
        } => nav::apply_open_feature_form(state, id, label, kind, strand, start, end),
        SubmitFeatureForm {
            id,
            label,
            kind,
            strand,
            start,
            end,
        } => edit::apply_submit_feature_form(state, id, label, kind, strand, start, end),
        OpenRenameFeature { id, label } => nav::apply_open_rename_feature(state, id, label),
        SubmitRenameFeature { id, label } => edit::apply_submit_rename_feature(state, id, label),
        SetTranslationDisplay(display) => {
            if let Some(vid) = state.workspace.active_view {
                state.seq_views.get_or_default(vid).translation = display;
            }
            Ok(None)
        }
        SetPrimerDisplay(display) => {
            if let Some(vid) = state.workspace.active_view {
                state.seq_views.get_or_default(vid).primer_display = display;
            }
            Ok(None)
        }
        SetFeatureVisibility(visibility) => {
            if let Some(vid) = state.workspace.active_view {
                state.seq_views.get_or_default(vid).feature_visibility = visibility;
            }
            Ok(None)
        }
        ToggleFeatureTranslation(id) => {
            if let Some(vid) = state.workspace.active_view {
                let sv = state.seq_views.get_or_default(vid);
                if !sv.translation.features.remove(&id) {
                    sv.translation.features.insert(id);
                }
            }
            Ok(None)
        }
        OpenTranslation {
            title,
            start,
            end,
            strand,
            frame,
        } => nav::apply_open_translation(state, title, start, end, strand, frame),

        // ── In-canvas staging (menu) ────────────────────────────────
        StageEdit(edit) => stage::apply_stage_edit(state, edit),

        // ── Tools ───────────────────────────────────────────────────
        InstallCli => file::apply_install_cli(state),

        // ── Editor save side-effects ────────────────────────────────
        SaveDocument { view, path } => file::apply_save_document(state, view, path),
        ExportFragment { source_view, index } => {
            file::apply_export_fragment(state, source_view, index)
        }

        // ── Assembly workbench ──
        NewRecipe => assembly::apply_new_recipe(state),
        CloseRecipe { id } => assembly::apply_close_recipe(state, id),
        EditRecipe { id, op } => assembly::apply_edit_recipe(state, id, op),
        RunRecipe { id } => assembly::apply_run_recipe(state, id),
        SaveRecipe { id, path, force } => assembly::apply_save_recipe(state, id, path, force),
        SaveRecipeSavingBuffersFirst {
            id,
            path,
            dirty_buffers,
        } => assembly::apply_save_recipe_saving_buffers_first(state, id, path, dirty_buffers),
        LoadRecipe { id, path } => assembly::apply_load_recipe(state, id, path),
        PromptAddSourceFile { recipe, bin } => {
            assembly::apply_prompt_add_source_file(state, recipe, bin)
        }
        PromptSaveRecipe { id } => assembly::apply_prompt_save_recipe(state, id),
        PromptLoadRecipe { id } => assembly::apply_prompt_load_recipe(state, id),
        PromptRunToDir { id } => assembly::apply_prompt_run_to_dir(state, id),
        RunRecipeToDir { id, dir } => assembly::apply_run_recipe_to_dir(state, id, dir),
        SetRecipeComboSelection { id, selected } => {
            assembly::apply_set_combo_selection(state, id, selected)
        }
        SetRecipeFidelity { id, dataset } => assembly::apply_set_fidelity(state, id, dataset),
        OpenSaveAs { view } => file::apply_open_save_as(state, view),

        // ── Config ──────────────────────────────────────────────────
        ReloadConfig => {
            let epoch = state.config.epoch;
            let old_shell = state.config.settings.terminal.shell.clone();
            let (new_cfg, warnings) = crate::config::Config::reload(epoch);
            let new_shell = new_cfg.settings.terminal.shell.clone();
            state.config = new_cfg;
            if warnings.is_empty() {
                state.toasts.success("Reloaded config");
            } else {
                for w in warnings {
                    state.toasts.warning(w);
                }
            }
            if new_shell != old_shell {
                state
                    .toasts
                    .info("Terminal shell change applies after restart");
            }
            Ok(None)
        }
        OpenSettingsFile => open_config_file(
            state,
            crate::config::paths::settings_path(),
            crate::config::defaults::SETTINGS_TEMPLATE,
        ),
        OpenKeybindingsFile => open_config_file(
            state,
            crate::config::paths::keybindings_path(),
            crate::config::defaults::KEYBINDINGS_TEMPLATE,
        ),
        OpenThemeFile => {
            let name = state.config.settings.theme.clone();
            let template = match name.as_str() {
                "default-light" => crate::config::defaults::DEFAULT_LIGHT,
                _ => crate::config::defaults::DEFAULT_DARK,
            };
            open_config_file(state, crate::config::paths::theme_path(&name), template)
        }
        OpenConfigDir => {
            let dir = crate::config::paths::config_dir();
            if let Err(e) = std::fs::create_dir_all(&dir) {
                state.toasts.error(format!("create config dir: {e}"));
                return Ok(None);
            }
            if let Err(e) = crate::config::open_in_editor(&dir) {
                state.toasts.error(format!("open config dir: {e}"));
            }
            Ok(None)
        }

        // ── Pass-through ────────────────────────────────────────────
        Viewer(req) => match req {
            ViewerRequest::Open { path } => file::apply_open_file(state, bio, path),
            ViewerRequest::Close => file::apply_close_doc(state),
            ViewerRequest::Buffers => layout::apply_buffers(state),
            ViewerRequest::Focus { target } => layout::apply_focus_doc(state, &target),

            // ── Editor write-ops → command/edit.rs (Phase 11 write path) ──
            // Intercepted here, never reaching `dispatch_active`/`core::dispatch`
            // (which read-lock); see commands.rs `dispatch` doc + editor.md §4.
            ViewerRequest::Insert { pos, bases, target } => {
                sedit::apply_insert(&mut state.workspace, target.view, pos, bases)
            }
            ViewerRequest::Delete { start, end, target } => {
                sedit::apply_delete(&mut state.workspace, target.view, start, end)
            }
            ViewerRequest::Replace {
                start,
                end,
                bases,
                target,
            } => sedit::apply_replace(&mut state.workspace, target.view, start, end, bases),
            ViewerRequest::ReverseComplement { start, end, target } => {
                sedit::apply_reverse_complement(&mut state.workspace, target.view, start, end)
            }
            // Clipboard ops reach the OS pasteboard, so they take a `Host`
            // alongside the workspace — disjoint borrows of `AppState`.
            ViewerRequest::Cut { start, end, target } => {
                let (ws, mut host) = state.session();
                sedit::apply_cut(ws, &mut host, target.view, start, end)
            }
            ViewerRequest::Copy { start, end, target } => {
                let (ws, mut host) = state.session();
                sedit::apply_copy(ws, &mut host, target.view, start, end)
            }
            ViewerRequest::Paste { pos, target } => {
                let (ws, mut host) = state.session();
                sedit::apply_paste(ws, &mut host, target.view, pos)
            }
            ViewerRequest::AddFeature {
                start,
                end,
                kind,
                label,
                strand,
                target,
            } => sedit::apply_add_feature(
                &mut state.workspace,
                target.view,
                start,
                end,
                kind,
                label,
                strand,
            ),
            ViewerRequest::RemoveFeature { id, target } => {
                sedit::apply_remove_feature(&mut state.workspace, target.view, id)
            }
            ViewerRequest::RenameFeature { id, label, target } => {
                sedit::apply_rename_feature(&mut state.workspace, target.view, id, label)
            }
            ViewerRequest::UpdateFeature {
                id,
                kind,
                label,
                strand,
                start,
                end,
                target,
            } => sedit::apply_update_feature(
                &mut state.workspace,
                target.view,
                id,
                kind,
                label,
                strand,
                start,
                end,
            ),
            ViewerRequest::AddPrimer {
                name,
                sequence,
                start,
                end,
                strand,
                target,
            } => sedit::apply_add_primer(
                &mut state.workspace,
                target.view,
                name,
                sequence,
                start,
                end,
                strand,
            ),
            ViewerRequest::UpdatePrimer {
                id,
                name,
                sequence,
                strand,
                start,
                end,
                detach,
                target,
            } => sedit::apply_update_primer(
                &mut state.workspace,
                target.view,
                id,
                name,
                sequence,
                strand,
                start,
                end,
                detach,
            ),
            ViewerRequest::RescanPrimer { id, target } => {
                sedit::apply_rescan_primer(&mut state.workspace, target.view, id)
            }
            ViewerRequest::AddPrimerSite {
                id,
                enzyme,
                overhang,
                flank,
                target,
            } => sedit::apply_add_primer_site(
                &mut state.workspace,
                target.view,
                id,
                enzyme,
                overhang,
                flank,
            ),
            ViewerRequest::RemovePrimer { id, target } => {
                sedit::apply_remove_primer(&mut state.workspace, target.view, id)
            }
            ViewerRequest::Pcr {
                fwd,
                rev,
                name,
                target,
            } => file::apply_pcr(state, target.view, fwd, rev, name),
            ViewerRequest::Digest { query, target } => {
                file::apply_digest(state, target.view, query)
            }
            ViewerRequest::Save { force, target } => edit::apply_save(state, target.view, force),
            ViewerRequest::SaveAs { path, target } => {
                // `SaveAs` with an explicit path is a direct write; no dialog.
                file::apply_save_document(state, target.view, path)
            }
            ViewerRequest::Assemble {
                inputs,
                method,
                topology,
                enzymes,
                expand,
                emit_recipe,
                dry_run: _,
                fidelity_dataset: _,
                fidelity_matrix: _,
                out,
                format,
                name_template,
                combos,
                origin,
            } => assembly::apply_assemble(
                state,
                inputs,
                method,
                topology,
                enzymes,
                expand,
                emit_recipe,
                out,
                format,
                name_template,
                combos,
                origin,
            ),
            ViewerRequest::Undo { target } => sedit::apply_undo(&mut state.workspace, target.view),
            ViewerRequest::Redo { target } => sedit::apply_redo(&mut state.workspace, target.view),

            // ── Buffer lifecycle / topology ──
            ViewerRequest::New { circular, name } => file::apply_new(state, circular, name),
            ViewerRequest::SetOrigin {
                index,
                feature,
                target,
            } => sedit::apply_set_origin(&mut state.workspace, target.view, index, feature),
            ViewerRequest::Linearize { at, target } => {
                sedit::apply_linearize(&mut state.workspace, target.view, at)
            }
            ViewerRequest::Circularize { origin, target } => {
                sedit::apply_circularize(&mut state.workspace, target.view, origin)
            }

            // ── Read-scoped (GoTo/Find/Enzymes) → core::dispatch ──
            other => {
                let sel_before = active_selection(state);
                let resp = dispatch_active(state, bio, other)?;
                if let ViewerResponse::SearchResults { count, .. } = &resp {
                    state
                        .events
                        .emit(AppEvent::SearchCompleted { hits: *count });
                }
                emit_selection_diff(state, sel_before);
                Ok(Some(resp))
            }
        },
    }
}

#[cfg(test)]
mod path_target_tests {
    //! `--in` is headless by definition, so a `path` target must never reach
    //! the session. The regression this guards: resolving it here called
    //! `Workspace::open_path`, which dedupes the buffer but adds a `View`
    //! unconditionally — minting a view no dock tab referenced, so it was never
    //! rendered, never cached, and never closed.

    use super::*;
    use seqforge_core::{Primer, Target, ViewerRequest};

    struct TestBio;
    impl BioOps for TestBio {
        fn load(&self, path: &std::path::Path) -> Result<seqforge_core::Document, String> {
            seqforge_bio::load(path).map_err(|e| e.to_string())
        }
        fn find_matches(
            &self,
            _: &[u8],
            _: &[u8],
            _: u8,
            _: bool,
        ) -> Vec<seqforge_core::SearchHit> {
            vec![]
        }
        fn find_cut_sites(&self, _: &[u8], _: &[&str], _: bool) -> Vec<seqforge_core::CutSite> {
            vec![]
        }
        fn resolve_enzyme_names(&self, _: &[u8], _: &str, _: bool) -> Vec<String> {
            vec![]
        }
        fn primer_infos(&self, _: &[u8], _: &[&Primer], _: bool) -> Vec<seqforge_core::PrimerInfo> {
            vec![]
        }
        fn methyl_states_for_sites(
            &self,
            sites: &[seqforge_core::CutSite],
            _: &[u8],
            _: &seqforge_core::MethylContext,
        ) -> Vec<seqforge_core::MethylState> {
            vec![seqforge_core::MethylState::Cuttable; sites.len()]
        }
    }

    fn fixture() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../seqforge-bio/tests/fixtures/pUC19.gbk")
    }

    #[test]
    fn a_path_target_is_refused_and_mints_no_view() {
        let mut state = AppState::default();
        let before = state.workspace.views.len();

        let err = apply(
            AppCommand::Viewer(ViewerRequest::ListFeatures {
                target: Target::path(fixture()),
            }),
            &mut state,
            &TestBio,
        )
        .expect_err("a file target cannot address the session");

        assert!(
            err.to_string().contains("runs locally"),
            "should say why, got: {err}"
        );
        assert_eq!(
            state.workspace.views.len(),
            before,
            "refusing the target must not open a view"
        );
    }

    /// The same file addressed by view still works — the route the rule points
    /// callers to ("`open` the file first, then target the view").
    #[test]
    fn the_same_document_addressed_by_view_succeeds() {
        let mut state = AppState::default();
        let vid = state
            .workspace
            .open_path(&fixture(), &TestBio)
            .expect("fixture opens");
        state.workspace.focus_view(vid);

        let resp = apply(
            AppCommand::Viewer(ViewerRequest::ListFeatures {
                target: Target::view(vid),
            }),
            &mut state,
            &TestBio,
        )
        .expect("a view target resolves");
        assert!(matches!(resp, Some(ViewerResponse::Features { .. })));
    }

    /// An ambiguous target is still rejected, and still before dispatch.
    #[test]
    fn both_view_and_path_is_still_ambiguous() {
        let mut state = AppState::default();
        let mut target = Target::path(fixture());
        target.view = Some(seqforge_core::ViewId(1));

        let err = apply(
            AppCommand::Viewer(ViewerRequest::ListFeatures { target }),
            &mut state,
            &TestBio,
        )
        .expect_err("both set is ambiguous");
        assert_eq!(state.workspace.views.len(), 0, "{err}");
    }
}
