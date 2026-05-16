//! Typed application commands and the single mutation site (`apply`).
//!
//! See [`docs/focus-refactor.md`](../../../docs/focus-refactor.md) §2.2.
//!
//! Every user-, menu-, hotkey-, bar-, and socket-initiated action is an
//! [`AppCommand`]. The frame loop in [`crate::app`] drains
//! `pending_commands` and routes each through [`apply`] — the *only*
//! function that mutates [`AppState`] in response to a command. Nothing
//! else in the crate may construct a `ViewerRequest` or directly touch
//! the fields that `apply` writes.
//!
//! Why a closed enum: plugin extensibility (an `AppCommand::Custom`
//! variant + handler registry) is deferred to future plugin work (§7
//! of the refactor doc). Keeping the enum closed buys exhaustive-match
//! safety in `apply()` and `is_enabled()`.

use std::path::PathBuf;
use std::sync::mpsc;

use egui_file_dialog::FileDialog;
use seqforge_core::{
    dispatch, BioOps, DispatchError, Selection, ViewId, ViewerRequest, ViewerResponse,
};

use crate::app::AppState;
use crate::cli_install;
use crate::event::AppEvent;
use crate::focus::FocusScope;
use crate::overlay::{FindBar, GoToBar, Overlay};
use crate::workspace::PaneId;

/// A queued command plus the optional one-shot channel that returns
/// the dispatch result. `None` for menu/hotkey/bar-originated commands;
/// `Some(tx)` for socket-originated commands (the CLI client awaits
/// the response over JSON-RPC).
pub type PendingCommand = (
    AppCommand,
    Option<mpsc::SyncSender<Result<ViewerResponse, DispatchError>>>,
);

/// Every user-, agent-, or code-initiated action. Closed enum.
///
/// `Viewer(ViewerRequest)` wraps the existing `seqforge-core` request
/// type so the JSON-RPC wire format and `dispatch()` path are
/// unchanged. GUI-only commands live alongside as explicit variants.
#[derive(Debug, Clone)]
pub enum AppCommand {
    // ── File / document ──────────────────────────────────────────────
    /// Open the native file-picker dialog.
    PromptOpenFile,
    /// Open a specific file by path (recent files, drag-and-drop,
    /// dialog completion all funnel through this).
    OpenFile(PathBuf),
    /// Clear the recent-files list.
    ClearRecent,
    /// Close the active view in the active pane. Cmd+W. When the
    /// closed view was the last reference to its buffer, the buffer is
    /// also dropped and a `DocClosed` event fires.
    CloseDoc,

    // ── Tabs (multi-view within a pane) ──────────────────────────────
    /// Switch a pane's active view to the named one. No-op if already
    /// active. Click on a tab strip entry, agent targeting, drag-reorder
    /// commit all route through here.
    SwitchTab { pane: PaneId, view: ViewId },
    /// Close a specific view in a specific pane. Generalises `CloseDoc`
    /// for the case where the user middle-clicks / X-clicks a non-active
    /// tab or an agent targets one by id.
    CloseTab { pane: PaneId, view: ViewId },
    /// Cycle to the next tab in the active pane. Cmd+Shift+].
    NextTab,
    /// Cycle to the previous tab in the active pane. Cmd+Shift+[.
    PrevTab,

    // ── Overlays ─────────────────────────────────────────────────────
    /// Open the inline Find bar.
    OpenFind,
    /// Open the inline GoTo bar.
    OpenGoTo,
    /// Pop the topmost overlay from [`AppState::overlays`].
    DismissOverlay,
    /// Bar submission: run a search.
    SubmitFind { pattern: String, mismatches: u8 },
    /// Bar submission: jump to a 1-based position.
    SubmitGoTo { position: usize },
    /// Acknowledge the CLI-install result window.
    DismissCliStatus,

    // ── Focus / layout ───────────────────────────────────────────────
    /// Explicit focus move (Stage 4 keymap and programmatic focus
    /// transfers route through this).
    FocusPane(FocusScope),
    /// Reset the dock layout to defaults.
    ResetLayout,

    // ── Selection (user-driven, click/drag) ──────────────────────────
    /// Set the cursor / range selection. `None` clears it. Issued by the
    /// viewer widget for every click / drag / shift-extend so the
    /// resulting mutation goes through the single `apply` site and
    /// `AppEvent::SelectionChanged` fires from one place.
    SetSelection(Option<Selection>),
    /// Set (or clear with `None`) the feature-bar highlight. Independent
    /// of `SetSelection`; clicks that select an annotation push both.
    SelectFeature(Option<usize>),

    // ── Tools ────────────────────────────────────────────────────────
    /// Symlink the bundled CLI into PATH.
    InstallCli,

    // ── Pass-through ─────────────────────────────────────────────────
    /// Wrap a raw `ViewerRequest` — used by the socket consumer and by
    /// any future caller that wants to drive `dispatch()` directly.
    Viewer(ViewerRequest),
}

/// Predicate: is this command currently runnable?
///
/// Used by:
/// - menu rendering to grey unavailable items,
/// - the keymap dispatcher (Stage 4) to gate `consume_key`,
/// - future agent reject paths to return a clear error.
pub fn is_enabled(cmd: &AppCommand, state: &AppState) -> bool {
    use AppCommand::*;
    match cmd {
        OpenFind | OpenGoTo | SubmitFind { .. } | SubmitGoTo { .. } | CloseDoc => {
            state.workspace.active_view().is_some()
        }
        // Tab cycling requires ≥2 tabs in the active pane.
        NextTab | PrevTab => state
            .workspace
            .active_pane()
            .map(|p| p.views.len() >= 2)
            .unwrap_or(false),
        // By-id variants: enablement decided by the resolver inside
        // `apply` (returns ViewNotFound if the target's gone).
        SwitchTab { .. } | CloseTab { .. } => true,
        // Pass-through: the underlying dispatcher enforces preconditions.
        Viewer(_) => true,
        // Selection commands only meaningful with a doc open, but harmless
        // as no-ops otherwise — keep them enabled so the viewer doesn't
        // need to ask before enqueuing.
        SetSelection(_) | SelectFeature(_) => true,
        // Universally available.
        PromptOpenFile | OpenFile(_) | ClearRecent | DismissOverlay | DismissCliStatus
        | FocusPane(_) | ResetLayout | InstallCli => true,
    }
}

/// Read the active view's selection. Used by `emit_selection_diff` and
/// by command arms that need a before-snapshot.
fn active_selection(state: &AppState) -> Option<Selection> {
    state.workspace.active_view().and_then(|v| v.selection)
}

/// Snapshot helper: emits `SelectionChanged` iff the active view's
/// selection differs from `before`. Pulled out so every dispatching
/// variant has the same diffing contract.
fn emit_selection_diff(state: &AppState, before: Option<Selection>) {
    let after = active_selection(state);
    if after != before {
        state.events.emit(AppEvent::SelectionChanged { selection: after });
    }
}

/// Shared apply path for both menu-driven `OpenFile` and socket-driven
/// `Viewer(ViewerRequest::Open { path })` — they should be observably
/// indistinguishable from event subscribers' perspective.
///
/// If a view for this path already exists somewhere in the workspace,
/// switch to it instead of opening a duplicate. This matches the
/// SnapGene / VSCode "switch to existing tab on re-open" convention.
fn apply_open_file<B: BioOps>(
    state: &mut AppState,
    bio: &B,
    path: PathBuf,
) -> Result<Option<ViewerResponse>, DispatchError> {
    state.recent_files.retain(|p| p != &path);
    state.recent_files.insert(0, path.clone());
    state.recent_files.truncate(crate::app::MAX_RECENT);

    // Already open? Switch to its tab.
    if let Some((pane_id, view_id)) = find_open_view_for(state, &path) {
        let pane = state
            .workspace
            .panes
            .get_mut(&pane_id)
            .expect("located");
        if pane.switch_to(view_id) {
            state.events.emit(AppEvent::TabSwitched { pane: pane_id, view: view_id });
            state.seq_view.reset();
        }
        return Ok(Some(ViewerResponse::Ok));
    }

    state.seq_view.reset();
    let sel_before = active_selection(state);

    // Workspace::open_path loads the buffer (via BioOps) and attaches a
    // new View in the active pane. Errors bubble up as BioError.
    let view_id = state
        .workspace
        .open_path(&path, bio)
        .map_err(DispatchError::BioError)?;

    // Emit DocOpened with the new view's buffer summary.
    if let Some((name, len)) = state.workspace.view(view_id).and_then(|v| {
        state
            .workspace
            .buffers
            .get(v.buffer_id)
            .and_then(|arc| arc.read().ok().map(|b| (b.name.clone(), b.len())))
    }) {
        state.events.emit(AppEvent::DocOpened { name, len });
    }

    emit_selection_diff(state, sel_before);
    Ok(Some(ViewerResponse::Ok))
}

/// Search every pane for an existing view whose buffer's source path
/// matches `path`. Used to dedupe open-of-already-open.
fn find_open_view_for(state: &AppState, path: &std::path::Path) -> Option<(PaneId, ViewId)> {
    for (&pane_id, pane) in &state.workspace.panes {
        for view in &pane.views {
            let arc = state.workspace.buffers.get(view.buffer_id)?;
            let buf = arc.read().ok()?;
            if buf.source_path.as_deref() == Some(path) {
                return Some((pane_id, view.id));
            }
        }
    }
    None
}

/// Shared apply path for `CloseDoc` (menu / hotkey, active tab) and
/// `Viewer(ViewerRequest::Close)` (socket).
fn apply_close_doc(
    state: &mut AppState,
) -> Result<Option<ViewerResponse>, DispatchError> {
    // Resolve the (pane, view) the close will affect, before mutation.
    let (pane_id, view_id) = {
        let pane = state.workspace.active_pane().ok_or(DispatchError::NoActiveView)?;
        let view = pane.active_view().ok_or(DispatchError::NoActiveView)?;
        (pane.id, view.id)
    };
    apply_close_view(state, pane_id, view_id)
}

/// Shared close-by-id path. Used by `CloseTab { pane, view }` and by
/// `apply_close_doc` (which resolves the active tab first). Emits
/// `TabClosed` always; emits `DocClosed` when the closed view held the
/// last reference to its buffer.
fn apply_close_view(
    state: &mut AppState,
    pane_id: PaneId,
    view_id: ViewId,
) -> Result<Option<ViewerResponse>, DispatchError> {
    let sel_before = active_selection(state);

    // Determine in advance whether this is the last view of the buffer
    // (we need this before close_view drops it).
    let buffer_id = state
        .workspace
        .view(view_id)
        .ok_or(DispatchError::ViewNotFound(view_id))?
        .buffer_id;
    let last_ref = state.workspace.panes.values().flat_map(|p| p.views.iter())
        .filter(|v| v.buffer_id == buffer_id)
        .count()
        == 1;

    state.workspace.close_view(view_id)?;

    state.events.emit(AppEvent::TabClosed { pane: pane_id, view: view_id });
    if last_ref {
        state.events.emit(AppEvent::DocClosed);
    }
    emit_selection_diff(state, sel_before);
    state.seq_view.reset();
    Ok(Some(ViewerResponse::Ok))
}

/// Cycle the active pane's active tab by `delta`. Wraps around in both
/// directions. No-op if there are fewer than two views.
fn apply_cycle_tab(
    state: &mut AppState,
    delta: isize,
) -> Result<Option<ViewerResponse>, DispatchError> {
    let pane_id = state
        .workspace
        .active_pane
        .ok_or(DispatchError::NoActiveView)?;
    let pane = state
        .workspace
        .panes
        .get_mut(&pane_id)
        .expect("active pane id always exists");
    let n = pane.views.len();
    if n < 2 {
        return Ok(None);
    }
    let new_idx = ((pane.active as isize + delta).rem_euclid(n as isize)) as usize;
    pane.active = new_idx;
    let view_id = pane.views[new_idx].id;
    state.events.emit(AppEvent::TabSwitched { pane: pane_id, view: view_id });
    state.seq_view.reset();
    Ok(None)
}

/// Dispatch a view-scoped `ViewerRequest` against the active view +
/// its buffer. Flattens the `with_active_buffer` -> dispatch nesting so
/// callers get a single `Result<ViewerResponse, DispatchError>`.
fn dispatch_active<B: BioOps>(
    state: &mut AppState,
    bio: &B,
    req: ViewerRequest,
) -> Result<ViewerResponse, DispatchError> {
    state
        .workspace
        .with_active_buffer(|view, buf, ann| dispatch(view, buf, ann, bio, req))
        .and_then(|inner| inner)
}

/// The single mutation site. Every command's effect on `AppState` is
/// here; nowhere else in the app may construct a `ViewerRequest` or
/// directly mutate the same fields.
///
/// Returns the `ViewerResponse` for commands that drive
/// `seqforge_core::dispatch` (so the socket caller can be notified);
/// `Ok(None)` for purely GUI-side commands.
pub fn apply<B: BioOps>(
    cmd: AppCommand,
    state: &mut AppState,
    bio: &B,
) -> Result<Option<ViewerResponse>, DispatchError> {
    use AppCommand::*;
    match cmd {
        PromptOpenFile => {
            let mut dialog = FileDialog::new();
            dialog.pick_file();
            if let Some(tag) = state
                .overlays
                .push_unique(Overlay::FileDialog(Box::new(dialog)))
            {
                state.events.emit(AppEvent::OverlayPushed(tag));
            }
            Ok(None)
        }

        OpenFile(path) => apply_open_file(state, bio, path),

        ClearRecent => {
            state.recent_files.clear();
            Ok(None)
        }

        CloseDoc => apply_close_doc(state),

        SwitchTab { pane, view } => {
            let pane_ref = state
                .workspace
                .panes
                .get_mut(&pane)
                .ok_or(DispatchError::ViewNotFound(view))?;
            if !pane_ref.switch_to(view) {
                return Err(DispatchError::ViewNotFound(view));
            }
            state.events.emit(AppEvent::TabSwitched { pane, view });
            // Switching to a different doc invalidates the viewer's
            // per-buffer caches; reset proactively. (The viewer would
            // also rebuild on its own via the version-keyed check, but
            // resetting drops feature stacks for the previous doc
            // immediately.)
            state.seq_view.reset();
            Ok(None)
        }

        CloseTab { pane, view } => apply_close_view(state, pane, view),

        NextTab => apply_cycle_tab(state, 1),

        PrevTab => apply_cycle_tab(state, -1),

        OpenFind => {
            if let Some(tag) = state
                .overlays
                .push_unique(Overlay::FindBar(FindBar::default()))
            {
                state.events.emit(AppEvent::OverlayPushed(tag));
            }
            Ok(None)
        }

        OpenGoTo => {
            if let Some(tag) = state
                .overlays
                .push_unique(Overlay::GoToBar(GoToBar::default()))
            {
                state.events.emit(AppEvent::OverlayPushed(tag));
            }
            Ok(None)
        }

        DismissOverlay => {
            if let Some(tag) = state.overlays.pop() {
                state.events.emit(AppEvent::OverlayPopped(tag));
            }
            Ok(None)
        }

        SubmitFind { pattern, mismatches } => {
            if let Some(tag) = state.overlays.pop_kind(Overlay::TAG_FIND_BAR) {
                state.events.emit(AppEvent::OverlayPopped(tag));
            }
            let sel_before = active_selection(state);
            let resp = dispatch_active(state, bio, ViewerRequest::Find { pattern, mismatches })?;
            if let ViewerResponse::SearchResults { count, .. } = &resp {
                state.events.emit(AppEvent::SearchCompleted { hits: *count });
            }
            emit_selection_diff(state, sel_before);
            Ok(Some(resp))
        }

        SubmitGoTo { position } => {
            if let Some(tag) = state.overlays.pop_kind(Overlay::TAG_GOTO_BAR) {
                state.events.emit(AppEvent::OverlayPopped(tag));
            }
            let sel_before = active_selection(state);
            let resp = dispatch_active(state, bio, ViewerRequest::GoTo { position })?;
            emit_selection_diff(state, sel_before);
            Ok(Some(resp))
        }

        DismissCliStatus => {
            if let Some(tag) = state.overlays.pop_kind(Overlay::TAG_CLI_STATUS) {
                state.events.emit(AppEvent::OverlayPopped(tag));
            }
            Ok(None)
        }

        FocusPane(scope) => {
            if state.focus.scope != scope {
                state.focus.set_scope(scope);
                state.events.emit(AppEvent::FocusChanged(scope));
            }
            Ok(None)
        }

        ResetLayout => {
            state.dock_state = AppState::default().dock_state;
            Ok(None)
        }

        SetSelection(new_sel) => {
            let before = active_selection(state);
            if let Some(view) = state.workspace.active_view_mut() {
                view.selection = new_sel;
            }
            emit_selection_diff(state, before);
            Ok(None)
        }

        SelectFeature(new_feat) => {
            if let Some(view) = state.workspace.active_view_mut() {
                view.selected_feature = new_feat;
            }
            Ok(None)
        }

        InstallCli => {
            let msg = match cli_install::install_cli_to_path() {
                Ok(r) => format!(
                    "✓ seqforge installed to {}{}",
                    r.target.display(),
                    if r.was_updated { " (updated)" } else { "" }
                ),
                Err(e) => format!("✗ Install failed: {e}"),
            };
            // Replace any prior CliStatus (a previous install attempt
            // may still be showing) so the user sees the latest result.
            state.overlays.pop_kind(Overlay::TAG_CLI_STATUS);
            if let Some(tag) = state.overlays.push_unique(Overlay::CliStatus(msg)) {
                state.events.emit(AppEvent::OverlayPushed(tag));
            }
            Ok(None)
        }

        Viewer(req) => {
            // Pass-through path: socket-originated commands. Open/Close
            // route through the same shared helpers as menu/hotkey so
            // event emission is identical regardless of origin.
            match req {
                ViewerRequest::Open { path } => apply_open_file(state, bio, path),
                ViewerRequest::Close => apply_close_doc(state),
                other => {
                    let sel_before = active_selection(state);
                    let resp = dispatch_active(state, bio, other)?;
                    if let ViewerResponse::SearchResults { count, .. } = &resp {
                        state.events.emit(AppEvent::SearchCompleted { hits: *count });
                    }
                    emit_selection_diff(state, sel_before);
                    Ok(Some(resp))
                }
            }
        }
    }
}

