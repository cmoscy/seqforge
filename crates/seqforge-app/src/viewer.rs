use std::collections::HashSet;
use std::time::Duration;

use egui::{
    Align2, Color32, FontId, Key, Modifiers, Pos2, Rect, Sense, Stroke, Vec2, text::LayoutJob,
};
use seqforge_core::{
    Annotations, Buffer, CutSite, Feature, FeatureId, FeatureKind, Selection, Strand, View, ViewId,
    ViewerRequest, mutations::apply_splice,
};

use crate::command::{AppCommand, PendingCommand};
use crate::config::{Config, LabelOverflow};

/// Cursor blink half-period (ms): the cursor toggles visible/hidden each
/// interval while the viewer has focus.
const BLINK_MS: u64 = 500;

/// Track-changes diff washes for the realized staged-edit preview (Phase 13.6).
/// A faint background channel painted *behind* the per-base glyphs so the
/// A/C/G/T foreground colours (`theme.bases.for_base`) stay legible — the diff
/// never recolours the bases.
const DIFF_ADD_BG: Color32 = Color32::from_rgba_premultiplied(40, 120, 75, 70); // added
const DIFF_DEL_BG: Color32 = Color32::from_rgba_premultiplied(120, 45, 45, 70); // removed
/// Strikethrough line struck through kept-but-deleted bases (drawn over the
/// glyphs, so opaque rather than a wash).
const DIFF_DEL_LINE: Color32 = Color32::from_rgb(214, 92, 92);

/// IUPAC nucleotide alphabet (DNA + ambiguity codes). Typed characters outside
/// it are silently dropped (plan §6) so junk keystrokes never reach the edit
/// layer; `edit::parse_bases` re-validates defensively for CLI/agent input.
const IUPAC: &[u8] = b"ACGTURYSWKMBDHVN";

/// Keep only IUPAC codes from typed text, upper-cased; drop everything else.
fn iupac_filter(s: &str) -> String {
    s.chars()
        .filter_map(|c| {
            let u = c.to_ascii_uppercase();
            (u.is_ascii() && IUPAC.contains(&(u as u8))).then_some(u)
        })
        .collect()
}

/// A staged in-canvas edit (§6 / ROADMAP decision 10). Armed by keyboard input,
/// previewed, and committed to exactly one `ViewerRequest` on `Enter`. The
/// buffer is never touched until commit, so this is the *only* in-canvas path
/// to a mutation. Clipboard ops (cut/copy/paste), reverse-complement, undo/redo
/// and save are **not** staged — their operands live in `AppState` or they are
/// whole commands, so they post directly (mirroring menu/CLI behaviour).
#[derive(Debug, Clone, PartialEq, Eq)]
enum PendingEdit {
    /// Type bases at a cursor; `staged` accumulates the typed IUPAC text.
    Insert { pos: usize, staged: String },
    /// Type bases over a range selection (replaces it on commit).
    Replace {
        start: usize,
        end: usize,
        staged: String,
    },
    /// Backspace/Delete over a selection, or extended ±1 at a cursor.
    Delete { start: usize, end: usize },
    /// ⌘X over a selection. Buffer effect is identical to `Delete` (same
    /// red-struck preview); commit additionally copies the bases to the
    /// clipboard. A distinct variant only so commit lowers to `Cut`, not
    /// `Delete`.
    Cut { start: usize, end: usize },
    /// ⌘V at a cursor / selection start. Buffer effect is identical to an
    /// `Insert` of the clipboard contents (same green-added preview); the
    /// staged bytes come from the clipboard (passed into the preview build),
    /// not from typing, so this variant carries only the position.
    Paste { pos: usize },
}

impl PendingEdit {
    /// Lower to the matching `ViewerRequest`, or `None` if there is nothing to
    /// commit yet (empty staged text, or a zero-width delete).
    fn to_request(&self, view: ViewId) -> Option<ViewerRequest> {
        match self {
            PendingEdit::Insert { pos, staged } => {
                (!staged.is_empty()).then(|| ViewerRequest::Insert {
                    pos: *pos,
                    bases: staged.clone(),
                    view: Some(view),
                })
            }
            PendingEdit::Replace { start, end, staged } => {
                (!staged.is_empty()).then(|| ViewerRequest::Replace {
                    start: *start,
                    end: *end,
                    bases: staged.clone(),
                    view: Some(view),
                })
            }
            PendingEdit::Delete { start, end } => (start < end).then_some(ViewerRequest::Delete {
                start: *start,
                end: *end,
                view: Some(view),
            }),
            PendingEdit::Cut { start, end } => (start < end).then_some(ViewerRequest::Cut {
                start: *start,
                end: *end,
                view: Some(view),
            }),
            PendingEdit::Paste { pos } => Some(ViewerRequest::Paste {
                pos: *pos,
                view: Some(view),
            }),
        }
    }
}

/// Memoized speculative render of the staged edit (Phase 13.6 — realized diff
/// preview). Built by cloning the committed text + annotations and running the
/// real `apply_splice` for `Insert`/`Replace` (so the preview is *provably
/// identical* to the post-commit state — one code path, zero divergence);
/// `Delete` keeps the committed bytes and marks the region struck in place.
///
/// **Transient render-only artifact:** building it never bumps `buf.version`
/// or touches any `Cache<K,V>` — those stay anchored to the committed buffer.
/// Rebuilt only when the fingerprint (`version`, `pending`) changes — never per
/// frame.
#[derive(Debug)]
struct Preview {
    /// Committed `buffer.version` this was built from (half the fingerprint).
    version: u64,
    /// The pending edit that produced it (the other half of the fingerprint).
    pending: PendingEdit,
    /// Speculative sequence bytes to render (== committed text for `Delete`).
    text: Vec<u8>,
    /// Speculative annotations — features reflowed for `Insert`/`Replace`,
    /// untouched for `Delete` (deletions mark in place until commit).
    annotations: Annotations,
    /// `[start, end)` columns of newly added bases (green wash), in render
    /// (preview) coordinates.
    added: Option<(usize, usize)>,
    /// `[start, end)` columns being removed, kept visible + struck (red), in
    /// render coordinates (committed == render for `Delete`).
    deleted: Option<(usize, usize)>,
}

impl Preview {
    /// Build the speculative preview for `pending` over the committed buffer.
    /// `clipboard` supplies the bytes for a staged `Paste` (ignored otherwise).
    fn build(
        buffer: &Buffer,
        annotations: &Annotations,
        pending: &PendingEdit,
        clipboard: Option<&[u8]>,
    ) -> Preview {
        let version = buffer.version;
        // Clone the committed state and run the *real* mutation primitive so
        // the preview can never diverge from what commit produces.
        let mut buf = buffer.clone();
        let mut ann = annotations.clone();
        let (added, deleted) = match pending {
            PendingEdit::Insert { pos, staged } => {
                let bytes = staged.as_bytes();
                apply_splice(&mut buf, &mut ann, *pos..*pos, bytes);
                (Some((*pos, *pos + bytes.len())), None)
            }
            PendingEdit::Replace { start, end, staged } => {
                let bytes = staged.as_bytes();
                apply_splice(&mut buf, &mut ann, *start..*end, bytes);
                (Some((*start, *start + bytes.len())), None)
            }
            // Delete / Cut keep the committed bytes (no virtual buffer): the
            // range is rendered struck-through in place so the user verifies
            // what's leaving. Commit removes it (Cut also copies it first).
            PendingEdit::Delete { start, end } | PendingEdit::Cut { start, end } => {
                (None, Some((*start, *end)))
            }
            // Paste materializes like an Insert of the clipboard bytes.
            PendingEdit::Paste { pos } => {
                let bytes = clipboard.unwrap_or_default();
                apply_splice(&mut buf, &mut ann, *pos..*pos, bytes);
                (Some((*pos, *pos + bytes.len())), None)
            }
        };
        Preview {
            version,
            pending: pending.clone(),
            text: buf.text,
            annotations: ann,
            added,
            deleted,
        }
    }
}

/// Fold one frame's typed bases + backspace/forward-delete counts into the
/// staged edit. **Pure** (no egui) so the staging state machine is unit-tested
/// directly. `range`/`cursor` are the current selection operands; `seq_len`
/// bounds a forward-delete. Typing extends an operand edit or arms one;
/// Backspace trims staged text, else arms/extends a Delete leftward; forward
/// Delete arms/extends rightward. Leaves `pending` `None` if nothing armed.
fn stage_input(
    pending: &mut Option<PendingEdit>,
    bases: &str,
    backspaces: usize,
    del_fwds: usize,
    range: Option<(usize, usize)>,
    cursor: Option<usize>,
    seq_len: usize,
) {
    // 1 · Typed bases extend or arm an operand edit.
    if !bases.is_empty() {
        match pending {
            Some(PendingEdit::Insert { staged, .. })
            | Some(PendingEdit::Replace { staged, .. }) => staged.push_str(bases),
            _ => {
                *pending = match range {
                    Some((start, end)) => Some(PendingEdit::Replace {
                        start,
                        end,
                        staged: bases.to_string(),
                    }),
                    None => cursor.map(|pos| PendingEdit::Insert {
                        pos,
                        staged: bases.to_string(),
                    }),
                };
            }
        }
    }

    // 2 · Backspace trims a staged operand, else arms/extends a Delete left.
    for _ in 0..backspaces {
        match pending {
            Some(PendingEdit::Insert { staged, .. })
            | Some(PendingEdit::Replace { staged, .. })
                if !staged.is_empty() =>
            {
                staged.pop();
            }
            Some(PendingEdit::Delete { start, .. }) | Some(PendingEdit::Cut { start, .. })
                if *start > 0 =>
            {
                *start -= 1
            }
            Some(PendingEdit::Delete { .. }) | Some(PendingEdit::Cut { .. }) => {} // at 0
            _ => {
                *pending = match range {
                    Some((start, end)) => Some(PendingEdit::Delete { start, end }),
                    None => cursor.filter(|&p| p > 0).map(|p| PendingEdit::Delete {
                        start: p - 1,
                        end: p,
                    }),
                };
            }
        }
    }

    // 3 · Forward delete arms/extends a Delete right.
    for _ in 0..del_fwds {
        match pending {
            Some(PendingEdit::Delete { end, .. }) | Some(PendingEdit::Cut { end, .. })
                if *end < seq_len =>
            {
                *end += 1
            }
            Some(PendingEdit::Delete { .. }) | Some(PendingEdit::Cut { .. }) => {}
            _ => {
                *pending = match range {
                    Some((start, end)) => Some(PendingEdit::Delete { start, end }),
                    None => cursor
                        .filter(|&p| p < seq_len)
                        .map(|p| PendingEdit::Delete {
                            start: p,
                            end: p + 1,
                        }),
                };
            }
        }
    }

    // Drop an operand edit whose staged text was fully trimmed away.
    if let Some(PendingEdit::Insert { staged, .. }) | Some(PendingEdit::Replace { staged, .. }) =
        pending
    {
        if staged.is_empty() {
            *pending = None;
        }
    }
}

/// Apply a signed arrow-key step to the selection focus, clamped to the valid
/// cursor range `0..=seq_len` (the upper bound is the insert-at-end position).
/// Pure, so the navigation math is unit-tested without egui.
fn move_focus(base: usize, delta: isize, seq_len: usize) -> usize {
    base.saturating_add_signed(delta).min(seq_len)
}

/// Read this frame's keyboard input on the focused viewer and drive the staged
/// `PendingEdit`. In-canvas edits stage (commit on `Enter`, cancel on `Esc`):
/// typing, Backspace/Delete, **⌘X (Cut)** and **⌘V (Paste)** all arm a staged
/// edit previewed before commit. ⌘C (Copy, read-only) and undo / redo / save
/// post directly — they have no "after" state to preview. `clipboard` gates
/// arming a Paste (nothing to paste ⇒ no stage). Never mutates the buffer or
/// `view.selection`.
fn handle_keyboard(
    pending: &mut Option<PendingEdit>,
    ui: &egui::Ui,
    view: &View,
    seq_len: usize,
    line_width: usize,
    clipboard: Option<&[u8]>,
    cmds: &mut Vec<PendingCommand>,
) {
    let vid = view.id;
    let sel = view.selection;
    let range = sel.filter(|s| !s.is_cursor()).map(|s| s.ordered());
    let cursor = match sel {
        Some(s) if s.is_cursor() => Some(s.focus),
        _ => None,
    };
    let post = |cmds: &mut Vec<PendingCommand>, cmd: AppCommand| cmds.push((cmd, None));
    let post_req = |cmds: &mut Vec<PendingCommand>, req: ViewerRequest| {
        cmds.push((AppCommand::Viewer(req), None))
    };

    // ── Commit / cancel ──
    if ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter)) {
        if let Some(req) = pending.take().and_then(|pe| pe.to_request(vid)) {
            post_req(cmds, req);
        }
        return;
    }
    if ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
        *pending = None;
        return;
    }

    // ── Arrow-key cursor navigation ───────────────────────────────────────
    // Left/Right move one base, Up/Down one line; Shift extends the selection
    // (anchor fixed, focus moves), else collapse to a cursor. Consumed so the
    // enclosing ScrollArea doesn't also scroll on them. An arrow cancels any
    // active stage — same consistency guard as a click / focus loss.
    let arrow = [
        (Key::ArrowLeft, -1isize),
        (Key::ArrowRight, 1),
        (Key::ArrowUp, -(line_width as isize)),
        (Key::ArrowDown, line_width as isize),
    ]
    .into_iter()
    .find_map(|(key, delta)| {
        if ui.input_mut(|i| i.consume_key(Modifiers::SHIFT, key)) {
            Some((delta, true))
        } else if ui.input_mut(|i| i.consume_key(Modifiers::NONE, key)) {
            Some((delta, false))
        } else {
            None
        }
    });
    if let Some((delta, extend)) = arrow {
        // `delta == 0` only for Up/Down before the first layout sets line_width;
        // consume the key but don't move.
        if delta != 0 {
            let base = sel.map_or(0, |s| s.focus);
            let anchor = sel.map_or(base, |s| s.anchor);
            let new_focus = move_focus(base, delta, seq_len);
            let new_sel = if extend {
                Selection {
                    anchor,
                    focus: new_focus,
                }
            } else {
                Selection::cursor(new_focus)
            };
            *pending = None;
            cmds.push((AppCommand::SetSelection(Some(new_sel)), None));
        }
        return;
    }

    // ── Direct-post ops (cancel any staging first) ──
    let cmd = Modifiers::COMMAND;
    let cmd_shift = Modifiers::COMMAND | Modifiers::SHIFT;
    let mut direct: Option<AppCommand> = None;
    // Save / Save-As (⌘S / ⇧⌘S) are handled app-level in the KEYMAP so they
    // fire regardless of which pane holds focus (Phase 15 B3).
    if ui.input_mut(|i| i.consume_key(cmd, Key::Z)) {
        direct = Some(AppCommand::Viewer(ViewerRequest::Undo { view: Some(vid) }));
    } else if ui.input_mut(|i| i.consume_key(cmd_shift, Key::Z))
        || ui.input_mut(|i| i.consume_key(cmd, Key::Y))
    {
        direct = Some(AppCommand::Viewer(ViewerRequest::Redo { view: Some(vid) }));
    }
    if let Some(c) = direct {
        *pending = None;
        post(cmds, c);
        return;
    }

    // ── Clipboard ops (⌘X / ⌘C / ⌘V) ──────────────────────────────────────
    // eframe translates the platform clipboard shortcuts into *semantic*
    // events (`Event::Cut` / `Copy` / `Paste`), NOT `Key::X/C/V` + modifier,
    // so `consume_key` silently misses them. Detect the semantic events (with
    // a raw-key fallback for backends that deliver them as keys). ⌘C copies
    // immediately (read-only, nothing to preview); ⌘X / ⌘V stage a Cut / Paste
    // previewed before commit — both lower to the same Cut/Paste the menu/CLI
    // post.
    let (mut want_cut, mut want_copy, mut want_paste) = (false, false, false);
    ui.input(|i| {
        for ev in &i.events {
            match ev {
                egui::Event::Cut => want_cut = true,
                egui::Event::Copy => want_copy = true,
                egui::Event::Paste(_) => want_paste = true,
                _ => {}
            }
        }
    });
    want_cut |= ui.input_mut(|i| i.consume_key(cmd, Key::X));
    want_copy |= ui.input_mut(|i| i.consume_key(cmd, Key::C));
    want_paste |= ui.input_mut(|i| i.consume_key(cmd, Key::V));

    if want_copy {
        // Read-only — leaves any in-progress stage untouched.
        if let Some((start, end)) = range {
            post_req(
                cmds,
                ViewerRequest::Copy {
                    start,
                    end,
                    view: Some(vid),
                },
            );
        }
        return;
    }
    if want_cut {
        if let Some((start, end)) = range {
            *pending = Some(PendingEdit::Cut { start, end });
        }
        return;
    }
    if want_paste {
        // Only arm if there's something to paste; the preview reads the same
        // in-memory clipboard the commit will.
        if let Some(pos) = cursor.or(range.map(|(s, _)| s)) {
            if clipboard.is_some_and(|c| !c.is_empty()) {
                *pending = Some(PendingEdit::Paste { pos });
            }
        }
        return;
    }

    // ── Staging: typed bases + backspace / forward-delete ──
    let cmd_held = ui.input(|i| i.modifiers.command);
    let events = ui.input(|i| i.events.clone());
    let mut typed = String::new();
    let mut backspaces = 0usize;
    let mut del_fwds = 0usize;
    for ev in &events {
        match ev {
            egui::Event::Text(t) if !cmd_held => typed.push_str(t),
            egui::Event::Key {
                key: Key::Backspace,
                pressed: true,
                modifiers,
                ..
            } if !modifiers.command => backspaces += 1,
            egui::Event::Key {
                key: Key::Delete,
                pressed: true,
                modifiers,
                ..
            } if !modifiers.command => del_fwds += 1,
            _ => {}
        }
    }
    let bases = iupac_filter(&typed);
    stage_input(
        pending, &bases, backspaces, del_fwds, range, cursor, seq_len,
    );
}

// ── Stacking ─────────────────────────────────────────────────────────────────

/// Core greedy interval stacking (port of seqviz `stackElements`).
/// Sorts ranges by start, then packs each into the first row whose last
/// element ends at or before the current range's start.
/// Returns `(item_idx → row, n_rows)`.
pub(crate) fn greedy_stack(ranges: &[(usize, usize)]) -> (Vec<usize>, usize) {
    if ranges.is_empty() {
        return (vec![], 0);
    }
    let mut order: Vec<usize> = (0..ranges.len()).collect();
    order.sort_by_key(|&i| ranges[i].0);

    let mut result = vec![0usize; ranges.len()];
    let mut row_ends: Vec<usize> = Vec::new();

    for idx in order {
        let (start, end) = ranges[idx];
        match row_ends.iter().position(|&e| e <= start) {
            Some(r) => {
                row_ends[r] = end;
                result[idx] = r;
            }
            None => {
                row_ends.push(end);
                result[idx] = row_ends.len() - 1;
            }
        }
    }
    (result, row_ends.len())
}

// ── Per-block layout ─────────────────────────────────────────────────────────
//
// Each rendered block (one line-wrap of the sequence) sizes itself to the
// items actually present in it. Cut-label rows and feature rows are stacked
// *per block* rather than across the whole document, so a section with one
// enzyme doesn't leave blank space matching the heaviest stack elsewhere.

/// Layout decisions for one block.
#[derive(Debug, Default, Clone)]
pub(crate) struct BlockLayout {
    /// `(feat_idx, row_in_block)` for features overlapping this block.
    pub feat_rows: Vec<(usize, usize)>,
    /// `(site_idx, row_in_block)` for cut sites whose `cut_pos` lies in
    /// `[block_start, block_end]`.
    pub cut_rows: Vec<(usize, usize)>,
    pub n_cut_rows: usize,
    /// Total height including ruler + both strands + gap.
    pub height: f32,
}

/// Build per-block layouts and the prefix-sum offsets used to convert block
/// indices to y-coordinates. `offsets[i]` is the y of the top of block `i`;
/// `offsets[n_blocks]` is the total content height.
#[allow(clippy::too_many_arguments)]
fn build_block_layouts(
    annotations: &Annotations,
    cut_sites: &[CutSite],
    seq_len: usize,
    line_width: usize,
    char_width: f32,
    label_char_w: f32,
    cut_label_row_h: f32,
    ruler_h: f32,
    strand_h: f32,
    annot_row_h: f32,
    block_gap: f32,
    // Height of the translation band (0 when no lanes shown). Sits between the
    // bottom strand and the annotation bars in every block.
    trans_band_h: f32,
) -> (Vec<BlockLayout>, Vec<f32>) {
    let n_blocks = seq_len.div_ceil(line_width).max(1);
    let mut layouts: Vec<BlockLayout> = Vec::with_capacity(n_blocks);
    let mut offsets: Vec<f32> = Vec::with_capacity(n_blocks + 1);
    offsets.push(0.0);

    for block_idx in 0..n_blocks {
        let block_start = block_idx * line_width;
        let block_end = (block_start + line_width).min(seq_len);

        // Features overlapping this block: clip ranges to the block for
        // the stacking pass so feature heights reflect what's actually
        // drawn in this row.
        let mut feat_idx_list: Vec<usize> = Vec::new();
        let mut feat_ranges: Vec<(usize, usize)> = Vec::new();
        for (i, f) in annotations.iter().enumerate() {
            if f.range.start < block_end && f.range.end > block_start {
                feat_idx_list.push(i);
                feat_ranges.push((f.range.start.max(block_start), f.range.end.min(block_end)));
            }
        }
        let (feat_local_rows, n_feat_rows) = greedy_stack(&feat_ranges);
        let feat_rows: Vec<(usize, usize)> =
            feat_idx_list.into_iter().zip(feat_local_rows).collect();

        // Cut sites whose top-strand cut sits in this block. Stacking
        // intervals use label half-width converted to base columns so
        // adjacent labels collide as the user expects.
        let mut cut_idx_list: Vec<usize> = Vec::new();
        let mut cut_ranges: Vec<(usize, usize)> = Vec::new();
        for (i, s) in cut_sites.iter().enumerate() {
            if s.cut_pos >= block_start && s.cut_pos <= block_end {
                cut_idx_list.push(i);
                let half_px = s.enzyme.len() as f32 * label_char_w * 0.5 + 4.0;
                let half_bases = (half_px / char_width).ceil() as usize + 1;
                cut_ranges.push((s.cut_pos.saturating_sub(half_bases), s.cut_pos + half_bases));
            }
        }
        let (cut_local_rows, n_cut_rows) = greedy_stack(&cut_ranges);
        let cut_rows: Vec<(usize, usize)> = cut_idx_list.into_iter().zip(cut_local_rows).collect();

        let cut_label_h = n_cut_rows as f32 * cut_label_row_h;
        let annot_section_h = n_feat_rows as f32 * annot_row_h;
        let height =
            cut_label_h + ruler_h + strand_h * 2.0 + trans_band_h + annot_section_h + block_gap;

        offsets.push(offsets.last().copied().unwrap_or(0.0) + height);
        layouts.push(BlockLayout {
            feat_rows,
            cut_rows,
            n_cut_rows,
            height,
        });
    }

    (layouts, offsets)
}

/// Locate the block containing a given y-coordinate (relative to the
/// allocated rect's top). Returns `None` if `rel_y` is negative.
fn y_to_block(rel_y: f32, offsets: &[f32]) -> Option<usize> {
    if rel_y < 0.0 || offsets.len() < 2 {
        return None;
    }
    // `offsets[i]` is the *top* of block i; total height is `offsets[n]`.
    // `partition_point` returns the first i where `offsets[i] > rel_y`, so
    // the containing block is one back.
    let idx = offsets.partition_point(|&o| o <= rel_y).saturating_sub(1);
    if idx >= offsets.len() - 1 {
        None
    } else {
        Some(idx)
    }
}

// ── Geometry helper ───────────────────────────────────────────────────────────

/// Clip a feature to the visible slice of a block and return its bar rect.
/// Returns `None` if the feature doesn't overlap this block at all.
fn annot_bar_rect(
    feat: &Feature,
    block_start: usize,
    block_end: usize,
    bar_row_y: f32, // top of the feature's stacked row
    seq_x0: f32,
    char_width: f32,
    row_h: f32,
) -> Option<Rect> {
    if feat.range.end <= block_start || feat.range.start >= block_end {
        return None;
    }
    let col_s = feat.range.start.max(block_start) - block_start;
    let col_e = feat.range.end.min(block_end) - block_start;
    Some(Rect::from_min_size(
        Pos2::new(seq_x0 + col_s as f32 * char_width, bar_row_y + 1.0),
        Vec2::new((col_e - col_s) as f32 * char_width, row_h - 2.0),
    ))
}

// ── Widget state ──────────────────────────────────────────────────────────────

/// Per-document state for the sequence viewer widget. With per-block
/// layouts there's no derived data worth caching across frames — each
/// block's stacking is O(items in block) and the total work is comparable
/// to a single pass over `features + cut_sites`.
#[derive(Debug, Default)]
pub struct SequenceView {
    drag_start: Option<usize>,
    /// In-canvas staged edit (§6 / ROADMAP decision 10). `None` when not
    /// editing. The buffer is never touched until this commits on `Enter`.
    pending: Option<PendingEdit>,
    /// Memoized realized-diff preview (Phase 13.6), rebuilt only when
    /// `pending` (or the committed version) changes. `None` when not staging.
    preview: Option<Preview>,
    /// Wrap width (bases per line) from the previous frame's layout. Arrow-key
    /// Up/Down navigation needs it, but `line_width` is computed *after*
    /// `handle_keyboard` runs, so we read last frame's value. Only changes on
    /// resize, so the one-frame lag is invisible; `0` until the first layout.
    last_line_width: usize,
    /// The feature under the pointer at the last right-click, captured so the
    /// context menu (Rename / Remove / Translate) can act on a stable
    /// `FeatureId` while the menu is open. `None` when the last secondary click
    /// missed every annotation bar (the menu then renders nothing).
    context_feature: Option<FeatureContext>,
    /// The ORF under the pointer at the last right-click in a frame lane (when no
    /// feature was hit), enabling "Annotate ORF as CDS feature".
    context_orf: Option<OrfPromote>,
    /// Which in-canvas translation lanes are shown (View → Translation). Transient
    /// per-view state (like the active enzyme set), toggled through `AppCommand`.
    pub translation: TranslationDisplay,
    /// Memoized translation lanes, rebuilt only when `(buffer.version, translation)`
    /// changes — never per frame. `None` when no lanes are shown.
    translation_cache: Option<TranslationCache>,
    /// The codon a translation selection is anchored to (forward nt range),
    /// set when a residue is clicked. Shift-clicking another residue keeps this
    /// whole codon selected in *either* direction — the two nt indices in
    /// `Selection` can't disambiguate which codon the anchor belongs to once
    /// the range is extended past a codon boundary. Cleared on any non-codon
    /// selection. See the shift-click handler below.
    translation_anchor: Option<std::ops::Range<usize>>,
}

/// Which in-canvas translation lanes are active.
///
/// Two independent kinds of translation, matching the biology:
/// - **Feature translations** are *feature-anchored* — a feature's protein reads
///   from its own start in its own strand (`/codon_start`), so it never needs a
///   global frame. `show_cds` auto-enables this for every CDS; `features` adds
///   individually-toggled features (of any kind).
/// - **Global frame lanes** (`frames`, indexed `[+1, +2, +3, −1, −2, −3]`) are
///   the *frameless* whole-sequence scan, where a reading frame must be chosen
///   because there's no feature to anchor to.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TranslationDisplay {
    pub frames: [bool; 6],
    /// Auto-translate every CDS feature (anchored to its start/strand).
    pub show_cds: bool,
    /// Individually-toggled features to translate inline (any kind), by id.
    pub features: HashSet<FeatureId>,
    /// Emphasize ORFs in the frame lanes (stops red, Met green, Met→stop wash).
    pub show_orfs: bool,
}

impl TranslationDisplay {
    /// Any lane visible at all?
    pub fn is_active(&self) -> bool {
        self.show_cds || !self.features.is_empty() || self.frames.iter().any(|f| *f)
    }

    /// Should this feature get an inline translation lane? Auto for CDS when
    /// `show_cds`, plus any feature explicitly toggled on.
    fn wants_feature(&self, id: FeatureId, is_cds: bool) -> bool {
        (self.show_cds && is_cds) || self.features.contains(&id)
    }
}

/// Frame lane index → (strand, codon_start). `0..3` forward, `3..6` reverse.
fn frame_spec(i: usize) -> (Strand, usize) {
    if i < 3 {
        (Strand::Forward, i + 1)
    } else {
        (Strand::Reverse, i - 2)
    }
}

/// Human label for a frame-lane index (`+1`…`−3`).
fn frame_label(i: usize) -> &'static str {
    ["+1", "+2", "+3", "−1", "−2", "−3"][i]
}

/// How to colour an amino-acid glyph in a translation lane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AaKind {
    Normal,
    /// Start codon (Met) — green when ORFs are emphasized.
    Start,
    /// Stop codon (`*`) — red when ORFs are emphasized.
    Stop,
}

/// One amino acid placed under the sequence. `pos` is the **forward-strand
/// 0-based position of the codon's middle base**, so the glyph aligns under
/// that column regardless of strand.
#[derive(Debug, Clone)]
struct AaGlyph {
    pos: usize,
    ch: char,
    kind: AaKind,
}

/// One translation lane (a global frame, or the merged CDS lane).
#[derive(Debug, Clone)]
struct TransLane {
    label: String,
    /// The lane's strand — used when promoting one of its ORFs to a feature.
    strand: Strand,
    glyphs: Vec<AaGlyph>,
    /// Met→stop ORF spans (forward nt `[start, end)`) for the wash + promote;
    /// empty unless ORF emphasis is on.
    orf_runs: Vec<(usize, usize)>,
}

/// An ORF the user right-clicked in a frame lane, ready to annotate as a CDS.
#[derive(Debug, Clone, Copy)]
struct OrfPromote {
    start: usize,
    end: usize,
    strand: Strand,
}

/// Memoized translation lanes for the whole buffer, rebuilt only when the
/// sequence version or the display toggles change.
#[derive(Debug, Clone)]
struct TranslationCache {
    version: u64,
    display: TranslationDisplay,
    /// Forward frame lanes then reverse frame lanes, in display order.
    frame_lanes: Vec<TransLane>,
    /// Feature-anchored lanes (auto-CDS + toggled features), each read from
    /// its own start/strand and packed so overlapping features never share a
    /// lane (same greedy interval stacking as the annotation bars).
    feature_lanes: Vec<TransLane>,
}

/// Compute AA glyphs for a whole-sequence reading frame. Reverse frames read the
/// reverse complement but map each glyph back to its forward column.
fn frame_glyphs(seq: &[u8], strand: Strand, frame1: usize) -> Vec<AaGlyph> {
    let offset = frame1 - 1;
    let oriented = match strand {
        Strand::Reverse => seqforge_bio::reverse_complement(seq),
        _ => seq.to_vec(),
    };
    let protein = seqforge_bio::translate(&oriented, Strand::Forward, frame1);
    let l = seq.len();
    protein
        .chars()
        .enumerate()
        .filter_map(|(j, ch)| {
            let o_mid = offset + 3 * j + 1;
            if o_mid >= l {
                return None;
            }
            let pos = match strand {
                Strand::Reverse => l - 1 - o_mid,
                _ => o_mid,
            };
            let kind = match ch {
                '*' => AaKind::Stop,
                'M' => AaKind::Start,
                _ => AaKind::Normal,
            };
            Some(AaGlyph { pos, ch, kind })
        })
        .collect()
}

/// Glyphs for one CDS feature translated in its own frame/strand, placed at
/// forward columns within the feature span.
fn cds_glyphs(
    seq: &[u8],
    range: std::ops::Range<usize>,
    strand: Strand,
    codon_start: usize,
) -> Vec<AaGlyph> {
    let end = range.end.min(seq.len());
    if range.start >= end {
        return Vec::new();
    }
    let sub = &seq[range.start..end];
    let sublen = sub.len();
    let offset = codon_start.clamp(1, 3) - 1;
    let oriented = match strand {
        Strand::Reverse => seqforge_bio::reverse_complement(sub),
        _ => sub.to_vec(),
    };
    let protein = seqforge_bio::translate(&oriented, Strand::Forward, codon_start);
    protein
        .chars()
        .enumerate()
        .filter_map(|(j, ch)| {
            let o_mid = offset + 3 * j + 1;
            if o_mid >= sublen {
                return None;
            }
            let pos = match strand {
                Strand::Reverse => range.start + (sublen - 1 - o_mid),
                _ => range.start + o_mid,
            };
            let kind = match ch {
                '*' => AaKind::Stop,
                'M' => AaKind::Start,
                _ => AaKind::Normal,
            };
            Some(AaGlyph { pos, ch, kind })
        })
        .collect()
}

/// Extend a codon-anchored translation selection so both the origin codon
/// (`anchor`) and the newly clicked codon (`clicked`) stay whole, preserving
/// drag direction. Reaching right, the range runs from the origin's 5′ edge to
/// the clicked codon's 3′ edge; reaching left, from the origin's 3′ edge to the
/// clicked codon's 5′ edge — so a reverse (3′→5′) selection keeps the origin
/// residue's 3′ bases instead of clipping them. Matches whole-codon selection
/// in Benchling / SnapGene.
fn codon_extend(anchor: &std::ops::Range<usize>, clicked: &std::ops::Range<usize>) -> Selection {
    if clicked.start >= anchor.start {
        Selection {
            anchor: anchor.start,
            focus: clicked.end,
        }
    } else {
        Selection {
            anchor: anchor.end,
            focus: clicked.start,
        }
    }
}

/// Build the memoized translation lanes for the current display toggles.
fn build_translation_cache(
    seq: &[u8],
    annotations: &Annotations,
    version: u64,
    display: TranslationDisplay,
) -> TranslationCache {
    // ORF spans (per strand+frame) for the wash, computed once.
    let all_orfs = if display.show_orfs {
        seqforge_bio::find_orfs(seq, 1, true, true)
    } else {
        Vec::new()
    };

    let mut frame_lanes = Vec::new();
    for i in 0..6 {
        if !display.frames[i] {
            continue;
        }
        let (strand, frame1) = frame_spec(i);
        let glyphs = frame_glyphs(seq, strand, frame1);
        let orf_runs = all_orfs
            .iter()
            .filter(|o| o.strand == strand && o.frame == frame1)
            .map(|o| (o.start, o.end))
            .collect();
        frame_lanes.push(TransLane {
            label: frame_label(i).to_string(),
            strand,
            glyphs,
            orf_runs,
        });
    }

    // Feature-anchored translation lanes: every feature the display wants
    // (auto-CDS + individually toggled), each read from its own start/strand.
    // Overlapping features are packed onto separate lanes via the same greedy
    // interval stacking the annotation bars use, so their residues never
    // collide in a shared row.
    let wanted: Vec<&Feature> = annotations
        .iter()
        .filter(|f| {
            let is_cds = matches!(FeatureKind::classify(&f.raw_kind), FeatureKind::Cds);
            display.wants_feature(f.id, is_cds)
        })
        .collect();
    let ranges: Vec<(usize, usize)> = wanted
        .iter()
        .map(|f| (f.range.start, f.range.end))
        .collect();
    let (rows, n_rows) = greedy_stack(&ranges);
    let mut feature_lanes: Vec<TransLane> = (0..n_rows)
        .map(|_| TransLane {
            label: "aa".to_string(),
            strand: Strand::Forward,
            glyphs: Vec::new(),
            orf_runs: Vec::new(),
        })
        .collect();
    for (i, f) in wanted.iter().enumerate() {
        let cs = f
            .qualifiers
            .get("codon_start")
            .and_then(|v| v.as_deref())
            .and_then(|s| s.trim().parse::<usize>().ok())
            .filter(|n| (1..=3).contains(n))
            .unwrap_or(1);
        feature_lanes[rows[i]]
            .glyphs
            .extend(cds_glyphs(seq, f.range.clone(), f.strand, cs));
    }

    TranslationCache {
        version,
        display,
        frame_lanes,
        feature_lanes,
    }
}

/// Snapshot of a right-clicked feature, driving the annotation-bar context
/// menu. Captured by value so the menu closure needs no live annotation borrow.
#[derive(Debug, Clone)]
struct FeatureContext {
    id: FeatureId,
    start: usize,
    end: usize,
    strand: Strand,
    label: String,
    /// Verbatim GenBank feature-type string (for the Edit dialog).
    kind: String,
    /// `true` when the feature classifies as a CDS — the menu offers a CDS
    /// translation prefilled with its reading frame.
    is_cds: bool,
    /// Reading frame from `/codon_start` (1, 2, or 3; defaults to 1).
    codon_start: usize,
}

/// GenBank-style strand flag for the feature forms (`+` / `-` / `.`).
fn strand_flag(strand: Strand) -> &'static str {
    match strand {
        Strand::Reverse => "-",
        Strand::None => ".",
        _ => "+",
    }
}

/// Build the edit-mode `OpenFeatureForm` command pre-filled from a right-clicked
/// / double-clicked feature.
fn open_edit_feature_cmd(fc: &FeatureContext) -> AppCommand {
    AppCommand::OpenFeatureForm {
        id: Some(fc.id),
        label: fc.label.clone(),
        kind: fc.kind.clone(),
        strand: strand_flag(fc.strand).to_string(),
        start: fc.start,
        end: fc.end,
    }
}

impl SequenceView {
    /// Reset transient interaction state on document change.
    pub fn reset(&mut self) {
        self.drag_start = None;
        self.pending = None;
        self.preview = None;
    }

    // ── Stage a destructive edit from outside the canvas (Edit menu) ──────
    // These arm the same `PendingEdit` an in-canvas keystroke would, so a menu
    // Cut/Delete/Paste previews before commit instead of mutating immediately.
    // The caller (`edit::apply_stage_edit`) also focuses the pane so the stage
    // survives (unfocus clears `pending`) and Enter commits it. The preview is
    // built next frame in `show` from `self.pending`; Paste reads the clipboard
    // passed into `show`.

    /// Arm a staged Cut over `[start, end)` (no-op for an empty range).
    pub fn stage_cut(&mut self, start: usize, end: usize) {
        self.pending = (start < end).then_some(PendingEdit::Cut { start, end });
    }

    /// Arm a staged Delete over `[start, end)` (no-op for an empty range).
    pub fn stage_delete(&mut self, start: usize, end: usize) {
        self.pending = (start < end).then_some(PendingEdit::Delete { start, end });
    }

    /// Arm a staged Paste at `pos` (clipboard bytes materialize in the preview).
    pub fn stage_paste(&mut self, pos: usize) {
        self.pending = Some(PendingEdit::Paste { pos });
    }

    /// One-line summary of the staged edit (op + size), or `None` when nothing
    /// is staged. Derived straight from `pending` (+ clipboard length for a
    /// Paste) so the status bar shows it without waiting on a preview rebuild.
    /// Excludes the `⏎ commit · esc cancel` hint — the status bar appends that.
    pub fn staged_summary(&self, clipboard: Option<&[u8]>) -> Option<String> {
        let summary = match self.pending.as_ref()? {
            PendingEdit::Insert { staged, .. } => format!("Insert {} bp", staged.len()),
            PendingEdit::Replace { start, end, staged } => {
                format!("Replace {}→{} bp", end - start, staged.len())
            }
            PendingEdit::Delete { start, end } => format!("Delete {} bp", end - start),
            PendingEdit::Cut { start, end } => format!("Cut {} bp", end - start),
            PendingEdit::Paste { .. } => {
                format!("Paste {} bp", clipboard.map_or(0, <[u8]>::len))
            }
        };
        Some(summary)
    }

    /// Whether an edit is currently staged (armed, awaiting `Enter`/`Esc`).
    /// Test-only for now — the menu-staging applier asserts arming through it.
    #[cfg(test)]
    pub fn is_staging(&self) -> bool {
        self.pending.is_some()
    }

    /// Rebuild the memoized preview iff the fingerprint changed. Keyed on the
    /// committed `version` + the `pending` edit, so it recomputes ~once per
    /// keystroke (when `pending` mutates), never per frame.
    fn refresh_preview(
        &mut self,
        buffer: &Buffer,
        annotations: &Annotations,
        clipboard: Option<&[u8]>,
    ) {
        let Some(pending) = self.pending.clone() else {
            self.preview = None;
            return;
        };
        if let Some(p) = &self.preview {
            if p.version == buffer.version && p.pending == pending {
                return; // fingerprint unchanged — keep the memo.
            }
        }
        self.preview = Some(Preview::build(buffer, annotations, &pending, clipboard));
    }

    /// Render the sequence viewer. Caller must have already resolved an
    /// active view and locked its buffer for read; this widget is
    /// inert if there's no doc — the placeholder rendering lives in
    /// `tabs.rs::ui` so this function can assume a real buffer.
    ///
    /// Selection / feature highlight mutations go through
    /// `AppCommand::SetSelection` / `SelectFeature` (pushed to `cmds`)
    /// so the single-applier invariant from the focus refactor holds.
    // Render entry point: takes the egui ui plus the view/buffer/annotation
    // handles the caller already holds locked, the command sink, config, and
    // focus. Bundling them into a struct would just move the plumbing around.
    #[allow(clippy::too_many_arguments)]
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        view: &mut View,
        buffer: &Buffer,
        annotations: &Annotations,
        cmds: &mut Vec<PendingCommand>,
        cfg: &Config,
        // App-level focus for this pane (`FocusScope::View` and no overlay open),
        // computed by the caller. Gates keyboard editing the same way the
        // terminal gates on `FocusScope::Terminal` — egui widget-focus on the
        // custom painter doesn't persist reliably across frames.
        focused: bool,
        // In-memory clipboard bytes (`AppState.clipboard`), needed so a staged
        // Paste can render the clipboard contents in its green-added preview.
        clipboard: Option<&[u8]>,
    ) {
        // ── Drive in-canvas staging from the keyboard, *before* layout ──
        // Editing must run before sizing so the realized diff preview (a buffer
        // that may be longer/shorter than the committed text) drives block
        // layout this same frame — otherwise an insert's extra bases would be
        // laid out against the stale committed length. Focus is the app-level
        // pane focus (`focused`), the same signal the terminal uses; losing it
        // abandons an uncommitted stage. Menus / CLI / agent post directly —
        // staging is in-canvas only (ROADMAP decision 10).
        if focused {
            handle_keyboard(
                &mut self.pending,
                ui,
                view,
                buffer.text.len(),
                self.last_line_width,
                clipboard,
                cmds,
            );
            ui.ctx()
                .request_repaint_after(Duration::from_millis(BLINK_MS));
        } else {
            self.pending = None;
        }

        // Rebuild the memoized realized-diff preview iff the fingerprint
        // changed (≈ once per keystroke), then take it out as an owned local so
        // the render closure can freely mutate `self` (drag_start / pending)
        // without aliasing the borrow.
        self.refresh_preview(buffer, annotations, clipboard);
        let preview = self.preview.take();
        let staging = preview.is_some();
        // Render source: the speculative preview while staging, else the
        // committed buffer. Derived overlays (cut sites / search) are suppressed
        // while staging — they're anchored to committed coordinates and would
        // otherwise need a re-scan over the virtual sequence each keystroke.
        let (seq, render_ann): (&[u8], &Annotations) = match &preview {
            Some(p) => (p.text.as_slice(), &p.annotations),
            None => (buffer.text.as_slice(), annotations),
        };
        let cut_sites: &[CutSite] = if staging { &[] } else { &view.cut_sites };
        let seq_len = seq.len();

        if seq_len == 0 {
            ui.centered_and_justified(|ui| {
                ui.label("Empty sequence.");
            });
            self.preview = preview;
            return;
        }

        // ── Resolve runtime sizing from config ───────────────────────
        let font_size = cfg.settings.font.sequence_size;
        let label_size = cfg.settings.font.label_size;
        let ruler_size = cfg.settings.font.ruler_size;
        let annot_row_h = (label_size + 2.0 * cfg.settings.editor.label_padding)
            .max(cfg.settings.editor.min_annot_row_height);
        let ruler_h = cfg.settings.editor.ruler_height.max(ruler_size + 2.0);
        let strand_h = cfg.settings.editor.strand_bar_height;
        let block_gap = cfg.settings.editor.block_gap;
        let left_margin = cfg.settings.editor.left_margin;
        let right_margin = cfg.settings.editor.right_margin;
        // Cut-site labels render at `label_size` (same as feature labels) so
        // they stay legible on dense plasmids. Row height tracks the label
        // font + a small gap; label_char_w (measured from the same font
        // below) feeds the stacking math, keeping hit-rects accurate.
        let cut_label_row_h = label_size + 3.0;
        let selection_color = cfg.theme.ui.selection.0;
        let cursor_color = cfg.theme.ui.cursor.0;
        let cut_site_color = cfg.theme.ui.cut_site.0;
        let label_text_light = cfg.theme.ui.label_text.0;
        let label_text_dark = cfg.theme.ui.label_text_alt.0;
        let label_overflow = cfg.settings.editor.label_overflow;

        let font_id = FontId::monospace(font_size);
        let small_font = FontId::proportional(label_size);
        let ruler_font = FontId::proportional(ruler_size);

        // Measure char_width from an actual galley so feature bar positions
        // use the same per-character advance that LayoutJob renders, not the
        // single-glyph metric which can differ due to subpixel rounding.
        let (char_width, char_height, label_char_w) = ui.fonts(|f| {
            let probe = f.layout_no_wrap("A".repeat(64), font_id.clone(), Color32::BLACK);
            let label_probe = f.layout_no_wrap("A".repeat(32), small_font.clone(), Color32::BLACK);
            (
                probe.rect.width() / 64.0,
                f.row_height(&font_id),
                label_probe.rect.width() / 32.0,
            )
        });

        // Fit the line width to the available pane width.
        let avail = (ui.available_width() - left_margin - right_margin).max(char_width);
        let line_width = ((avail / char_width) as usize).max(10);
        let n_blocks = seq_len.div_ceil(line_width);
        // Remember it for next frame's arrow-key Up/Down (computed here, after
        // `handle_keyboard` has already run this frame).
        self.last_line_width = line_width;

        // ── Translation lanes (memoized on version + display) ────────────
        // Suppressed while staging (like cut sites / search): the lanes are
        // derived from the committed buffer and would need a re-scan over the
        // virtual sequence each keystroke otherwise.
        let show_trans = self.translation.is_active() && !staging;
        let mut trans_cache = self.translation_cache.take();
        if show_trans {
            let stale = trans_cache
                .as_ref()
                .is_none_or(|c| c.version != buffer.version || c.display != self.translation);
            if stale {
                trans_cache = Some(build_translation_cache(
                    buffer.text.as_slice(),
                    annotations,
                    buffer.version,
                    self.translation.clone(),
                ));
            }
        } else {
            trans_cache = None;
        }
        let aa_row_h = char_height;
        let band_rows = trans_cache
            .as_ref()
            .map_or(0, |c| c.frame_lanes.len() + c.feature_lanes.len());
        let trans_band_h = band_rows as f32 * aa_row_h;

        // Per-block layout: each block sizes itself to the items it contains.
        // `block_offsets[i]` is the y-coord (within the allocated rect) of
        // the top of block i; `block_offsets[n_blocks]` is the total height.
        let (block_layouts, block_offsets) = build_block_layouts(
            render_ann,
            cut_sites,
            seq_len,
            line_width,
            char_width,
            label_char_w,
            cut_label_row_h,
            ruler_h,
            strand_h,
            annot_row_h,
            block_gap,
            trans_band_h,
        );
        let total_height = *block_offsets.last().unwrap_or(&0.0);
        let content_width = left_margin + line_width as f32 * char_width + right_margin;
        let alloc_width = content_width.max(ui.available_width());

        // Consume the one-shot scroll request: center the target block in
        // the viewport this frame, then clear so the user can scroll freely.
        let scroll_offset = view.scroll_to.take().map(|pos| {
            let block_idx = (pos / line_width).min(n_blocks.saturating_sub(1));
            let block_top = block_offsets[block_idx];
            let block_h = block_layouts[block_idx].height;
            let viewport_h = ui.available_height();
            (block_top - viewport_h / 2.0 + block_h / 2.0).max(0.0)
        });

        let mut scroll_area = egui::ScrollArea::vertical().auto_shrink([false, false]);
        if let Some(offset) = scroll_offset {
            scroll_area = scroll_area.vertical_scroll_offset(offset);
        }
        let mut computed_visible: Option<(usize, usize)> = None;
        scroll_area.show(ui, |ui| {
            let (response, painter) = ui.allocate_painter(
                Vec2::new(alloc_width, total_height),
                Sense::click_and_drag(),
            );
            let rect = response.rect;
            let clip = painter.clip_rect();
            let text_color = ui.visuals().text_color();
            let seq_x0 = rect.min.x + left_margin;

            // ── Pass 1: collect click-rects for all interactive elements ──

            let mut annot_hits: Vec<(Rect, usize)> = Vec::new();
            let mut search_hit_rects: Vec<(Rect, usize)> = Vec::new();
            // `Vec<(label_rect, site_idx)>` — label is the click + hover
            // target. The full staple only paints when this rect is
            // hovered (or the site is the persistent click selection).
            let mut cut_site_rects: Vec<(Rect, usize)> = Vec::new();
            // ORF runs in the frame lanes — click targets for "Annotate as CDS".
            let mut orf_hits: Vec<(Rect, OrfPromote)> = Vec::new();
            // Codon cells in every translation lane — clicking a residue selects
            // its 3 nucleotides. One rect per cell segment visible in this block;
            // the payload is the codon's forward nt range.
            let mut aa_hits: Vec<(Rect, std::ops::Range<usize>)> = Vec::new();
            for block_idx in 0..n_blocks {
                let block_y = rect.min.y + block_offsets[block_idx];
                let block_h = block_layouts[block_idx].height;
                if block_y + block_h < clip.min.y {
                    continue;
                }
                if block_y > clip.max.y {
                    break;
                }
                let block_start = block_idx * line_width;
                let block_end = (block_start + line_width).min(seq_len);
                let layout = &block_layouts[block_idx];
                let cut_label_h = layout.n_cut_rows as f32 * cut_label_row_h;
                let top_y = block_y + cut_label_h + ruler_h;
                let bot_y = top_y + strand_h;
                // Annotation bars sit below the strands *and* the translation band.
                let annot_base_y = bot_y + strand_h + trans_band_h;

                for &(feat_idx, row) in &layout.feat_rows {
                    let feat = render_ann
                        .by_position(feat_idx)
                        .expect("feat_idx from this frame's layout");
                    let bar_row_y = annot_base_y + row as f32 * annot_row_h;
                    if let Some(r) = annot_bar_rect(
                        feat,
                        block_start,
                        block_end,
                        bar_row_y,
                        seq_x0,
                        char_width,
                        annot_row_h,
                    ) {
                        annot_hits.push((r, feat_idx));
                    }
                }
                // ORF-run click targets in the translation band (one rect per
                // run segment visible in this block; all map to the full ORF).
                if let Some(tc) = &trans_cache {
                    let trans_y = bot_y + strand_h;
                    for (lane_i, lane) in tc.frame_lanes.iter().enumerate() {
                        let lane_y = trans_y + lane_i as f32 * aa_row_h;
                        for &(rs, re) in &lane.orf_runs {
                            let vis_s = rs.max(block_start);
                            let vis_e = re.min(block_end);
                            if vis_s < vis_e {
                                let sx = seq_x0 + (vis_s - block_start) as f32 * char_width;
                                let sw = (vis_e - vis_s) as f32 * char_width;
                                orf_hits.push((
                                    Rect::from_min_size(
                                        Pos2::new(sx, lane_y),
                                        Vec2::new(sw, aa_row_h),
                                    ),
                                    OrfPromote {
                                        start: rs,
                                        end: re,
                                        strand: lane.strand,
                                    },
                                ));
                            }
                        }
                    }
                    // Codon-cell click targets across all lanes (frame + feature).
                    for (lane_i, lane) in tc
                        .frame_lanes
                        .iter()
                        .chain(tc.feature_lanes.iter())
                        .enumerate()
                    {
                        let lane_y = trans_y + lane_i as f32 * aa_row_h;
                        for g in &lane.glyphs {
                            if g.pos < block_start || g.pos >= block_end {
                                continue;
                            }
                            let ncs = g.pos.saturating_sub(1).max(block_start);
                            let nce = (g.pos + 2).min(block_end);
                            if ncs < nce {
                                let cx = seq_x0 + (ncs - block_start) as f32 * char_width;
                                let cw = (nce - ncs) as f32 * char_width;
                                aa_hits.push((
                                    Rect::from_min_size(
                                        Pos2::new(cx, lane_y),
                                        Vec2::new(cw, aa_row_h),
                                    ),
                                    g.pos.saturating_sub(1)..(g.pos + 2).min(seq_len),
                                ));
                            }
                        }
                    }
                }
                if !staging {
                    for (hit_idx, hit) in view.search_hits.iter().enumerate() {
                        let vis_s = hit.start.max(block_start).min(block_end);
                        let vis_e = hit.end.min(block_end);
                        if vis_s < vis_e && vis_e > block_start {
                            let sx = seq_x0 + (vis_s - block_start) as f32 * char_width;
                            let sw = (vis_e - vis_s) as f32 * char_width;
                            search_hit_rects.push((
                                Rect::from_min_size(
                                    Pos2::new(sx, top_y),
                                    Vec2::new(sw, strand_h * 2.0),
                                ),
                                hit_idx,
                            ));
                        }
                    }
                }
                // Click target is the label only — not the staple line through the
                // strands. Clicking the strand near a cut site places a cursor as
                // expected; only the label is the intentional selection handle.
                // (`cut_sites` is empty while staging, so this loop is inert then.)
                for &(site_idx, row) in &layout.cut_rows {
                    let site = &cut_sites[site_idx];
                    let cx = seq_x0 + (site.cut_pos - block_start) as f32 * char_width;
                    let label_w = site.enzyme.len() as f32 * label_char_w + 8.0;
                    cut_site_rects.push((
                        Rect::from_center_size(
                            Pos2::new(cx, block_y + (row as f32 + 0.5) * cut_label_row_h),
                            Vec2::new(label_w, cut_label_row_h),
                        ),
                        site_idx,
                    ));
                }
            }

            // ── Interactions ──────────────────────────────────────────────

            let ptr = response.interact_pointer_pos();
            let ptr_seq = ptr.and_then(|p| {
                screen_to_seq(
                    p,
                    rect,
                    char_width,
                    line_width,
                    seq_len,
                    &block_offsets,
                    left_margin,
                )
            });

            let shift_held = ui.input(|i| i.modifiers.shift);

            // Helpers that close over `cmds` so each branch is one push.
            // The viewer never mutates `view.selection` / `selected_feature`
            // directly; one-frame visual lag is the documented trade-off
            // (see focus-refactor §2, "render never mutates state").
            let push_sel = |cmds: &mut Vec<PendingCommand>, sel: Option<Selection>| {
                cmds.push((AppCommand::SetSelection(sel), None));
            };
            let push_feat = |cmds: &mut Vec<PendingCommand>, feat: Option<FeatureId>| {
                cmds.push((AppCommand::SelectFeature(feat), None));
            };

            if response.clicked() {
                if let Some(pos) = ptr {
                    if shift_held {
                        // Shift+click extends the selection. Over a translation
                        // codon cell, keep *both* the origin codon and the clicked
                        // codon whole, in whichever direction we're reaching — the
                        // two nt indices in `Selection` can't say which codon the
                        // anchor belongs to once extended, so `translation_anchor`
                        // remembers the origin codon (set on the residue click).
                        let over_codon = aa_hits
                            .iter()
                            .find(|(r, _)| r.contains(pos))
                            .map(|(_, c)| c.clone());
                        let new_sel = match (over_codon, self.translation_anchor.clone()) {
                            (Some(codon), Some(ac)) => Some(codon_extend(&ac, &codon)),
                            // Residue click but no codon anchor yet: snap the focus
                            // to the clicked codon relative to the nt-level anchor.
                            (Some(codon), None) => Some(match view.selection {
                                Some(sel) => {
                                    let focus = if codon.start >= sel.anchor {
                                        codon.end
                                    } else {
                                        codon.start
                                    };
                                    Selection {
                                        anchor: sel.anchor,
                                        focus,
                                    }
                                }
                                None => Selection::range(codon.start, codon.end),
                            }),
                            // Not over a residue: nucleotide-level extend as before.
                            (None, _) => ptr_seq.map(|p| match view.selection {
                                Some(sel) => Selection {
                                    anchor: sel.anchor,
                                    focus: p,
                                },
                                None => Selection::cursor(p),
                            }),
                        };
                        if let Some(sel) = new_sel {
                            push_sel(cmds, Some(sel));
                        }
                    } else {
                        // Any fresh (non-extending) click clears the codon anchor;
                        // a residue click re-sets it in that branch below.
                        self.translation_anchor = None;
                        if let Some(&(_, feat_idx)) =
                            annot_hits.iter().find(|(r, _)| r.contains(pos))
                        {
                            let feat = render_ann
                                .by_position(feat_idx)
                                .expect("feat_idx from this frame's layout");
                            push_sel(
                                cmds,
                                Some(Selection::range(feat.range.start, feat.range.end)),
                            );
                            push_feat(cmds, Some(feat.id));
                        } else if let Some(&(_, hit_idx)) =
                            search_hit_rects.iter().find(|(r, _)| r.contains(pos))
                        {
                            let hit = &view.search_hits[hit_idx];
                            push_sel(cmds, Some(Selection::range(hit.start, hit.end)));
                            push_feat(cmds, None);
                        } else if let Some(&(_, site_idx)) =
                            cut_site_rects.iter().find(|(r, _)| r.contains(pos))
                        {
                            let site = &cut_sites[site_idx];
                            push_sel(
                                cmds,
                                Some(Selection::range(
                                    site.recognition_start,
                                    site.recognition_end,
                                )),
                            );
                            push_feat(cmds, None);
                        } else if let Some((_, codon)) =
                            aa_hits.iter().find(|(r, _)| r.contains(pos))
                        {
                            // Click a residue → select its codon and anchor to it.
                            self.translation_anchor = Some(codon.clone());
                            push_sel(cmds, Some(Selection::range(codon.start, codon.end)));
                            push_feat(cmds, None);
                        } else if let Some(seq_pos) = ptr_seq {
                            push_sel(cmds, Some(Selection::cursor(seq_pos)));
                            push_feat(cmds, None);
                        } else {
                            push_sel(cmds, None);
                            push_feat(cmds, None);
                        }
                    }
                }
            }

            // ── Double-click a feature → Edit dialog ──────────────────────
            if response.double_clicked() {
                if let Some(p) = ptr {
                    if let Some(fc) = annot_hits
                        .iter()
                        .find(|(r, _)| r.contains(p))
                        .and_then(|&(_, fi)| render_ann.by_position(fi))
                        .map(|f| FeatureContext {
                            id: f.id,
                            start: f.range.start,
                            end: f.range.end,
                            strand: f.strand,
                            kind: f.raw_kind.clone(),
                            label: f.label.clone(),
                            is_cds: false,
                            codon_start: 1,
                        })
                    {
                        cmds.push((open_edit_feature_cmd(&fc), None));
                    }
                }
            }

            // ── Right-click a feature → context menu ──────────────────────
            // Capture which feature (if any) was under the pointer at the
            // secondary click; the menu below reads this stable snapshot.
            if response.secondary_clicked() {
                self.context_feature = ptr.and_then(|p| {
                    annot_hits
                        .iter()
                        .find(|(r, _)| r.contains(p))
                        .and_then(|&(_, fi)| render_ann.by_position(fi))
                        .map(|f| FeatureContext {
                            id: f.id,
                            start: f.range.start,
                            end: f.range.end,
                            strand: f.strand,
                            kind: f.raw_kind.clone(),
                            label: f.label.clone(),
                            is_cds: matches!(FeatureKind::classify(&f.raw_kind), FeatureKind::Cds),
                            codon_start: f
                                .qualifiers
                                .get("codon_start")
                                .and_then(|v| v.as_deref())
                                .and_then(|s| s.trim().parse::<usize>().ok())
                                .filter(|n| (1..=3).contains(n))
                                .unwrap_or(1),
                        })
                });
                // If no feature was hit, capture an ORF run under the pointer.
                self.context_orf = if self.context_feature.is_some() {
                    None
                } else {
                    ptr.and_then(|p| {
                        orf_hits
                            .iter()
                            .find(|(r, _)| r.contains(p))
                            .map(|(_, o)| *o)
                    })
                };
            }
            let ctx_feat = self.context_feature.clone();
            let ctx_orf = self.context_orf;
            // Is the right-clicked feature currently translated inline?
            let feat_translated = ctx_feat
                .as_ref()
                .is_some_and(|fc| self.translation.features.contains(&fc.id));
            response.context_menu(|ui| {
                let Some(fc) = ctx_feat else {
                    // No feature under the click — offer ORF promotion if one is.
                    if let Some(orf) = ctx_orf {
                        ui.label(egui::RichText::new("ORF").strong());
                        ui.separator();
                        if ui.button("Annotate as CDS feature").clicked() {
                            cmds.push((
                                AppCommand::Viewer(ViewerRequest::AddFeature {
                                    start: orf.start,
                                    end: orf.end,
                                    kind: "CDS".to_string(),
                                    label: "ORF".to_string(),
                                    strand: strand_flag(orf.strand).to_string(),
                                    view: None,
                                }),
                                None,
                            ));
                            ui.close_menu();
                        }
                    } else {
                        ui.close_menu();
                    }
                    return;
                };
                let title = if fc.label.is_empty() {
                    format!("Feature {}", fc.id)
                } else {
                    fc.label.clone()
                };
                ui.label(egui::RichText::new(title).strong());
                ui.separator();
                if ui.button("Edit…").clicked() {
                    cmds.push((open_edit_feature_cmd(&fc), None));
                    ui.close_menu();
                }
                if ui.button("Rename…").clicked() {
                    cmds.push((
                        AppCommand::OpenRenameFeature {
                            id: fc.id,
                            label: fc.label.clone(),
                        },
                        None,
                    ));
                    ui.close_menu();
                }
                if ui.button("Remove").clicked() {
                    cmds.push((
                        AppCommand::Viewer(ViewerRequest::RemoveFeature {
                            id: fc.id,
                            view: None,
                        }),
                        None,
                    ));
                    ui.close_menu();
                }
                ui.separator();
                // Inline translation, anchored to the feature's start + strand
                // (no global frame needed). Auto-on for CDS; toggle for any kind.
                let inline_label = if feat_translated {
                    "Hide translation"
                } else {
                    "Show translation"
                };
                if ui.button(inline_label).clicked() {
                    cmds.push((AppCommand::ToggleFeatureTranslation(fc.id), None));
                    ui.close_menu();
                }
                // Separate on-demand window (arbitrary strand/frame, all-6 view).
                if ui.button("Translate in window…").clicked() {
                    cmds.push((
                        AppCommand::OpenTranslation {
                            title: if fc.label.is_empty() {
                                "Feature".to_string()
                            } else {
                                fc.label.clone()
                            },
                            start: fc.start,
                            end: fc.end,
                            strand: fc.strand,
                            frame: if fc.is_cds { fc.codon_start } else { 1 },
                        },
                        None,
                    ));
                    ui.close_menu();
                }
            });

            if response.drag_started() {
                let on_annot = ptr.is_some_and(|p| annot_hits.iter().any(|(r, _)| r.contains(p)));
                let on_hit =
                    ptr.is_some_and(|p| search_hit_rects.iter().any(|(r, _)| r.contains(p)));
                let on_site =
                    ptr.is_some_and(|p| cut_site_rects.iter().any(|(r, _)| r.contains(p)));
                if !on_annot && !on_hit && !on_site {
                    // drag_start is view-local (not document state) so it
                    // stays on `self`.
                    self.drag_start = ptr_seq;
                    push_feat(cmds, None);
                    push_sel(cmds, ptr_seq.map(Selection::cursor));
                }
            }
            if response.dragged() {
                if let (Some(anchor), Some(focus)) = (self.drag_start, ptr_seq) {
                    push_sel(cmds, Some(Selection { anchor, focus }));
                }
            }
            if response.drag_stopped() {
                self.drag_start = None;
                // A zero-length drag stays as a cursor; non-zero stays as a range.
            }

            // ── In-canvas editing (Phase 13, staged §6) ───────────────────
            // Keyboard staging + preview were processed before layout (above);
            // here we only cancel staging on a click (the click also moves the
            // cursor). The realized diff is rendered from `preview` in pass 2.
            if response.clicked() {
                self.pending = None;
            }
            // Cursor is solid when unfocused, blinks (~2 Hz) when focused.
            let blink_on =
                !focused || (ui.input(|i| i.time) * 1000.0 / BLINK_MS as f64) as i64 % 2 == 0;

            // ── Pass 2: render all visible blocks ─────────────────────────

            for block_idx in 0..n_blocks {
                let block_y = rect.min.y + block_offsets[block_idx];
                let block_h = block_layouts[block_idx].height;
                if block_y + block_h < clip.min.y {
                    continue;
                }
                if block_y > clip.max.y {
                    break;
                }

                let block_start = block_idx * line_width;
                let block_end = (block_start + line_width).min(seq_len);
                let block_len = block_end - block_start;
                let layout = &block_layouts[block_idx];
                let cut_label_h = layout.n_cut_rows as f32 * cut_label_row_h;

                // ── Ruler ─────────────────────────────────────────────────
                let ruler_y = block_y + cut_label_h;
                let ruler_text = cfg.theme.ui.ruler_text.0;
                for col in 0..block_len {
                    let abs = block_start + col;
                    if abs == 0 || (abs + 1) % 10 == 0 {
                        painter.text(
                            Pos2::new(seq_x0 + col as f32 * char_width, ruler_y),
                            Align2::LEFT_TOP,
                            format!("{}", abs + 1),
                            ruler_font.clone(),
                            ruler_text,
                        );
                    }
                }

                let top_y = ruler_y + ruler_h;
                let bot_y = top_y + strand_h;
                // Translation band sits directly under the bottom strand; the
                // annotation bars follow it.
                let trans_y = bot_y + strand_h;
                let annot_base_y = trans_y + trans_band_h;

                // ── Search hit highlights (behind selection and text) ──────
                // Suppressed while staging — derived overlays are anchored to
                // committed coordinates and refresh on commit.
                if !staging {
                    for hit in &view.search_hits {
                        let vis_s = hit.start.max(block_start).min(block_end);
                        let vis_e = hit.end.min(block_end); // clamp wrap-arounds
                        if vis_s < vis_e && vis_e > block_start {
                            let sx = seq_x0 + (vis_s - block_start) as f32 * char_width;
                            let sw = (vis_e - vis_s) as f32 * char_width;
                            let color = search_hit_color(&cfg.theme, hit.strand);
                            painter.rect_filled(
                                Rect::from_min_size(
                                    Pos2::new(sx, top_y),
                                    Vec2::new(sw, char_height),
                                ),
                                2.0,
                                color,
                            );
                            painter.rect_filled(
                                Rect::from_min_size(
                                    Pos2::new(sx, bot_y),
                                    Vec2::new(sw, char_height),
                                ),
                                2.0,
                                color,
                            );
                        }
                    }
                }

                // ── Selection highlight / cursor (behind text) ────────────
                // Suppressed while staging — the realized diff wash (below) is
                // the active visual; selection coords are committed-space and
                // would mislead against the speculative buffer.
                if let Some(sel) = view.selection.filter(|_| !staging) {
                    if sel.is_cursor() {
                        // Thin vertical line between bases spanning both strands.
                        let pos = sel.anchor;
                        if blink_on && pos >= block_start && pos <= block_end {
                            let cx = seq_x0 + (pos - block_start) as f32 * char_width;
                            painter.rect_filled(
                                Rect::from_min_size(
                                    Pos2::new(cx - 0.75, top_y),
                                    Vec2::new(1.5, strand_h * 2.0),
                                ),
                                0.0,
                                cursor_color,
                            );
                        }
                    } else {
                        let (sel_s, sel_e) = sel.ordered();
                        let vis_s = sel_s.max(block_start);
                        let vis_e = sel_e.min(block_end);
                        if vis_s < vis_e {
                            let sx = seq_x0 + (vis_s - block_start) as f32 * char_width;
                            let sw = (vis_e - vis_s) as f32 * char_width;
                            painter.rect_filled(
                                Rect::from_min_size(
                                    Pos2::new(sx, top_y),
                                    Vec2::new(sw, char_height),
                                ),
                                0.0,
                                selection_color,
                            );
                            painter.rect_filled(
                                Rect::from_min_size(
                                    Pos2::new(sx, bot_y),
                                    Vec2::new(sw, char_height),
                                ),
                                0.0,
                                selection_color.gamma_multiply(0.7),
                            );
                        }
                    }
                }

                // ── Realized diff wash (Phase 13.6) ───────────────────────
                // The diff is drawn over the *preview* bytes (this block is
                // already laid out against them), behind the strand glyphs so
                // the per-base A/C/G/T colours stay legible. `added`/`deleted`
                // are render-space column ranges on the preview. Strikethrough
                // on deleted bases lands in 13.6b.
                if let Some(p) = &preview {
                    if let Some((rs, re)) = p.added {
                        let vis_s = rs.max(block_start);
                        let vis_e = re.min(block_end);
                        if vis_s < vis_e {
                            let sx = seq_x0 + (vis_s - block_start) as f32 * char_width;
                            let sw = (vis_e - vis_s) as f32 * char_width;
                            painter.rect_filled(
                                Rect::from_min_size(
                                    Pos2::new(sx, top_y),
                                    Vec2::new(sw, strand_h * 2.0),
                                ),
                                0.0,
                                DIFF_ADD_BG,
                            );
                        }
                    }
                    if let Some((rs, re)) = p.deleted {
                        let vis_s = rs.max(block_start);
                        let vis_e = re.min(block_end);
                        if vis_s < vis_e {
                            let sx = seq_x0 + (vis_s - block_start) as f32 * char_width;
                            let sw = (vis_e - vis_s) as f32 * char_width;
                            painter.rect_filled(
                                Rect::from_min_size(
                                    Pos2::new(sx, top_y),
                                    Vec2::new(sw, strand_h * 2.0),
                                ),
                                0.0,
                                DIFF_DEL_BG,
                            );
                        }
                    }
                }

                // ── 5'/3' labels (first block only) ───────────────────────
                if block_idx == 0 {
                    painter.text(
                        Pos2::new(rect.min.x, top_y),
                        Align2::LEFT_TOP,
                        "5'",
                        font_id.clone(),
                        text_color.gamma_multiply(0.45),
                    );
                    painter.text(
                        Pos2::new(rect.min.x, bot_y),
                        Align2::LEFT_TOP,
                        "3'",
                        font_id.clone(),
                        text_color.gamma_multiply(0.45),
                    );
                }

                // ── Strands ───────────────────────────────────────────────
                let top_galley = build_strand_galley(
                    ui,
                    &seq[block_start..block_end],
                    &font_id,
                    1.0,
                    &cfg.theme,
                );
                painter.galley(Pos2::new(seq_x0, top_y), top_galley, text_color);

                // Bottom strand is the complement of the visible block,
                // derived on demand — never stored on the buffer (see
                // docs/architecture.md: derived sequence data is computed,
                // not persisted).
                let block_comp = seqforge_bio::complement(&seq[block_start..block_end]);
                let bot_galley = build_strand_galley(ui, &block_comp, &font_id, 0.65, &cfg.theme);
                painter.galley(Pos2::new(seq_x0, bot_y), bot_galley, text_color);

                // ── Translation band (frame + CDS amino-acid lanes) ───────
                if let Some(tc) = &trans_cache {
                    let aa_normal = text_color.gamma_multiply(0.72);
                    let aa_stop = cfg.theme.translation.stop.0;
                    let aa_start = cfg.theme.translation.start.0;
                    let orf_wash = cfg.theme.translation.orf_wash.0;
                    let show_orfs = self.translation.show_orfs;
                    for (lane_i, lane) in tc
                        .frame_lanes
                        .iter()
                        .chain(tc.feature_lanes.iter())
                        .enumerate()
                    {
                        let lane_y = trans_y + lane_i as f32 * aa_row_h;
                        // ORF wash behind the lane (frame lanes only).
                        if show_orfs {
                            for &(rs, re) in &lane.orf_runs {
                                let vis_s = rs.max(block_start);
                                let vis_e = re.min(block_end);
                                if vis_s < vis_e {
                                    let sx = seq_x0 + (vis_s - block_start) as f32 * char_width;
                                    let sw = (vis_e - vis_s) as f32 * char_width;
                                    painter.rect_filled(
                                        Rect::from_min_size(
                                            Pos2::new(sx, lane_y),
                                            Vec2::new(sw, aa_row_h),
                                        ),
                                        0.0,
                                        orf_wash,
                                    );
                                }
                            }
                        }
                        // Lane label in the left margin (first block only).
                        if block_idx == 0 {
                            painter.text(
                                Pos2::new(rect.min.x, lane_y),
                                Align2::LEFT_TOP,
                                &lane.label,
                                small_font.clone(),
                                text_color.gamma_multiply(0.5),
                            );
                        }
                        // Amino-acid glyphs whose codon midpoint falls in this block.
                        for g in &lane.glyphs {
                            if g.pos < block_start || g.pos >= block_end {
                                continue;
                            }
                            let color = if show_orfs {
                                match g.kind {
                                    AaKind::Stop => aa_stop,
                                    AaKind::Start => aa_start,
                                    AaKind::Normal => aa_normal,
                                }
                            } else {
                                aa_normal
                            };
                            // Codon cell spanning the residue's 3 nucleotides
                            // (clamped to the block at a wrap). The faint outline
                            // groups the codon and marks the click target that
                            // selects those bases (hit-rect collected in pass 1).
                            let ncs = g.pos.saturating_sub(1).max(block_start);
                            let nce = (g.pos + 2).min(block_end);
                            if ncs < nce {
                                let cx = seq_x0 + (ncs - block_start) as f32 * char_width;
                                let cw = (nce - ncs) as f32 * char_width;
                                painter.rect_stroke(
                                    Rect::from_min_size(
                                        Pos2::new(cx, lane_y),
                                        Vec2::new(cw, aa_row_h),
                                    ),
                                    2.0,
                                    Stroke::new(1.0, text_color.gamma_multiply(0.16)),
                                    egui::StrokeKind::Inside,
                                );
                            }
                            let x = seq_x0
                                + (g.pos - block_start) as f32 * char_width
                                + char_width * 0.5;
                            painter.text(
                                Pos2::new(x, lane_y),
                                Align2::CENTER_TOP,
                                g.ch,
                                font_id.clone(),
                                color,
                            );
                        }
                    }
                }

                // ── Delete strikethrough (Phase 13.6b) ────────────────────
                // Deleted bases are kept visible (verify-what's-leaving) with a
                // strikethrough struck through both strands, drawn *over* the
                // glyphs. The red wash behind them was painted in pass 2's diff
                // block above.
                if let Some((rs, re)) = preview.as_ref().and_then(|p| p.deleted) {
                    let vis_s = rs.max(block_start);
                    let vis_e = re.min(block_end);
                    if vis_s < vis_e {
                        let sx = seq_x0 + (vis_s - block_start) as f32 * char_width;
                        let ex = seq_x0 + (vis_e - block_start) as f32 * char_width;
                        let stroke = Stroke::new(1.5, DIFF_DEL_LINE);
                        for strand_top in [top_y, bot_y] {
                            let my = strand_top + char_height * 0.5;
                            painter.line_segment([Pos2::new(sx, my), Pos2::new(ex, my)], stroke);
                        }
                    }
                }

                // ── Annotation bars (below strands) ───────────────────────
                for &(feat_idx, row) in &layout.feat_rows {
                    let feat = render_ann
                        .by_position(feat_idx)
                        .expect("feat_idx from this frame's layout");
                    let bar_row_y = annot_base_y + row as f32 * annot_row_h;
                    if let Some(bar) = annot_bar_rect(
                        feat,
                        block_start,
                        block_end,
                        bar_row_y,
                        seq_x0,
                        char_width,
                        annot_row_h,
                    ) {
                        // Resolve the selected *id* to this frame's position for
                        // the highlight — position never leaves the frame.
                        let is_selected = view.selected_feature == Some(feat.id);
                        painter.rect_filled(
                            bar,
                            2.0,
                            cfg.theme
                                .feature_color(FeatureKind::classify(&feat.raw_kind)),
                        );
                        if is_selected {
                            painter.rect_stroke(
                                bar,
                                2.0,
                                Stroke::new(1.5, Color32::WHITE),
                                egui::StrokeKind::Inside,
                            );
                        }
                        if !feat.label.is_empty() {
                            let swatch = cfg
                                .theme
                                .feature_color(FeatureKind::classify(&feat.raw_kind));
                            let fg = crate::config::theme::pick_contrast(
                                swatch,
                                label_text_light,
                                label_text_dark,
                            );
                            paint_feature_label(
                                &painter,
                                &small_font,
                                fg,
                                label_overflow,
                                label_char_w,
                                bar,
                                &feat.label,
                            );
                        }
                    }
                }

                // ── Cut site tick marks + labels (resting state) ──────────
                // SnapGene-style: at rest each site shows only a label in
                // its stacked row plus a short tick descending from the
                // label toward the ruler. The full staple — descender
                // through both strands, overhang step, and wedge arrows —
                // appears only when the user hovers the label (see the
                // hover-reveal pass below).
                let cut_label_font = FontId::proportional(label_size);
                for &(site_idx, row) in &layout.cut_rows {
                    let site = &view.cut_sites[site_idx];
                    let top_cut = site.cut_pos;
                    let tcx = seq_x0 + (top_cut - block_start) as f32 * char_width;
                    let label_y = block_y + row as f32 * cut_label_row_h;
                    painter.text(
                        Pos2::new(tcx, label_y),
                        Align2::CENTER_TOP,
                        &site.enzyme,
                        cut_label_font.clone(),
                        cut_site_color,
                    );
                    // Short tick from the bottom of the label row toward
                    // the ruler — just enough visual to read "site here"
                    // without committing to the full staple.
                    let tick_top = block_y + (row + 1) as f32 * cut_label_row_h;
                    let tick_bot = (tick_top + cut_label_row_h * 0.6).min(ruler_y);
                    painter.line_segment(
                        [Pos2::new(tcx, tick_top), Pos2::new(tcx, tick_bot)],
                        Stroke::new(1.0, cut_site_color),
                    );
                }

                // ── Hover-reveal: full staple + wedges for one site ──────
                // Hover hit-test uses the label rects collected in pass 1.
                // Only the topmost hovered site promotes; non-hovered
                // sites keep their tick + label.
                // `interact_pointer_pos` only fires during click/drag;
                // `hover_pos` covers idle mouse-over which is what we want.
                let hovered_site_idx = response.hover_pos().and_then(|p| {
                    cut_site_rects
                        .iter()
                        .find(|(r, _)| r.contains(p))
                        .map(|(_, idx)| *idx)
                });
                if let Some(idx) = hovered_site_idx {
                    let site = &view.cut_sites[idx];
                    let top_cut = site.cut_pos;
                    let bot_cut = site.bottom_cut_pos;
                    // Find the row this site occupies in *this* block's layout.
                    // Sites whose `cut_pos` is in this block were stacked here.
                    let row_in_block = layout
                        .cut_rows
                        .iter()
                        .find(|(i, _)| *i == idx)
                        .map(|(_, r)| *r);
                    if let Some(row) =
                        row_in_block.filter(|_| top_cut >= block_start && top_cut <= block_end)
                    {
                        let tcx = seq_x0 + (top_cut - block_start) as f32 * char_width;
                        let stroke = Stroke::new(1.5, cut_site_color);
                        let line_top = block_y + (row + 1) as f32 * cut_label_row_h;

                        // Descender from label row through the top strand.
                        painter.line_segment(
                            [Pos2::new(tcx, line_top), Pos2::new(tcx, bot_y)],
                            stroke,
                        );

                        // Strand connector + overhang step.
                        if top_cut == bot_cut {
                            painter.line_segment(
                                [Pos2::new(tcx, bot_y), Pos2::new(tcx, bot_y + strand_h)],
                                stroke,
                            );
                        } else if bot_cut >= block_start && bot_cut <= block_end {
                            let bcx = seq_x0 + (bot_cut - block_start) as f32 * char_width;
                            painter.line_segment(
                                [Pos2::new(tcx, bot_y), Pos2::new(bcx, bot_y)],
                                stroke,
                            );
                            painter.line_segment(
                                [Pos2::new(bcx, bot_y), Pos2::new(bcx, bot_y + strand_h)],
                                stroke,
                            );
                        } else {
                            // Bottom cut in another block — stub the top half.
                            painter.line_segment(
                                [
                                    Pos2::new(tcx, bot_y),
                                    Pos2::new(tcx, bot_y + strand_h * 0.5),
                                ],
                                Stroke::new(1.5, cut_site_color.gamma_multiply(0.4)),
                            );
                        }

                        // ── Wedge arrows ─────────────────────────────────
                        // Two small filled triangles indicate cut points:
                        //   * top wedge on the top strand at `cut_pos`
                        //   * bottom wedge on the bottom strand at
                        //     `bottom_cut_pos`
                        // Triangles point *into* the cut from the strand
                        // they sit on (top wedge points down toward the
                        // top-strand cut line; bottom wedge points up).
                        // This matches the SnapGene convention of arrows
                        // marking the precise scissile bond on each strand.
                        let bcx_in_block = bot_cut >= block_start && bot_cut <= block_end;
                        let bcx = if bcx_in_block {
                            Some(seq_x0 + (bot_cut - block_start) as f32 * char_width)
                        } else {
                            None
                        };
                        let wedge_half = (char_width * 0.45).clamp(2.5, 5.0);
                        let wedge_h = (strand_h * 0.55).clamp(3.0, 7.0);
                        paint_wedge_down(&painter, tcx, top_y, wedge_half, wedge_h, cut_site_color);
                        if let Some(bcx) = bcx {
                            paint_wedge_up(
                                &painter,
                                bcx,
                                bot_y + strand_h,
                                wedge_half,
                                wedge_h,
                                cut_site_color,
                            );
                        }
                    }
                }

                // ── Block separator ───────────────────────────────────────
                if block_idx + 1 < n_blocks {
                    painter.hline(
                        rect.min.x..=rect.min.x + content_width,
                        block_y + block_h - block_gap * 0.5,
                        Stroke::new(0.5, text_color.gamma_multiply(0.08)),
                    );
                }
            }

            // The staged-edit op summary (e.g. "Insert 6 bp") now lives in the
            // app status bar (see `staged_summary` + app.rs) — the in-canvas
            // track-changes diff wash is the only in-place staging cue here.

            // Visible range for minimap viewport indicator. With
            // variable block heights we look up the first / last blocks
            // via the prefix-sum offsets rather than dividing by a
            // single block height.
            let scroll_top = (clip.min.y - rect.min.y).max(0.0);
            let scroll_bot = scroll_top + clip.height();
            let first_block = y_to_block(scroll_top, &block_offsets).unwrap_or(0);
            let last_block =
                y_to_block(scroll_bot, &block_offsets).unwrap_or(n_blocks.saturating_sub(1));
            computed_visible = Some((
                (first_block * line_width).min(seq_len),
                ((last_block + 1) * line_width).min(seq_len),
            ));
        });
        view.visible_range = computed_visible;
        // Put the memoized preview back (taken out above so the render closure
        // could mutate `self`). A click that cancelled `pending` this frame
        // leaves it stale; next frame's `refresh_preview` clears it.
        self.preview = preview;
        // Same for the memoized translation cache.
        self.translation_cache = trans_cache;
    }
}

// ── Free helpers ──────────────────────────────────────────────────────────────

/// Screen → 0-based sequence offset. Returns positions in the closed
/// range `0..=seq_len` — the upper bound is the "insert-at-end"
/// cursor (one past the last base), an editor-grade affordance that
/// view-only code doesn't strictly need but the edit path (Tier 3d)
/// does. Tier 2 #9.
///
/// Callers that want a strictly-on-base position (selection range
/// endpoints) should bound-check `< seq_len` themselves.
fn screen_to_seq(
    pos: Pos2,
    rect: Rect,
    char_width: f32,
    line_width: usize,
    seq_len: usize,
    block_offsets: &[f32],
    left_margin: f32,
) -> Option<usize> {
    let rel_x = pos.x - rect.min.x - left_margin;
    let rel_y = pos.y - rect.min.y;
    if rel_x < 0.0 || rel_y < 0.0 {
        return None;
    }
    let block_idx = y_to_block(rel_y, block_offsets)?;
    let col = (rel_x / char_width) as usize;
    if col >= line_width {
        return None;
    }
    let p = block_idx * line_width + col;
    if p > seq_len { None } else { Some(p) }
}

/// Draw a small filled triangle pointing downward, with its apex at `(cx, y)`
/// and its base `height` units above (so the wedge sits *on top of* the
/// strand line and points into the cut). Used for the top-strand cut wedge.
fn paint_wedge_down(
    painter: &egui::Painter,
    cx: f32,
    y: f32,
    half_w: f32,
    height: f32,
    color: Color32,
) {
    let points = vec![
        Pos2::new(cx - half_w, y - height),
        Pos2::new(cx + half_w, y - height),
        Pos2::new(cx, y),
    ];
    painter.add(egui::Shape::convex_polygon(
        points,
        color,
        egui::Stroke::NONE,
    ));
}

/// Draw a small filled triangle pointing upward, with its apex at `(cx, y)`
/// and its base `height` units below. Used for the bottom-strand cut wedge.
fn paint_wedge_up(
    painter: &egui::Painter,
    cx: f32,
    y: f32,
    half_w: f32,
    height: f32,
    color: Color32,
) {
    let points = vec![
        Pos2::new(cx - half_w, y + height),
        Pos2::new(cx + half_w, y + height),
        Pos2::new(cx, y),
    ];
    painter.add(egui::Shape::convex_polygon(
        points,
        color,
        egui::Stroke::NONE,
    ));
}

fn search_hit_color(theme: &crate::config::Theme, strand: Strand) -> Color32 {
    match strand {
        Strand::Forward => theme.strand.forward.0,
        Strand::Reverse => theme.strand.reverse.0,
        _ => theme.strand.unknown.0,
    }
}

fn build_strand_galley(
    ui: &egui::Ui,
    bases: &[u8],
    font_id: &FontId,
    alpha: f32,
    theme: &crate::config::Theme,
) -> std::sync::Arc<egui::Galley> {
    let mut job = LayoutJob::default();
    for &b in bases {
        job.append(
            &(b.to_ascii_uppercase() as char).to_string(),
            0.0,
            egui::text::TextFormat {
                font_id: font_id.clone(),
                color: theme.bases.for_base(b).gamma_multiply(alpha),
                ..Default::default()
            },
        );
    }
    ui.fonts(|f| f.layout_job(job))
}

/// Draw a feature label according to the `label_overflow` policy.
/// Contrast is guaranteed by the WCAG-aware text colour picker at the
/// call site, so no outline is needed.
fn paint_feature_label(
    painter: &egui::Painter,
    font: &FontId,
    color: Color32,
    overflow: LabelOverflow,
    label_char_w: f32,
    bar: Rect,
    label: &str,
) {
    let bar_w = bar.width();
    let full_w = label.chars().count() as f32 * label_char_w;
    let text: Option<String> = if bar_w >= full_w {
        Some(label.to_string())
    } else {
        match overflow {
            LabelOverflow::Truncate => None,
            LabelOverflow::Extend => Some(label.to_string()),
            LabelOverflow::Ellipsis => {
                let usable = (bar_w - label_char_w).max(0.0);
                let n = (usable / label_char_w).floor() as usize;
                if n == 0 {
                    None
                } else {
                    let mut s: String = label.chars().take(n).collect();
                    s.push('…');
                    Some(s)
                }
            }
        }
    };
    let Some(text) = text else { return };
    painter.text(
        bar.center(),
        Align2::CENTER_CENTER,
        &text,
        font.clone(),
        color,
    );
}

#[cfg(test)]
mod tests {
    use super::{
        AaKind, TranslationDisplay, build_translation_cache, codon_extend, frame_glyphs,
        iupac_filter,
    };
    use seqforge_core::Strand;

    #[test]
    fn frame_glyphs_forward_positions_and_kinds() {
        // ATG AAA TAA → M(start) K(normal) *(stop) at codon-middle columns 1,4,7.
        let g = frame_glyphs(b"ATGAAATAA", Strand::Forward, 1);
        assert_eq!(g.len(), 3);
        assert_eq!((g[0].pos, g[0].ch, g[0].kind), (1, 'M', AaKind::Start));
        assert_eq!((g[1].pos, g[1].ch), (4, 'K'));
        assert_eq!((g[2].pos, g[2].ch, g[2].kind), (7, '*', AaKind::Stop));
    }

    #[test]
    fn reverse_frame_glyphs_map_to_forward_columns() {
        // revcomp("TTATTTCAT") = "ATGAAATAA" (M K *). On the reverse lane the
        // glyphs anchor to forward columns (descending), still within bounds.
        let g = frame_glyphs(b"TTATTTCAT", Strand::Reverse, 1);
        assert_eq!(g.len(), 3);
        assert!(g.iter().all(|gl| gl.pos < 9));
        assert!(g.iter().any(|gl| gl.ch == 'M'));
    }

    #[test]
    fn translation_cache_builds_enabled_frames_only() {
        let seq = b"ATGAAATAAATGCCC";
        let ann = seqforge_core::Annotations::new(vec![]);
        let mut d = TranslationDisplay::default();
        d.frames[0] = true; // +1 only
        let cache = build_translation_cache(seq, &ann, 1, d);
        assert_eq!(cache.frame_lanes.len(), 1);
        assert!(cache.feature_lanes.is_empty());
    }

    #[test]
    fn non_cds_feature_translates_when_toggled_on() {
        use seqforge_core::{Feature, Strand};
        let seq = b"ATGAAATAA";
        let mut ann = seqforge_core::Annotations::new(vec![]);
        let id = ann.add(Feature {
            id: Default::default(),
            range: 0..9,
            raw_kind: "misc_feature".to_string(),
            label: "region".to_string(),
            strand: Strand::Forward,
            qualifiers: Default::default(),
            provenance: None,
        });
        // show_cds is off; a misc_feature only translates when individually toggled.
        let mut d = TranslationDisplay::default();
        assert!(
            build_translation_cache(seq, &ann, 1, d.clone())
                .feature_lanes
                .is_empty()
        );
        d.features.insert(id);
        let cache = build_translation_cache(seq, &ann, 1, d);
        assert_eq!(
            cache.feature_lanes.len(),
            1,
            "toggled feature must translate"
        );
        // ATG AAA TAA anchored at the feature start → M K *.
        assert_eq!(
            cache.feature_lanes[0]
                .glyphs
                .iter()
                .map(|g| g.ch)
                .collect::<String>(),
            "MK*"
        );
    }

    #[test]
    fn codon_extend_keeps_both_codons_whole_in_either_direction() {
        // Codons: A=[0,3), B=[3,6), C=[6,9).
        // Reaching right from A to C → [A.start, C.end) = 0..9.
        let s = codon_extend(&(0..3), &(6..9));
        assert_eq!(s.ordered(), (0, 9));
        // Reaching left from C to A → still 0..9; the origin codon C keeps its
        // 3′ base (index 8), which the old nt-level path clipped.
        let s = codon_extend(&(6..9), &(0..3));
        assert_eq!(s.ordered(), (0, 9));
        assert_eq!(
            s.anchor, 9,
            "reverse selection anchors at the origin's 3′ edge"
        );
        // Clicking the anchor codon itself selects exactly that codon.
        assert_eq!(codon_extend(&(3..6), &(3..6)).ordered(), (3, 6));
    }

    #[test]
    fn overlapping_translated_features_get_separate_lanes() {
        use seqforge_core::{Feature, Strand};
        let seq = b"ATGAAATAAATGCCCTAA";
        let mut ann = seqforge_core::Annotations::new(vec![]);
        let mk = |range: std::ops::Range<usize>| Feature {
            id: Default::default(),
            range,
            raw_kind: "CDS".to_string(),
            label: "c".to_string(),
            strand: Strand::Forward,
            qualifiers: Default::default(),
            provenance: None,
        };
        // Two overlapping CDS features (0..12 and 6..18 share columns 6..12).
        ann.add(mk(0..12));
        ann.add(mk(6..18));
        let d = TranslationDisplay {
            show_cds: true,
            ..Default::default()
        };
        let cache = build_translation_cache(seq, &ann, 1, d);
        assert_eq!(
            cache.feature_lanes.len(),
            2,
            "overlapping features must be packed onto separate lanes"
        );
    }

    #[test]
    fn iupac_filter_uppercases_and_keeps_valid() {
        assert_eq!(iupac_filter("atgc"), "ATGC");
        assert_eq!(iupac_filter("ACGTN"), "ACGTN");
        // ambiguity codes are valid
        assert_eq!(iupac_filter("ryswkm"), "RYSWKM");
    }

    #[test]
    fn iupac_filter_drops_junk_silently() {
        // digits, punctuation, and non-IUPAC letters (e.g. Z, J) are dropped
        assert_eq!(iupac_filter("A1T-G zJ C"), "ATGC");
        assert_eq!(iupac_filter("123"), "");
    }

    // ── Staged-edit state machine (Phase 13) ─────────────────────────────────

    use super::{PendingEdit, stage_input};
    use seqforge_core::{ViewId, ViewerRequest};

    fn stage(
        pending: &mut Option<PendingEdit>,
        bases: &str,
        bs: usize,
        del: usize,
        range: Option<(usize, usize)>,
        cursor: Option<usize>,
    ) {
        stage_input(pending, bases, bs, del, range, cursor, 100);
    }

    #[test]
    fn typing_at_cursor_arms_and_extends_insert() {
        let mut p = None;
        stage(&mut p, "A", 0, 0, None, Some(10));
        assert_eq!(
            p,
            Some(PendingEdit::Insert {
                pos: 10,
                staged: "A".into()
            })
        );
        // more typing extends the same staged edit (still one pending edit)
        stage(&mut p, "TG", 0, 0, None, Some(10));
        assert_eq!(
            p,
            Some(PendingEdit::Insert {
                pos: 10,
                staged: "ATG".into()
            })
        );
    }

    #[test]
    fn typing_over_range_arms_replace() {
        let mut p = None;
        stage(&mut p, "GG", 0, 0, Some((5, 9)), None);
        assert_eq!(
            p,
            Some(PendingEdit::Replace {
                start: 5,
                end: 9,
                staged: "GG".into()
            })
        );
    }

    #[test]
    fn backspace_trims_staged_then_clears() {
        let mut p = Some(PendingEdit::Insert {
            pos: 3,
            staged: "AT".into(),
        });
        stage(&mut p, "", 1, 0, None, Some(3));
        assert_eq!(
            p,
            Some(PendingEdit::Insert {
                pos: 3,
                staged: "A".into()
            })
        );
        // trimming the last char drops the pending edit entirely
        stage(&mut p, "", 1, 0, None, Some(3));
        assert_eq!(p, None);
    }

    #[test]
    fn backspace_over_range_arms_delete() {
        let mut p = None;
        stage(&mut p, "", 1, 0, Some((4, 8)), None);
        assert_eq!(p, Some(PendingEdit::Delete { start: 4, end: 8 }));
    }

    #[test]
    fn backspace_at_cursor_arms_and_extends_delete_left() {
        let mut p = None;
        stage(&mut p, "", 1, 0, None, Some(10));
        assert_eq!(p, Some(PendingEdit::Delete { start: 9, end: 10 }));
        // a second backspace extends the deletion span leftward
        stage(&mut p, "", 1, 0, None, Some(10));
        assert_eq!(p, Some(PendingEdit::Delete { start: 8, end: 10 }));
    }

    #[test]
    fn forward_delete_extends_right_bounded_by_len() {
        let mut p = None;
        stage_input(&mut p, "", 0, 1, None, Some(99), 100);
        assert_eq!(
            p,
            Some(PendingEdit::Delete {
                start: 99,
                end: 100
            })
        );
        // can't extend past seq_len
        stage_input(&mut p, "", 0, 1, None, Some(99), 100);
        assert_eq!(
            p,
            Some(PendingEdit::Delete {
                start: 99,
                end: 100
            })
        );
    }

    #[test]
    fn to_request_lowers_to_viewer_request() {
        let v = ViewId(1);
        assert!(matches!(
            PendingEdit::Insert {
                pos: 2,
                staged: "AT".into()
            }
            .to_request(v),
            Some(ViewerRequest::Insert { pos: 2, .. })
        ));
        assert!(matches!(
            PendingEdit::Delete { start: 1, end: 4 }.to_request(v),
            Some(ViewerRequest::Delete {
                start: 1,
                end: 4,
                ..
            })
        ));
        // nothing to commit: empty staged text / zero-width delete
        assert!(
            PendingEdit::Insert {
                pos: 0,
                staged: String::new()
            }
            .to_request(v)
            .is_none()
        );
        assert!(
            PendingEdit::Delete { start: 5, end: 5 }
                .to_request(v)
                .is_none()
        );
    }

    // ── Realized diff preview (Phase 13.6) ───────────────────────────────────

    use super::{Preview, SequenceView, apply_splice, move_focus};
    use seqforge_core::{Annotations, Buffer, Topology};

    fn buf(bytes: &[u8]) -> Buffer {
        Buffer::new("t".into(), None, bytes.to_vec(), Topology::Linear)
    }

    /// Insert/Replace previews are built by the *same* `apply_splice` commit
    /// runs, so the speculative text is provably identical to the result.
    #[test]
    fn insert_preview_is_identical_to_commit() {
        let b = buf(b"AAAAAAAAAA");
        let ann = Annotations::default();
        let pe = PendingEdit::Insert {
            pos: 4,
            staged: "GGG".into(),
        };
        let p = Preview::build(&b, &ann, &pe, None);
        // Same transform commit will run:
        let mut committed = b.clone();
        let mut cann = ann.clone();
        apply_splice(&mut committed, &mut cann, 4..4, b"GGG");
        assert_eq!(p.text, committed.text);
        assert_eq!(p.text, b"AAAAGGGAAAAAA");
        assert_eq!(p.added, Some((4, 7)));
        assert_eq!(p.deleted, None);
    }

    #[test]
    fn replace_preview_is_identical_to_commit() {
        let b = buf(b"AAAAAAAAAA");
        let ann = Annotations::default();
        let pe = PendingEdit::Replace {
            start: 2,
            end: 6,
            staged: "CC".into(),
        };
        let p = Preview::build(&b, &ann, &pe, None);
        let mut committed = b.clone();
        let mut cann = ann.clone();
        apply_splice(&mut committed, &mut cann, 2..6, b"CC");
        assert_eq!(p.text, committed.text);
        assert_eq!(p.text, b"AACCAAAA");
        // green wash covers the new bases, not the old range
        assert_eq!(p.added, Some((2, 4)));
        assert_eq!(p.deleted, None);
    }

    /// Delete keeps the committed bytes (no virtual buffer) and marks the range
    /// struck in place; the bytes only leave on commit.
    #[test]
    fn delete_preview_keeps_committed_bytes() {
        let b = buf(b"AAAAAAAAAA");
        let ann = Annotations::default();
        let pe = PendingEdit::Delete { start: 3, end: 7 };
        let p = Preview::build(&b, &ann, &pe, None);
        assert_eq!(p.text, b.text); // unchanged — bases stay visible
        assert_eq!(p.deleted, Some((3, 7)));
        assert_eq!(p.added, None);
    }

    /// Cut reuses the Delete preview verbatim (red-struck, bytes kept) — only
    /// its commit lowering differs (→ `Cut`, which also copies).
    #[test]
    fn cut_preview_matches_delete() {
        let b = buf(b"AAAAAAAAAA");
        let ann = Annotations::default();
        let cut = Preview::build(&b, &ann, &PendingEdit::Cut { start: 3, end: 7 }, None);
        let del = Preview::build(&b, &ann, &PendingEdit::Delete { start: 3, end: 7 }, None);
        assert_eq!(cut.text, del.text);
        assert_eq!(cut.text, b.text); // bytes kept visible
        assert_eq!(cut.deleted, Some((3, 7)));
        assert_eq!(cut.added, None);
        // Lowers to Cut, not Delete.
        assert!(matches!(
            PendingEdit::Cut { start: 3, end: 7 }.to_request(ViewId(1)),
            Some(ViewerRequest::Cut {
                start: 3,
                end: 7,
                ..
            })
        ));
    }

    /// Paste materializes the clipboard bytes exactly like an Insert of them,
    /// so the preview is provably identical to what commit produces.
    #[test]
    fn paste_preview_materializes_clipboard() {
        let b = buf(b"AAAAAAAAAA");
        let ann = Annotations::default();
        let clip = b"CCC";
        let p = Preview::build(&b, &ann, &PendingEdit::Paste { pos: 4 }, Some(clip));
        let mut committed = b.clone();
        let mut cann = ann.clone();
        apply_splice(&mut committed, &mut cann, 4..4, clip);
        assert_eq!(p.text, committed.text);
        assert_eq!(p.text, b"AAAACCCAAAAAA");
        assert_eq!(p.added, Some((4, 7))); // green wash over the pasted bases
        assert_eq!(p.deleted, None);
        assert!(matches!(
            PendingEdit::Paste { pos: 4 }.to_request(ViewId(1)),
            Some(ViewerRequest::Paste { pos: 4, .. })
        ));
    }

    /// The memo rebuilds only when the fingerprint (version + pending) changes,
    /// never per frame: a tampered cached preview survives a same-fingerprint
    /// refresh and is discarded once the pending edit changes.
    #[test]
    fn refresh_preview_is_memoized_on_fingerprint() {
        let b = buf(b"AAAAAAAAAA");
        let ann = Annotations::default();
        let mut view = SequenceView {
            pending: Some(PendingEdit::Insert {
                pos: 0,
                staged: "G".into(),
            }),
            ..Default::default()
        };
        view.refresh_preview(&b, &ann, None);
        // Tamper the cache; a same-fingerprint refresh must NOT rebuild it.
        view.preview.as_mut().unwrap().text = b"SENTINEL".to_vec();
        view.refresh_preview(&b, &ann, None);
        assert_eq!(view.preview.as_ref().unwrap().text, b"SENTINEL");
        // Changing the pending edit changes the fingerprint → rebuild.
        view.pending = Some(PendingEdit::Insert {
            pos: 0,
            staged: "GG".into(),
        });
        view.refresh_preview(&b, &ann, None);
        assert_eq!(view.preview.as_ref().unwrap().text, b"GGAAAAAAAAAA");
        // Clearing pending drops the preview.
        view.pending = None;
        view.refresh_preview(&b, &ann, None);
        assert!(view.preview.is_none());
    }

    /// Arrow-key focus math: ±1 / ±line_width steps clamp to `0..=seq_len`.
    #[test]
    fn move_focus_steps_and_clamps() {
        // Right / Left by one base.
        assert_eq!(move_focus(5, 1, 10), 6);
        assert_eq!(move_focus(5, -1, 10), 4);
        // Down / Up by a line (line_width = 4).
        assert_eq!(move_focus(2, 4, 100), 6);
        assert_eq!(move_focus(6, -4, 100), 2);
        // Clamp at both ends — including the insert-at-end position (== seq_len).
        assert_eq!(move_focus(0, -1, 10), 0);
        assert_eq!(move_focus(10, 1, 10), 10);
        assert_eq!(move_focus(8, 5, 10), 10);
        assert_eq!(move_focus(2, -5, 10), 0);
    }

    /// The status-bar summary reflects each staged op + size, reads the
    /// clipboard length for a Paste, and is `None` when nothing is staged.
    #[test]
    fn staged_summary_names_op_and_size() {
        let mut view = SequenceView::default();
        assert_eq!(view.staged_summary(None), None);

        view.pending = Some(PendingEdit::Insert {
            pos: 0,
            staged: "ATG".into(),
        });
        assert_eq!(view.staged_summary(None).as_deref(), Some("Insert 3 bp"));

        view.pending = Some(PendingEdit::Replace {
            start: 2,
            end: 6,
            staged: "AT".into(),
        });
        assert_eq!(view.staged_summary(None).as_deref(), Some("Replace 4→2 bp"));

        view.pending = Some(PendingEdit::Delete { start: 1, end: 5 });
        assert_eq!(view.staged_summary(None).as_deref(), Some("Delete 4 bp"));

        view.pending = Some(PendingEdit::Cut { start: 3, end: 10 });
        assert_eq!(view.staged_summary(None).as_deref(), Some("Cut 7 bp"));

        view.pending = Some(PendingEdit::Paste { pos: 4 });
        assert_eq!(
            view.staged_summary(Some(b"CCCCC")).as_deref(),
            Some("Paste 5 bp")
        );
    }
}
