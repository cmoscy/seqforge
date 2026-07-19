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
    DispatchError, EditKind, Feature, FeatureId, Location, Orient, PartialPolicy, Primer, PrimerId,
    Selection, SeqSlice, Span, Strand, ViewId, ViewSelection, ViewerResponse,
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
/// Extract `range` into an annotated [`SeqSlice`] (bytes + features + primers in
/// local coords) — the clipboard payload for a region copy/cut. `DropPartials`
/// mirrors the Biopython/pydna `record[a:b]` default; circularity comes from the
/// buffer topology so a selection wrapping the origin carries correctly.
fn extract_region(
    state: &mut AppState,
    vid: ViewId,
    range: Range<usize>,
) -> Result<SeqSlice, DispatchError> {
    state.workspace.with_buffer(vid, |v, buf, ann| {
        let total = buf.text.len();
        if range.end > total {
            return Err(DispatchError::OutOfRange {
                position: range.end,
                seq_len: total,
            });
        }
        // The `Span` is the single wrap encoding. Honor a live wrapping selection
        // (P3): a shift-select through the origin whose bounds match this request
        // extracts the origin-crossing arc, not the `[lo, hi)` interval it is the
        // complement of. Any other range is a plain linear span.
        let span = match v.selection.text_range() {
            Some(sel) if sel.wrap && sel.ordered() == (range.start, range.end) => {
                sel.to_span(total)
            }
            _ => Span::from_range(range),
        };
        Ok(seqforge_core::transport::extract(
            &buf.text,
            ann,
            span,
            PartialPolicy::DropPartials,
            &buf.name,
        ))
    })?
}

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
    // Whole-molecule RC also mirrors the annotation layer (features flip
    // coordinates + strand), riding this edit's single undo unit. A sub-range
    // inversion stays byte-only for now (feature mirroring within a window is a
    // follow-up). RC preserves length, so `end == len` still holds.
    let len = buffer_len(state, vid);
    if start == 0 && end == len {
        state.workspace.reverse_complement_annotations_whole(vid)?;
    }
    edited(len)
}

/// Set Origin: rotate a circular buffer so `index` becomes position 0.
pub(super) fn apply_set_origin(
    state: &mut AppState,
    view: Option<ViewId>,
    index: usize,
) -> Result<Option<ViewerResponse>, DispatchError> {
    let vid = resolve_target(state, view)?;
    state.workspace.set_origin(vid, index)?;
    edited(buffer_len(state, vid))
}

/// Linearize a circular buffer, cutting at `at` (default position 0).
pub(super) fn apply_linearize(
    state: &mut AppState,
    view: Option<ViewId>,
    at: Option<usize>,
) -> Result<Option<ViewerResponse>, DispatchError> {
    let vid = resolve_target(state, view)?;
    state.workspace.linearize(vid, at)?;
    edited(buffer_len(state, vid))
}

/// Circularize a linear buffer (optionally rotating the origin).
pub(super) fn apply_circularize(
    state: &mut AppState,
    view: Option<ViewId>,
    origin: Option<usize>,
) -> Result<Option<ViewerResponse>, DispatchError> {
    let vid = resolve_target(state, view)?;
    state.workspace.circularize(vid, origin)?;
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
            let id = v.selection.selected_primer()?;
            let p = ann.primer(id)?;
            let is_footprint = p
                .binding
                .as_ref()
                .is_some_and(|b| b.start == start && b.start + b.len == end);
            (start == end || is_footprint).then(|| p.sequence.clone().into_bytes())
        })
        .ok()
        .flatten();

    // A selected-primer copy carries only the authored oligo bytes (no template
    // features/primers ride along — it's a reagent, not a region). A region copy
    // carries the full annotated slice (features + primers) via `extract`.
    let slice = match oligo {
        Some(bytes) => SeqSlice {
            bytes,
            features: Vec::new(),
            primers: Vec::new(),
        },
        None => extract_region(state, vid, start..end)?,
    };
    let len = slice.bytes.len();
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
    // Carry the annotated slice, then delete the region (which shifts/drops the
    // remaining annotations via the splice policy).
    let slice = extract_region(state, vid, start..end)?;
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
    let slice = state
        .clipboard
        .clone()
        .ok_or_else(|| DispatchError::InvalidInput("clipboard is empty".into()))?;
    if slice.bytes.is_empty() {
        return Err(DispatchError::InvalidInput("clipboard is empty".into()));
    }
    // A paste is its own undo unit (`Other`) — never coalesces with typing.
    // Bytes + carried features/primers land in one transaction; `merge=true`
    // reunites same-lineage pieces (provenance-gated — ordinary pastes don't
    // fuse). Copy/paste is always `Identity`; `Rev` is first used at ligation.
    state
        .workspace
        .paste_slice(vid, pos, &slice, Orient::Identity, true)?;
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
            location: Location::simple(start..end),
            raw_kind: kind,
            label,
            strand: parse_strand(&strand),
            qualifiers: Default::default(),
            lineage: None,
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
        let total = buf.text.len();
        let cur = ann
            .get(id)
            .ok_or_else(|| DispatchError::InvalidInput(format!("no feature with id {id}")))?;
        // The feature's current *linear* extent, used only to default an
        // unspecified endpoint. A wrapping or spliced (`Join`) feature has no
        // single linear extent — its `bounds` are the lossy `0..len` — so a
        // partial re-range there is ill-defined and we require both endpoints
        // explicitly rather than resizing from a phantom range (`plans/span.md`
        // P5a: correct-by-omission, not a silent flatten). `Span` is `Copy`, so
        // this drops the immutable borrow before `get_mut` below.
        let cur_linear = cur.location.as_span().filter(|s| !s.wraps(total));
        // Rebuild the geometry to a single crisp range only when the caller
        // actually re-ranged the feature; a label/kind/strand-only edit must not
        // touch a multi-segment `Join`.
        let re_range = start.is_some() || end.is_some();
        let f = ann.get_mut(id).expect("present — checked just above");
        if re_range {
            let (new_start, new_end) = match (
                start.or(cur_linear.map(|s| s.start)),
                // Non-wrapping (filtered into `cur_linear`) → `start+len`, not
                // `end(total)` (which is `0` for a feature ending at `len`).
                end.or(cur_linear.map(|s| s.start + s.len)),
            ) {
                (Some(s), Some(e)) => (s, e),
                _ => {
                    return Err(DispatchError::InvalidInput(format!(
                        "feature {id} wraps the origin or is spliced; \
                         resize requires explicit start and end"
                    )));
                }
            };
            if new_start >= new_end || new_end > total {
                return Err(DispatchError::OutOfRange {
                    position: new_end,
                    seq_len: total,
                });
            }
            f.location = Location::simple(new_start..new_end);
        }
        if let Some(k) = kind {
            f.raw_kind = k;
        }
        if let Some(l) = label {
            f.label = l;
        }
        if let Some(s) = strand {
            f.strand = parse_strand(&s);
        }
        Ok(total)
    })?;
    // If this feature is the current selection, re-sync the stored range from the
    // live annotations — `edit_annotations` doesn't reset selection (unlike text
    // edits), so a geometry change would otherwise leave `Feature{range}` stale.
    // Read the *actual* new range (source of truth), so this never re-denormalizes.
    if state
        .workspace
        .view(vid)
        .and_then(|v| v.selection.selected_feature())
        == Some(id)
    {
        let span = state
            .workspace
            .with_buffer(vid, |_, b, ann| {
                ann.get(id)
                    .map(|f| (f.selection_span(b.text.len()), b.text.len()))
            })
            .ok()
            .flatten();
        if let (Some((span, buf_len)), Some(view)) = (span, state.workspace.view_mut(vid)) {
            view.selection = ViewSelection::Feature {
                id,
                range: Selection::from_span(span, buf_len),
            };
        }
    }
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
    current: Option<&Span>,
) -> Result<Option<Span>, DispatchError> {
    match (start, end) {
        (None, None) => Ok(current.copied()),
        (s, e) => {
            let cs = s.or_else(|| current.map(|b| b.start));
            let ce = e.or_else(|| current.map(|b| b.start + b.len));
            match (cs, ce) {
                (Some(a), Some(b)) => Ok(Some(Span::from_range(a..b))),
                _ => Err(DispatchError::InvalidInput(
                    "binding start/end need both ends (no current binding to combine with)".into(),
                )),
            }
        }
    }
}

/// Validate a binding footprint against the buffer (non-empty, within bounds).
/// Linear check — a primer footprint doesn't yet wrap the origin.
fn check_binding(binding: Option<&Span>, len: usize) -> Result<(), DispatchError> {
    if let Some(b) = binding {
        let end = b.start + b.len;
        if b.len == 0 || end > len {
            return Err(DispatchError::OutOfRange {
                position: end,
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
        p.binding = Some(best.span);
        p.strand = best.strand;
        Ok(buf.text.len())
    })?;
    Ok(Some(ViewerResponse::Edited { len, changed: true }))
}

/// Compose a restriction site onto a primer's 5' tail (Phase 2.2a): build the
/// tail via `seqforge_bio::restriction_tail` and prepend it to the authored
/// oligo. The binding footprint is unchanged (the added bases are a 5' tail, so
/// `decompose_primer`/QC/off-target re-scan all treat them as such). Builder
/// failures (unknown enzyme, wrong overhang length, …) surface as `InvalidInput`.
pub(super) fn apply_add_primer_site(
    state: &mut AppState,
    view: Option<ViewId>,
    id: PrimerId,
    enzyme: String,
    overhang: Option<String>,
    flank: Option<String>,
) -> Result<Option<ViewerResponse>, DispatchError> {
    let tail = seqforge_bio::restriction_tail(&enzyme, overhang.as_deref(), flank.as_deref())
        .map_err(|e| DispatchError::InvalidInput(e.to_string()))?;
    let vid = resolve_target(state, view)?;
    let len = state.workspace.edit_annotations(vid, |ann, buf| {
        let p = ann
            .primer_mut(id)
            .ok_or_else(|| DispatchError::InvalidInput(format!("no primer with id {id}")))?;
        p.sequence = format!("{tail}{}", p.sequence);
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
        apply_copy(&mut s, None, 4, 12).unwrap();
        assert!(s.clipboard.is_some(), "region copy fills the clipboard");

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
        apply_paste(&mut s, None, 0).unwrap();
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

        apply_circularize(&mut s, None, None).unwrap();
        assert!(is_circular(&mut s), "circularize flips topology");

        apply_set_origin(&mut s, None, 4).unwrap();
        assert_eq!(
            text(&mut s),
            b"CCCCGGGGTTTTAAAA",
            "set-origin rotates the bytes"
        );

        apply_linearize(&mut s, None, Some(0)).unwrap();
        assert!(!is_circular(&mut s), "linearize flips topology back");

        // Undo restores topology (the history topology-stamp), not just bytes.
        apply_undo(&mut s, None).unwrap();
        assert!(is_circular(&mut s), "undo restores circular topology");
        apply_undo(&mut s, None).unwrap();
        assert_eq!(
            text(&mut s),
            b"AAAACCCCGGGGTTTT",
            "undo restores the rotation"
        );
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
        assert_eq!(
            s.clipboard.as_ref().map(|c| c.bytes()),
            Some(b"GC".as_slice())
        );
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
        assert_eq!(
            s.clipboard.as_ref().map(|c| c.bytes()),
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
        apply_copy(&mut s, None, 5, 8).unwrap();
        assert_eq!(
            s.clipboard.as_ref().map(|c| c.bytes()),
            Some(b"GCA".as_slice())
        );

        // A copy over a *different* range stays a literal template slice — the
        // gate is `range == binding`, so CLI/agent range copies are unaffected.
        apply_copy(&mut s, None, 0, 3).unwrap();
        assert_eq!(
            s.clipboard.as_ref().map(|c| c.bytes()),
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

        apply_copy(&mut s, None, 0, 0).unwrap();
        assert_eq!(
            s.clipboard.as_ref().map(|c| c.bytes()),
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

        apply_copy(&mut s, None, 0, 0).unwrap();
        assert_eq!(
            s.clipboard.as_ref().map(|c| c.bytes()),
            Some(b"TTTGGG".as_slice())
        );
    }

    #[test]
    fn copy_without_selected_primer_is_a_template_slice() {
        let mut s = state_with(b"AAATGCGG");
        apply_copy(&mut s, None, 3, 6).unwrap();
        assert_eq!(
            s.clipboard.as_ref().map(|c| c.bytes()),
            Some(b"TGC".as_slice())
        );
    }

    #[test]
    fn paste_inserts_clipboard() {
        let mut s = state_with(b"ATGC");
        s.clipboard = Some(clip(b"NN"));
        apply_paste(&mut s, None, 4).unwrap();
        assert_eq!(text(&mut s), b"ATGCNN");
    }

    #[test]
    fn paste_empty_clipboard_errors() {
        let mut s = state_with(b"ATGC");
        let err = apply_paste(&mut s, None, 0).unwrap_err();
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
        apply_copy(&mut s, None, 2, 8).unwrap();
        apply_paste(&mut s, None, 12).unwrap();

        assert_eq!(text(&mut s), b"ATGCATGCATGCGCATGC");
        assert_eq!(feature_spans(&mut s), vec![3..6, 13..16]);
        assert_eq!(
            primer_bindings(&mut s),
            vec![Some(4..7), Some(14..17)],
            "primer carried with shifted binding"
        );

        // Undo removes the pasted bytes AND the placed annotations (one txn).
        apply_undo(&mut s, None).unwrap();
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
        apply_copy(&mut s, None, 0, 4).unwrap();
        apply_paste(&mut s, None, 4).unwrap(); // paste abutting at 4 → [4,8)
        assert_eq!(feature_spans(&mut s), vec![0..4, 4..8], "no silent merge");
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
        apply_insert(&mut s, None, 100, "ATGCATGC".into()).unwrap();
        ops += 1;
        apply_delete(&mut s, None, 500, 520).unwrap();
        ops += 1;
        apply_replace(&mut s, None, 200, 210, "TTTTAAAA".into()).unwrap();
        ops += 1;
        apply_reverse_complement(&mut s, None, 1000, 1040).unwrap();
        ops += 1;
        let fid = match apply_add_feature(
            &mut s,
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
        apply_update_feature(
            &mut s,
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
        apply_remove_feature(&mut s, None, fid).unwrap();
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
        let err = apply_update_feature(&mut s, None, id, None, None, None, Some(2), Some(99))
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
        let err =
            apply_update_feature(&mut s, None, id, None, None, None, Some(5), None).unwrap_err();
        assert!(matches!(err, DispatchError::InvalidInput(_)));
        // A label-only edit (no re-range) still works on a wrapping feature.
        apply_update_feature(
            &mut s,
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
        apply_update_feature(&mut s, None, id, None, None, None, Some(2), Some(6)).unwrap();
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

        apply_update_feature(&mut s, None, id, None, None, None, Some(4), Some(9)).unwrap();

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

        apply_update_feature(&mut s, None, id, None, None, None, Some(4), Some(9)).unwrap();

        assert_eq!(
            s.workspace.active_view().unwrap().selection.text_range(),
            Some(Selection::range(6, 10)),
            "an unrelated text selection is left intact"
        );
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
            first_primer(&mut s, |p| p.binding.map(|b| b.start..b.start + b.len)),
            None,
            "starts floating"
        );
        apply_rescan_primer(&mut s, None, id).unwrap();
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
            apply_rescan_primer(&mut s, None, id).unwrap_err(),
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
        apply_add_primer_site(
            &mut s,
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
            apply_add_primer_site(&mut s, None, id, "BsaI".into(), Some("AA".into()), None)
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
        s.clipboard = Some(clip(b"AA"));
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
