//! Layout / tab / focus / document commands. Owns the center dock-tab
//! invariants (Welcome ↔ View, place-view-tab targeting) and the CLI-parity
//! document surface (`buffers` / `focus`). Side-by-side is egui_dock's native
//! tab-drag — no hand-rolled split.

use seqforge_core::{DispatchError, DocInfo, ViewId, ViewerResponse};

use super::view_tab_order;
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

// Side-by-side of *different* documents is egui_dock's native tab-drag
// (drag a tab to an edge). The old hand-rolled `SplitPane` (which cloned the
// active buffer into a second view) is gone — it was the only path that put one
// buffer in two panes, and the sole trigger of the egui ID-collision (decision
// 19 follow-up). Different buffers never collide.

// ── Document management (GUI ↔ CLI parity — decision 19) ────────────────────

/// `buffers` — list the open documents in tab order. The `index` is the stable
/// 1-based handle `focus` accepts (alongside a path / basename).
pub(super) fn apply_buffers(state: &mut AppState) -> Result<Option<ViewerResponse>, DispatchError> {
    let active = state.workspace.active_view;
    let docs: Vec<DocInfo> = view_tab_order(state)
        .into_iter()
        .enumerate()
        .filter_map(|(i, vid)| doc_info(state, vid, i + 1, active == Some(vid)))
        .collect();
    Ok(Some(ViewerResponse::Buffers { docs }))
}

fn doc_info(state: &AppState, vid: ViewId, index: usize, active: bool) -> Option<DocInfo> {
    let view = state.workspace.view(vid)?;
    let arc = state.workspace.buffers.get(view.buffer_id)?;
    let buf = arc.read().ok()?;
    Some(DocInfo {
        index,
        name: crate::workspace::display_name(&buf),
        path: buf.source_path.clone(),
        dirty: buf.dirty,
        active,
    })
}

/// `focus <target>` — activate an open document by 1-based index (from
/// `buffers`), an exact path, or a file basename. The GUI equivalent is
/// clicking the document's tab.
pub(super) fn apply_focus_doc(
    state: &mut AppState,
    target: &str,
) -> Result<Option<ViewerResponse>, DispatchError> {
    let vid = resolve_doc(state, target).ok_or_else(|| {
        DispatchError::InvalidInput(format!("no open document matching {target:?}"))
    })?;
    apply_switch_tab(state, vid).map(|_| Some(ViewerResponse::Ok))
}

/// Resolve a document handle to a `ViewId`: all-digits → 1-based index into tab
/// order; else an exact path match, then a case-insensitive basename match.
fn resolve_doc(state: &AppState, target: &str) -> Option<ViewId> {
    let t = target.trim();
    let order = view_tab_order(state);
    if let Ok(n) = t.parse::<usize>() {
        return order.get(n.checked_sub(1)?).copied();
    }
    if let Some(vid) = state.workspace.find_view_for_path(std::path::Path::new(t)) {
        return Some(vid);
    }
    order.into_iter().find(|&vid| {
        let Some(view) = state.workspace.view(vid) else {
            return false;
        };
        let Some(arc) = state.workspace.buffers.get(view.buffer_id) else {
            return false;
        };
        let Ok(buf) = arc.read() else {
            return false;
        };
        buf.source_path
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|f| f.to_string_lossy().eq_ignore_ascii_case(t))
            .unwrap_or(false)
    })
}

pub(super) fn apply_reset_layout(
    state: &mut AppState,
) -> Result<Option<ViewerResponse>, DispatchError> {
    // Restore all shell regions (decision 19) + the center dock. Views are
    // preserved: rebuild_default_dock only resets the empty center; re-place any
    // open views afterward so Reset Layout never closes documents.
    state.show_files = true;
    state.show_terminal = true;
    state.show_inspector = true;
    let open: Vec<ViewId> = view_tab_order(state);
    crate::app::rebuild_default_dock(&mut state.dock_state, &state.config);
    for vid in open {
        place_view_tab(state, vid);
    }
    ensure_welcome_invariant(state);
    if let Some(vid) = state.workspace.active_view {
        dock_activate_view(state, vid);
    }
    Ok(None)
}

/// Toggle the Inspector region's visibility (native `SidePanel::right` —
/// decision 19). Flipping on focuses it; flipping off returns focus to the
/// active view (or terminal) if it was on the Inspector.
pub(super) fn apply_toggle_inspector(
    state: &mut AppState,
) -> Result<Option<ViewerResponse>, DispatchError> {
    state.show_inspector = !state.show_inspector;
    let scope = if state.show_inspector {
        FocusScope::Inspector
    } else if state.focus.scope == FocusScope::Inspector {
        state
            .workspace
            .active_view
            .map_or(FocusScope::Terminal, FocusScope::View)
    } else {
        return Ok(None);
    };
    state.focus.set_scope(scope);
    state.events.emit(AppEvent::FocusChanged(scope));
    Ok(None)
}

/// Make the Inspector region visible if it isn't. Shared by `ToggleInspector`
/// and the ⌘E enzyme re-target (decision 15). Does **not** touch focus —
/// callers decide that.
pub(super) fn ensure_inspector_visible(state: &mut AppState) {
    state.show_inspector = true;
}

/// Show/hide the minimap overview (a sibling region in the right column,
/// independent of the Inspector). It grabs no keyboard focus, so this is a
/// plain visibility flip.
pub(super) fn apply_toggle_minimap(
    state: &mut AppState,
) -> Result<Option<ViewerResponse>, DispatchError> {
    state.show_minimap = !state.show_minimap;
    Ok(None)
}

/// Toggle the Files region's visibility (native `SidePanel::left` — decision 19).
/// Flipping on focuses it; flipping off returns focus to the active view (or
/// terminal) if it was on the browser.
pub(super) fn apply_toggle_files(
    state: &mut AppState,
) -> Result<Option<ViewerResponse>, DispatchError> {
    state.show_files = !state.show_files;
    let scope = if state.show_files {
        FocusScope::Browser
    } else if state.focus.scope == FocusScope::Browser {
        state
            .workspace
            .active_view
            .map_or(FocusScope::Terminal, FocusScope::View)
    } else {
        return Ok(None);
    };
    state.focus.set_scope(scope);
    state.events.emit(AppEvent::FocusChanged(scope));
    Ok(None)
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
                        // A Recipe tab is content too, so it suppresses Welcome
                        // just like a View (else a lone recipe re-adds Welcome).
                        Tab::View(_) | Tab::Recipe(_) => view_count += 1,
                        Tab::Welcome => welcome_count += 1,
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

/// Place a new `Tab::View(view_id)` into the center dock. Chains into the active
/// view's leaf (a new doc opens as a tab in the currently-focused pane, so it
/// respects native splits); otherwise egui_dock's focused leaf. The center dock
/// holds only `View`/`Welcome` tabs now (the shell is native panels — decision
/// 19), so no cross-leaf hunting is needed.
pub(super) fn place_view_tab(state: &mut AppState, view_id: ViewId) {
    if let Some(active_vid) = state.workspace.active_view {
        if active_vid != view_id {
            if let Some((si, ni, _)) = state.dock_state.find_tab(&Tab::View(active_vid)) {
                state.dock_state[si][ni].append_tab(Tab::View(view_id));
                return;
            }
        }
    }
    state.dock_state.push_to_focused_leaf(Tab::View(view_id));
}

/// Activate `view_id`'s tab in the dock.
pub(super) fn dock_activate_view(state: &mut AppState, view_id: ViewId) {
    if let Some((si, ni, ti)) = state.dock_state.find_tab(&Tab::View(view_id)) {
        // Set egui_dock's *focused node* too, not just the active tab —
        // otherwise the end-of-frame reconciler (which trusts
        // `find_active_focused`) sees the still-focused old leaf and enqueues a
        // `SwitchTab` back, reverting a programmatic `focus`.
        state.dock_state.set_focused_node_and_surface((si, ni));
        state.dock_state.set_active_tab((si, ni, ti));
    }
}

/// Place a new `Tab::Recipe(id)` into the center dock (mirrors `place_view_tab`;
/// chains into the active view's leaf so it respects native splits).
pub(super) fn place_recipe_tab(state: &mut AppState, id: seqforge_core::RecipeId) {
    if let Some(active_vid) = state.workspace.active_view {
        if let Some((si, ni, _)) = state.dock_state.find_tab(&Tab::View(active_vid)) {
            state.dock_state[si][ni].append_tab(Tab::Recipe(id));
            return;
        }
    }
    state.dock_state.push_to_focused_leaf(Tab::Recipe(id));
}

/// Activate a recipe tab in the dock.
pub(super) fn dock_activate_recipe(state: &mut AppState, id: seqforge_core::RecipeId) {
    if let Some((si, ni, ti)) = state.dock_state.find_tab(&Tab::Recipe(id)) {
        state.dock_state.set_focused_node_and_surface((si, ni));
        state.dock_state.set_active_tab((si, ni, ti));
    }
}

/// Remove a recipe tab from the dock (its document is dropped by the caller).
pub(super) fn remove_recipe_tab(state: &mut AppState, id: seqforge_core::RecipeId) {
    if let Some(loc) = state.dock_state.find_tab(&Tab::Recipe(id)) {
        let _ = state.dock_state.remove_tab(loc);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toggle_inspector_flips_visibility_and_focus() {
        // Native right region (decision 19): default visible.
        let mut state = AppState::default();
        assert!(state.show_inspector);

        apply_toggle_inspector(&mut state).unwrap();
        assert!(
            !state.show_inspector,
            "toggle off hides the Inspector region"
        );

        apply_toggle_inspector(&mut state).unwrap();
        assert!(state.show_inspector, "toggle on shows it again");
        assert_eq!(state.focus.scope, FocusScope::Inspector);
    }

    // ── buffers / focus (GUI–CLI parity) ────────────────────────────────────

    struct TestBio;
    impl seqforge_core::BioOps for TestBio {
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
        fn primer_infos(
            &self,
            _: &[u8],
            _: &[&seqforge_core::Primer],
            _: bool,
        ) -> Vec<seqforge_core::PrimerInfo> {
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

    /// Write a temp fasta and open it through the real command path (so it gets a
    /// dock tab + active-view tracking). Returns the file path.
    fn open_temp(state: &mut AppState, tag: &str, seq: &str) -> std::path::PathBuf {
        use std::io::Write;
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let uniq = COUNTER.fetch_add(1, Ordering::Relaxed);
        let mut path = std::env::temp_dir();
        path.push(format!(
            "sf_buffers_{}_{uniq}_{tag}.fasta",
            std::process::id()
        ));
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, ">{tag}\n{seq}").unwrap();
        crate::command::apply(
            crate::command::AppCommand::Viewer(seqforge_core::ViewerRequest::Open {
                path: path.clone(),
            }),
            state,
            &TestBio,
        )
        .unwrap();
        path
    }

    #[test]
    fn buffers_lists_open_docs_with_active_flag() {
        let mut state = AppState::default();
        let pa = open_temp(&mut state, "a", "ACGT");
        let pb = open_temp(&mut state, "b", "TTTT"); // opened last → active

        let resp = apply_buffers(&mut state).unwrap();
        let seqforge_core::ViewerResponse::Buffers { docs } = resp.unwrap() else {
            panic!("expected Buffers");
        };
        assert_eq!(docs.len(), 2);
        // Indices are 1-based and stable; exactly one is active (the last opened).
        assert_eq!(docs.iter().filter(|d| d.active).count(), 1);
        assert!(docs.iter().any(|d| d.path.as_deref() == Some(pa.as_path())));
        assert!(
            docs.iter()
                .find(|d| d.path.as_deref() == Some(pb.as_path()))
                .is_some_and(|d| d.active)
        );
        let _ = (std::fs::remove_file(pa), std::fs::remove_file(pb));
    }

    #[test]
    fn focus_by_index_and_basename_switches_active() {
        let mut state = AppState::default();
        let pa = open_temp(&mut state, "a", "ACGT");
        let pb = open_temp(&mut state, "b", "TTTT");
        let vid_a = state.workspace.find_view_for_path(&pa).unwrap();

        // Focus doc #1 (a) by index.
        apply_focus_doc(&mut state, "1").unwrap();
        assert_eq!(state.workspace.active_view, Some(vid_a));

        // Focus b back by basename.
        let base_b = pb.file_name().unwrap().to_string_lossy().to_string();
        apply_focus_doc(&mut state, &base_b).unwrap();
        assert_eq!(
            state.workspace.active_view,
            state.workspace.find_view_for_path(&pb)
        );

        // Unknown handle errors.
        assert!(apply_focus_doc(&mut state, "nope.gb").is_err());
        let _ = (std::fs::remove_file(pa), std::fs::remove_file(pb));
    }
}
