//! Layout / tab / focus / split commands. Owns the dock-tree
//! invariants (Welcome ↔ View, place-view-tab targeting) and the
//! egui_dock wiggle-room around `NodeIndex` instability.

use egui_dock::{Node, Split, SurfaceIndex};
use seqforge_core::{DispatchError, ViewId, ViewKind, ViewerResponse};

use super::{SplitDirection, view_tab_order};
use crate::app::AppState;
use crate::event::AppEvent;
use crate::focus::FocusScope;
use crate::tabs::Tab;

// ── Public command applies ──────────────────────────────────────────────────

pub(super) fn apply_switch_tab(
    state: &mut AppState,
    view: ViewId,
) -> Result<Option<ViewerResponse>, DispatchError> {
    if state.workspace.view(view).is_none() {
        return Err(DispatchError::ViewNotFound(view));
    }
    state.workspace.focus_view(view);
    dock_activate_view(state, view);
    state.focus.set_scope(FocusScope::View(view));
    state.events.emit(AppEvent::TabSwitched { view });
    Ok(None)
}

/// Cycle the focused view by `delta` (wrapping) through dock
/// traversal order over view tabs.
pub(super) fn apply_cycle_tab(
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

pub(super) fn apply_focus_pane(
    state: &mut AppState,
    scope: FocusScope,
) -> Result<Option<ViewerResponse>, DispatchError> {
    if let FocusScope::View(vid) = scope {
        state.workspace.focus_view(vid);
    }
    if state.focus.scope != scope {
        state.focus.set_scope(scope);
        state.events.emit(AppEvent::FocusChanged(scope));
    }
    Ok(None)
}

pub(super) fn apply_focus_pane_by_index(
    state: &mut AppState,
    n: usize,
) -> Result<Option<ViewerResponse>, DispatchError> {
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

/// Split the dock leaf hosting the active view; clone the active
/// view's buffer into a new `View` in the new leaf (Zed convention).
pub(super) fn apply_split_pane(
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
        .ok_or(DispatchError::ViewNotFound(active_vid))?;

    let new_vid = state.workspace.add_view(buffer_id, ViewKind::TextView);
    let split = match direction {
        SplitDirection::Horizontal => Split::Right,
        SplitDirection::Vertical => Split::Below,
    };
    let _ = state
        .dock_state
        .split((surface, node), split, 0.5, Node::leaf(Tab::View(new_vid)));

    state.workspace.focus_view(new_vid);
    let scope = FocusScope::View(new_vid);
    state.focus.set_scope(scope);
    state.events.emit(AppEvent::FocusChanged(scope));
    Ok(None)
}

pub(super) fn apply_reset_layout(
    state: &mut AppState,
) -> Result<Option<ViewerResponse>, DispatchError> {
    crate::app::rebuild_default_dock(&mut state.dock_state, &state.config);
    Ok(None)
}

/// Toggle the Inspector pane: remove it if docked, otherwise dock it on the
/// right of a viewer-bearing leaf (matching the default layout) and focus it.
/// The escape hatch for sessions whose persisted layout predates the pane.
pub(super) fn apply_toggle_inspector(
    state: &mut AppState,
) -> Result<Option<ViewerResponse>, DispatchError> {
    // Already docked → hide it.
    if let Some(loc) = state.dock_state.find_tab(&Tab::Inspector) {
        let _ = state.dock_state.remove_tab(loc);
        if state.focus.scope == FocusScope::Inspector {
            let scope = state
                .workspace
                .active_view
                .map_or(FocusScope::Terminal, FocusScope::View);
            state.focus.set_scope(scope);
            state.events.emit(AppEvent::FocusChanged(scope));
        }
        return Ok(None);
    }

    // Otherwise dock it and focus it.
    dock_inspector_if_absent(state);
    let scope = FocusScope::Inspector;
    state.focus.set_scope(scope);
    state.events.emit(AppEvent::FocusChanged(scope));
    Ok(None)
}

/// Dock the Inspector on the right of a viewer-bearing leaf if it isn't already
/// present; a no-op when it is. Shared by `ToggleInspector` and the ⌘E enzyme
/// re-target (decision 15). Does **not** touch focus — callers decide that.
pub(super) fn dock_inspector_if_absent(state: &mut AppState) {
    if state.dock_state.find_tab(&Tab::Inspector).is_some() {
        return;
    }
    // `split`'s fraction is the retained (viewer) share, so pass the complement
    // of the pane's width.
    let target = {
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
    let frac = 1.0 - state.config.settings.layout.inspector_fraction;
    match target {
        Some((si, ni)) => {
            let _ =
                state
                    .dock_state
                    .split((si, ni), Split::Right, frac, Node::leaf(Tab::Inspector));
        }
        None => state.dock_state.push_to_focused_leaf(Tab::Inspector),
    }
}

// ── Dock-tree helpers (also used by file.rs) ────────────────────────────────

/// Ensure exactly one `Tab::Welcome` exists iff zero `Tab::View(_)`
/// exist. Removals re-locate the tab on each iteration because
/// `egui_dock::Tree::remove_tab` restructures the tree when a leaf
/// empties — pre-collecting indices and removing in batch would
/// panic on the second remove (`is_leaf` assert).
pub(super) fn ensure_welcome_invariant(state: &mut AppState) {
    let mut view_count = 0usize;
    let mut welcome_count = 0usize;
    for surface in state.dock_state.iter_surfaces() {
        for node in surface.iter_nodes() {
            if let Some(tabs) = node.tabs() {
                for tab in tabs {
                    match tab {
                        Tab::View(_) => view_count += 1,
                        Tab::Welcome => welcome_count += 1,
                        _ => {}
                    }
                }
            }
        }
    }

    if view_count > 0 {
        for _ in 0..welcome_count {
            if let Some(loc) = state.dock_state.find_tab(&Tab::Welcome) {
                let _ = state.dock_state.remove_tab(loc);
            } else {
                break;
            }
        }
    } else if welcome_count == 0 {
        state.dock_state.push_to_focused_leaf(Tab::Welcome);
    }
}

/// Push a new `Tab::View(view_id)` into the dock. Targeting rules
/// (in order):
///   1. Same leaf as the currently active view (chain into the user's
///      current pane).
///   2. Any leaf already holding a `Tab::View(_)` or `Tab::Welcome`
///      (never push into Browser / Terminal).
///   3. Focused leaf as last resort.
pub(super) fn place_view_tab(state: &mut AppState, view_id: ViewId) {
    // (1) Active view's leaf.
    if let Some(active_vid) = state.workspace.active_view {
        if active_vid != view_id {
            if let Some((si, ni, _)) = state.dock_state.find_tab(&Tab::View(active_vid)) {
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
        state.dock_state.push_to_focused_leaf(Tab::View(view_id));
    }
}

/// Activate `view_id`'s tab in the dock.
pub(super) fn dock_activate_view(state: &mut AppState, view_id: ViewId) {
    if let Some((si, ni, ti)) = state.dock_state.find_tab(&Tab::View(view_id)) {
        state.dock_state.set_active_tab((si, ni, ti));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toggle_inspector_docks_then_undocks() {
        // Fresh state's stub dock has a Welcome leaf but no Inspector.
        let mut state = AppState::default();
        assert!(state.dock_state.find_tab(&Tab::Inspector).is_none());

        apply_toggle_inspector(&mut state).unwrap();
        assert!(
            state.dock_state.find_tab(&Tab::Inspector).is_some(),
            "toggle on must dock the Inspector"
        );
        assert_eq!(state.focus.scope, FocusScope::Inspector);

        apply_toggle_inspector(&mut state).unwrap();
        assert!(
            state.dock_state.find_tab(&Tab::Inspector).is_none(),
            "toggle off must remove the Inspector"
        );
    }
}
