//! Editor write-ops: the v0.2 mutation command layer.
//!
//! Every editor action (insert/delete/replace/RC/cut/copy/paste, feature
//! add/remove/rename, undo/redo, save) arrives here as a `ViewerRequest`
//! variant — from the GUI, the embedded terminal, or an external agent over
//! the socket — and is lowered onto the **Phase 11 write path**
//! (`workspace.edit/undo/redo`, which records the reverse delta + annotation
//! snapshot, bumps `version`, sets `dirty`, and moves the cursor).
//!
//! ## Primitive vs composed (see `docs/architecture.md` "Edit operations")
//!
//! - **Content-given** edits (insert/delete/replace) carry their own bytes and
//!   go straight to `workspace.edit`.
//! - **Composed** edits derive their bytes via `seqforge-bio` first, then splice.
//!   `reverse_complement` is the first of these; cloning/mutagenesis follow the
//!   same shape. Byte-derivation lives here (the app layer), never in `core`,
//!   so `core` never gains a `bio` dependency.
//!
//! Feature ops (add/remove/rename) mutate `Annotations` and bump `buf.version`
//! (the cache-invalidation contract) but are **not** yet undoable — feature-op
//! history is Phase 14. Copy is read-only and likewise records no history.

use std::ops::Range;

use seqforge_core::{DispatchError, EditKind, Feature, Strand, ViewId, ViewerResponse};

use crate::app::AppState;
use crate::command::AppCommand;

// ── Shared helpers ────────────────────────────────────────────────────────────

/// Resolve a request's optional `view` target to a concrete `ViewId`:
/// the explicit target if present (erroring if it has been closed), else the
/// active view. Mirrors `dispatch_active`'s rule for the write path. Shared
/// with `file.rs` (the save handlers).
pub(super) fn resolve_target(
    state: &AppState,
    view: Option<ViewId>,
) -> Result<ViewId, DispatchError> {
    match view {
        Some(vid) => {
            if state.workspace.view(vid).is_some() {
                Ok(vid)
            } else {
                Err(DispatchError::ViewNotFound(vid))
            }
        }
        None => state
            .workspace
            .active_view()
            .map(|v| v.id)
            .ok_or(DispatchError::NoActiveView),
    }
}

/// IUPAC nucleotide alphabet (DNA + ambiguity codes). Validation is the command
/// layer's job so a malformed CLI/agent insert is rejected with a clear error;
/// the GUI keystroke path (Phase 13) pre-filters before it ever gets here.
const IUPAC: &[u8] = b"ACGTURYSWKMBDHVN";

/// Uppercase, strip ASCII whitespace, validate IUPAC. Returns the clean bytes
/// or `InvalidInput` naming the first offending character.
fn parse_bases(s: &str) -> Result<Vec<u8>, DispatchError> {
    let mut out = Vec::with_capacity(s.len());
    for ch in s.chars() {
        if ch.is_ascii_whitespace() {
            continue;
        }
        let up = ch.to_ascii_uppercase();
        if up.is_ascii() && IUPAC.contains(&(up as u8)) {
            out.push(up as u8);
        } else {
            return Err(DispatchError::InvalidInput(format!(
                "`{ch}` is not an IUPAC nucleotide code"
            )));
        }
    }
    Ok(out)
}

/// Read `[start, end)` from the target buffer, validating the range. Used by the
/// composed edits (RC, cut, copy) that need the old bytes before splicing.
fn read_slice(
    state: &mut AppState,
    vid: ViewId,
    range: Range<usize>,
) -> Result<Vec<u8>, DispatchError> {
    state.workspace.with_buffer(vid, |_, buf, _| {
        buf.text
            .get(range.clone())
            .map(<[u8]>::to_vec)
            .ok_or(DispatchError::OutOfRange {
                position: range.end,
                seq_len: buf.text.len(),
            })
    })?
}

/// The buffer length after an edit, for the `Edited` response.
fn buffer_len(state: &mut AppState, vid: ViewId) -> usize {
    state
        .workspace
        .with_buffer(vid, |_, buf, _| buf.text.len())
        .unwrap_or(0)
}

fn edited(len: usize) -> Result<Option<ViewerResponse>, DispatchError> {
    Ok(Some(ViewerResponse::Edited { len, changed: true }))
}

// ── Content-given edits (12b) ─────────────────────────────────────────────────

pub(super) fn apply_insert(
    state: &mut AppState,
    view: Option<ViewId>,
    pos: usize,
    bases: String,
) -> Result<Option<ViewerResponse>, DispatchError> {
    let vid = resolve_target(state, view)?;
    let bytes = parse_bases(&bases)?;
    state
        .workspace
        .edit(vid, EditKind::Insert, pos..pos, &bytes)?;
    edited(buffer_len(state, vid))
}

pub(super) fn apply_delete(
    state: &mut AppState,
    view: Option<ViewId>,
    start: usize,
    end: usize,
) -> Result<Option<ViewerResponse>, DispatchError> {
    let vid = resolve_target(state, view)?;
    state
        .workspace
        .edit(vid, EditKind::Delete, start..end, &[])?;
    edited(buffer_len(state, vid))
}

pub(super) fn apply_replace(
    state: &mut AppState,
    view: Option<ViewId>,
    start: usize,
    end: usize,
    bases: String,
) -> Result<Option<ViewerResponse>, DispatchError> {
    let vid = resolve_target(state, view)?;
    let bytes = parse_bases(&bases)?;
    state
        .workspace
        .edit(vid, EditKind::Other, start..end, &bytes)?;
    edited(buffer_len(state, vid))
}

// ── Composed edit: reverse-complement (12c) ────────────────────────────────────

pub(super) fn apply_reverse_complement(
    state: &mut AppState,
    view: Option<ViewId>,
    start: usize,
    end: usize,
) -> Result<Option<ViewerResponse>, DispatchError> {
    let vid = resolve_target(state, view)?;
    let slice = read_slice(state, vid, start..end)?;
    // Bytes derived by bio, then installed via the same splice path — the
    // primitive-vs-composed split that the cloning roadmap rides.
    let rc = seqforge_bio::reverse_complement(&slice);
    state
        .workspace
        .edit(vid, EditKind::Other, start..end, &rc)?;
    edited(buffer_len(state, vid))
}

// ── Clipboard (12c) ─────────────────────────────────────────────────────────--

pub(super) fn apply_copy(
    state: &mut AppState,
    view: Option<ViewId>,
    start: usize,
    end: usize,
) -> Result<Option<ViewerResponse>, DispatchError> {
    let vid = resolve_target(state, view)?;
    let slice = read_slice(state, vid, start..end)?;
    let len = slice.len();
    state.clipboard = Some(slice);
    // Copy doesn't mutate the buffer — report the copied length, not a buffer
    // change, and record no history.
    Ok(Some(ViewerResponse::Edited {
        len,
        changed: false,
    }))
}

pub(super) fn apply_cut(
    state: &mut AppState,
    view: Option<ViewId>,
    start: usize,
    end: usize,
) -> Result<Option<ViewerResponse>, DispatchError> {
    let vid = resolve_target(state, view)?;
    let slice = read_slice(state, vid, start..end)?;
    state.clipboard = Some(slice);
    state
        .workspace
        .edit(vid, EditKind::Delete, start..end, &[])?;
    edited(buffer_len(state, vid))
}

pub(super) fn apply_paste(
    state: &mut AppState,
    view: Option<ViewId>,
    pos: usize,
) -> Result<Option<ViewerResponse>, DispatchError> {
    let vid = resolve_target(state, view)?;
    let bytes = state
        .clipboard
        .clone()
        .ok_or_else(|| DispatchError::InvalidInput("clipboard is empty".into()))?;
    if bytes.is_empty() {
        return Err(DispatchError::InvalidInput("clipboard is empty".into()));
    }
    // A paste is its own undo unit (`Other`) — never coalesces with typing.
    state
        .workspace
        .edit(vid, EditKind::Other, pos..pos, &bytes)?;
    edited(buffer_len(state, vid))
}

// ── History ops (12b) ─────────────────────────────────────────────────────────

pub(super) fn apply_undo(
    state: &mut AppState,
    view: Option<ViewId>,
) -> Result<Option<ViewerResponse>, DispatchError> {
    let vid = resolve_target(state, view)?;
    let changed = state.workspace.undo(vid)?;
    Ok(Some(ViewerResponse::Edited {
        len: buffer_len(state, vid),
        changed,
    }))
}

pub(super) fn apply_redo(
    state: &mut AppState,
    view: Option<ViewId>,
) -> Result<Option<ViewerResponse>, DispatchError> {
    let vid = resolve_target(state, view)?;
    let changed = state.workspace.redo(vid)?;
    Ok(Some(ViewerResponse::Edited {
        len: buffer_len(state, vid),
        changed,
    }))
}

// ── Feature ops (12d) ──────────────────────────────────────────────────────────
//
// Annotation-only mutations: they bump `buf.version` (the cache-invalidation
// contract — see editor.md Phase 14) but record no undo history yet.

fn parse_strand(s: &str) -> Strand {
    match s.trim() {
        "-" | "reverse" | "Reverse" => Strand::Reverse,
        "." | "none" | "None" => Strand::None,
        "both" | "Both" => Strand::Both,
        _ => Strand::Forward,
    }
}

pub(super) fn apply_add_feature(
    state: &mut AppState,
    view: Option<ViewId>,
    start: usize,
    end: usize,
    kind: String,
    label: String,
    strand: String,
) -> Result<Option<ViewerResponse>, DispatchError> {
    let vid = resolve_target(state, view)?;
    state.workspace.with_buffer_mut(vid, |_, buf, ann| {
        if start >= end || end > buf.text.len() {
            return Err(DispatchError::OutOfRange {
                position: end,
                seq_len: buf.text.len(),
            });
        }
        ann.features.push(Feature {
            range: start..end,
            raw_kind: kind,
            label,
            strand: parse_strand(&strand),
            qualifiers: Default::default(),
            provenance: None,
        });
        buf.version += 1;
        Ok(())
    })??;
    Ok(Some(ViewerResponse::Ok))
}

pub(super) fn apply_remove_feature(
    state: &mut AppState,
    view: Option<ViewId>,
    index: usize,
) -> Result<Option<ViewerResponse>, DispatchError> {
    let vid = resolve_target(state, view)?;
    state.workspace.with_buffer_mut(vid, |_, buf, ann| {
        if index >= ann.features.len() {
            return Err(DispatchError::InvalidInput(format!(
                "no feature at index {index} (have {})",
                ann.features.len()
            )));
        }
        ann.features.remove(index);
        buf.version += 1;
        Ok(())
    })??;
    Ok(Some(ViewerResponse::Ok))
}

pub(super) fn apply_rename_feature(
    state: &mut AppState,
    view: Option<ViewId>,
    index: usize,
    label: String,
) -> Result<Option<ViewerResponse>, DispatchError> {
    let vid = resolve_target(state, view)?;
    state
        .workspace
        .with_buffer_mut(vid, |_, buf, ann| match ann.features.get_mut(index) {
            Some(f) => {
                f.label = label;
                buf.version += 1;
                Ok(())
            }
            None => Err(DispatchError::InvalidInput(format!(
                "no feature at index {index} (have {})",
                ann.features.len()
            ))),
        })??;
    Ok(Some(ViewerResponse::Ok))
}

// ── Save (12e) ─────────────────────────────────────────────────────────────────

/// Save the target buffer. If it has a source path, save synchronously (so a
/// CLI/agent `save` gets immediate success/failure). Otherwise fall back to the
/// GUI Save-As dialog. `SaveAs` (below) is the dialog-driven path.
pub(super) fn apply_save(
    state: &mut AppState,
    view: Option<ViewId>,
) -> Result<Option<ViewerResponse>, DispatchError> {
    let vid = resolve_target(state, view)?;
    let path = state
        .workspace
        .with_buffer(vid, |_, buf, _| buf.source_path.clone())?;
    match path {
        Some(path) => {
            super::file::save_buffer(state, vid, &path).map(|()| Some(ViewerResponse::Ok))
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
    let vid = resolve_target(state, view)?;
    state
        .pending_commands
        .push((AppCommand::OpenSaveAs { view: Some(vid) }, None));
    Ok(None)
}

// ── Tests ──────────────────────────────────────────────────────────────────--

#[cfg(test)]
mod tests {
    use super::*;
    use seqforge_core::{Topology, ViewKind};

    /// Headless `AppState` with one active view over `seq`.
    fn state_with(seq: &[u8]) -> AppState {
        let mut state = AppState::default();
        let bid = state
            .workspace
            .buffers
            .insert_raw("test".into(), seq.to_vec(), Topology::Linear);
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
            .with_active_buffer(|_, _, ann| ann.features.len())
            .unwrap()
    }

    #[test]
    fn insert_lowers_to_splice() {
        let mut s = state_with(b"ATGC");
        let resp = apply_insert(&mut s, None, 2, "TT".into()).unwrap();
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
        apply_delete(&mut s, None, 1, 4).unwrap();
        assert_eq!(text(&mut s), b"AAA");

        let undo = apply_undo(&mut s, None).unwrap();
        assert!(matches!(
            undo,
            Some(ViewerResponse::Edited { changed: true, .. })
        ));
        assert_eq!(text(&mut s), b"ATGCAA");

        apply_redo(&mut s, None).unwrap();
        assert_eq!(text(&mut s), b"AAA");
    }

    #[test]
    fn undo_with_empty_history_reports_unchanged() {
        let mut s = state_with(b"ATGC");
        let resp = apply_undo(&mut s, None).unwrap();
        assert!(matches!(
            resp,
            Some(ViewerResponse::Edited { changed: false, .. })
        ));
    }

    #[test]
    fn replace_swaps_region() {
        let mut s = state_with(b"AAGGCC");
        apply_replace(&mut s, None, 2, 4, "TT".into()).unwrap();
        assert_eq!(text(&mut s), b"AATTCC");
    }

    #[test]
    fn reverse_complement_composes_bio_then_splice() {
        let mut s = state_with(b"AAATGCCC");
        // bytes 1..5 are "AATG"; reverse-complement is "CATT".
        apply_reverse_complement(&mut s, None, 1, 5).unwrap();
        assert_eq!(text(&mut s), b"ACATTCCC");
    }

    #[test]
    fn cut_copies_to_clipboard_and_deletes() {
        let mut s = state_with(b"ATGCAA");
        apply_cut(&mut s, None, 2, 4).unwrap();
        assert_eq!(text(&mut s), b"ATAA");
        assert_eq!(s.clipboard.as_deref(), Some(b"GC".as_slice()));
    }

    #[test]
    fn copy_leaves_buffer_unchanged() {
        let mut s = state_with(b"ATGC");
        let resp = apply_copy(&mut s, None, 0, 2).unwrap();
        assert_eq!(text(&mut s), b"ATGC");
        assert_eq!(s.clipboard.as_deref(), Some(b"AT".as_slice()));
        assert!(matches!(
            resp,
            Some(ViewerResponse::Edited { changed: false, .. })
        ));
    }

    #[test]
    fn paste_inserts_clipboard() {
        let mut s = state_with(b"ATGC");
        s.clipboard = Some(b"NN".to_vec());
        apply_paste(&mut s, None, 4).unwrap();
        assert_eq!(text(&mut s), b"ATGCNN");
    }

    #[test]
    fn paste_empty_clipboard_errors() {
        let mut s = state_with(b"ATGC");
        let err = apply_paste(&mut s, None, 0).unwrap_err();
        assert!(matches!(err, DispatchError::InvalidInput(_)));
    }

    #[test]
    fn insert_rejects_non_iupac() {
        let mut s = state_with(b"ATGC");
        let err = apply_insert(&mut s, None, 0, "ATZ".into()).unwrap_err();
        assert!(matches!(err, DispatchError::InvalidInput(_)));
        assert_eq!(text(&mut s), b"ATGC", "rejected insert must not mutate");
    }

    #[test]
    fn insert_strips_whitespace() {
        let mut s = state_with(b"ATGC");
        apply_insert(&mut s, None, 0, "a t g".into()).unwrap();
        assert_eq!(text(&mut s), b"ATGATGC");
    }

    #[test]
    fn add_remove_rename_feature() {
        let mut s = state_with(b"ATGCATGC");
        apply_add_feature(&mut s, None, 0, 3, "CDS".into(), "gene1".into(), "+".into()).unwrap();
        assert_eq!(feature_count(&mut s), 1);

        apply_rename_feature(&mut s, None, 0, "renamed".into()).unwrap();
        let label = s
            .workspace
            .with_active_buffer(|_, _, ann| ann.features[0].label.clone())
            .unwrap();
        assert_eq!(label, "renamed");

        apply_remove_feature(&mut s, None, 0).unwrap();
        assert_eq!(feature_count(&mut s), 0);
    }

    #[test]
    fn add_feature_bumps_version() {
        let mut s = state_with(b"ATGCATGC");
        let v0 = s
            .workspace
            .with_active_buffer(|_, buf, _| buf.version)
            .unwrap();
        apply_add_feature(&mut s, None, 0, 3, "CDS".into(), "g".into(), "+".into()).unwrap();
        let v1 = s
            .workspace
            .with_active_buffer(|_, buf, _| buf.version)
            .unwrap();
        assert_eq!(v1, v0 + 1, "annotation edits must bump version (cache key)");
    }

    #[test]
    fn add_feature_out_of_range_errors() {
        let mut s = state_with(b"ATGC");
        let err = apply_add_feature(&mut s, None, 2, 99, "CDS".into(), "g".into(), "+".into())
            .unwrap_err();
        assert!(matches!(err, DispatchError::OutOfRange { .. }));
    }

    #[test]
    fn remove_feature_bad_index_errors() {
        let mut s = state_with(b"ATGC");
        let err = apply_remove_feature(&mut s, None, 5).unwrap_err();
        assert!(matches!(err, DispatchError::InvalidInput(_)));
    }

    #[test]
    fn explicit_view_target_resolves() {
        let mut s = state_with(b"ATGC");
        let vid = s.workspace.active_view().unwrap().id;
        apply_insert(&mut s, Some(vid), 0, "G".into()).unwrap();
        assert_eq!(text(&mut s), b"GATGC");
    }

    #[test]
    fn closed_view_target_errors() {
        let mut s = state_with(b"ATGC");
        let bogus = ViewId(9999);
        let err = apply_insert(&mut s, Some(bogus), 0, "G".into()).unwrap_err();
        assert!(matches!(err, DispatchError::ViewNotFound(_)));
    }

    /// Phase 12f refinement D: menu/keymap greying follows live state.
    #[test]
    fn menu_enablement_tracks_state() {
        use crate::command::is_enabled;
        use seqforge_core::{Selection, ViewerRequest};

        let mut s = state_with(b"ATGC");
        let undo = AppCommand::Viewer(ViewerRequest::Undo { view: None });
        let save = AppCommand::Viewer(ViewerRequest::Save { view: None });
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
        apply_insert(&mut s, None, 0, "G".into()).unwrap();
        assert!(is_enabled(&undo, &s));
        assert!(is_enabled(&save, &s));

        // Clipboard populated → paste available.
        s.clipboard = Some(b"AA".to_vec());
        assert!(is_enabled(&paste, &s));

        // Range selection → cut available; a bare cursor does not enable it.
        s.workspace.active_view_mut().unwrap().selection = Some(Selection::cursor(1));
        assert!(!is_enabled(&cut, &s), "cursor is not a range");
        s.workspace.active_view_mut().unwrap().selection = Some(Selection::range(0, 2));
        assert!(is_enabled(&cut, &s));
    }
}
