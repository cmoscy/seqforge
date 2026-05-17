//! File / document commands: Open, Close, recents, CLI install.

use std::path::PathBuf;

use egui_file_dialog::FileDialog;
use seqforge_core::{BioOps, DispatchError, ViewId, ViewerResponse};

use super::{
    active_selection, emit_selection_diff, snapshot_focus_for_overlay, layout,
};
use crate::app::AppState;
use crate::cli_install;
use crate::event::AppEvent;
use crate::focus::FocusScope;
use crate::overlay::Overlay;
use crate::tabs::Tab;

pub(super) fn apply_prompt_open(
    state: &mut AppState,
) -> Result<Option<ViewerResponse>, DispatchError> {
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

pub(super) fn apply_clear_recent(
    state: &mut AppState,
) -> Result<Option<ViewerResponse>, DispatchError> {
    state.recent_files.clear();
    Ok(None)
}

/// Open `path`, dedup against already-open views, target the right
/// dock leaf, restore per-file state if a `pending_file_state` entry
/// exists, focus the new view.
pub(super) fn apply_open_file<B: BioOps>(
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
        layout::dock_activate_view(state, view_id);
        state.focus.set_scope(FocusScope::View(view_id));
        state.events.emit(AppEvent::TabSwitched { view: view_id });
        return Ok(Some(ViewerResponse::Ok));
    }

    let sel_before = active_selection(state);
    let view_id = state
        .workspace
        .open_path(&path, bio)
        .map_err(DispatchError::BioError)?;

    // If we have persisted state for this path (from session restore
    // OR from a prior close+reopen), apply it before the view paints.
    if let Some(fs) = state.pending_file_state.remove(&path) {
        if let Some(view) = state.workspace.view_mut(view_id) {
            view.selection = fs.selection;
            view.scroll_pos = fs.scroll_pos;
        }
    }

    layout::place_view_tab(state, view_id);
    layout::ensure_welcome_invariant(state);
    layout::dock_activate_view(state, view_id);
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

pub(super) fn apply_close_doc(
    state: &mut AppState,
) -> Result<Option<ViewerResponse>, DispatchError> {
    let view_id = state
        .workspace
        .active_view()
        .ok_or(DispatchError::NoActiveView)?
        .id;
    apply_close_view(state, view_id)
}

/// Close one view: stash its UI state by path (so a subsequent
/// reopen picks up selection/scroll), remove from dock, drop from
/// workspace, drop buffer if last reference, fire events.
pub(super) fn apply_close_view(
    state: &mut AppState,
    view_id: ViewId,
) -> Result<Option<ViewerResponse>, DispatchError> {
    let sel_before = active_selection(state);

    // Capture per-file state under the buffer's path so a later
    // reopen restores selection/scroll. This makes close+reopen
    // feel editor-grade without explicit user action.
    let buffer_id = state
        .workspace
        .view(view_id)
        .ok_or(DispatchError::ViewNotFound(view_id))?
        .buffer_id;
    if let Some(view) = state.workspace.view(view_id) {
        if let Some(arc) = state.workspace.buffers.get(view.buffer_id) {
            if let Ok(buf) = arc.read() {
                if let Some(path) = buf.source_path.clone() {
                    state.pending_file_state.insert(
                        path,
                        crate::persistence::FileState {
                            selection: view.selection,
                            scroll_pos: view.scroll_pos,
                        },
                    );
                }
            }
        }
    }

    let last_ref = state
        .workspace
        .views
        .values()
        .filter(|v| v.buffer_id == buffer_id)
        .count()
        == 1;

    if let Some((si, ni, ti)) = state.dock_state.find_tab(&Tab::View(view_id)) {
        let _ = state.dock_state.remove_tab((si, ni, ti));
    }
    state.workspace.close_view(view_id)?;
    layout::ensure_welcome_invariant(state);

    state.events.emit(AppEvent::TabClosed { view: view_id });
    if last_ref {
        state.events.emit(AppEvent::DocClosed);
    }
    emit_selection_diff(state, sel_before);
    Ok(Some(ViewerResponse::Ok))
}

pub(super) fn apply_dismiss_cli_status(
    state: &mut AppState,
) -> Result<Option<ViewerResponse>, DispatchError> {
    if let Some(tag) = state.overlays.pop_kind(Overlay::TAG_CLI_STATUS) {
        state.events.emit(AppEvent::OverlayPopped(tag));
    }
    super::restore_focus_after_overlay(state);
    Ok(None)
}

pub(super) fn apply_install_cli(
    state: &mut AppState,
) -> Result<Option<ViewerResponse>, DispatchError> {
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
