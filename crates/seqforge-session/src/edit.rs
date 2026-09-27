//! The headless write path: every command that mutates a document.
//!
//! Each handler takes a `&mut Workspace` and, where it must reach the user or
//! the clipboard, a `&mut dyn Host`. Nothing here knows what is driving it, so
//! the GUI, the CLI, and an agent over the socket run the same code
//! (ROADMAP decision 27).
//!
//! Two neighbours deliberately stay in `seqforge-app`, because they are GUI
//! gestures rather than verbs: canvas edit *staging*, and the modal form
//! submitters that dismiss an overlay before delegating to the verbs here.

use std::ops::Range;

use crate::bases::parse_bases;
use crate::host::Host;
use crate::workspace::Workspace;

use seqforge_core::{
    DispatchError, EditKind, Feature, FeatureId, Location, Orient, PartialPolicy, Primer, PrimerId,
    Selection, SeqSlice, Span, Strand, ViewId, ViewSelection, ViewerResponse,
};

// ── Shared helpers ────────────────────────────────────────────────────────────

/// Resolve a request's optional `view` target to a concrete `ViewId`:
/// the explicit target if present (erroring if it has been closed), else the
/// active view. Mirrors `dispatch_active`'s rule for the write path. Shared
/// with `file.rs` (the save handlers).
pub fn resolve_target(ws: &Workspace, view: Option<ViewId>) -> Result<ViewId, DispatchError> {
    match view {
        Some(vid) => {
            if ws.view(vid).is_some() {
                Ok(vid)
            } else {
                Err(DispatchError::ViewNotFound(vid))
            }
        }
        None => ws
            .active_view()
            .map(|v| v.id)
            .ok_or(DispatchError::NoActiveView),
    }
}

/// Read `[start, end)` from the target buffer, validating the range. Used by the
/// composed edits (RC, cut, copy) that need the old bytes before splicing.
fn read_slice(
    ws: &mut Workspace,
    vid: ViewId,
    range: Range<usize>,
) -> Result<Vec<u8>, DispatchError> {
    ws.with_buffer(vid, |_, buf, _| {
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
    ws: &mut Workspace,
    vid: ViewId,
    range: Range<usize>,
) -> Result<SeqSlice, DispatchError> {
    ws.with_buffer(vid, |v, buf, ann| {
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

fn buffer_len(ws: &mut Workspace, vid: ViewId) -> usize {
    ws.with_buffer(vid, |_, buf, _| buf.text.len()).unwrap_or(0)
}

fn edited(len: usize) -> Result<Option<ViewerResponse>, DispatchError> {
    Ok(Some(ViewerResponse::Edited { len, changed: true }))
}

// ── Content-given edits (12b) ─────────────────────────────────────────────────

pub fn apply_insert(
    ws: &mut Workspace,
    view: Option<ViewId>,
    pos: usize,
    bases: String,
) -> Result<Option<ViewerResponse>, DispatchError> {
    let vid = resolve_target(ws, view)?;
    let bytes = parse_bases(&bases)?;
    ws.edit(vid, EditKind::Insert, pos..pos, &bytes)?;
    edited(buffer_len(ws, vid))
}

pub fn apply_delete(
    ws: &mut Workspace,
    view: Option<ViewId>,
    start: usize,
    end: usize,
) -> Result<Option<ViewerResponse>, DispatchError> {
    let vid = resolve_target(ws, view)?;
    ws.edit(vid, EditKind::Delete, start..end, &[])?;
    edited(buffer_len(ws, vid))
}

pub fn apply_replace(
    ws: &mut Workspace,
    view: Option<ViewId>,
    start: usize,
    end: usize,
    bases: String,
) -> Result<Option<ViewerResponse>, DispatchError> {
    let vid = resolve_target(ws, view)?;
    let bytes = parse_bases(&bases)?;
    ws.edit(vid, EditKind::Other, start..end, &bytes)?;
    edited(buffer_len(ws, vid))
}

// ── Composed edit: reverse-complement (12c) ────────────────────────────────────

pub fn apply_reverse_complement(
    ws: &mut Workspace,
    view: Option<ViewId>,
    start: usize,
    end: usize,
) -> Result<Option<ViewerResponse>, DispatchError> {
    let vid = resolve_target(ws, view)?;
    let slice = read_slice(ws, vid, start..end)?;
    // Bytes derived by bio, then installed via the same splice path — the
    // primitive-vs-composed split that the cloning roadmap rides.
    let rc = seqforge_bio::reverse_complement(&slice);
    ws.edit(vid, EditKind::Other, start..end, &rc)?;
    // Whole-molecule RC also mirrors the annotation layer (features flip
    // coordinates + strand), riding this edit's single undo unit. A sub-range
    // inversion stays byte-only for now (feature mirroring within a window is a
    // follow-up). RC preserves length, so `end == len` still holds.
    let len = buffer_len(ws, vid);
    if start == 0 && end == len {
        ws.reverse_complement_annotations_whole(vid)?;
    }
    edited(len)
}

/// Set Origin: rotate a circular buffer so `index` becomes position 0.
pub fn apply_set_origin(
    ws: &mut Workspace,
    view: Option<ViewId>,
    index: Option<usize>,
    feature: Option<String>,
) -> Result<Option<ViewerResponse>, DispatchError> {
    let vid = resolve_target(ws, view)?;
    // A label is resolved against this buffer's own features; `resolve_origin`
    // rejects a label that matches none or several rather than guessing.
    let index = match (index, feature) {
        (Some(i), _) => i,
        (None, Some(label)) => {
            let bid = ws
                .view(vid)
                .map(|v| v.buffer_id)
                .ok_or_else(|| DispatchError::InvalidInput(format!("no view {vid}")))?;
            let len = ws
                .buffers
                .get(bid)
                .and_then(|b| b.read().ok().map(|b| b.text.len()))
                .ok_or_else(|| DispatchError::InvalidInput("buffer unavailable".into()))?;
            let features: Vec<seqforge_core::Feature> = ws
                .buffers
                .annotations(bid)
                .map(|a| a.iter().cloned().collect())
                .unwrap_or_default();
            seqforge_bio::resolve_origin(&seqforge_bio::OriginSpec::Feature(label), &features, len)
                .map_err(DispatchError::InvalidInput)?
        }
        (None, None) => {
            return Err(DispatchError::InvalidInput(
                "set-origin needs an index or --feature".into(),
            ));
        }
    };
    ws.set_origin(vid, index)?;
    edited(buffer_len(ws, vid))
}

/// Linearize a circular buffer, cutting at `at` (default position 0).
pub fn apply_linearize(
    ws: &mut Workspace,
    view: Option<ViewId>,
    at: Option<usize>,
) -> Result<Option<ViewerResponse>, DispatchError> {
    let vid = resolve_target(ws, view)?;
    ws.linearize(vid, at)?;
    edited(buffer_len(ws, vid))
}

/// Circularize a linear buffer (optionally rotating the origin).
pub fn apply_circularize(
    ws: &mut Workspace,
    view: Option<ViewId>,
    origin: Option<usize>,
) -> Result<Option<ViewerResponse>, DispatchError> {
    let vid = resolve_target(ws, view)?;
    ws.circularize(vid, origin)?;
    edited(buffer_len(ws, vid))
}

// ── Clipboard (12c) ─────────────────────────────────────────────────────────--

pub fn apply_copy(
    ws: &mut Workspace,
    host: &mut dyn Host,
    view: Option<ViewId>,
    start: usize,
    end: usize,
    reverse: bool,
) -> Result<Option<ViewerResponse>, DispatchError> {
    let vid = resolve_target(ws, view)?;
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
    //
    // `reverse` (copy-as-RC) skips the oligo shortcut: a reverse-complement copy is
    // always a region of the template (or the RC of those bases when the range is
    // empty / oligo-only callers pass reverse=false).
    let oligo = if reverse {
        None
    } else {
        ws.with_buffer(vid, |v, _buf, ann| {
            let id = v.selection.selected_primer()?;
            let p = ann.primer(id)?;
            let is_footprint = p
                .binding
                .as_ref()
                .is_some_and(|b| b.start == start && b.start + b.len == end);
            (start == end || is_footprint).then(|| p.sequence.clone().into_bytes())
        })
        .ok()
        .flatten()
    };

    // A selected-primer copy carries only the authored oligo bytes (no template
    // features/primers ride along — it's a reagent, not a region). A region copy
    // carries the full annotated slice (features + primers) via `extract`.
    let mut slice = match oligo {
        Some(bytes) => SeqSlice {
            bytes,
            features: Vec::new(),
            primers: Vec::new(),
        },
        None => extract_region(ws, vid, start..end)?,
    };
    if reverse {
        // Bytes via bio; annotation mirror in core (place's Orient::Rev at 0).
        slice.bytes = seqforge_bio::reverse_complement(&slice.bytes);
        seqforge_core::transport::reverse_complement_annotations(&mut slice);
    }
    let len = slice.bytes.len();
    host.clipboard_set(slice);
    // Copy doesn't mutate the buffer — report the copied length, not a buffer
    // change, and record no history.
    Ok(Some(ViewerResponse::Edited {
        len,
        changed: false,
    }))
}

pub fn apply_cut(
    ws: &mut Workspace,
    host: &mut dyn Host,
    view: Option<ViewId>,
    start: usize,
    end: usize,
) -> Result<Option<ViewerResponse>, DispatchError> {
    let vid = resolve_target(ws, view)?;
    // Carry the annotated slice, then delete the region (which shifts/drops the
    // remaining annotations via the splice policy).
    let slice = extract_region(ws, vid, start..end)?;
    host.clipboard_set(slice);
    ws.edit(vid, EditKind::Delete, start..end, &[])?;
    edited(buffer_len(ws, vid))
}

pub fn apply_paste(
    ws: &mut Workspace,
    host: &mut dyn Host,
    view: Option<ViewId>,
    pos: usize,
) -> Result<Option<ViewerResponse>, DispatchError> {
    let vid = resolve_target(ws, view)?;
    // The host's clipboard is authoritative — in the GUI that reconciles
    // against the OS pasteboard before we trust the session cache.
    let slice = host
        .clipboard_get()
        .ok_or_else(|| DispatchError::InvalidInput("clipboard is empty".into()))?;
    if slice.bytes.is_empty() {
        return Err(DispatchError::InvalidInput("clipboard is empty".into()));
    }
    // A paste is its own undo unit (`Other`) — never coalesces with typing.
    // Bytes + carried features/primers land in one transaction; `merge=true`
    // reunites same-lineage pieces (provenance-gated — ordinary pastes don't
    // fuse). Copy/paste is always `Identity`; `Rev` is first used at ligation.
    ws.paste_slice(vid, pos, &slice, Orient::Identity, true)?;
    edited(buffer_len(ws, vid))
}

// ── History ops (12b) ─────────────────────────────────────────────────────────

pub fn apply_undo(
    ws: &mut Workspace,
    view: Option<ViewId>,
) -> Result<Option<ViewerResponse>, DispatchError> {
    let vid = resolve_target(ws, view)?;
    let changed = ws.undo(vid)?;
    Ok(Some(ViewerResponse::Edited {
        len: buffer_len(ws, vid),
        changed,
    }))
}

pub fn apply_redo(
    ws: &mut Workspace,
    view: Option<ViewId>,
) -> Result<Option<ViewerResponse>, DispatchError> {
    let vid = resolve_target(ws, view)?;
    let changed = ws.redo(vid)?;
    Ok(Some(ViewerResponse::Edited {
        len: buffer_len(ws, vid),
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

pub fn apply_add_feature(
    ws: &mut Workspace,
    view: Option<ViewId>,
    start: usize,
    end: usize,
    kind: String,
    label: String,
    strand: String,
) -> Result<Option<ViewerResponse>, DispatchError> {
    let vid = resolve_target(ws, view)?;
    let (id, len) = ws.edit_annotations(vid, |ann, buf| {
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

pub fn apply_remove_feature(
    ws: &mut Workspace,
    view: Option<ViewId>,
    id: FeatureId,
) -> Result<Option<ViewerResponse>, DispatchError> {
    let vid = resolve_target(ws, view)?;
    let len = ws.edit_annotations(vid, |ann, buf| {
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

pub fn apply_rename_feature(
    ws: &mut Workspace,
    view: Option<ViewId>,
    id: FeatureId,
    label: String,
) -> Result<Option<ViewerResponse>, DispatchError> {
    let vid = resolve_target(ws, view)?;
    let len = ws.edit_annotations(vid, |ann, buf| {
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
pub fn apply_update_feature(
    ws: &mut Workspace,
    view: Option<ViewId>,
    id: FeatureId,
    kind: Option<String>,
    label: Option<String>,
    strand: Option<String>,
    start: Option<usize>,
    end: Option<usize>,
) -> Result<Option<ViewerResponse>, DispatchError> {
    let vid = resolve_target(ws, view)?;
    let len = ws.edit_annotations(vid, |ann, buf| {
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
    if ws.view(vid).and_then(|v| v.selection.selected_feature()) == Some(id) {
        let span = ws
            .with_buffer(vid, |_, b, ann| {
                ann.get(id)
                    .map(|f| (f.selection_span(b.text.len()), b.text.len()))
            })
            .ok()
            .flatten();
        if let (Some((span, buf_len)), Some(view)) = (span, ws.view_mut(vid)) {
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
pub fn apply_add_primer(
    ws: &mut Workspace,
    view: Option<ViewId>,
    name: Option<String>,
    sequence: String,
    start: Option<usize>,
    end: Option<usize>,
    strand: String,
) -> Result<Option<ViewerResponse>, DispatchError> {
    let vid = resolve_target(ws, view)?;
    let sequence = parse_oligo(&sequence)?;
    let binding = resolve_binding(start, end, None)?;
    let (id, len) = ws.edit_annotations(vid, |ann, buf| {
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
pub fn apply_update_primer(
    ws: &mut Workspace,
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
    let vid = resolve_target(ws, view)?;
    let new_seq = sequence.map(|s| parse_oligo(&s)).transpose()?;
    let len = ws.edit_annotations(vid, |ann, buf| {
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
pub fn apply_rescan_primer(
    ws: &mut Workspace,
    view: Option<ViewId>,
    id: PrimerId,
) -> Result<Option<ViewerResponse>, DispatchError> {
    let vid = resolve_target(ws, view)?;
    let len = ws.edit_annotations(vid, |ann, buf| {
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
pub fn apply_add_primer_site(
    ws: &mut Workspace,
    view: Option<ViewId>,
    id: PrimerId,
    enzyme: String,
    overhang: Option<String>,
    flank: Option<String>,
) -> Result<Option<ViewerResponse>, DispatchError> {
    let tail = seqforge_bio::restriction_tail(&enzyme, overhang.as_deref(), flank.as_deref())
        .map_err(|e| DispatchError::InvalidInput(e.to_string()))?;
    let vid = resolve_target(ws, view)?;
    let len = ws.edit_annotations(vid, |ann, buf| {
        let p = ann
            .primer_mut(id)
            .ok_or_else(|| DispatchError::InvalidInput(format!("no primer with id {id}")))?;
        p.sequence = format!("{tail}{}", p.sequence);
        Ok(buf.text.len())
    })?;
    Ok(Some(ViewerResponse::Edited { len, changed: true }))
}

pub fn apply_remove_primer(
    ws: &mut Workspace,
    view: Option<ViewId>,
    id: PrimerId,
) -> Result<Option<ViewerResponse>, DispatchError> {
    let vid = resolve_target(ws, view)?;
    let len = ws.edit_annotations(vid, |ann, buf| {
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

// ── Tests ─────────────────────────────────────────────────────────────────────
//
// These exist to prove the layering, not to re-cover the verbs (the app crate's
// suite already does that against the same functions). What matters here is that
// they compile and run inside a crate that *cannot* link egui: a real edit
// session — open, mutate, copy, paste, undo — driven entirely through
// `Workspace` + `NullHost`.

#[cfg(test)]
mod tests {
    use seqforge_core::{Topology, ViewKind};

    use super::*;
    use crate::host::{Level, NullHost};
    use crate::workspace::Workspace;

    /// A workspace with one active view over `seq`. The headless analogue of the
    /// GUI's `state_with` fixture — note there is no `AppState` to build.
    fn ws_with(seq: &[u8]) -> Workspace {
        let mut ws = Workspace::default();
        let bid = ws
            .buffers
            .new_scratch("test".into(), seq.to_vec(), Topology::Linear);
        ws.add_view(bid, ViewKind::TextView);
        ws
    }

    fn text(ws: &mut Workspace) -> Vec<u8> {
        ws.with_active_buffer(|_, buf, _| buf.text.clone()).unwrap()
    }

    #[test]
    fn insert_delete_and_undo_run_without_a_renderer() {
        let mut ws = ws_with(b"ATGC");
        apply_insert(&mut ws, None, 4, "GGG".into()).unwrap();
        assert_eq!(text(&mut ws), b"ATGCGGG");

        apply_delete(&mut ws, None, 0, 2).unwrap();
        assert_eq!(text(&mut ws), b"GCGGG");

        apply_undo(&mut ws, None).unwrap();
        assert_eq!(
            text(&mut ws),
            b"ATGCGGG",
            "undo is workspace state, not GUI"
        );
    }

    #[test]
    fn copy_and_paste_round_trip_through_a_null_host() {
        let mut ws = ws_with(b"ATGCATGC");
        let mut host = NullHost::default();

        apply_copy(&mut ws, &mut host, None, 0, 4, false).unwrap();
        apply_paste(&mut ws, &mut host, None, 8).unwrap();

        assert_eq!(text(&mut ws), b"ATGCATGCATGC");
    }

    #[test]
    fn copy_reverse_puts_rc_on_clipboard_without_mutating() {
        let mut ws = ws_with(b"ATGCATGC");
        let mut host = NullHost::default();
        apply_copy(&mut ws, &mut host, None, 0, 4, true).unwrap();
        assert_eq!(text(&mut ws), b"ATGCATGC", "copy-as-RC must not edit");
        let slice = host.clipboard_get().expect("clipboard filled");
        assert_eq!(slice.bytes(), b"GCAT", "RC of ATGC");
        apply_paste(&mut ws, &mut host, None, 8).unwrap();
        assert_eq!(text(&mut ws), b"ATGCATGCGCAT");
    }

    #[test]
    fn paste_with_an_empty_host_clipboard_is_an_error_not_a_panic() {
        let mut ws = ws_with(b"ATGC");
        let mut host = NullHost::default();
        let err = apply_paste(&mut ws, &mut host, None, 0).unwrap_err();
        assert!(err.to_string().contains("clipboard is empty"));
    }

    #[test]
    fn cut_hands_the_removed_slice_to_the_host() {
        let mut ws = ws_with(b"ATGCATGC");
        let mut host = NullHost::default();

        apply_cut(&mut ws, &mut host, None, 0, 4).unwrap();

        assert_eq!(text(&mut ws), b"ATGC");
        assert_eq!(
            host.clipboard_get().map(|s| s.bytes().to_vec()),
            Some(b"ATGC".to_vec()),
            "the cut bytes are on the host clipboard, ready to paste"
        );
    }

    #[test]
    fn a_feature_survives_add_rename_remove_headlessly() {
        let mut ws = ws_with(b"ATGCATGCATGC");
        apply_add_feature(&mut ws, None, 0, 6, "CDS".into(), "orf".into(), "+".into()).unwrap();

        let id = ws
            .with_active_buffer(|_, _, ann| ann.iter().next().map(|f| f.id))
            .unwrap()
            .expect("feature was added");

        apply_rename_feature(&mut ws, None, id, "renamed".into()).unwrap();
        let label = ws
            .with_active_buffer(|_, _, ann| ann.get(id).map(|f| f.label.clone()))
            .unwrap();
        assert_eq!(label.as_deref(), Some("renamed"));

        apply_remove_feature(&mut ws, None, id).unwrap();
        let n = ws.with_active_buffer(|_, _, ann| ann.len()).unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn null_host_records_notifications_instead_of_dropping_them() {
        let mut host = NullHost::default();
        crate::host::Host::notify(&mut host, Level::Warning, "careful".into());
        assert_eq!(host.notices, vec![(Level::Warning, "careful".to_string())]);
    }
}
