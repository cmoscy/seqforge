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
//! - **`layout.rs`** — split / focus / tab cycling / dock-layout
//!   invariants (Welcome, place-view-tab, activate-tab).
//!
//! Splitting by domain keeps `apply` short and each file under ~250
//! lines as edit, multi-cursor, plugin variants land in Tier 3+.

use std::path::PathBuf;
use std::sync::mpsc;

use seqforge_core::{
    dispatch, BioOps, DispatchError, Selection, ViewId, ViewerRequest, ViewerResponse,
};

use crate::app::AppState;
use crate::event::AppEvent;
use crate::focus::FocusScope;

mod file;
mod layout;
mod nav;

/// Direction for `AppCommand::SplitPane`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitDirection {
    Horizontal,
    Vertical,
}

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

    // ── Tabs ─────────────────────────────────────────────────────────
    SwitchTab { view: ViewId },
    CloseTab { view: ViewId },
    NextTab,
    PrevTab,

    // ── Overlays ─────────────────────────────────────────────────────
    OpenFind,
    OpenGoTo,
    OpenEnzymes,
    DismissOverlay,
    SubmitFind { pattern: String, mismatches: u8 },
    SubmitGoTo { position: usize },
    /// Replace the active enzyme set with the query's result (`EnzymeOp::Set`).
    SubmitEnzymes { query: String },
    /// Union the query's enzymes into the active set (`EnzymeOp::Add`).
    AddEnzymes { query: String },
    /// Remove a single enzyme from the active set by name (`EnzymeOp::Remove`).
    RemoveEnzyme { name: String },
    /// Select a 0-based half-open range in the active view and scroll it into
    /// view. Used by the enzyme overlay to jump to a cut site; generic enough
    /// to reuse for search results / features later.
    RevealRange { start: usize, end: usize },
    DismissCliStatus,

    // ── Focus / layout ───────────────────────────────────────────────
    FocusPane(FocusScope),
    FocusPaneByIndex(usize),
    SplitPane { direction: SplitDirection },
    ResetLayout,

    // ── Selection ────────────────────────────────────────────────────
    SetSelection(Option<Selection>),
    SelectFeature(Option<usize>),

    // ── Tools ────────────────────────────────────────────────────────
    InstallCli,

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

    // ── Pass-through ─────────────────────────────────────────────────
    Viewer(ViewerRequest),
}

/// Predicate: is this command currently runnable?
pub fn is_enabled(cmd: &AppCommand, state: &AppState) -> bool {
    use AppCommand::*;
    match cmd {
        OpenFind | OpenGoTo | OpenEnzymes | SubmitFind { .. } | SubmitGoTo { .. }
        | SubmitEnzymes { .. } | AddEnzymes { .. } | RemoveEnzyme { .. } | CloseDoc
        | SplitPane { .. } => state.workspace.active_view().is_some(),
        NextTab | PrevTab => count_view_tabs(state) >= 2,
        SwitchTab { .. } | CloseTab { .. } => true,
        Viewer(_) => true,
        SetSelection(_) | SelectFeature(_) => true,
        RevealRange { .. } => state.workspace.active_view().is_some(),
        PromptOpenFile | OpenFile(_) | ClearRecent | DismissOverlay | DismissCliStatus
        | FocusPane(_) | FocusPaneByIndex(_) | ResetLayout | InstallCli
        | ReloadConfig | OpenSettingsFile | OpenKeybindingsFile | OpenThemeFile
        | OpenConfigDir => true,
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

pub(super) fn active_selection(state: &AppState) -> Option<Selection> {
    state.workspace.active_view().and_then(|v| v.selection)
}

pub(super) fn emit_selection_diff(state: &AppState, before: Option<Selection>) {
    let after = active_selection(state);
    if after != before {
        state.events.emit(AppEvent::SelectionChanged { selection: after });
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
    if let Some(target) = req.target_view() {
        return state
            .workspace
            .with_buffer(target, |view, buf, ann| dispatch(view, buf, ann, bio, req))
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

pub fn apply<B: BioOps>(
    cmd: AppCommand,
    state: &mut AppState,
    bio: &B,
) -> Result<Option<ViewerResponse>, DispatchError> {
    use AppCommand::*;
    match cmd {
        // ── File / document ─────────────────────────────────────────
        PromptOpenFile => file::apply_prompt_open(state),
        OpenFile(path) => file::apply_open_file(state, bio, path),
        ClearRecent => file::apply_clear_recent(state),
        CloseDoc => file::apply_close_doc(state),

        // ── Tabs ────────────────────────────────────────────────────
        SwitchTab { view } => layout::apply_switch_tab(state, view),
        CloseTab { view } => file::apply_close_view(state, view),
        NextTab => layout::apply_cycle_tab(state, 1),
        PrevTab => layout::apply_cycle_tab(state, -1),

        // ── Overlays ────────────────────────────────────────────────
        OpenFind => nav::apply_open_find(state),
        OpenGoTo => nav::apply_open_goto(state),
        OpenEnzymes => nav::apply_open_enzymes(state),
        DismissOverlay => nav::apply_dismiss_overlay(state),
        SubmitFind { pattern, mismatches } => {
            nav::apply_submit_find(state, bio, pattern, mismatches)
        }
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
        RevealRange { start, end } => nav::apply_reveal_range(state, start, end),
        DismissCliStatus => file::apply_dismiss_cli_status(state),

        // ── Focus / layout ──────────────────────────────────────────
        FocusPane(scope) => layout::apply_focus_pane(state, scope),
        FocusPaneByIndex(n) => layout::apply_focus_pane_by_index(state, n),
        SplitPane { direction } => layout::apply_split_pane(state, direction),
        ResetLayout => layout::apply_reset_layout(state),

        // ── Selection ───────────────────────────────────────────────
        SetSelection(new_sel) => nav::apply_set_selection(state, new_sel),
        SelectFeature(new_feat) => nav::apply_select_feature(state, new_feat),

        // ── Tools ───────────────────────────────────────────────────
        InstallCli => file::apply_install_cli(state),

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
                state.toasts.info("Terminal shell change applies after restart");
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
            open_config_file(
                state,
                crate::config::paths::theme_path(&name),
                template,
            )
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
            other => {
                let sel_before = active_selection(state);
                let resp = dispatch_active(state, bio, other)?;
                if let ViewerResponse::SearchResults { count, .. } = &resp {
                    state.events.emit(AppEvent::SearchCompleted { hits: *count });
                }
                emit_selection_diff(state, sel_before);
                Ok(Some(resp))
            }
        },
    }
}
