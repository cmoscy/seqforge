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
//! Feature ops (add/remove/rename) mutate `Annotations` through the id-only
//! API and are undoable via `workspace.edit_annotations` (empty splice delta +
//! annotation snapshot; Phase 14). Copy is read-only and records no history.

use std::ops::Range;

use seqforge_core::{
    DispatchError, EditKind, Feature, FeatureId, Primer, PrimerId, Strand, ViewId, ViewerResponse,
};

use crate::app::AppState;
use crate::command::{AppCommand, StagedEdit};
use crate::focus::FocusScope;

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

/// Arm a staged, destructive edit on the active view's canvas (the menu path
/// for Cut/Delete/Paste). This does **not** mutate the buffer — it sets the
/// same `PendingEdit` an in-canvas keystroke would, so the menu previews before
/// commit. Commit (`Enter`) then rides the identical keyboard path
/// (`PendingEdit::to_request` → one `ViewerRequest` → `apply_splice`).
///
/// Focusing the view is essential: staging is gated on pane focus and losing
/// focus *clears* `pending`, so without this a menu-armed stage would vanish
/// the next frame (the menu may have been opened from another pane).
pub(super) fn apply_stage_edit(
    state: &mut AppState,
    edit: StagedEdit,
) -> Result<Option<ViewerResponse>, DispatchError> {
    let vid = state
        .workspace
        .active_view()
        .map(|v| v.id)
        .ok_or(DispatchError::NoActiveView)?;
    // Focus the target pane so the stage survives + Enter reaches it.
    state.workspace.focus_view(vid);
    state.focus.set_scope(FocusScope::View(vid));
    if let Some(sv) = state.workspace.seq_views.get_mut(&vid) {
        match edit {
            StagedEdit::Cut { start, end } => sv.stage_cut(start, end),
            StagedEdit::Delete { start, end } => sv.stage_delete(start, end),
            StagedEdit::Paste { pos } => sv.stage_paste(pos),
        }
    }
    Ok(None)
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
    // Object-aware copy (decision 15 / Phase 1.5c + 1.5e): copy the authored oligo
    // (5'→3', tail included) — the reagent — instead of the template slice when the
    // copy targets a selected primer. The template slice is the *wrong strand* for
    // a reverse primer and can't represent a 5' tail (which has no template
    // column). Two triggers: a **bare cursor** (`start == end`) is the canvas ⌘C /
    // menu Copy of a selected primer, which post-1.5e carries no template range;
    // an explicit **footprint range** (`range == binding`) keeps the 1.5c path so
    // an off-footprint range copy (CLI/agent) still yields a literal slice — parity
    // holds. The object-vs-range invariant means `selected_primer` is only set when
    // there's no conflicting text selection.
    let oligo = state
        .workspace
        .with_buffer(vid, |v, _buf, ann| {
            let id = v.selected_primer?;
            let p = ann.primer(id)?;
            let is_footprint = p
                .binding
                .as_ref()
                .is_some_and(|b| b.start == start && b.end == end);
            (start == end || is_footprint).then(|| p.sequence.clone().into_bytes())
        })
        .ok()
        .flatten();

    let slice = match oligo {
        Some(o) => o,
        None => read_slice(state, vid, start..end)?,
    };
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

// ── Feature ops (Phase 14) ──────────────────────────────────────────────────────
//
// Annotation-only mutations routed through `workspace.edit_annotations`, which
// records an undoable history entry (empty splice delta + annotation snapshot)
// and bumps `buf.version` (the cache-invalidation contract). Features are
// addressed by `FeatureId` — never by a positional index (ROADMAP decision 12).

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
    let (id, len) = state.workspace.edit_annotations(vid, |ann, buf| {
        if start >= end || end > buf.text.len() {
            return Err(DispatchError::OutOfRange {
                position: end,
                seq_len: buf.text.len(),
            });
        }
        let id = ann.add(Feature {
            id: Default::default(), // reassigned by `add`
            range: start..end,
            raw_kind: kind,
            label,
            strand: parse_strand(&strand),
            qualifiers: Default::default(),
            provenance: None,
        });
        Ok((id, buf.text.len()))
    })?;
    Ok(Some(ViewerResponse::FeatureAdded { id, len }))
}

pub(super) fn apply_remove_feature(
    state: &mut AppState,
    view: Option<ViewId>,
    id: FeatureId,
) -> Result<Option<ViewerResponse>, DispatchError> {
    let vid = resolve_target(state, view)?;
    let len = state.workspace.edit_annotations(vid, |ann, buf| {
        if ann.remove(id) {
            Ok(buf.text.len())
        } else {
            Err(DispatchError::InvalidInput(format!(
                "no feature with id {id}"
            )))
        }
    })?;
    Ok(Some(ViewerResponse::Edited { len, changed: true }))
}

pub(super) fn apply_rename_feature(
    state: &mut AppState,
    view: Option<ViewId>,
    id: FeatureId,
    label: String,
) -> Result<Option<ViewerResponse>, DispatchError> {
    let vid = resolve_target(state, view)?;
    let len = state.workspace.edit_annotations(vid, |ann, buf| {
        if ann.rename(id, label) {
            Ok(buf.text.len())
        } else {
            Err(DispatchError::InvalidInput(format!(
                "no feature with id {id}"
            )))
        }
    })?;
    Ok(Some(ViewerResponse::Edited { len, changed: true }))
}

/// Edit a feature's geometry/type in place (`UpdateFeature`): only the
/// `Some(_)` fields change. Undoable via `edit_annotations`; validates the
/// (possibly-partial) new range against the buffer.
#[allow(clippy::too_many_arguments)]
pub(super) fn apply_update_feature(
    state: &mut AppState,
    view: Option<ViewId>,
    id: FeatureId,
    kind: Option<String>,
    label: Option<String>,
    strand: Option<String>,
    start: Option<usize>,
    end: Option<usize>,
) -> Result<Option<ViewerResponse>, DispatchError> {
    let vid = resolve_target(state, view)?;
    let len = state.workspace.edit_annotations(vid, |ann, buf| {
        let cur = ann
            .get(id)
            .ok_or_else(|| DispatchError::InvalidInput(format!("no feature with id {id}")))?;
        let new_start = start.unwrap_or(cur.range.start);
        let new_end = end.unwrap_or(cur.range.end);
        if new_start >= new_end || new_end > buf.text.len() {
            return Err(DispatchError::OutOfRange {
                position: new_end,
                seq_len: buf.text.len(),
            });
        }
        let f = ann.get_mut(id).expect("present — checked just above");
        f.range = new_start..new_end;
        if let Some(k) = kind {
            f.raw_kind = k;
        }
        if let Some(l) = label {
            f.label = l;
        }
        if let Some(s) = strand {
            f.strand = parse_strand(&s);
        }
        Ok(buf.text.len())
    })?;
    Ok(Some(ViewerResponse::Edited { len, changed: true }))
}

// ── Primer ops (Phase 2.1) ──────────────────────────────────────────────────────
//
// Siblings of the feature ops (ROADMAP decision 11/14): annotation-only
// mutations routed through `workspace.edit_annotations` (undoable snapshot +
// version bump), addressed by `PrimerId`, content-given → **no `bio`**. Edits
// never *delete* a primer implicitly — an anchor-destroying sequence edit sets
// `binding = None` via the primer-specific shift handler; only an explicit
// `RemovePrimer` deletes.

/// Normalize + validate a primer oligo: uppercase, strip whitespace, IUPAC-check
/// (reusing [`parse_bases`]), reject empty. Returns the clean 5'→3' string.
fn parse_oligo(sequence: &str) -> Result<String, DispatchError> {
    let bytes = parse_bases(sequence)?;
    if bytes.is_empty() {
        return Err(DispatchError::InvalidInput(
            "primer sequence is empty".into(),
        ));
    }
    Ok(String::from_utf8(bytes).expect("IUPAC bytes are ASCII"))
}

/// Resolve an optional `(start, end)` pair into an annealing footprint:
/// both `None` → a detached/floating oligo (`None`); both `Some` → `start..end`;
/// exactly one `Some` is only valid when combined with a current binding.
fn resolve_binding(
    start: Option<usize>,
    end: Option<usize>,
    current: Option<&Range<usize>>,
) -> Result<Option<Range<usize>>, DispatchError> {
    match (start, end) {
        (None, None) => Ok(current.cloned()),
        (s, e) => {
            let cs = s.or_else(|| current.map(|b| b.start));
            let ce = e.or_else(|| current.map(|b| b.end));
            match (cs, ce) {
                (Some(a), Some(b)) => Ok(Some(a..b)),
                _ => Err(DispatchError::InvalidInput(
                    "binding start/end need both ends (no current binding to combine with)".into(),
                )),
            }
        }
    }
}

/// Validate a binding footprint against the buffer (half-open, within bounds).
fn check_binding(binding: Option<&Range<usize>>, len: usize) -> Result<(), DispatchError> {
    if let Some(b) = binding {
        if b.start >= b.end || b.end > len {
            return Err(DispatchError::OutOfRange {
                position: b.end,
                seq_len: len,
            });
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(super) fn apply_add_primer(
    state: &mut AppState,
    view: Option<ViewId>,
    name: Option<String>,
    sequence: String,
    start: Option<usize>,
    end: Option<usize>,
    strand: String,
) -> Result<Option<ViewerResponse>, DispatchError> {
    let vid = resolve_target(state, view)?;
    let sequence = parse_oligo(&sequence)?;
    let binding = resolve_binding(start, end, None)?;
    let (id, len) = state.workspace.edit_annotations(vid, |ann, buf| {
        check_binding(binding.as_ref(), buf.text.len())?;
        // Naming is never a blocker (decision 9): an empty/absent name falls back
        // to the one shared `suggest_primer_name()` generator.
        let name = name
            .filter(|n| !n.trim().is_empty())
            .unwrap_or_else(|| ann.suggest_primer_name());
        let id = ann.add_primer(Primer {
            id: PrimerId::default(), // reassigned by `add_primer`
            name,
            sequence,
            binding,
            strand: parse_strand(&strand),
            qualifiers: Default::default(),
        });
        Ok((id, buf.text.len()))
    })?;
    Ok(Some(ViewerResponse::PrimerAdded { id, len }))
}

/// Edit a primer in place (`UpdatePrimer`): only the `Some(_)` fields change.
/// Binding is resolved from the partial `start`/`end` against the current
/// footprint. An explicit empty name is ignored (never blanks the name).
#[allow(clippy::too_many_arguments)]
pub(super) fn apply_update_primer(
    state: &mut AppState,
    view: Option<ViewId>,
    id: PrimerId,
    name: Option<String>,
    sequence: Option<String>,
    strand: Option<String>,
    start: Option<usize>,
    end: Option<usize>,
    detach: bool,
) -> Result<Option<ViewerResponse>, DispatchError> {
    if detach && (start.is_some() || end.is_some()) {
        return Err(DispatchError::InvalidInput(
            "--detach clears the binding; don't pass start/end with it".into(),
        ));
    }
    let vid = resolve_target(state, view)?;
    let new_seq = sequence.map(|s| parse_oligo(&s)).transpose()?;
    let len = state.workspace.edit_annotations(vid, |ann, buf| {
        let cur = ann
            .primer(id)
            .ok_or_else(|| DispatchError::InvalidInput(format!("no primer with id {id}")))?;
        // `detach` explicitly clears the footprint (floating oligo); otherwise
        // `resolve_binding` treats an absent start/end as "keep current".
        let new_binding = if detach {
            None
        } else {
            resolve_binding(start, end, cur.binding.as_ref())?
        };
        check_binding(new_binding.as_ref(), buf.text.len())?;
        let p = ann.primer_mut(id).expect("present — checked just above");
        p.binding = new_binding;
        if let Some(n) = name.filter(|n| !n.trim().is_empty()) {
            p.name = n;
        }
        if let Some(s) = new_seq {
            p.sequence = s;
        }
        if let Some(s) = strand {
            p.strand = parse_strand(&s);
        }
        Ok(buf.text.len())
    })?;
    Ok(Some(ViewerResponse::Edited { len, changed: true }))
}

/// Re-anchor a primer to its best binding site on the current template
/// (footprint + strand), turning a Drifted/Detached primer back into Confirmed
/// without hand-entering coordinates. "Best" = fewest mismatches, then a clean
/// 3' anchor. Errors (no mutation) if the oligo binds nowhere.
pub(super) fn apply_rescan_primer(
    state: &mut AppState,
    view: Option<ViewId>,
    id: PrimerId,
) -> Result<Option<ViewerResponse>, DispatchError> {
    let vid = resolve_target(state, view)?;
    let len = state.workspace.edit_annotations(vid, |ann, buf| {
        let cur = ann
            .primer(id)
            .ok_or_else(|| DispatchError::InvalidInput(format!("no primer with id {id}")))?;
        let oligo = cur.sequence.clone();
        let settings = seqforge_bio::AnnealSettings::default();
        let best =
            seqforge_bio::find_primer_binding_sites(&oligo, &buf.text, buf.is_circular(), settings)
                .into_iter()
                .min_by_key(|s| (s.mismatches, !s.three_prime_match))
                .ok_or_else(|| {
                    DispatchError::InvalidInput(format!(
                        "primer {id} binds nowhere on this template"
                    ))
                })?;
        let p = ann.primer_mut(id).expect("present — checked just above");
        p.binding = Some(best.range);
        p.strand = best.strand;
        Ok(buf.text.len())
    })?;
    Ok(Some(ViewerResponse::Edited { len, changed: true }))
}

pub(super) fn apply_remove_primer(
    state: &mut AppState,
    view: Option<ViewId>,
    id: PrimerId,
) -> Result<Option<ViewerResponse>, DispatchError> {
    let vid = resolve_target(state, view)?;
    let len = state.workspace.edit_annotations(vid, |ann, buf| {
        if ann.remove_primer(id) {
            Ok(buf.text.len())
        } else {
            Err(DispatchError::InvalidInput(format!(
                "no primer with id {id}"
            )))
        }
    })?;
    Ok(Some(ViewerResponse::Edited { len, changed: true }))
}

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
            apply_add_feature(state, None, start, end, kind, label.clone(), strand)?;
            super::nav::apply_dismiss_overlay(state)?;
            let shown = if label.is_empty() { "feature" } else { &label };
            state.toasts.success(format!("Added {shown}"));
        }
        Some(id) => {
            apply_update_feature(
                state,
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
    apply_rename_feature(state, None, id, label)?;
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
    let vid = resolve_target(state, view)?;
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
            .with_active_buffer(|_, _, ann| ann.len())
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
    fn stage_edit_arms_preview_without_mutating() {
        let mut s = state_with(b"ATGCAA");
        // A menu Cut used to delete immediately; now it stages a preview.
        apply_stage_edit(&mut s, StagedEdit::Cut { start: 2, end: 4 }).unwrap();
        assert_eq!(text(&mut s), b"ATGCAA"); // buffer untouched until Enter
        let vid = s.workspace.active_view().unwrap().id;
        // Focuses the target view (so the stage survives + Enter commits) and
        // arms the canvas pending edit.
        assert_eq!(s.focus.scope, FocusScope::View(vid));
        assert!(s.workspace.seq_views.get(&vid).unwrap().is_staging());
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
                    binding: Some(5..8),
                    strand: Strand::Reverse,
                    qualifiers: Default::default(),
                });
                v.selected_primer = Some(id);
            })
            .unwrap();

        // Copying the primer's exact footprint copies the oligo, not "TGC".
        apply_copy(&mut s, None, 5, 8).unwrap();
        assert_eq!(s.clipboard.as_deref(), Some(b"GCA".as_slice()));

        // A copy over a *different* range stays a literal template slice — the
        // gate is `range == binding`, so CLI/agent range copies are unaffected.
        apply_copy(&mut s, None, 0, 3).unwrap();
        assert_eq!(s.clipboard.as_deref(), Some(b"AAA".as_slice()));
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
                    binding: Some(5..8),
                    strand: Strand::Reverse,
                    qualifiers: Default::default(),
                });
                v.selected_primer = Some(id);
            })
            .unwrap();

        apply_copy(&mut s, None, 0, 0).unwrap();
        assert_eq!(s.clipboard.as_deref(), Some(b"GCA".as_slice()));
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
                v.selected_primer = Some(id);
            })
            .unwrap();

        apply_copy(&mut s, None, 0, 0).unwrap();
        assert_eq!(s.clipboard.as_deref(), Some(b"TTTGGG".as_slice()));
    }

    #[test]
    fn copy_without_selected_primer_is_a_template_slice() {
        let mut s = state_with(b"AAATGCGG");
        apply_copy(&mut s, None, 3, 6).unwrap();
        assert_eq!(s.clipboard.as_deref(), Some(b"TGC".as_slice()));
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

    /// Add a feature and return its minted id.
    fn add_feat(state: &mut AppState, start: usize, end: usize, label: &str) -> FeatureId {
        match apply_add_feature(
            state,
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

        apply_rename_feature(&mut s, None, id, "renamed".into()).unwrap();
        assert_eq!(first_label(&mut s), "renamed");

        apply_remove_feature(&mut s, None, id).unwrap();
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
            .with_active_buffer(|_, _, ann| {
                let f = ann.get(id).unwrap();
                (f.label.clone(), f.range.clone())
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
        apply_rename_feature(&mut s, None, id, "renamed".into()).unwrap();
        apply_remove_feature(&mut s, None, id).unwrap();
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
    fn update_feature_partial_and_undoable() {
        let mut s = state_with(b"ATGCATGCATGC");
        let vid = s.workspace.active_view().unwrap().id;
        let id = add_feat(&mut s, 0, 3, "orig");

        // Change only the range + kind; label/strand left untouched.
        apply_update_feature(
            &mut s,
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
            .with_active_buffer(|_, _, ann| {
                let f = ann.get(id).unwrap();
                (f.range.clone(), f.raw_kind.clone(), f.label.clone())
            })
            .unwrap();
        assert_eq!(range, 4..9);
        assert_eq!(kind, "misc_feature");
        assert_eq!(label, "orig", "unspecified fields are preserved");

        // Undo restores the original geometry.
        s.workspace.undo(vid).unwrap();
        let range = s
            .workspace
            .with_active_buffer(|_, _, ann| ann.get(id).unwrap().range.clone())
            .unwrap();
        assert_eq!(range, 0..3);
    }

    #[test]
    fn update_feature_bad_range_errors() {
        let mut s = state_with(b"ATGC");
        let id = add_feat(&mut s, 0, 3, "f");
        let err = apply_update_feature(&mut s, None, id, None, None, None, Some(2), Some(99))
            .unwrap_err();
        assert!(matches!(err, DispatchError::OutOfRange { .. }));
    }

    #[test]
    fn remove_feature_bad_id_errors() {
        let mut s = state_with(b"ATGC");
        let err = apply_remove_feature(&mut s, None, FeatureId(999)).unwrap_err();
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
        match apply_add_primer(
            state,
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
    fn add_primer_attached_and_floating() {
        let mut s = state_with(b"ATGCATGCATGC");
        let attached = add_primer(&mut s, Some("fwd"), "atg c", Some(0), Some(4), "+");
        // Sequence normalized (uppercased, whitespace stripped).
        let (name, seq, binding, strand) = first_primer(&mut s, |p| {
            (
                p.name.clone(),
                p.sequence.clone(),
                p.binding.clone(),
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
            apply_add_primer(&mut s, None, None, "ATGC".into(), Some(0), None, "+".into())
                .unwrap_err(),
            DispatchError::InvalidInput(_)
        ));
        // Binding past the end.
        assert!(matches!(
            apply_add_primer(
                &mut s,
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
            apply_add_primer(&mut s, None, None, "".into(), None, None, "+".into()).unwrap_err(),
            DispatchError::InvalidInput(_)
        ));
    }

    #[test]
    fn update_primer_partial_and_undoable() {
        let mut s = state_with(b"ATGCATGCATGC");
        let vid = s.workspace.active_view().unwrap().id;
        let id = add_primer(&mut s, Some("orig"), "ATGC", Some(0), Some(4), "+");

        // Change only the binding end + strand; name/sequence untouched.
        apply_update_primer(
            &mut s,
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
        let (name, binding, strand) =
            first_primer(&mut s, |p| (p.name.clone(), p.binding.clone(), p.strand));
        assert_eq!(name, "orig", "unspecified fields preserved");
        assert_eq!(binding, Some(0..8), "end updated, start kept");
        assert_eq!(strand, Strand::Reverse);

        // Undo restores the original binding + strand.
        s.workspace.undo(vid).unwrap();
        let (binding, strand) = first_primer(&mut s, |p| (p.binding.clone(), p.strand));
        assert_eq!(binding, Some(0..4));
        assert_eq!(strand, Strand::Forward);
    }

    #[test]
    fn update_primer_empty_name_is_ignored() {
        let mut s = state_with(b"ATGCATGC");
        let id = add_primer(&mut s, Some("keep"), "ATGC", Some(0), Some(4), "+");
        apply_update_primer(
            &mut s,
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
            apply_update_primer(
                &mut s,
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
        apply_update_primer(&mut s, None, id, None, None, None, None, None, true).unwrap();
        assert_eq!(
            first_primer(&mut s, |p| p.binding.clone()),
            None,
            "detach clears the footprint → floating oligo"
        );
        s.workspace.undo(vid).unwrap();
        assert_eq!(
            first_primer(&mut s, |p| p.binding.clone()),
            Some(0..4),
            "undo restores the binding"
        );
    }

    #[test]
    fn update_primer_detach_with_explicit_range_is_rejected() {
        let mut s = state_with(b"ATGCATGC");
        let id = add_primer(&mut s, Some("p"), "ATGC", Some(0), Some(4), "+");
        assert!(matches!(
            apply_update_primer(&mut s, None, id, None, None, None, Some(0), Some(4), true)
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
            first_primer(&mut s, |p| p.binding.clone()),
            None,
            "starts floating"
        );
        apply_rescan_primer(&mut s, None, id).unwrap();
        let (binding, strand) = first_primer(&mut s, |p| (p.binding.clone(), p.strand));
        assert_eq!(binding, Some(2..8), "re-anchored to the forward site");
        assert_eq!(strand, Strand::Forward);
    }

    #[test]
    fn rescan_primer_that_binds_nowhere_errors_without_mutation() {
        let mut s = state_with(b"AAAAAAAAAA");
        let id = add_primer(&mut s, Some("p"), "GCGTAC", None, None, "+");
        assert!(matches!(
            apply_rescan_primer(&mut s, None, id).unwrap_err(),
            DispatchError::InvalidInput(_)
        ));
        assert_eq!(
            first_primer(&mut s, |p| p.binding.clone()),
            None,
            "failed rescan leaves the primer untouched"
        );
    }

    #[test]
    fn remove_primer_and_bad_id() {
        let mut s = state_with(b"ATGCATGC");
        let id = add_primer(&mut s, Some("p"), "ATGC", Some(0), Some(4), "+");
        assert_eq!(primer_count(&mut s), 1);
        apply_remove_primer(&mut s, None, id).unwrap();
        assert_eq!(primer_count(&mut s), 0);

        assert!(matches!(
            apply_remove_primer(&mut s, None, PrimerId(999)).unwrap_err(),
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
