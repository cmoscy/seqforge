//! Sequence viewer widget. The render engine is a block-aware **Track**
//! abstraction (T2 of the render-track refactor — see `plans/render-tracks.md`):
//! [`track`] holds the `Track` trait + `BlockCtx`/`BlockGeom`/`TrackStack`, the
//! [`tracks`] submodules hold the concrete tracks, and [`translation`] owns the
//! derived reading-frame lanes. This module keeps the widget itself: the staged
//! in-canvas edit state machine, keyboard handling, and the `show` entry point
//! that drives the `TrackStack`.
//!
//! This is a rendering/interaction refactor only — the domain model is untouched
//! (decisions 8/12/13).

mod track;
mod tracks;
mod translation;

use std::time::Duration;

use egui::{Key, Modifiers, Rect, Sense, Stroke, Vec2};
use seqforge_core::{
    Annotations, Buffer, CutSite, Selection, View, ViewId, ViewerRequest, mutations::apply_splice,
};

use crate::command::{AppCommand, PendingCommand};
use crate::config::Config;

use track::{
    BlockCtx, FeatureContext, Hit, Style, TrackStack, build_block_layouts, find_hit,
    open_edit_feature_cmd, screen_to_seq, strand_flag, y_to_block,
};
use translation::{OrfPromote, TranslationCache, build_translation_cache, codon_extend};

// Re-exports consumed elsewhere in the crate.
pub(crate) use track::greedy_stack;
pub use translation::TranslationDisplay;

/// Cursor blink half-period (ms): the cursor toggles visible/hidden each
/// interval while the viewer has focus.
const BLINK_MS: u64 = 500;

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

// ── Widget state ──────────────────────────────────────────────────────────────

/// Per-document state for the sequence viewer widget.
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

    /// Render the sequence viewer. Caller must have already resolved an active
    /// view and locked its buffer for read; this widget is inert if there's no
    /// doc — the placeholder rendering lives in `tabs.rs::ui` so this function
    /// can assume a real buffer.
    ///
    /// Selection / feature highlight mutations go through
    /// `AppCommand::SetSelection` / `SelectFeature` (pushed to `cmds`) so the
    /// single-applier invariant from the focus refactor holds.
    //
    // The render engine is the `TrackStack`: one virtualized block loop over
    // the ordered tracks. Interaction (click / drag / context menu) stays here,
    // resolving the unified `Vec<(Rect, Hit)>` the tracks emit.
    #[allow(clippy::too_many_arguments)]
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        view: &mut View,
        buffer: &Buffer,
        annotations: &Annotations,
        cmds: &mut Vec<PendingCommand>,
        cfg: &Config,
        focused: bool,
        clipboard: Option<&[u8]>,
    ) {
        // ── Drive in-canvas staging from the keyboard, *before* layout ──
        // Editing must run before sizing so the realized diff preview drives
        // block layout this same frame. Focus is the app-level pane focus;
        // losing it abandons an uncommitted stage.
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
        // changed, then take it out as an owned local so the render closure can
        // freely mutate `self` without aliasing the borrow.
        self.refresh_preview(buffer, annotations, clipboard);
        let preview = self.preview.take();
        let staging = preview.is_some();
        // Render source: the speculative preview while staging, else the
        // committed buffer. Derived overlays (cut sites / search) are suppressed
        // while staging — they're anchored to committed coordinates.
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
        // Cut-site labels render at `label_size` (same as feature labels).
        let cut_label_row_h = label_size + 3.0;
        let label_overflow = cfg.settings.editor.label_overflow;

        let font_id = egui::FontId::monospace(font_size);
        let small_font = egui::FontId::proportional(label_size);
        let ruler_font = egui::FontId::proportional(ruler_size);

        // Measure char_width from an actual galley so feature bar positions use
        // the same per-character advance that LayoutJob renders.
        let (char_width, char_height, label_char_w) = ui.fonts(|f| {
            let probe = f.layout_no_wrap("A".repeat(64), font_id.clone(), egui::Color32::BLACK);
            let label_probe =
                f.layout_no_wrap("A".repeat(32), small_font.clone(), egui::Color32::BLACK);
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
        self.last_line_width = line_width;

        // ── Translation lanes (memoized on version + display) ────────────
        // Suppressed while staging (like cut sites / search).
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
        // Only the global frame lanes form the position-owned band; per-feature
        // CDS translations ride under their own bar (Features track, T3).
        let frame_band_rows = trans_cache.as_ref().map_or(0, |c| c.frame_band_rows());
        let trans_band_h = frame_band_rows as f32 * aa_row_h;

        // Shared per-frame render style (sizing + fonts + colours).
        let style = Style {
            char_width,
            char_height,
            label_char_w,
            ruler_h,
            strand_h,
            annot_row_h,
            cut_label_row_h,
            aa_row_h,
            block_gap,
            line_width,
            label_overflow,
            font_id: font_id.clone(),
            small_font: small_font.clone(),
            ruler_font: ruler_font.clone(),
            text_color: ui.visuals().text_color(),
            selection_color: cfg.theme.ui.selection.0,
            cursor_color: cfg.theme.ui.cursor.0,
            cut_site_color: cfg.theme.ui.cut_site.0,
            label_text_light: cfg.theme.ui.label_text.0,
            label_text_dark: cfg.theme.ui.label_text_alt.0,
            ruler_text: cfg.theme.ui.ruler_text.0,
            aa_stop: cfg.theme.translation.stop.0,
            aa_start: cfg.theme.translation.start.0,
            orf_wash: cfg.theme.translation.orf_wash.0,
        };

        // Per-block layout: each block sizes itself to the items it contains
        // (feature rows grow to fit a translated feature's CDS sub-row).
        let (block_layouts, block_offsets) = build_block_layouts(
            render_ann,
            cut_sites,
            seq_len,
            &style,
            trans_band_h,
            trans_cache.as_ref(),
        );
        let total_height = *block_offsets.last().unwrap_or(&0.0);
        let content_width = left_margin + line_width as f32 * char_width + right_margin;
        let alloc_width = content_width.max(ui.available_width());

        // Consume the one-shot scroll request: center the target block in the
        // viewport this frame, then clear so the user can scroll freely.
        let scroll_offset = view.scroll_to.take().map(|pos| {
            let block_idx = (pos / line_width).min(n_blocks.saturating_sub(1));
            let block_top = block_offsets[block_idx];
            let block_h = block_layouts[block_idx].height;
            let viewport_h = ui.available_height();
            (block_top - viewport_h / 2.0 + block_h / 2.0).max(0.0)
        });

        // Realized-diff column ranges (Sequence-track decorations).
        let (diff_added, diff_deleted) = preview
            .as_ref()
            .map_or((None, None), |p| (p.added, p.deleted));

        let stack = TrackStack::new();
        let theme = &cfg.theme;
        let show_orfs = self.translation.show_orfs;
        let selection = view.selection;
        let selected_feature = view.selected_feature;

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
            let seq_x0 = rect.min.x + left_margin;

            // Build the read-only per-block context. `blink_on` / `hovered` are
            // only used at paint time; hit collection passes placeholders.
            let make_ctx = |block_idx: usize, blink_on: bool, hovered: Option<usize>| BlockCtx {
                block_idx,
                block_start: block_idx * line_width,
                block_end: ((block_idx + 1) * line_width).min(seq_len),
                seq,
                seq_len,
                render_ann,
                cut_sites,
                search_hits: &view.search_hits,
                trans_cache: trans_cache.as_ref(),
                show_orfs,
                theme,
                style: &style,
                staging,
                added: diff_added,
                deleted: diff_deleted,
                selection,
                selected_feature,
                blink_on,
                hovered_cut_site: hovered,
                layout: &block_layouts[block_idx],
            };

            // ── Pass 1: collect interactive rects across every visible block ──
            let mut hits: Vec<(Rect, Hit)> = Vec::new();
            for block_idx in 0..n_blocks {
                let block_y = rect.min.y + block_offsets[block_idx];
                let block_h = block_layouts[block_idx].height;
                if block_y + block_h < clip.min.y {
                    continue;
                }
                if block_y > clip.max.y {
                    break;
                }
                let ctx = make_ctx(block_idx, false, None);
                stack.hit_block(&ctx, block_y, seq_x0, rect.min.x, &mut hits);
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

            let push_sel = |cmds: &mut Vec<PendingCommand>, sel: Option<Selection>| {
                cmds.push((AppCommand::SetSelection(sel), None));
            };
            let push_feat = |cmds: &mut Vec<PendingCommand>,
                             feat: Option<seqforge_core::FeatureId>| {
                cmds.push((AppCommand::SelectFeature(feat), None));
            };

            if response.clicked() {
                if let Some(pos) = ptr {
                    if shift_held {
                        // Shift+click extends the selection. Over a translation
                        // codon cell, keep *both* the origin codon and the
                        // clicked codon whole (see `translation_anchor`).
                        let over_codon = find_hit(&hits, pos, Hit::as_codon);
                        let new_sel = match (over_codon, self.translation_anchor.clone()) {
                            (Some(codon), Some(ac)) => Some(codon_extend(&ac, &codon)),
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
                        if let Some(feat_idx) = find_hit(&hits, pos, Hit::as_feature) {
                            let feat = render_ann
                                .by_position(feat_idx)
                                .expect("feat_idx from this frame's layout");
                            push_sel(
                                cmds,
                                Some(Selection::range(feat.range.start, feat.range.end)),
                            );
                            push_feat(cmds, Some(feat.id));
                        } else if let Some(hit_idx) = find_hit(&hits, pos, Hit::as_search) {
                            let hit = &view.search_hits[hit_idx];
                            push_sel(cmds, Some(Selection::range(hit.start, hit.end)));
                            push_feat(cmds, None);
                        } else if let Some(site_idx) = find_hit(&hits, pos, Hit::as_cut_site) {
                            let site = &cut_sites[site_idx];
                            push_sel(
                                cmds,
                                Some(Selection::range(
                                    site.recognition_start,
                                    site.recognition_end,
                                )),
                            );
                            push_feat(cmds, None);
                        } else if let Some(codon) = find_hit(&hits, pos, Hit::as_codon) {
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
                    if let Some(fc) = find_hit(&hits, p, Hit::as_feature)
                        .and_then(|fi| render_ann.by_position(fi))
                        .map(FeatureContext::from_feature)
                    {
                        cmds.push((open_edit_feature_cmd(&fc), None));
                    }
                }
            }

            // ── Right-click a feature → context menu ──────────────────────
            if response.secondary_clicked() {
                self.context_feature = ptr.and_then(|p| {
                    find_hit(&hits, p, Hit::as_feature)
                        .and_then(|fi| render_ann.by_position(fi))
                        .map(FeatureContext::from_feature)
                });
                // If no feature was hit, capture an ORF run under the pointer.
                self.context_orf = if self.context_feature.is_some() {
                    None
                } else {
                    ptr.and_then(|p| find_hit(&hits, p, Hit::as_orf))
                };
            }
            let ctx_feat = self.context_feature.clone();
            let ctx_orf = self.context_orf;
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
                // Inline translation, anchored to the feature's start + strand.
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
                let on_annot = ptr.is_some_and(|p| find_hit(&hits, p, Hit::as_feature).is_some());
                let on_hit = ptr.is_some_and(|p| find_hit(&hits, p, Hit::as_search).is_some());
                let on_site = ptr.is_some_and(|p| find_hit(&hits, p, Hit::as_cut_site).is_some());
                if !on_annot && !on_hit && !on_site {
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
            }

            // In-canvas editing: a click cancels an active stage (it also moves
            // the cursor). The realized diff renders from `ctx.added/deleted`.
            if response.clicked() {
                self.pending = None;
            }
            // Cursor is solid when unfocused, blinks (~2 Hz) when focused.
            let blink_on =
                !focused || (ui.input(|i| i.time) * 1000.0 / BLINK_MS as f64) as i64 % 2 == 0;
            // The hovered cut site promotes its full staple (paint-time only).
            let hovered_site_idx = response
                .hover_pos()
                .and_then(|p| find_hit(&hits, p, Hit::as_cut_site));

            // ── Pass 2: paint every visible block via the track stack ─────
            for block_idx in 0..n_blocks {
                let block_y = rect.min.y + block_offsets[block_idx];
                let block_h = block_layouts[block_idx].height;
                if block_y + block_h < clip.min.y {
                    continue;
                }
                if block_y > clip.max.y {
                    break;
                }
                let ctx = make_ctx(block_idx, blink_on, hovered_site_idx);
                stack.paint_block(&ctx, block_y, seq_x0, rect.min.x, &painter);

                // Block separator (stack chrome).
                if block_idx + 1 < n_blocks {
                    painter.hline(
                        rect.min.x..=rect.min.x + content_width,
                        block_y + block_h - block_gap * 0.5,
                        Stroke::new(0.5, style.text_color.gamma_multiply(0.08)),
                    );
                }
            }

            // Visible range for the minimap viewport indicator.
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
        // Put the memoized preview + translation cache back (taken out above so
        // the render closure could mutate `self`).
        self.preview = preview;
        self.translation_cache = trans_cache;
    }
}

#[cfg(test)]
mod tests {
    use super::{
        PendingEdit, Preview, SequenceView, apply_splice, iupac_filter, move_focus, stage_input,
    };
    use seqforge_core::{Annotations, Buffer, Topology, ViewId, ViewerRequest};

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

    fn buf(bytes: &[u8]) -> Buffer {
        Buffer::new("t".into(), None, bytes.to_vec(), Topology::Linear)
    }

    #[test]
    fn insert_preview_is_identical_to_commit() {
        let b = buf(b"AAAAAAAAAA");
        let ann = Annotations::default();
        let pe = PendingEdit::Insert {
            pos: 4,
            staged: "GGG".into(),
        };
        let p = Preview::build(&b, &ann, &pe, None);
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
        assert_eq!(p.added, Some((2, 4)));
        assert_eq!(p.deleted, None);
    }

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
        assert!(matches!(
            PendingEdit::Cut { start: 3, end: 7 }.to_request(ViewId(1)),
            Some(ViewerRequest::Cut {
                start: 3,
                end: 7,
                ..
            })
        ));
    }

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
        assert_eq!(p.added, Some((4, 7)));
        assert_eq!(p.deleted, None);
        assert!(matches!(
            PendingEdit::Paste { pos: 4 }.to_request(ViewId(1)),
            Some(ViewerRequest::Paste { pos: 4, .. })
        ));
    }

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

    #[test]
    fn move_focus_steps_and_clamps() {
        assert_eq!(move_focus(5, 1, 10), 6);
        assert_eq!(move_focus(5, -1, 10), 4);
        assert_eq!(move_focus(2, 4, 100), 6);
        assert_eq!(move_focus(6, -4, 100), 2);
        assert_eq!(move_focus(0, -1, 10), 0);
        assert_eq!(move_focus(10, 1, 10), 10);
        assert_eq!(move_focus(8, 5, 10), 10);
        assert_eq!(move_focus(2, -5, 10), 0);
    }

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
