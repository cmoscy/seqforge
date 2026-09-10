//! The GUI-side remainder of the write path.
//!
//! The verbs themselves live in `seqforge_session::edit`, where the CLI and the
//! socket can reach them. What stays here is what is genuinely a *gesture*
//! rather than a command: the two modal-form submitters, which dismiss an
//! overlay before delegating to a verb, and Save, which routes through the
//! GUI's file layer (dialogs, the dirty/external-change guards) rather than
//! writing bytes directly. See ROADMAP decision 27.

use seqforge_core::{DispatchError, FeatureId, ViewId, ViewerResponse};
use seqforge_session::edit;
#[cfg(test)]
use seqforge_session::edit as sedit;

use crate::app::AppState;
use crate::command::AppCommand;

/// Commit the unified feature modal, then dismiss it. `id = None` creates a new
/// feature (`AddFeature`); `id = Some` edits an existing one (`UpdateFeature`,
/// all fields). The modal always targets the active view.
#[allow(clippy::too_many_arguments)]
pub(super) fn apply_submit_feature_form(
    state: &mut AppState,
    id: Option<FeatureId>,
    label: String,
    kind: String,
    strand: String,
    start: usize,
    end: usize,
) -> Result<Option<ViewerResponse>, DispatchError> {
    match id {
        None => {
            edit::apply_add_feature(
                &mut state.workspace,
                None,
                start,
                end,
                kind,
                label.clone(),
                strand,
            )?;
            super::nav::apply_dismiss_overlay(state)?;
            let shown = if label.is_empty() { "feature" } else { &label };
            state.toasts.success(format!("Added {shown}"));
        }
        Some(id) => {
            edit::apply_update_feature(
                &mut state.workspace,
                None,
                id,
                Some(kind),
                Some(label),
                Some(strand),
                Some(start),
                Some(end),
            )?;
            super::nav::apply_dismiss_overlay(state)?;
        }
    }
    Ok(Some(ViewerResponse::Ok))
}

/// Commit the Rename modal: one `RenameFeature`, dismiss the modal.
pub(super) fn apply_submit_rename_feature(
    state: &mut AppState,
    id: FeatureId,
    label: String,
) -> Result<Option<ViewerResponse>, DispatchError> {
    edit::apply_rename_feature(&mut state.workspace, None, id, label)?;
    super::nav::apply_dismiss_overlay(state)?;
    Ok(Some(ViewerResponse::Ok))
}

// ── Save (12e) ─────────────────────────────────────────────────────────────────

/// Save the target buffer. If it has a source path, save synchronously (so a
/// CLI/agent `save` gets immediate success/failure). Otherwise fall back to the
/// GUI Save-As dialog. `SaveAs` (below) is the dialog-driven path.
pub(super) fn apply_save(
    state: &mut AppState,
    view: Option<ViewId>,
    force: bool,
) -> Result<Option<ViewerResponse>, DispatchError> {
    let vid = edit::resolve_target(&state.workspace, view)?;
    let path = state
        .workspace
        .with_buffer(vid, |_, buf, _| buf.source_path.clone())?;
    match path {
        Some(path) => {
            super::file::save_buffer(state, vid, &path, force).map(|()| Some(ViewerResponse::Ok))
        }
        None => {
            // No path yet — route to Save-As (GUI dialog). Headless callers get
            // a clear error rather than a silent no-op.
            apply_save_as(state, view)
        }
    }
}

pub(super) fn apply_save_as(
    state: &mut AppState,
    view: Option<ViewId>,
) -> Result<Option<ViewerResponse>, DispatchError> {
    let vid = edit::resolve_target(&state.workspace, view)?;
    state
        .pending_commands
        .push((AppCommand::OpenSaveAs { view: Some(vid) }, None));
    Ok(None)
}
#[cfg(test)]
mod tests {
    use std::ops::Range;

    use seqforge_core::{PrimerId, Strand};

    use crate::command::StagedEdit;
    use crate::focus::FocusScope;

    // ── Clipboard verbs need a Host ───────────────────────────────────────
    // These three reach the OS pasteboard, so they take `(&mut Workspace,
    // &mut dyn Host)`. `AppState::session()` hands out both as disjoint
    // borrows; `ClipboardState::default()` is memory-only under `cfg(test)`,
    // so no display is touched.

    fn h_copy(
        s: &mut AppState,
        view: Option<ViewId>,
        start: usize,
        end: usize,
    ) -> Result<Option<ViewerResponse>, DispatchError> {
        let (ws, mut host) = s.session();
        sedit::apply_copy(ws, &mut host, view, start, end)
    }

    fn h_cut(
        s: &mut AppState,
        view: Option<ViewId>,
        start: usize,
        end: usize,
    ) -> Result<Option<ViewerResponse>, DispatchError> {
        let (ws, mut host) = s.session();
        sedit::apply_cut(ws, &mut host, view, start, end)
    }

    fn h_paste(
        s: &mut AppState,
        view: Option<ViewId>,
        pos: usize,
    ) -> Result<Option<ViewerResponse>, DispatchError> {
        let (ws, mut host) = s.session();
        sedit::apply_paste(ws, &mut host, view, pos)
    }
    use super::*;
    use seqforge_core::{
        BioOps, Feature, Primer, Selection, SeqSlice, Topology, ViewKind, ViewSelection,
    };

    /// A bytes-only clipboard slice (no carried annotations) for paste tests.
    fn clip(bytes: &[u8]) -> SeqSlice {
        SeqSlice {
            bytes: bytes.to_vec(),
            features: Vec::new(),
            primers: Vec::new(),
        }
    }

    /// Headless `AppState` with one active view over `seq`.
    fn state_with(seq: &[u8]) -> AppState {
        let mut state = AppState::default();
        let bid =
            state
                .workspace
                .buffers
                .new_scratch("test".into(), seq.to_vec(), Topology::Linear);
        state.workspace.add_view(bid, ViewKind::TextView);
        state
    }

    fn text(state: &mut AppState) -> Vec<u8> {
        state
            .workspace
            .with_active_buffer(|_, buf, _| buf.text.clone())
            .unwrap()
    }

    fn feature_count(state: &mut AppState) -> usize {
        state
            .workspace
            .with_active_buffer(|_, _, ann| ann.len())
            .unwrap()
    }

    fn is_circular(state: &mut AppState) -> bool {
        state
            .workspace
            .with_active_buffer(|_, b, _| b.is_circular())
            .unwrap()
    }

    #[test]
    fn copy_new_paste_carries_features_across_buffers() {
        // The transport foundation smoke test: copy an annotated region, create a
        // NEW buffer, paste, and confirm the carried feature lands (cross-buffer
        // via the app-level clipboard).
        let mut s = state_with(b"ATGCATGCATGCATGCATGC"); // 20 bp
        add_feat(&mut s, 4, 12, "gene");
        h_copy(&mut s, None, 4, 12).unwrap();
        assert!(
            s.clipboard.slice.is_some(),
            "region copy fills the clipboard"
        );

        // New empty circular buffer becomes the active view.
        crate::command::file::apply_new(&mut s, true, Some("construct".into())).unwrap();
        assert_eq!(text(&mut s), b"", "new buffer starts empty");
        assert_eq!(feature_count(&mut s), 0);
        assert!(is_circular(&mut s), "New --circular");
        {
            let v = s.workspace.active_view().unwrap();
            assert_eq!(
                v.selection.text_range(),
                Some(Selection::cursor(0)),
                "New opens with a live caret at 0 so ⌘V can arm"
            );
        }

        // Paste the fragment; its carried feature re-homes via `place`.
        h_paste(&mut s, None, 0).unwrap();
        assert_eq!(text(&mut s), b"ATGCATGC", "copied 8 bp landed");
        assert_eq!(
            feature_count(&mut s),
            1,
            "carried feature landed in the new buffer"
        );
    }

    #[test]
    fn circularize_set_origin_linearize_with_topology_undo() {
        let mut s = state_with(b"AAAACCCCGGGGTTTT"); // 16 bp, linear
        assert!(!is_circular(&mut s));

        sedit::apply_circularize(&mut s.workspace, None, None).unwrap();
        assert!(is_circular(&mut s), "circularize flips topology");

        sedit::apply_set_origin(&mut s.workspace, None, Some(4), None).unwrap();
        assert_eq!(
            text(&mut s),
            b"CCCCGGGGTTTTAAAA",
            "set-origin rotates the bytes"
        );

        sedit::apply_linearize(&mut s.workspace, None, Some(0)).unwrap();
        assert!(!is_circular(&mut s), "linearize flips topology back");

        // Undo restores topology (the history topology-stamp), not just bytes.
        sedit::apply_undo(&mut s.workspace, None).unwrap();
        assert!(is_circular(&mut s), "undo restores circular topology");
        sedit::apply_undo(&mut s.workspace, None).unwrap();
        assert_eq!(
            text(&mut s),
            b"AAAACCCCGGGGTTTT",
            "undo restores the rotation"
        );
    }

    #[test]
    fn insert_lowers_to_splice() {
        let mut s = state_with(b"ATGC");
        let resp = sedit::apply_insert(&mut s.workspace, None, 2, "TT".into()).unwrap();
        assert_eq!(text(&mut s), b"ATTTGC");
        assert!(matches!(
            resp,
            Some(ViewerResponse::Edited {
                len: 6,
                changed: true
            })
        ));
    }

    #[test]
    fn delete_then_undo_redo_round_trips() {
        let mut s = state_with(b"ATGCAA");
        sedit::apply_delete(&mut s.workspace, None, 1, 4).unwrap();
        assert_eq!(text(&mut s), b"AAA");

        let undo = sedit::apply_undo(&mut s.workspace, None).unwrap();
        assert!(matches!(
            undo,
            Some(ViewerResponse::Edited { changed: true, .. })
        ));
        assert_eq!(text(&mut s), b"ATGCAA");

        sedit::apply_redo(&mut s.workspace, None).unwrap();
        assert_eq!(text(&mut s), b"AAA");
    }

    #[test]
    fn undo_with_empty_history_reports_unchanged() {
        let mut s = state_with(b"ATGC");
        let resp = sedit::apply_undo(&mut s.workspace, None).unwrap();
        assert!(matches!(
            resp,
            Some(ViewerResponse::Edited { changed: false, .. })
        ));
    }

    #[test]
    fn replace_swaps_region() {
        let mut s = state_with(b"AAGGCC");
        sedit::apply_replace(&mut s.workspace, None, 2, 4, "TT".into()).unwrap();
        assert_eq!(text(&mut s), b"AATTCC");
    }

    #[test]
    fn reverse_complement_composes_bio_then_splice() {
        let mut s = state_with(b"AAATGCCC");
        // bytes 1..5 are "AATG"; reverse-complement is "CATT".
        sedit::apply_reverse_complement(&mut s.workspace, None, 1, 5).unwrap();
        assert_eq!(text(&mut s), b"ACATTCCC");
    }

    #[test]
    fn cut_copies_to_clipboard_and_deletes() {
        let mut s = state_with(b"ATGCAA");
        h_cut(&mut s, None, 2, 4).unwrap();
        assert_eq!(text(&mut s), b"ATAA");
        assert_eq!(
            s.clipboard.slice.as_ref().map(|c| c.bytes()),
            Some(b"GC".as_slice())
        );
    }

    #[test]
    fn stage_edit_arms_preview_without_mutating() {
        let mut s = state_with(b"ATGCAA");
        // A menu Cut used to delete immediately; now it stages a preview.
        crate::command::stage::apply_stage_edit(&mut s, StagedEdit::Cut { start: 2, end: 4 })
            .unwrap();
        assert_eq!(text(&mut s), b"ATGCAA"); // buffer untouched until Enter
        let vid = s.workspace.active_view().unwrap().id;
        // Focuses the target view (so the stage survives + Enter commits) and
        // arms the canvas pending edit.
        assert_eq!(s.focus.scope, FocusScope::View(vid));
        assert!(s.seq_views.get(vid).unwrap().is_staging());
    }

    #[test]
    fn copy_leaves_buffer_unchanged() {
        let mut s = state_with(b"ATGC");
        let resp = h_copy(&mut s, None, 0, 2).unwrap();
        assert_eq!(text(&mut s), b"ATGC");
        assert_eq!(
            s.clipboard.slice.as_ref().map(|c| c.bytes()),
            Some(b"AT".as_slice())
        );
        assert!(matches!(
            resp,
            Some(ViewerResponse::Edited { changed: false, .. })
        ));
    }

    #[test]
    fn copy_selected_primer_yields_the_oligo_not_the_template_slice() {
        use seqforge_core::{Primer, PrimerId, Strand};
        let mut s = state_with(b"AAAAATGCGGGGG");
        let vid = s.workspace.active_view().unwrap().id;
        // A reverse primer bound at 5..8 ("TGC" on the top strand); its authored
        // oligo is the revcomp ("GCA") — deliberately ≠ the template slice, so a
        // wrong (slice-based) copy is observable.
        s.workspace
            .with_buffer_mut(vid, |v, _b, ann| {
                let id = ann.add_primer(Primer {
                    id: PrimerId::default(),
                    name: "rev".into(),
                    sequence: "GCA".into(),
                    binding: Some(seqforge_core::Span::from_range(5..8)),
                    strand: Strand::Reverse,
                    qualifiers: Default::default(),
                });
                v.selection = ViewSelection::Primer(id);
            })
            .unwrap();

        // Copying the primer's exact footprint copies the oligo, not "TGC".
        h_copy(&mut s, None, 5, 8).unwrap();
        assert_eq!(
            s.clipboard.slice.as_ref().map(|c| c.bytes()),
            Some(b"GCA".as_slice())
        );

        // A copy over a *different* range stays a literal template slice — the
        // gate is `range == binding`, so CLI/agent range copies are unaffected.
        h_copy(&mut s, None, 0, 3).unwrap();
        assert_eq!(
            s.clipboard.slice.as_ref().map(|c| c.bytes()),
            Some(b"AAA".as_slice())
        );
    }

    #[test]
    fn copy_bare_cursor_with_selected_primer_yields_the_oligo() {
        use seqforge_core::{Primer, PrimerId, Strand};
        // Phase 1.5e: a selected primer carries no template range, so the canvas
        // ⌘C posts a zero (bare-cursor) Copy. `apply_copy` must still copy the
        // authored oligo — keyed off `selected_primer` — not an empty slice.
        let mut s = state_with(b"AAAAATGCGGGGG");
        let vid = s.workspace.active_view().unwrap().id;
        s.workspace
            .with_buffer_mut(vid, |v, _b, ann| {
                let id = ann.add_primer(Primer {
                    id: PrimerId::default(),
                    name: "rev".into(),
                    sequence: "GCA".into(),
                    binding: Some(seqforge_core::Span::from_range(5..8)),
                    strand: Strand::Reverse,
                    qualifiers: Default::default(),
                });
                v.selection = ViewSelection::Primer(id);
            })
            .unwrap();

        h_copy(&mut s, None, 0, 0).unwrap();
        assert_eq!(
            s.clipboard.slice.as_ref().map(|c| c.bytes()),
            Some(b"GCA".as_slice())
        );
    }

    #[test]
    fn copy_bare_cursor_with_detached_selected_primer_yields_the_oligo() {
        use seqforge_core::{Primer, PrimerId, Strand};
        // A detached (floating) selected oligo has no binding, but ⌘C still copies
        // its authored sequence via the bare-cursor trigger.
        let mut s = state_with(b"AAAAATGCGGGGG");
        let vid = s.workspace.active_view().unwrap().id;
        s.workspace
            .with_buffer_mut(vid, |v, _b, ann| {
                let id = ann.add_primer(Primer {
                    id: PrimerId::default(),
                    name: "float".into(),
                    sequence: "TTTGGG".into(),
                    binding: None,
                    strand: Strand::Forward,
                    qualifiers: Default::default(),
                });
                v.selection = ViewSelection::Primer(id);
            })
            .unwrap();

        h_copy(&mut s, None, 0, 0).unwrap();
        assert_eq!(
            s.clipboard.slice.as_ref().map(|c| c.bytes()),
            Some(b"TTTGGG".as_slice())
        );
    }

    #[test]
    fn copy_without_selected_primer_is_a_template_slice() {
        let mut s = state_with(b"AAATGCGG");
        h_copy(&mut s, None, 3, 6).unwrap();
        assert_eq!(
            s.clipboard.slice.as_ref().map(|c| c.bytes()),
            Some(b"TGC".as_slice())
        );
    }

    #[test]
    fn paste_inserts_clipboard() {
        let mut s = state_with(b"ATGC");
        s.clipboard.slice = Some(clip(b"NN"));
        h_paste(&mut s, None, 4).unwrap();
        assert_eq!(text(&mut s), b"ATGCNN");
    }

    #[test]
    fn paste_empty_clipboard_errors() {
        let mut s = state_with(b"ATGC");
        let err = h_paste(&mut s, None, 0).unwrap_err();
        assert!(matches!(err, DispatchError::InvalidInput(_)));
    }

    /// Feature spans (hulls) in definition order on the active buffer.
    fn feature_spans(state: &mut AppState) -> Vec<Range<usize>> {
        state
            .workspace
            .with_active_buffer(|_, b, ann| ann.iter().map(|f| f.bounds(b.text.len())).collect())
            .unwrap()
    }

    /// Primer bindings in definition order on the active buffer.
    fn primer_bindings(state: &mut AppState) -> Vec<Option<Range<usize>>> {
        state
            .workspace
            .with_active_buffer(|_, _, ann| {
                ann.primers()
                    .map(|p| p.binding.map(|b| b.start..b.start + b.len))
                    .collect()
            })
            .unwrap()
    }

    #[test]
    fn copy_carries_feature_and_primer_through_paste() {
        // Region [2,8) contains feature [3,6) and primer binding [4,7).
        let mut s = state_with(b"ATGCATGCATGC");
        add_feat(&mut s, 3, 6, "gene");
        s.workspace
            .with_active_buffer_mut(|_, _, ann| {
                ann.add_primer(Primer {
                    id: Default::default(),
                    name: "p1".into(),
                    sequence: "GCA".into(),
                    binding: Some(seqforge_core::Span::from_range(4..7)),
                    strand: seqforge_core::Strand::Forward,
                    qualifiers: Default::default(),
                });
            })
            .unwrap();

        // Copy [2,8) → paste at 12 (end). Feature localizes to [1,4) then +12 →
        // [13,16); primer binding [2,5)+12 → [14,17).
        h_copy(&mut s, None, 2, 8).unwrap();
        h_paste(&mut s, None, 12).unwrap();

        assert_eq!(text(&mut s), b"ATGCATGCATGCGCATGC");
        assert_eq!(feature_spans(&mut s), vec![3..6, 13..16]);
        assert_eq!(
            primer_bindings(&mut s),
            vec![Some(4..7), Some(14..17)],
            "primer carried with shifted binding"
        );

        // Undo removes the pasted bytes AND the placed annotations (one txn).
        sedit::apply_undo(&mut s.workspace, None).unwrap();
        assert_eq!(text(&mut s), b"ATGCATGCATGC");
        assert_eq!(feature_spans(&mut s), vec![3..6]);
        assert_eq!(primer_bindings(&mut s), vec![Some(4..7)]);
    }

    #[test]
    fn copy_paste_through_dispatch_carries_features() {
        // CLI/GUI parity: both surfaces build these exact `ViewerRequest` values
        // and route through the one `command::apply` dispatch (no GUI-emits-CLI-
        // text). Driving that dispatch must carry features, same as the GUI walk.
        use seqforge_core::ViewerRequest;
        let mut s = state_with(b"ATGCATGCATGC");
        add_feat(&mut s, 3, 6, "gene");
        crate::command::apply(
            AppCommand::Viewer(ViewerRequest::Copy {
                start: 2,
                end: 8,
                view: None,
            }),
            &mut s,
            &LoadBio,
        )
        .unwrap();
        crate::command::apply(
            AppCommand::Viewer(ViewerRequest::Paste {
                pos: 12,
                view: None,
            }),
            &mut s,
            &LoadBio,
        )
        .unwrap();
        assert_eq!(feature_spans(&mut s), vec![3..6, 13..16]);
    }

    #[test]
    fn paste_of_whole_feature_next_to_source_does_not_merge() {
        // Ordinary paste: the source feature is unstamped (provenance None), so
        // even abutting the copy it stays two distinct features (merge is
        // provenance-gated; a fresh copy never fuses with a loaded feature).
        let mut s = state_with(b"ATGCATGC");
        add_feat(&mut s, 0, 4, "gene"); // [0,4)
        h_copy(&mut s, None, 0, 4).unwrap();
        h_paste(&mut s, None, 4).unwrap(); // paste abutting at 4 → [4,8)
        assert_eq!(feature_spans(&mut s), vec![0..4, 4..8], "no silent merge");
    }

    #[test]
    fn insert_rejects_non_iupac() {
        let mut s = state_with(b"ATGC");
        let err = sedit::apply_insert(&mut s.workspace, None, 0, "ATZ".into()).unwrap_err();
        assert!(matches!(err, DispatchError::InvalidInput(_)));
        assert_eq!(text(&mut s), b"ATGC", "rejected insert must not mutate");
    }

    #[test]
    fn insert_strips_whitespace() {
        let mut s = state_with(b"ATGC");
        sedit::apply_insert(&mut s.workspace, None, 0, "a t g".into()).unwrap();
        assert_eq!(text(&mut s), b"ATGATGC");
    }

    /// Add a feature and return its minted id.
    fn add_feat(state: &mut AppState, start: usize, end: usize, label: &str) -> FeatureId {
        match sedit::apply_add_feature(
            &mut state.workspace,
            None,
            start,
            end,
            "CDS".into(),
            label.into(),
            "+".into(),
        )
        .unwrap()
        {
            Some(ViewerResponse::FeatureAdded { id, .. }) => id,
            other => panic!("expected FeatureAdded, got {other:?}"),
        }
    }

    fn first_label(state: &mut AppState) -> String {
        state
            .workspace
            .with_active_buffer(|_, _, ann| ann.iter().next().unwrap().label.clone())
            .unwrap()
    }

    #[test]
    fn add_remove_rename_feature() {
        let mut s = state_with(b"ATGCATGC");
        let id = add_feat(&mut s, 0, 3, "gene1");
        assert_eq!(feature_count(&mut s), 1);

        sedit::apply_rename_feature(&mut s.workspace, None, id, "renamed".into()).unwrap();
        assert_eq!(first_label(&mut s), "renamed");

        sedit::apply_remove_feature(&mut s.workspace, None, id).unwrap();
        assert_eq!(feature_count(&mut s), 0);
    }

    #[test]
    fn submit_feature_form_create_adds_and_dismisses() {
        use crate::overlay::{FeatureForm, Overlay};
        let mut s = state_with(b"ATGCATGC");
        s.overlays
            .push_unique(Overlay::FeatureForm(FeatureForm::create(0, 3)));
        // id = None → create path.
        apply_submit_feature_form(&mut s, None, "gene1".into(), "CDS".into(), "+".into(), 0, 3)
            .unwrap();
        assert_eq!(feature_count(&mut s), 1);
        // The modal was dismissed on submit.
        assert!(s.overlays.is_empty());
    }

    #[test]
    fn submit_feature_form_edit_updates_and_dismisses() {
        use crate::overlay::{FeatureForm, Overlay};
        let mut s = state_with(b"ATGCATGCATGC");
        let id = add_feat(&mut s, 0, 3, "orig");
        s.overlays
            .push_unique(Overlay::FeatureForm(FeatureForm::edit(
                id,
                "orig".into(),
                "CDS".into(),
                "+".into(),
                0,
                3,
            )));
        // id = Some → update path.
        apply_submit_feature_form(
            &mut s,
            Some(id),
            "renamed".into(),
            "gene".into(),
            "-".into(),
            4,
            9,
        )
        .unwrap();
        let (label, range) = s
            .workspace
            .with_active_buffer(|_, b, ann| {
                let f = ann.get(id).unwrap();
                (f.label.clone(), f.bounds(b.text.len()))
            })
            .unwrap();
        assert_eq!(label, "renamed");
        assert_eq!(range, 4..9);
        assert!(s.overlays.is_empty());
    }

    #[test]
    fn feature_ops_are_undoable() {
        let mut s = state_with(b"ATGCATGC");
        let vid = s.workspace.active_view().unwrap().id;

        let id = add_feat(&mut s, 0, 3, "orig");
        sedit::apply_rename_feature(&mut s.workspace, None, id, "renamed".into()).unwrap();
        sedit::apply_remove_feature(&mut s.workspace, None, id).unwrap();
        assert_eq!(feature_count(&mut s), 0);

        // Undo remove → feature back, still "renamed".
        s.workspace.undo(vid).unwrap();
        assert_eq!(feature_count(&mut s), 1);
        assert_eq!(first_label(&mut s), "renamed");

        // Undo rename → "orig".
        s.workspace.undo(vid).unwrap();
        assert_eq!(first_label(&mut s), "orig");

        // Undo add → gone.
        s.workspace.undo(vid).unwrap();
        assert_eq!(feature_count(&mut s), 0);

        // Redo add → back.
        s.workspace.redo(vid).unwrap();
        assert_eq!(feature_count(&mut s), 1);
        assert_eq!(first_label(&mut s), "orig");
    }

    /// Minimal `BioOps` whose `load` uses the real parser; the rest is inert
    /// (the edit/undo path never calls them).
    struct LoadBio;
    impl BioOps for LoadBio {
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

    /// Editor history-correctness property (Phase 16): loading a real circular
    /// plasmid, applying a mixed edit script (insert / delete / replace /
    /// reverse-complement / feature add·update·remove), then undoing everything
    /// must restore a **byte-for-byte identical** buffer + annotation model. This
    /// exercises the snapshot-based undo (decision 1) over a feature-rich,
    /// origin-topology fixture — the regression net for silent undo/shift bugs.
    #[test]
    fn puc19_mixed_edit_script_undoes_to_identical_model() {
        // `Feature`/`Primer` aren't `PartialEq`; project to comparable tuples.
        type FeatProj = (
            std::ops::Range<usize>,
            String,
            String,
            Strand,
            std::collections::BTreeMap<String, Option<String>>,
            Option<seqforge_core::Lineage>,
        );
        fn proj_feats(fs: &[Feature], len: usize) -> Vec<FeatProj> {
            fs.iter()
                .map(|f| {
                    (
                        f.bounds(len),
                        f.raw_kind.clone(),
                        f.label.clone(),
                        f.strand,
                        f.qualifiers.clone(),
                        f.lineage.clone(),
                    )
                })
                .collect()
        }
        type PrimerProj = (
            String,
            String,
            Option<std::ops::Range<usize>>,
            Strand,
            std::collections::BTreeMap<String, Option<String>>,
        );
        fn proj_primers(ps: &[Primer]) -> Vec<PrimerProj> {
            ps.iter()
                .map(|p| {
                    (
                        p.name.clone(),
                        p.sequence.clone(),
                        p.binding.map(|b| b.start..b.start + b.len),
                        p.strand,
                        p.qualifiers.clone(),
                    )
                })
                .collect()
        }

        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../seqforge-bio/tests/fixtures/pUC19.gbk");
        let mut s = AppState::default();
        let vid = s
            .workspace
            .open_path(&path, &LoadBio)
            .expect("load pUC19 fixture");
        s.workspace.focus_view(vid);

        let snapshot = |s: &mut AppState| {
            s.workspace
                .with_active_buffer(|_, buf, ann| {
                    (
                        buf.text.clone(),
                        buf.topology,
                        ann.iter().cloned().collect::<Vec<Feature>>(),
                        ann.primers().cloned().collect::<Vec<Primer>>(),
                    )
                })
                .unwrap()
        };
        let original = snapshot(&mut s);
        assert_eq!(original.0.len(), 2686, "pUC19 is 2686 bp");
        assert_eq!(original.1, Topology::Circular);
        assert!(!original.2.is_empty(), "pUC19 has features to exercise");

        // Fixed, mixed edit script — positions stay valid across prior edits.
        let mut ops = 0;
        sedit::apply_insert(&mut s.workspace, None, 100, "ATGCATGC".into()).unwrap();
        ops += 1;
        sedit::apply_delete(&mut s.workspace, None, 500, 520).unwrap();
        ops += 1;
        sedit::apply_replace(&mut s.workspace, None, 200, 210, "TTTTAAAA".into()).unwrap();
        ops += 1;
        sedit::apply_reverse_complement(&mut s.workspace, None, 1000, 1040).unwrap();
        ops += 1;
        let fid = match sedit::apply_add_feature(
            &mut s.workspace,
            None,
            50,
            80,
            "misc_feature".into(),
            "scratch".into(),
            "+".into(),
        )
        .unwrap()
        {
            Some(ViewerResponse::FeatureAdded { id, .. }) => id,
            other => panic!("expected FeatureAdded, got {other:?}"),
        };
        ops += 1;
        sedit::apply_update_feature(
            &mut s.workspace,
            None,
            fid,
            Some("CDS".into()),
            Some("renamed".into()),
            Some("-".into()),
            Some(55),
            Some(85),
        )
        .unwrap();
        ops += 1;
        sedit::apply_remove_feature(&mut s.workspace, None, fid).unwrap();
        ops += 1;

        // Sanity: the script actually changed the buffer.
        assert_ne!(
            snapshot(&mut s).0,
            original.0,
            "edits should mutate the buffer"
        );

        // Undo everything (LIFO).
        for _ in 0..ops {
            s.workspace.undo(vid).unwrap();
        }

        let restored = snapshot(&mut s);
        assert_eq!(restored.0, original.0, "sequence not restored by undo");
        assert_eq!(restored.1, original.1, "topology not restored by undo");
        assert_eq!(
            proj_feats(&restored.2, restored.0.len()),
            proj_feats(&original.2, original.0.len()),
            "features not restored by undo"
        );
        assert_eq!(
            proj_primers(&restored.3),
            proj_primers(&original.3),
            "primers not restored by undo"
        );
    }

    #[test]
    fn add_feature_bumps_version() {
        let mut s = state_with(b"ATGCATGC");
        let v0 = s
            .workspace
            .with_active_buffer(|_, buf, _| buf.version)
            .unwrap();
        sedit::apply_add_feature(
            &mut s.workspace,
            None,
            0,
            3,
            "CDS".into(),
            "g".into(),
            "+".into(),
        )
        .unwrap();
        let v1 = s
            .workspace
            .with_active_buffer(|_, buf, _| buf.version)
            .unwrap();
        assert_eq!(v1, v0 + 1, "annotation edits must bump version (cache key)");
    }

    #[test]
    fn add_feature_out_of_range_errors() {
        let mut s = state_with(b"ATGC");
        let err = sedit::apply_add_feature(
            &mut s.workspace,
            None,
            2,
            99,
            "CDS".into(),
            "g".into(),
            "+".into(),
        )
        .unwrap_err();
        assert!(matches!(err, DispatchError::OutOfRange { .. }));
    }

    #[test]
    fn update_feature_partial_and_undoable() {
        let mut s = state_with(b"ATGCATGCATGC");
        let vid = s.workspace.active_view().unwrap().id;
        let id = add_feat(&mut s, 0, 3, "orig");

        // Change only the range + kind; label/strand left untouched.
        sedit::apply_update_feature(
            &mut s.workspace,
            None,
            id,
            Some("misc_feature".into()),
            None,
            None,
            Some(4),
            Some(9),
        )
        .unwrap();
        let (range, kind, label) = s
            .workspace
            .with_active_buffer(|_, b, ann| {
                let f = ann.get(id).unwrap();
                (f.bounds(b.text.len()), f.raw_kind.clone(), f.label.clone())
            })
            .unwrap();
        assert_eq!(range, 4..9);
        assert_eq!(kind, "misc_feature");
        assert_eq!(label, "orig", "unspecified fields are preserved");

        // Undo restores the original geometry.
        s.workspace.undo(vid).unwrap();
        let range = s
            .workspace
            .with_active_buffer(|_, b, ann| ann.get(id).unwrap().bounds(b.text.len()))
            .unwrap();
        assert_eq!(range, 0..3);
    }

    #[test]
    fn update_feature_bad_range_errors() {
        let mut s = state_with(b"ATGC");
        let id = add_feat(&mut s, 0, 3, "f");
        let err = sedit::apply_update_feature(
            &mut s.workspace,
            None,
            id,
            None,
            None,
            None,
            Some(2),
            Some(99),
        )
        .unwrap_err();
        assert!(matches!(err, DispatchError::OutOfRange { .. }));
    }

    #[test]
    fn resize_of_wrapping_feature_requires_both_endpoints() {
        // P5a correct-by-omission: a wrapping (or spliced) feature has no single
        // linear extent, so a *partial* re-range can't default the missing
        // endpoint — it's rejected rather than flattened through `0..len`.
        use seqforge_core::{Location, Span};
        let mut s = state_with(b"ATGCATGCATGC"); // len 12
        let id = add_feat(&mut s, 0, 3, "w");
        let vid = s.workspace.active_view().unwrap().id;
        // Re-home the feature onto an origin-wrapping span (10..12 ∪ 0..2).
        s.workspace
            .edit_annotations(vid, |ann, _buf| {
                ann.get_mut(id).unwrap().location = Location::from_span(Span::new(10, 4));
                Ok::<_, DispatchError>(())
            })
            .unwrap();
        // Only `start` given → no linear default for `end` → rejected.
        let err = sedit::apply_update_feature(
            &mut s.workspace,
            None,
            id,
            None,
            None,
            None,
            Some(5),
            None,
        )
        .unwrap_err();
        assert!(matches!(err, DispatchError::InvalidInput(_)));
        // A label-only edit (no re-range) still works on a wrapping feature.
        sedit::apply_update_feature(
            &mut s.workspace,
            None,
            id,
            Some("gene".into()),
            None,
            None,
            None,
            None,
        )
        .unwrap();
        // Both endpoints given → redefines to a crisp linear region, succeeds.
        sedit::apply_update_feature(
            &mut s.workspace,
            None,
            id,
            None,
            None,
            None,
            Some(2),
            Some(6),
        )
        .unwrap();
        let span = s
            .workspace
            .with_buffer(vid, |_, b, ann| ann.get(id).unwrap().bounds(b.text.len()))
            .unwrap();
        assert_eq!(span, 2..6);
    }

    #[test]
    fn update_feature_resyncs_selection_range() {
        // A geometry edit of the *selected* feature re-syncs the stored selection
        // range (edit_annotations doesn't reset selection) so the wash/copy don't
        // act on a stale span.
        let mut s = state_with(b"ATGCATGCATGC");
        let id = add_feat(&mut s, 0, 3, "sel");
        s.workspace.active_view_mut().unwrap().selection = ViewSelection::Feature {
            id,
            range: Selection::range(0, 3),
        };

        sedit::apply_update_feature(
            &mut s.workspace,
            None,
            id,
            None,
            None,
            None,
            Some(4),
            Some(9),
        )
        .unwrap();

        let sel = &s.workspace.active_view().unwrap().selection;
        assert_eq!(sel.selected_feature(), Some(id));
        assert_eq!(
            sel.text_range(),
            Some(Selection::range(4, 9)),
            "selection range follows the feature's new geometry"
        );
    }

    #[test]
    fn update_feature_leaves_other_selection_untouched() {
        // Editing a feature that is NOT the current selection must not hijack it.
        let mut s = state_with(b"ATGCATGCATGC");
        let id = add_feat(&mut s, 0, 3, "a");
        s.workspace.active_view_mut().unwrap().selection =
            ViewSelection::Text(Selection::range(6, 10));

        sedit::apply_update_feature(
            &mut s.workspace,
            None,
            id,
            None,
            None,
            None,
            Some(4),
            Some(9),
        )
        .unwrap();

        assert_eq!(
            s.workspace.active_view().unwrap().selection.text_range(),
            Some(Selection::range(6, 10)),
            "an unrelated text selection is left intact"
        );
    }

    #[test]
    fn remove_feature_bad_id_errors() {
        let mut s = state_with(b"ATGC");
        let err = sedit::apply_remove_feature(&mut s.workspace, None, FeatureId(999)).unwrap_err();
        assert!(matches!(err, DispatchError::InvalidInput(_)));
    }

    // ── Primer ops (Phase 2.1) ────────────────────────────────────────────────

    fn primer_count(state: &mut AppState) -> usize {
        state
            .workspace
            .with_active_buffer(|_, _, ann| ann.primers_len())
            .unwrap()
    }

    fn add_primer(
        state: &mut AppState,
        name: Option<&str>,
        seq: &str,
        start: Option<usize>,
        end: Option<usize>,
        strand: &str,
    ) -> PrimerId {
        match sedit::apply_add_primer(
            &mut state.workspace,
            None,
            name.map(str::to_string),
            seq.into(),
            start,
            end,
            strand.into(),
        )
        .unwrap()
        {
            Some(ViewerResponse::PrimerAdded { id, .. }) => id,
            other => panic!("expected PrimerAdded, got {other:?}"),
        }
    }

    fn first_primer<T>(state: &mut AppState, f: impl FnOnce(&Primer) -> T) -> T {
        state
            .workspace
            .with_active_buffer(|_, _, ann| f(ann.primers().next().unwrap()))
            .unwrap()
    }

    #[test]
    fn pcr_builds_product_buffer_inheriting_annotations() {
        // Template (30 bp), forward primer at [4,10), reverse primer at [20,26)
        // → amplicon [4,26) = 22 bp, no tails (tail_f_len 0).
        const T: &[u8] = b"AAAACCCCGGGGTTTTAAAACCCCGGGGTT";
        let mut s = state_with(T);

        let fwd_seq = std::str::from_utf8(&T[4..10]).unwrap().to_string();
        let rev_seq = String::from_utf8(seqforge_bio::reverse_complement(&T[20..26])).unwrap();
        let fwd = add_primer(&mut s, Some("F"), &fwd_seq, Some(4), Some(10), "+");
        let rev = add_primer(&mut s, Some("R"), &rev_seq, Some(20), Some(26), "-");
        // A primer straddling the amplicon's 3' edge → detaches on extract → dropped.
        add_primer(
            &mut s,
            Some("straddler-primer"),
            "ACGTAC",
            Some(24),
            Some(30),
            "+",
        );

        add_feat(&mut s, 12, 16, "interior"); // fully inside → carries, shifted
        add_feat(&mut s, 22, 28, "straddle"); // crosses re=26 → truncated + fuzzy
        add_feat(&mut s, 0, 3, "outside"); // fully outside → dropped

        crate::command::file::apply_pcr(&mut s, None, fwd, rev, None).unwrap();

        // The active view is now the product; its bytes are the amplicon.
        assert_eq!(text(&mut s), T[4..26].to_vec());

        let (labels, interior_bounds, straddle, fuzzy_count, primer_count, all_attached) = s
            .workspace
            .with_active_buffer(|_, buf, ann| {
                let len = buf.text.len();
                let labels: Vec<String> = ann.iter().map(|f| f.label.clone()).collect();
                let interior = ann
                    .iter()
                    .find(|f| f.label == "interior")
                    .map(|f| f.bounds(len));
                let straddle = ann
                    .iter()
                    .find(|f| f.label == "straddle")
                    .map(|f| (f.bounds(len), f.location.is_fuzzy()));
                let fuzzy_count = ann.iter().filter(|f| f.location.is_fuzzy()).count();
                let primer_count = ann.primers().count();
                let all_attached = ann.primers().all(|p| p.binding.is_some());
                (
                    labels,
                    interior,
                    straddle,
                    fuzzy_count,
                    primer_count,
                    all_attached,
                )
            })
            .unwrap();

        // Interior feature re-homed by tail_f_len (0) → template [12,16) → [8,12).
        assert_eq!(
            interior_bounds,
            Some(8..12),
            "interior feature shifted onto product"
        );
        // Straddler truncated to the amplicon edge (template [22,26) → [18,22)) + fuzzy.
        assert_eq!(
            straddle,
            Some((18..22, true)),
            "straddler truncated + fuzzy-marked"
        );
        assert_eq!(fuzzy_count, 1, "only the straddler is fuzzy");
        assert!(
            !labels.iter().any(|l| l == "outside"),
            "outside feature dropped"
        );
        // No whole-product marker feature — only inherited annotations carry.
        assert!(
            !labels.iter().any(|l| l.starts_with("PCR product")),
            "no whole-product marker feature: {labels:?}"
        );
        // Only the two in-amplicon primers carry; the straddler primer is dropped.
        assert_eq!(primer_count, 2, "fwd + rev carried, straddler dropped");
        assert!(
            all_attached,
            "carried primers keep their bindings (no floating)"
        );
    }

    #[test]
    fn pcr_detached_primer_errors() {
        const T: &[u8] = b"AAAACCCCGGGGTTTTAAAACCCCGGGGTT";
        let mut s = state_with(T);
        let fwd_seq = std::str::from_utf8(&T[4..10]).unwrap().to_string();
        let rev_seq = String::from_utf8(seqforge_bio::reverse_complement(&T[20..26])).unwrap();
        // Forward primer created floating (no binding) → PCR refuses.
        let fwd = add_primer(&mut s, Some("F"), &fwd_seq, None, None, "+");
        let rev = add_primer(&mut s, Some("R"), &rev_seq, Some(20), Some(26), "-");
        let err = crate::command::file::apply_pcr(&mut s, None, fwd, rev, None).unwrap_err();
        assert!(
            matches!(err, DispatchError::InvalidInput(ref m) if m.contains("attach or rescan")),
            "detached primer errors with an attach/rescan hint: {err:?}"
        );
    }

    #[test]
    fn pcr_carries_inherited_features_without_marker() {
        // The product carries the inherited annotations (each with its own
        // extract-stamped lineage) but *no* hand-rolled whole-product marker
        // feature — product-level provenance is the recipe's job (the composed
        // Lineage map), not a whole-span feature. See docs/architecture.md.
        const T: &[u8] = b"AAAACCCCGGGGTTTTAAAACCCCGGGGTT";
        let mut s = state_with(T);
        let fwd_seq = std::str::from_utf8(&T[4..10]).unwrap().to_string();
        let rev_seq = String::from_utf8(seqforge_bio::reverse_complement(&T[20..26])).unwrap();
        let fwd = add_primer(&mut s, Some("F"), &fwd_seq, Some(4), Some(10), "+");
        let rev = add_primer(&mut s, Some("R"), &rev_seq, Some(20), Some(26), "-");
        add_feat(&mut s, 12, 16, "interior");

        crate::command::file::apply_pcr(&mut s, None, fwd, rev, None).unwrap();

        let labels: Vec<String> = s
            .workspace
            .with_active_buffer(|_, _, ann| ann.iter().map(|f| f.label.clone()).collect())
            .unwrap();
        assert!(
            labels.iter().any(|l| l == "interior"),
            "inherited feature still carries: {labels:?}"
        );
        assert!(
            !labels.iter().any(|l| l.starts_with("PCR product")),
            "no whole-product marker feature: {labels:?}"
        );
    }

    #[test]
    fn add_primer_attached_and_floating() {
        let mut s = state_with(b"ATGCATGCATGC");
        let attached = add_primer(&mut s, Some("fwd"), "atg c", Some(0), Some(4), "+");
        // Sequence normalized (uppercased, whitespace stripped).
        let (name, seq, binding, strand) = first_primer(&mut s, |p| {
            (
                p.name.clone(),
                p.sequence.clone(),
                p.binding.map(|b| b.start..b.start + b.len),
                p.strand,
            )
        });
        assert_eq!(name, "fwd");
        assert_eq!(seq, "ATGC");
        assert_eq!(binding, Some(0..4));
        assert_eq!(strand, Strand::Forward);
        assert_ne!(attached, PrimerId::default());

        // A floating oligo: no start/end → binding None.
        add_primer(&mut s, Some("float"), "GGGG", None, None, "-");
        assert_eq!(primer_count(&mut s), 2);
    }

    #[test]
    fn add_primer_missing_name_uses_suggested_default() {
        let mut s = state_with(b"ATGCATGC");
        // Empty + absent both fall back to the shared generator (decision 9).
        add_primer(&mut s, None, "ATGC", Some(0), Some(4), "+");
        add_primer(&mut s, Some("  "), "TTTT", None, None, "+");
        let names: Vec<String> = s
            .workspace
            .with_active_buffer(|_, _, ann| ann.primers().map(|p| p.name.clone()).collect())
            .unwrap();
        assert_eq!(names, vec!["Primer 1".to_string(), "Primer 2".to_string()]);
    }

    #[test]
    fn add_primer_partial_binding_and_bad_range_error() {
        let mut s = state_with(b"ATGC");
        // Exactly one of start/end is invalid.
        assert!(matches!(
            sedit::apply_add_primer(
                &mut s.workspace,
                None,
                None,
                "ATGC".into(),
                Some(0),
                None,
                "+".into()
            )
            .unwrap_err(),
            DispatchError::InvalidInput(_)
        ));
        // Binding past the end.
        assert!(matches!(
            sedit::apply_add_primer(
                &mut s.workspace,
                None,
                None,
                "ATGC".into(),
                Some(0),
                Some(99),
                "+".into()
            )
            .unwrap_err(),
            DispatchError::OutOfRange { .. }
        ));
        // Empty sequence.
        assert!(matches!(
            sedit::apply_add_primer(
                &mut s.workspace,
                None,
                None,
                "".into(),
                None,
                None,
                "+".into()
            )
            .unwrap_err(),
            DispatchError::InvalidInput(_)
        ));
    }

    #[test]
    fn update_primer_partial_and_undoable() {
        let mut s = state_with(b"ATGCATGCATGC");
        let vid = s.workspace.active_view().unwrap().id;
        let id = add_primer(&mut s, Some("orig"), "ATGC", Some(0), Some(4), "+");

        // Change only the binding end + strand; name/sequence untouched.
        sedit::apply_update_primer(
            &mut s.workspace,
            None,
            id,
            None,
            None,
            Some("-".into()),
            None,
            Some(8),
            false,
        )
        .unwrap();
        let (name, binding, strand) = first_primer(&mut s, |p| {
            (
                p.name.clone(),
                p.binding.map(|b| b.start..b.start + b.len),
                p.strand,
            )
        });
        assert_eq!(name, "orig", "unspecified fields preserved");
        assert_eq!(binding, Some(0..8), "end updated, start kept");
        assert_eq!(strand, Strand::Reverse);

        // Undo restores the original binding + strand.
        s.workspace.undo(vid).unwrap();
        let (binding, strand) = first_primer(&mut s, |p| {
            (p.binding.map(|b| b.start..b.start + b.len), p.strand)
        });
        assert_eq!(binding, Some(0..4));
        assert_eq!(strand, Strand::Forward);
    }

    #[test]
    fn update_primer_empty_name_is_ignored() {
        let mut s = state_with(b"ATGCATGC");
        let id = add_primer(&mut s, Some("keep"), "ATGC", Some(0), Some(4), "+");
        sedit::apply_update_primer(
            &mut s.workspace,
            None,
            id,
            Some("  ".into()),
            None,
            None,
            None,
            None,
            false,
        )
        .unwrap();
        assert_eq!(first_primer(&mut s, |p| p.name.clone()), "keep");
    }

    #[test]
    fn update_primer_empty_sequence_is_rejected() {
        // Parity with add: an update can't blank the oligo (parse_oligo guards both).
        let mut s = state_with(b"ATGCATGC");
        let id = add_primer(&mut s, Some("keep"), "ATGC", Some(0), Some(4), "+");
        assert!(matches!(
            sedit::apply_update_primer(
                &mut s.workspace,
                None,
                id,
                None,
                Some("".into()),
                None,
                None,
                None,
                false
            )
            .unwrap_err(),
            DispatchError::InvalidInput(_)
        ));
        // The original oligo is untouched by the rejected edit.
        assert_eq!(first_primer(&mut s, |p| p.sequence.clone()), "ATGC");
    }

    #[test]
    fn update_primer_detach_clears_binding_and_is_undoable() {
        let mut s = state_with(b"ATGCATGCATGC");
        let vid = s.workspace.active_view().unwrap().id;
        let id = add_primer(&mut s, Some("p"), "ATGC", Some(0), Some(4), "+");
        sedit::apply_update_primer(
            &mut s.workspace,
            None,
            id,
            None,
            None,
            None,
            None,
            None,
            true,
        )
        .unwrap();
        assert_eq!(
            first_primer(&mut s, |p| p.binding.map(|b| b.start..b.start + b.len)),
            None,
            "detach clears the footprint → floating oligo"
        );
        s.workspace.undo(vid).unwrap();
        assert_eq!(
            first_primer(&mut s, |p| p.binding.map(|b| b.start..b.start + b.len)),
            Some(0..4),
            "undo restores the binding"
        );
    }

    #[test]
    fn update_primer_detach_with_explicit_range_is_rejected() {
        let mut s = state_with(b"ATGCATGC");
        let id = add_primer(&mut s, Some("p"), "ATGC", Some(0), Some(4), "+");
        assert!(matches!(
            sedit::apply_update_primer(
                &mut s.workspace,
                None,
                id,
                None,
                None,
                None,
                Some(0),
                Some(4),
                true
            )
            .unwrap_err(),
            DispatchError::InvalidInput(_)
        ));
    }

    #[test]
    fn rescan_primer_reanchors_detached_oligo() {
        // Oligo GCGTAC binds the clean forward site 2..8 of this template.
        let mut s = state_with(b"ATGCGTACCA");
        let id = add_primer(&mut s, Some("p"), "GCGTAC", None, None, "+");
        assert_eq!(
            first_primer(&mut s, |p| p.binding.map(|b| b.start..b.start + b.len)),
            None,
            "starts floating"
        );
        sedit::apply_rescan_primer(&mut s.workspace, None, id).unwrap();
        let (binding, strand) = first_primer(&mut s, |p| {
            (p.binding.map(|b| b.start..b.start + b.len), p.strand)
        });
        assert_eq!(binding, Some(2..8), "re-anchored to the forward site");
        assert_eq!(strand, Strand::Forward);
    }

    #[test]
    fn rescan_primer_that_binds_nowhere_errors_without_mutation() {
        let mut s = state_with(b"AAAAAAAAAA");
        let id = add_primer(&mut s, Some("p"), "GCGTAC", None, None, "+");
        assert!(matches!(
            sedit::apply_rescan_primer(&mut s.workspace, None, id).unwrap_err(),
            DispatchError::InvalidInput(_)
        ));
        assert_eq!(
            first_primer(&mut s, |p| p.binding.map(|b| b.start..b.start + b.len)),
            None,
            "failed rescan leaves the primer untouched"
        );
    }

    #[test]
    fn add_primer_site_prepends_tail_keeps_binding_and_is_undoable() {
        let mut s = state_with(b"ATGCGTACCATGCGTAC");
        let vid = s.workspace.active_view().unwrap().id;
        let id = add_primer(&mut s, Some("p"), "GCGTAC", Some(2), Some(8), "+");
        // BsaI (Type IIs) with a 4-nt overhang, empty flank for a deterministic tail.
        sedit::apply_add_primer_site(
            &mut s.workspace,
            None,
            id,
            "BsaI".into(),
            Some("AATG".into()),
            Some("".into()),
        )
        .unwrap();
        let (seq, binding) = first_primer(&mut s, |p| {
            (
                p.sequence.clone(),
                p.binding.map(|b| b.start..b.start + b.len),
            )
        });
        assert_eq!(seq, "GGTCTCAAATGGCGTAC", "tail prepended to the oligo");
        assert_eq!(
            binding,
            Some(2..8),
            "binding footprint unchanged (tail is 5')"
        );
        s.workspace.undo(vid).unwrap();
        assert_eq!(first_primer(&mut s, |p| p.sequence.clone()), "GCGTAC");
    }

    #[test]
    fn add_primer_site_surfaces_builder_errors() {
        let mut s = state_with(b"ATGCATGC");
        let id = add_primer(&mut s, Some("p"), "ATGC", Some(0), Some(4), "+");
        // Wrong overhang length for BsaI (expects 4).
        assert!(matches!(
            sedit::apply_add_primer_site(
                &mut s.workspace,
                None,
                id,
                "BsaI".into(),
                Some("AA".into()),
                None
            )
            .unwrap_err(),
            DispatchError::InvalidInput(_)
        ));
        assert_eq!(
            first_primer(&mut s, |p| p.sequence.clone()),
            "ATGC",
            "failed compose leaves the oligo untouched"
        );
    }

    #[test]
    fn remove_primer_and_bad_id() {
        let mut s = state_with(b"ATGCATGC");
        let id = add_primer(&mut s, Some("p"), "ATGC", Some(0), Some(4), "+");
        assert_eq!(primer_count(&mut s), 1);
        sedit::apply_remove_primer(&mut s.workspace, None, id).unwrap();
        assert_eq!(primer_count(&mut s), 0);

        assert!(matches!(
            sedit::apply_remove_primer(&mut s.workspace, None, PrimerId(999)).unwrap_err(),
            DispatchError::InvalidInput(_)
        ));
    }

    #[test]
    fn primer_ops_bump_version() {
        let mut s = state_with(b"ATGCATGC");
        let v0 = s
            .workspace
            .with_active_buffer(|_, buf, _| buf.version)
            .unwrap();
        add_primer(&mut s, Some("p"), "ATGC", Some(0), Some(4), "+");
        let v1 = s
            .workspace
            .with_active_buffer(|_, buf, _| buf.version)
            .unwrap();
        assert_eq!(v1, v0 + 1, "primer add must bump version (cache key)");
    }

    #[test]
    fn explicit_view_target_resolves() {
        let mut s = state_with(b"ATGC");
        let vid = s.workspace.active_view().unwrap().id;
        sedit::apply_insert(&mut s.workspace, Some(vid), 0, "G".into()).unwrap();
        assert_eq!(text(&mut s), b"GATGC");
    }

    #[test]
    fn closed_view_target_errors() {
        let mut s = state_with(b"ATGC");
        let bogus = ViewId(9999);
        let err = sedit::apply_insert(&mut s.workspace, Some(bogus), 0, "G".into()).unwrap_err();
        assert!(matches!(err, DispatchError::ViewNotFound(_)));
    }

    /// Phase 12f refinement D: menu/keymap greying follows live state.
    #[test]
    fn menu_enablement_tracks_state() {
        use crate::command::is_enabled;
        use seqforge_core::{Selection, ViewerRequest};

        let mut s = state_with(b"ATGC");
        let undo = AppCommand::Viewer(ViewerRequest::Undo { view: None });
        let save = AppCommand::Viewer(ViewerRequest::Save {
            force: false,
            view: None,
        });
        let paste = AppCommand::Viewer(ViewerRequest::Paste { pos: 0, view: None });
        let cut = AppCommand::Viewer(ViewerRequest::Cut {
            start: 0,
            end: 0,
            view: None,
        });

        // Fresh buffer: no history, not dirty, empty clipboard, no range.
        assert!(!is_enabled(&undo, &s), "nothing to undo yet");
        assert!(!is_enabled(&save, &s), "not dirty yet");
        assert!(!is_enabled(&paste, &s), "clipboard empty");
        assert!(!is_enabled(&cut, &s), "no range selection");

        // After an edit: undo available + buffer dirty → save available.
        sedit::apply_insert(&mut s.workspace, None, 0, "G".into()).unwrap();
        assert!(is_enabled(&undo, &s));
        assert!(is_enabled(&save, &s));

        // Clipboard populated → paste available.
        s.clipboard.slice = Some(clip(b"AA"));
        assert!(is_enabled(&paste, &s));

        // Range selection → cut available; a bare cursor does not enable it.
        s.workspace.active_view_mut().unwrap().selection =
            ViewSelection::Text(Selection::cursor(1));
        assert!(!is_enabled(&cut, &s), "cursor is not a range");
        s.workspace.active_view_mut().unwrap().selection =
            ViewSelection::Text(Selection::range(0, 2));
        assert!(is_enabled(&cut, &s));
    }
}
