//! Navigation, search, selection commands.

use seqforge_core::{BioOps, DispatchError, Selection, ViewerRequest, ViewerResponse};

use super::{
    active_selection, dispatch_active, emit_selection_diff,
    restore_focus_after_overlay, snapshot_focus_for_overlay,
};
use crate::app::AppState;
use crate::event::AppEvent;
use crate::focus::FocusScope;
use crate::overlay::{EnzymeBar, FindBar, GoToBar, Overlay};

pub(super) fn apply_open_find(
    state: &mut AppState,
) -> Result<Option<ViewerResponse>, DispatchError> {
    snapshot_focus_for_overlay(state);
    // Pull focus to the active viewer so the bar appears in the pane
    // that will receive the search.
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

pub(super) fn apply_open_goto(
    state: &mut AppState,
) -> Result<Option<ViewerResponse>, DispatchError> {
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

pub(super) fn apply_open_enzymes(
    state: &mut AppState,
) -> Result<Option<ViewerResponse>, DispatchError> {
    snapshot_focus_for_overlay(state);
    if let Some(vid) = state.workspace.active_view {
        state.focus.set_scope(FocusScope::View(vid));
    }
    if let Some(tag) = state
        .overlays
        .push_unique(Overlay::EnzymeBar(EnzymeBar::default()))
    {
        state.events.emit(AppEvent::OverlayPushed(tag));
    }
    Ok(None)
}

pub(super) fn apply_dismiss_overlay(
    state: &mut AppState,
) -> Result<Option<ViewerResponse>, DispatchError> {
    if let Some(tag) = state.overlays.pop() {
        state.events.emit(AppEvent::OverlayPopped(tag));
    }
    restore_focus_after_overlay(state);
    Ok(None)
}

pub(super) fn apply_submit_find<B: BioOps>(
    state: &mut AppState,
    bio: &B,
    pattern: String,
    mismatches: u8,
) -> Result<Option<ViewerResponse>, DispatchError> {
    if let Some(tag) = state.overlays.pop_kind(Overlay::TAG_FIND_BAR) {
        state.events.emit(AppEvent::OverlayPopped(tag));
    }
    let sel_before = active_selection(state);
    let resp = dispatch_active(
        state,
        bio,
        ViewerRequest::Find { pattern, mismatches, view: None },
    )?;
    if let ViewerResponse::SearchResults { count, .. } = &resp {
        state.events.emit(AppEvent::SearchCompleted { hits: *count });
    }
    emit_selection_diff(state, sel_before);
    restore_focus_after_overlay(state);
    Ok(Some(resp))
}

pub(super) fn apply_submit_enzymes<B: BioOps>(
    state: &mut AppState,
    bio: &B,
    query: String,
) -> Result<Option<ViewerResponse>, DispatchError> {
    if let Some(tag) = state.overlays.pop_kind(Overlay::TAG_ENZYME_BAR) {
        state.events.emit(AppEvent::OverlayPopped(tag));
    }
    let resp = dispatch_active(
        state,
        bio,
        ViewerRequest::Enzymes { query, view: None },
    )?;
    restore_focus_after_overlay(state);
    Ok(Some(resp))
}

pub(super) fn apply_submit_goto<B: BioOps>(
    state: &mut AppState,
    bio: &B,
    position: usize,
) -> Result<Option<ViewerResponse>, DispatchError> {
    if let Some(tag) = state.overlays.pop_kind(Overlay::TAG_GOTO_BAR) {
        state.events.emit(AppEvent::OverlayPopped(tag));
    }
    let sel_before = active_selection(state);
    let resp = dispatch_active(
        state,
        bio,
        ViewerRequest::GoTo { position, view: None },
    )?;
    emit_selection_diff(state, sel_before);
    restore_focus_after_overlay(state);
    Ok(Some(resp))
}

pub(super) fn apply_set_selection(
    state: &mut AppState,
    new_sel: Option<Selection>,
) -> Result<Option<ViewerResponse>, DispatchError> {
    let before = active_selection(state);
    if let Some(view) = state.workspace.active_view_mut() {
        view.selection = new_sel;
    }
    emit_selection_diff(state, before);
    Ok(None)
}

pub(super) fn apply_select_feature(
    state: &mut AppState,
    new_feat: Option<usize>,
) -> Result<Option<ViewerResponse>, DispatchError> {
    if let Some(view) = state.workspace.active_view_mut() {
        view.selected_feature = new_feat;
    }
    Ok(None)
}
