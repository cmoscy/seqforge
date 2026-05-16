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

use std::path::PathBuf;
use std::sync::mpsc;

use egui_dock::{Node, Split, SurfaceIndex};
use egui_file_dialog::FileDialog;
use seqforge_core::{
    dispatch, BioOps, DispatchError, Selection, ViewId, ViewKind, ViewerRequest, ViewerResponse,
};

use crate::app::AppState;
use crate::cli_install;
use crate::event::AppEvent;
use crate::focus::FocusScope;
use crate::overlay::{FindBar, GoToBar, Overlay};
use crate::tabs::Tab;

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
    /// Make `view` the focused view (the dock activates its tab, the
    /// workspace updates `active_view`, focus moves to `View(view)`).
    SwitchTab { view: ViewId },
    /// Close a specific view: remove it from the workspace, remove its
    /// dock tab, drop the buffer if it was the last reference.
    CloseTab { view: ViewId },
    /// Cycle to the next view tab in the dock's traversal order. ⌘⇧].
    NextTab,
    /// Cycle to the previous view tab. ⌘⇧[.
    PrevTab,

    // ── Overlays ─────────────────────────────────────────────────────
    OpenFind,
    OpenGoTo,
    DismissOverlay,
    SubmitFind { pattern: String, mismatches: u8 },
    SubmitGoTo { position: usize },
    DismissCliStatus,

    // ── Focus / layout ───────────────────────────────────────────────
    FocusPane(FocusScope),
    /// Focus the Nth viewer tab (1-based, dock traversal order).
    /// `Cmd+1`..`Cmd+9`. No-op if out of range.
    FocusPaneByIndex(usize),
    /// Split the dock leaf hosting the active view; clone the active
    /// view's buffer into a new view in the new leaf. Side-by-side
    /// comparison in one keypress. ⌘\ (horizontal).
    SplitPane { direction: SplitDirection },
    ResetLayout,

    // ── Selection ────────────────────────────────────────────────────
    SetSelection(Option<Selection>),
    SelectFeature(Option<usize>),

    // ── Tools ────────────────────────────────────────────────────────
    InstallCli,

    // ── Pass-through ─────────────────────────────────────────────────
    Viewer(ViewerRequest),
}

/// Predicate: is this command currently runnable?
pub fn is_enabled(cmd: &AppCommand, state: &AppState) -> bool {
    use AppCommand::*;
    match cmd {
        OpenFind | OpenGoTo | SubmitFind { .. } | SubmitGoTo { .. } | CloseDoc
        | SplitPane { .. } => state.workspace.active_view().is_some(),
        // Cycling requires ≥2 view tabs in the dock.
        NextTab | PrevTab => count_view_tabs(state) >= 2,
        SwitchTab { .. } | CloseTab { .. } => true,
        Viewer(_) => true,
        SetSelection(_) | SelectFeature(_) => true,
        PromptOpenFile | OpenFile(_) | ClearRecent | DismissOverlay | DismissCliStatus
        | FocusPane(_) | FocusPaneByIndex(_) | ResetLayout | InstallCli => true,
    }
}

fn count_view_tabs(state: &AppState) -> usize {
    let mut n = 0;
    for surface in state.dock_state.iter_surfaces() {
        for node in surface.iter_nodes() {
            if let Some(tabs) = node.tabs() {
                for t in tabs {
                    if matches!(t, Tab::View(_)) {
                        n += 1;
                    }
                }
            }
        }
    }
    n
}

/// Walk every view tab in the dock in traversal order. Used by
/// `FocusPaneByIndex`, `NextTab`/`PrevTab`, and any code that needs a
/// stable ordering over view tabs.
fn view_tab_order(state: &AppState) -> Vec<ViewId> {
    let mut out = Vec::new();
    for surface in state.dock_state.iter_surfaces() {
        for node in surface.iter_nodes() {
            if let Some(tabs) = node.tabs() {
                for t in tabs {
                    if let Tab::View(vid) = t {
                        out.push(*vid);
                    }
                }
            }
        }
    }
    out
}

fn active_selection(state: &AppState) -> Option<Selection> {
    state.workspace.active_view().and_then(|v| v.selection)
}

fn emit_selection_diff(state: &AppState, before: Option<Selection>) {
    let after = active_selection(state);
    if after != before {
        state.events.emit(AppEvent::SelectionChanged { selection: after });
    }
}

/// Snapshot focus on empty → non-empty overlay transitions. Call
/// *before* pushing any overlay. Idempotent within a single non-empty
/// run: while one overlay is open, pushing a second doesn't overwrite
/// the original snapshot.
fn snapshot_focus_for_overlay(state: &mut AppState) {
    if state.overlays.is_empty() {
        state.focus_before_overlay = Some(state.focus.scope);
    }
}

/// Restore focus on non-empty → empty overlay transitions. Call
/// *after* popping. Only restores when the stack is now empty, so
/// stacked overlays don't snap focus around as each one pops.
fn restore_focus_after_overlay(state: &mut AppState) {
    if !state.overlays.is_empty() {
        return;
    }
    if let Some(scope) = state.focus_before_overlay.take() {
        if state.focus.scope != scope {
            // Mirror viewer-scope restores into workspace.active_view
            // so subsequent commands address the right view.
            if let FocusScope::View(vid) = scope {
                state.workspace.focus_view(vid);
            }
            state.focus.set_scope(scope);
            state.events.emit(AppEvent::FocusChanged(scope));
        }
    }
}

/// Ensure exactly one `Tab::Welcome` exists iff zero `Tab::View(_)`
/// exist. Called after every open/close so the central dock area
/// never becomes an empty void.
fn ensure_welcome_invariant(state: &mut AppState) {
    let mut view_count = 0usize;
    let mut welcome_locations: Vec<(SurfaceIndex, egui_dock::NodeIndex, egui_dock::TabIndex)> =
        Vec::new();
    for (s_idx, surface) in state.dock_state.iter_surfaces().enumerate() {
        let si = SurfaceIndex(s_idx);
        for (n_idx, node) in surface.iter_nodes().enumerate() {
            let ni = egui_dock::NodeIndex(n_idx);
            if let Some(tabs) = node.tabs() {
                for (t_idx, tab) in tabs.iter().enumerate() {
                    match tab {
                        Tab::View(_) => view_count += 1,
                        Tab::Welcome => welcome_locations.push((si, ni, egui_dock::TabIndex(t_idx))),
                        _ => {}
                    }
                }
            }
        }
    }

    if view_count > 0 {
        // Remove every Welcome tab. Process in reverse so indices stay valid.
        for (si, ni, ti) in welcome_locations.into_iter().rev() {
            let _ = state.dock_state.remove_tab((si, ni, ti));
        }
    } else if welcome_locations.is_empty() {
        // No views, no Welcome → drop one in.
        state.dock_state.push_to_focused_leaf(Tab::Welcome);
    }
}

/// Push a new `Tab::View(view_id)` into the dock. Targeting rules
/// (in order):
///   1. Same leaf as the currently active view (so opens chain into
///      the user's current pane).
///   2. Any leaf already holding a `Tab::View(_)` or `Tab::Welcome`
///      (never push into Browser / Terminal leaves).
///   3. As a last resort, push to the focused leaf — only reached if
///      the user has somehow eliminated every viewer leaf.
fn place_view_tab(state: &mut AppState, view_id: ViewId) {
    // (1) Active view's leaf.
    if let Some(active_vid) = state.workspace.active_view {
        if active_vid != view_id {
            if let Some((si, ni, _)) =
                state.dock_state.find_tab(&Tab::View(active_vid))
            {
                state.dock_state[si][ni].append_tab(Tab::View(view_id));
                return;
            }
        }
    }

    // (2) Any viewer-bearing leaf.
    let viewer_leaf = {
        let mut found = None;
        for (s_idx, surface) in state.dock_state.iter_surfaces().enumerate() {
            for (n_idx, node) in surface.iter_nodes().enumerate() {
                if let Some(tabs) = node.tabs() {
                    if tabs
                        .iter()
                        .any(|t| matches!(t, Tab::View(_) | Tab::Welcome))
                    {
                        found = Some((SurfaceIndex(s_idx), egui_dock::NodeIndex(n_idx)));
                        break;
                    }
                }
            }
            if found.is_some() {
                break;
            }
        }
        found
    };

    if let Some((si, ni)) = viewer_leaf {
        state.dock_state[si][ni].append_tab(Tab::View(view_id));
    } else {
        // (3) Last resort — should not normally occur because the
        // Welcome invariant keeps at least one viewer-bearing leaf
        // alive whenever the workspace has zero views.
        state.dock_state.push_to_focused_leaf(Tab::View(view_id));
    }
}

/// Activate `view_id`'s tab in the dock: locate the tab and call
/// `set_active_tab`. No-op if the tab isn't found.
fn dock_activate_view(state: &mut AppState, view_id: ViewId) {
    if let Some((si, ni, ti)) = state.dock_state.find_tab(&Tab::View(view_id)) {
        state.dock_state.set_active_tab((si, ni, ti));
    }
}

/// Shared apply path for menu-driven `OpenFile` and socket-driven
/// `Viewer(ViewerRequest::Open { path })`.
fn apply_open_file<B: BioOps>(
    state: &mut AppState,
    bio: &B,
    path: PathBuf,
) -> Result<Option<ViewerResponse>, DispatchError> {
    state.recent_files.retain(|p| p != &path);
    state.recent_files.insert(0, path.clone());
    state.recent_files.truncate(crate::app::MAX_RECENT);

    // Already open? Switch to its tab.
    if let Some(view_id) = state.workspace.find_view_for_path(&path) {
        state.workspace.focus_view(view_id);
        dock_activate_view(state, view_id);
        state.focus.set_scope(FocusScope::View(view_id));
        state.events.emit(AppEvent::TabSwitched { view: view_id });
        return Ok(Some(ViewerResponse::Ok));
    }

    let sel_before = active_selection(state);
    let view_id = state
        .workspace
        .open_path(&path, bio)
        .map_err(DispatchError::BioError)?;

    place_view_tab(state, view_id);
    ensure_welcome_invariant(state);
    dock_activate_view(state, view_id);
    state.focus.set_scope(FocusScope::View(view_id));

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

fn apply_close_doc(
    state: &mut AppState,
) -> Result<Option<ViewerResponse>, DispatchError> {
    let view_id = state
        .workspace
        .active_view()
        .ok_or(DispatchError::NoActiveView)?
        .id;
    apply_close_view(state, view_id)
}

fn apply_close_view(
    state: &mut AppState,
    view_id: ViewId,
) -> Result<Option<ViewerResponse>, DispatchError> {
    let sel_before = active_selection(state);

    let buffer_id = state
        .workspace
        .view(view_id)
        .ok_or(DispatchError::ViewNotFound(view_id))?
        .buffer_id;
    let last_ref = state
        .workspace
        .views
        .values()
        .filter(|v| v.buffer_id == buffer_id)
        .count()
        == 1;

    // Remove the dock tab first (if present), then the workspace view.
    if let Some((si, ni, ti)) = state.dock_state.find_tab(&Tab::View(view_id)) {
        let _ = state.dock_state.remove_tab((si, ni, ti));
    }
    state.workspace.close_view(view_id)?;
    ensure_welcome_invariant(state);

    state.events.emit(AppEvent::TabClosed { view: view_id });
    if last_ref {
        state.events.emit(AppEvent::DocClosed);
    }
    emit_selection_diff(state, sel_before);
    Ok(Some(ViewerResponse::Ok))
}

/// Split the dock leaf hosting the active view. The new leaf gets a
/// fresh `View` onto the same buffer — side-by-side comparison in one
/// keypress (Zed convention).
fn apply_split_pane(
    state: &mut AppState,
    direction: SplitDirection,
) -> Result<Option<ViewerResponse>, DispatchError> {
    let active_vid = state
        .workspace
        .active_view()
        .ok_or(DispatchError::NoActiveView)?
        .id;
    let buffer_id = state.workspace.view(active_vid).expect("located").buffer_id;

    let (surface, node, _) = state
        .dock_state
        .find_tab(&Tab::View(active_vid))
        .ok_or(DispatchError::NoActiveView)?;

    // Allocate a sibling view onto the same buffer, then split the
    // leaf with the new view as its sole tab.
    let new_vid = state.workspace.add_view(buffer_id, ViewKind::TextView);
    let split = match direction {
        SplitDirection::Horizontal => Split::Right,
        SplitDirection::Vertical => Split::Below,
    };
    let _ = state.dock_state.split(
        (surface, node),
        split,
        0.5,
        Node::leaf(Tab::View(new_vid)),
    );

    state.workspace.focus_view(new_vid);
    let scope = FocusScope::View(new_vid);
    state.focus.set_scope(scope);
    state.events.emit(AppEvent::FocusChanged(scope));
    Ok(None)
}

/// Cycle the focused view by `delta` (wrapping) through the dock's
/// traversal order over view tabs.
fn apply_cycle_tab(
    state: &mut AppState,
    delta: isize,
) -> Result<Option<ViewerResponse>, DispatchError> {
    let order = view_tab_order(state);
    if order.len() < 2 {
        return Ok(None);
    }
    let current_vid = state.workspace.active_view.unwrap_or(order[0]);
    let cur_idx = order.iter().position(|v| *v == current_vid).unwrap_or(0);
    let n = order.len();
    let new_idx = ((cur_idx as isize + delta).rem_euclid(n as isize)) as usize;
    let new_vid = order[new_idx];
    state.workspace.focus_view(new_vid);
    dock_activate_view(state, new_vid);
    state.focus.set_scope(FocusScope::View(new_vid));
    state.events.emit(AppEvent::TabSwitched { view: new_vid });
    Ok(None)
}

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
            snapshot_focus_for_overlay(state);
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

        SwitchTab { view } => {
            if state.workspace.view(view).is_none() {
                return Err(DispatchError::ViewNotFound(view));
            }
            state.workspace.focus_view(view);
            dock_activate_view(state, view);
            state.focus.set_scope(FocusScope::View(view));
            state.events.emit(AppEvent::TabSwitched { view });
            Ok(None)
        }

        CloseTab { view } => apply_close_view(state, view),

        NextTab => apply_cycle_tab(state, 1),

        PrevTab => apply_cycle_tab(state, -1),

        OpenFind => {
            snapshot_focus_for_overlay(state);
            // Opening Find pulls focus to the active viewer so the bar
            // appears in the pane that will receive the search. If
            // focus was already on a viewer pane, this is a no-op.
            if let Some(vid) = state.workspace.active_view {
                state.focus.set_scope(FocusScope::View(vid));
            }
            if let Some(tag) = state
                .overlays
                .push_unique(Overlay::FindBar(FindBar::default()))
            {
                state.events.emit(AppEvent::OverlayPushed(tag));
            }
            Ok(None)
        }

        OpenGoTo => {
            snapshot_focus_for_overlay(state);
            if let Some(vid) = state.workspace.active_view {
                state.focus.set_scope(FocusScope::View(vid));
            }
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
            restore_focus_after_overlay(state);
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
            restore_focus_after_overlay(state);
            Ok(Some(resp))
        }

        SubmitGoTo { position } => {
            if let Some(tag) = state.overlays.pop_kind(Overlay::TAG_GOTO_BAR) {
                state.events.emit(AppEvent::OverlayPopped(tag));
            }
            let sel_before = active_selection(state);
            let resp = dispatch_active(state, bio, ViewerRequest::GoTo { position })?;
            emit_selection_diff(state, sel_before);
            restore_focus_after_overlay(state);
            Ok(Some(resp))
        }

        DismissCliStatus => {
            if let Some(tag) = state.overlays.pop_kind(Overlay::TAG_CLI_STATUS) {
                state.events.emit(AppEvent::OverlayPopped(tag));
            }
            restore_focus_after_overlay(state);
            Ok(None)
        }

        FocusPane(scope) => {
            if let FocusScope::View(vid) = scope {
                state.workspace.focus_view(vid);
            }
            if state.focus.scope != scope {
                state.focus.set_scope(scope);
                state.events.emit(AppEvent::FocusChanged(scope));
            }
            Ok(None)
        }

        FocusPaneByIndex(n) => {
            let order = view_tab_order(state);
            if let Some(vid) = order.get(n.saturating_sub(1)).copied() {
                state.workspace.focus_view(vid);
                dock_activate_view(state, vid);
                let scope = FocusScope::View(vid);
                if state.focus.scope != scope {
                    state.focus.set_scope(scope);
                    state.events.emit(AppEvent::FocusChanged(scope));
                }
            }
            Ok(None)
        }

        SplitPane { direction } => apply_split_pane(state, direction),

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
            state.overlays.pop_kind(Overlay::TAG_CLI_STATUS);
            snapshot_focus_for_overlay(state);
            if let Some(tag) = state.overlays.push_unique(Overlay::CliStatus(msg)) {
                state.events.emit(AppEvent::OverlayPushed(tag));
            }
            Ok(None)
        }

        Viewer(req) => match req {
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
        },
    }
}
