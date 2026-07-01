//! Navigation, search, selection commands.

use seqforge_core::{
    BioOps, DispatchError, EnzymeOp, FeatureId, Selection, Strand, ViewerRequest, ViewerResponse,
};

use super::{
    active_selection, dispatch_active, emit_selection_diff, restore_focus_after_overlay,
    snapshot_focus_for_overlay,
};
use crate::app::AppState;
use crate::event::AppEvent;
use crate::focus::FocusScope;
use crate::overlay::{
    EnzymeBar, FeatureForm, FindBar, GoToBar, Overlay, RenameFeatureForm, TranslationView,
};

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
        ViewerRequest::Find {
            pattern,
            mismatches,
            view: None,
        },
    )?;
    if let ViewerResponse::SearchResults { count, .. } = &resp {
        state
            .events
            .emit(AppEvent::SearchCompleted { hits: *count });
    }
    emit_selection_diff(state, sel_before);
    restore_focus_after_overlay(state);
    Ok(Some(resp))
}

/// Set / Add / Remove against the active enzyme set. Unlike Find / GoTo, the
/// enzyme overlay is **persistent**: these ops mutate the set and re-render
/// without closing it, so the user can refine the set in place. Only
/// `DismissOverlay` (Esc / ✕) closes the bar.
pub(super) fn apply_enzyme_op<B: BioOps>(
    state: &mut AppState,
    bio: &B,
    query: String,
    op: EnzymeOp,
) -> Result<Option<ViewerResponse>, DispatchError> {
    let resp = dispatch_active(
        state,
        bio,
        ViewerRequest::Enzymes {
            query,
            op,
            view: None,
        },
    )?;
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
        ViewerRequest::GoTo {
            position,
            view: None,
        },
    )?;
    emit_selection_diff(state, sel_before);
    restore_focus_after_overlay(state);
    Ok(Some(resp))
}

/// Select `start..end` (0-based, half-open) in the active view and scroll it
/// into view. The enzyme overlay stays open (it's persistent), so the user can
/// keep clicking sites; the viewer behind it scrolls and highlights.
pub(super) fn apply_reveal_range(
    state: &mut AppState,
    start: usize,
    end: usize,
) -> Result<Option<ViewerResponse>, DispatchError> {
    let before = active_selection(state);
    if let Some(view) = state.workspace.active_view_mut() {
        view.selection = Some(Selection::range(start, end));
        view.scroll_to = Some(start);
        view.selected_feature = None;
    }
    emit_selection_diff(state, before);
    Ok(None)
}

pub(super) fn apply_set_selection(
    state: &mut AppState,
    new_sel: Option<Selection>,
) -> Result<Option<ViewerResponse>, DispatchError> {
    let before = active_selection(state);
    if let Some(view) = state.workspace.active_view_mut() {
        view.selection = new_sel;
        // Keep the moving end (focus) on screen. Fires only when the focus is
        // outside the last-rendered visible range — a no-op for clicks (always
        // within view), so this just serves off-screen moves like arrow-key nav.
        if let (Some(sel), Some((start, end))) = (new_sel, view.visible_range) {
            if sel.focus < start || sel.focus >= end {
                view.scroll_to = Some(sel.focus);
            }
        }
    }
    emit_selection_diff(state, before);
    Ok(None)
}

pub(super) fn apply_select_feature(
    state: &mut AppState,
    new_feat: Option<FeatureId>,
) -> Result<Option<ViewerResponse>, DispatchError> {
    if let Some(view) = state.workspace.active_view_mut() {
        view.selected_feature = new_feat;
    }
    Ok(None)
}

/// Open a modal overlay (feature editing / translation). Shared plumbing:
/// snapshot focus for restore-on-dismiss, keep the active view focused so the
/// modal's Escape (via the `"Overlay"` context tag) works, push uniquely.
fn open_modal(
    state: &mut AppState,
    overlay: Overlay,
) -> Result<Option<ViewerResponse>, DispatchError> {
    snapshot_focus_for_overlay(state);
    if let Some(vid) = state.workspace.active_view {
        state.focus.set_scope(FocusScope::View(vid));
    }
    if let Some(tag) = state.overlays.push_unique(overlay) {
        state.events.emit(AppEvent::OverlayPushed(tag));
    }
    Ok(None)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn apply_open_feature_form(
    state: &mut AppState,
    id: Option<FeatureId>,
    label: String,
    kind: String,
    strand: String,
    start: usize,
    end: usize,
) -> Result<Option<ViewerResponse>, DispatchError> {
    let form = match id {
        Some(id) => FeatureForm::edit(id, label, kind, strand, start, end),
        None => FeatureForm::create(start, end),
    };
    open_modal(state, Overlay::FeatureForm(form))
}

pub(super) fn apply_open_rename_feature(
    state: &mut AppState,
    id: FeatureId,
    label: String,
) -> Result<Option<ViewerResponse>, DispatchError> {
    open_modal(
        state,
        Overlay::RenameFeature(RenameFeatureForm::new(id, label)),
    )
}

pub(super) fn apply_open_translation(
    state: &mut AppState,
    title: String,
    start: usize,
    end: usize,
    strand: Strand,
    frame: usize,
) -> Result<Option<ViewerResponse>, DispatchError> {
    open_modal(
        state,
        Overlay::Translation(TranslationView {
            title,
            start,
            end,
            strand,
            frame,
            all_frames: false,
        }),
    )
}
