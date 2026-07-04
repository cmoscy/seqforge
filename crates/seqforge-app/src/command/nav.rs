//! Navigation, search, selection commands.

use seqforge_core::{
    BioOps, DispatchError, EnzymeOp, FeatureId, PrimerId, Selection, Strand, ViewerRequest,
    ViewerResponse,
};

use super::{
    active_selection, dispatch_active, emit_selection_diff, restore_focus_after_overlay,
    snapshot_focus_for_overlay,
};
use crate::app::AppState;
use crate::event::AppEvent;
use crate::focus::FocusScope;
use crate::overlay::{FeatureForm, FindBar, GoToBar, Overlay, RenameFeatureForm, TranslationView};

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
    // ⌘E is now a pane verb: open the Inspector's Cut-sites tab and focus its
    // enzyme query (decision 15 / Phase 1.5b). The standalone enzyme bar is
    // retired; querying manages the persistent `active_enzymes` collection there.
    super::layout::dock_inspector_if_absent(state);
    state.inspector.reveal_enzyme_query();
    state.focus.set_scope(FocusScope::Inspector);
    state
        .events
        .emit(AppEvent::FocusChanged(FocusScope::Inspector));
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

/// Set / Add / Remove against the active enzyme set. Driven from the Inspector's
/// Enzymes (Cut-sites) tab (decision 15 / Phase 1.5b): the query header posts
/// Set/Add and per-row ✕ posts Remove; the set is persistent view state the tab
/// manages in place. (`active_enzymes` mutates; cut sites re-derive.)
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
/// into view. Used by Inspector row clicks (enzyme sites, cut sites) to jump the
/// map to a location while the pane stays put.
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
        // A fresh text selection deselects any feature *object* — this keeps the
        // object-vs-range invariant: `selected_feature.is_some()` ⟺ the feature's
        // range is the current selection. Delete-on-feature relies on it. A
        // feature click re-sets it (its `SelectFeature` applies after this).
        view.selected_feature = None;
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

/// Set `selected_primer` (panel highlight) without moving the map — the
/// map-click counterpart of [`apply_select_feature`]. Selecting a primer clears
/// the feature selection (mutually exclusive).
pub(super) fn apply_select_primer(
    state: &mut AppState,
    new_primer: Option<PrimerId>,
) -> Result<Option<ViewerResponse>, DispatchError> {
    if let Some(view) = state.workspace.active_view_mut() {
        view.selected_primer = new_primer;
        if new_primer.is_some() {
            view.selected_feature = None;
        }
    }
    Ok(None)
}

/// Select a primer by id (Inspector row-click). Sets `selected_primer`, clears
/// `selected_feature` (mutually exclusive panel selection), and — when the
/// primer is attached — selects + reveals its footprint (lighting the status-bar
/// Tm/%GC readout, like a map click). A detached/floating oligo is panel-only:
/// selected by id, no map move.
pub(super) fn apply_reveal_primer(
    state: &mut AppState,
    id: PrimerId,
) -> Result<Option<ViewerResponse>, DispatchError> {
    let before = active_selection(state);
    // Look up the authored footprint in the active buffer's annotations.
    let binding = state
        .workspace
        .with_active_buffer(|_v, _b, ann| ann.primer(id).and_then(|p| p.binding.clone()))
        .ok()
        .flatten();
    if let Some(view) = state.workspace.active_view_mut() {
        view.selected_primer = Some(id);
        view.selected_feature = None;
        if let Some(b) = &binding {
            view.selection = Some(Selection::range(b.start, b.end));
            view.scroll_to = Some(b.start);
        }
    }
    emit_selection_diff(state, before);
    Ok(None)
}

/// Select a feature by id (Inspector row-click): sets `selected_feature`, clears
/// `selected_primer`, and selects + reveals its range. Mirror of
/// [`apply_reveal_primer`].
pub(super) fn apply_reveal_feature(
    state: &mut AppState,
    id: FeatureId,
) -> Result<Option<ViewerResponse>, DispatchError> {
    let before = active_selection(state);
    let range = state
        .workspace
        .with_active_buffer(|_v, _b, ann| ann.get(id).map(|f| f.range.clone()))
        .ok()
        .flatten();
    if let Some(view) = state.workspace.active_view_mut() {
        view.selected_feature = Some(id);
        view.selected_primer = None;
        if let Some(r) = &range {
            view.selection = Some(Selection::range(r.start, r.end));
            view.scroll_to = Some(r.start);
        }
    }
    emit_selection_diff(state, before);
    Ok(None)
}

/// Route a feature into the Inspector's inline editor (decision 15,
/// tab-exclusive editing): dock the pane if hidden, enter the inline editor
/// pre-filled from the feature's current fields, select + reveal it on the map,
/// and move focus to the pane. `arm_delete` opens with the two-step delete armed.
/// The canvas edit/delete gestures route here instead of the center modal.
pub(super) fn apply_edit_feature_in_inspector(
    state: &mut AppState,
    id: FeatureId,
    arm_delete: bool,
) -> Result<Option<ViewerResponse>, DispatchError> {
    // Pull the feature's fields to seed the draft (id-addressed; decision 12).
    let fields = state
        .workspace
        .with_active_buffer(|_v, _b, ann| {
            ann.get(id).map(|f| {
                let flag = match f.strand {
                    Strand::Forward => "+",
                    Strand::Reverse => "-",
                    _ => ".",
                };
                (
                    f.label.clone(),
                    f.raw_kind.clone(),
                    flag.to_string(),
                    f.range.start,
                    f.range.end,
                )
            })
        })
        .ok()
        .flatten();
    let Some((label, kind, strand, start, end)) = fields else {
        return Ok(None); // feature vanished — nothing to edit
    };

    let before = active_selection(state);
    super::layout::dock_inspector_if_absent(state);
    state
        .inspector
        .begin_feature_edit(id, label, kind, strand, start, end, arm_delete);
    if let Some(view) = state.workspace.active_view_mut() {
        view.selected_feature = Some(id);
        view.selected_primer = None;
        view.selection = Some(Selection::range(start, end));
        view.scroll_to = Some(start);
    }
    emit_selection_diff(state, before);
    state.focus.set_scope(FocusScope::Inspector);
    state
        .events
        .emit(AppEvent::FocusChanged(FocusScope::Inspector));
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

#[cfg(test)]
mod tests {
    use super::*;
    use seqforge_core::{Primer, PrimerId};
    use std::io::Write;

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
    }

    fn open_with_primer(
        binding: Option<std::ops::Range<usize>>,
        tag: &str,
    ) -> (AppState, PrimerId) {
        let mut path = std::env::temp_dir();
        path.push(format!("sf_nav_{}_{tag}.fasta", std::process::id()));
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, ">t\nATGCGTACCA").unwrap();

        let mut state = AppState::default();
        let vid = state.workspace.open_path(&path, &TestBio).unwrap();
        state.workspace.focus_view(vid);
        let id = state
            .workspace
            .with_active_buffer_mut(|_v, _b, ann| {
                ann.add_primer(Primer {
                    id: PrimerId::default(),
                    name: "p".into(),
                    sequence: "GCGTAC".into(),
                    binding,
                    strand: Strand::Forward,
                    qualifiers: Default::default(),
                })
            })
            .unwrap();
        let _ = std::fs::remove_file(&path);
        (state, id)
    }

    #[test]
    fn reveal_attached_primer_selects_footprint_and_clears_feature() {
        let (mut state, id) = open_with_primer(Some(2..8), "attached");
        state.workspace.active_view_mut().unwrap().selected_feature = Some(FeatureId(9));

        apply_reveal_primer(&mut state, id).unwrap();

        let v = state.workspace.active_view().unwrap();
        assert_eq!(v.selected_primer, Some(id));
        assert_eq!(v.selected_feature, None, "primer selection clears feature");
        assert_eq!(v.selection, Some(Selection::range(2, 8)));
        assert_eq!(v.scroll_to, Some(2));
    }

    #[test]
    fn reveal_detached_primer_is_panel_only() {
        let (mut state, id) = open_with_primer(None, "detached");

        apply_reveal_primer(&mut state, id).unwrap();

        let v = state.workspace.active_view().unwrap();
        assert_eq!(v.selected_primer, Some(id));
        assert_eq!(v.scroll_to, None, "detached primer must not move the map");
    }

    #[test]
    fn select_primer_highlights_without_moving_map() {
        let (mut state, id) = open_with_primer(Some(2..8), "selp");
        state.workspace.active_view_mut().unwrap().selected_feature = Some(FeatureId(9));

        apply_select_primer(&mut state, Some(id)).unwrap();

        let v = state.workspace.active_view().unwrap();
        assert_eq!(v.selected_primer, Some(id));
        assert_eq!(
            v.selected_feature, None,
            "selecting a primer clears feature"
        );
        assert_eq!(v.scroll_to, None, "select (vs reveal) must not scroll");
    }

    #[test]
    fn reveal_feature_selects_range_and_clears_primer() {
        let mut path = std::env::temp_dir();
        path.push(format!("sf_nav_{}_feat.fasta", std::process::id()));
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, ">t\nATGCGTACCA").unwrap();
        let mut state = AppState::default();
        let vid = state.workspace.open_path(&path, &TestBio).unwrap();
        state.workspace.focus_view(vid);
        let fid = state
            .workspace
            .with_active_buffer_mut(|_v, _b, ann| {
                ann.add(seqforge_core::Feature {
                    id: FeatureId::default(),
                    range: 1..5,
                    raw_kind: "CDS".into(),
                    label: "gene".into(),
                    strand: Strand::Forward,
                    qualifiers: Default::default(),
                    provenance: None,
                })
            })
            .unwrap();
        state.workspace.active_view_mut().unwrap().selected_primer = Some(PrimerId(7));
        let _ = std::fs::remove_file(&path);

        apply_reveal_feature(&mut state, fid).unwrap();

        let v = state.workspace.active_view().unwrap();
        assert_eq!(v.selected_feature, Some(fid));
        assert_eq!(v.selected_primer, None, "selecting a feature clears primer");
        assert_eq!(v.selection, Some(Selection::range(1, 5)));
        assert_eq!(v.scroll_to, Some(1));
    }

    /// Build a state with one feature (range 2..8, reverse) for the routing tests.
    fn open_with_feature() -> (AppState, FeatureId) {
        let mut path = std::env::temp_dir();
        path.push(format!("sf_nav_{}_editfeat.fasta", std::process::id()));
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, ">t\nATGCGTACCAATGC").unwrap();
        let mut state = AppState::default();
        let vid = state.workspace.open_path(&path, &TestBio).unwrap();
        state.workspace.focus_view(vid);
        let fid = state
            .workspace
            .with_active_buffer_mut(|_v, _b, ann| {
                ann.add(seqforge_core::Feature {
                    id: FeatureId::default(),
                    range: 2..8,
                    raw_kind: "CDS".into(),
                    label: "lacZ".into(),
                    strand: Strand::Reverse,
                    qualifiers: Default::default(),
                    provenance: None,
                })
            })
            .unwrap();
        let _ = std::fs::remove_file(&path);
        (state, fid)
    }

    #[test]
    fn set_selection_clears_the_feature_object_selection() {
        // The object-vs-range invariant Delete-on-feature relies on: a fresh text
        // selection deselects the feature object.
        let (mut state, fid) = open_with_feature();
        state.workspace.active_view_mut().unwrap().selected_feature = Some(fid);
        apply_set_selection(&mut state, Some(Selection::range(0, 3))).unwrap();
        assert_eq!(
            state.workspace.active_view().unwrap().selected_feature,
            None
        );
    }

    #[test]
    fn edit_feature_in_inspector_enters_editor_selects_and_focuses_pane() {
        let (mut state, fid) = open_with_feature();
        apply_edit_feature_in_inspector(&mut state, fid, true).unwrap();
        assert!(
            state.inspector.is_editing(),
            "inline editor should be armed"
        );
        let v = state.workspace.active_view().unwrap();
        assert_eq!(v.selected_feature, Some(fid));
        assert_eq!(v.selection, Some(Selection::range(2, 8)));
        assert_eq!(state.focus.scope, FocusScope::Inspector);
    }

    #[test]
    fn edit_feature_in_inspector_missing_feature_is_a_noop() {
        let (mut state, _fid) = open_with_feature();
        apply_edit_feature_in_inspector(&mut state, FeatureId(9999), false).unwrap();
        assert!(!state.inspector.is_editing());
    }
}
