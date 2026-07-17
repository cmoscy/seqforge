//! Transient UI surfaces: bars, dialogs, modals.
//!
//! See [`docs/focus-refactor.md`](../../../docs/focus-refactor.md) §2.5.
//!
//! Three previously-independent `AppState` fields (`active_bar`,
//! `open_dialog`, `cli_status`) are collapsed into one [`OverlayStack`].
//! The stack is the single source of truth for which transient surfaces
//! are present; rendering still fans out to the appropriate site per
//! overlay kind (inline bars in the viewer pane; dialogs and status
//! windows at the top level of `update()`).
//!
//! ## Rendering model
//!
//! Overlay variants are heterogenous: a `FindBar` renders inline at
//! the top of the viewer; a `FileDialog` and `CliStatus` render as
//! top-level egui Windows. The stack does not unify rendering — each
//! render site asks the stack for the specific overlay it knows how
//! to draw. The unified concerns are:
//!
//! - **Key-context tags** — every overlay on the stack contributes its
//!   tag to [`crate::focus::KeyContext`]. The generic `"Overlay"` tag
//!   is also present whenever the stack is non-empty, so a single
//!   Escape binding in the keymap can dismiss any overlay.
//! - **Dismiss semantics** — `AppCommand::DismissOverlay` pops the top
//!   of stack regardless of which overlay is up.
//! - **Terminal yielding** — the terminal pane reads
//!   `!overlays.is_empty()` and releases keyboard capture while any
//!   overlay is active.

use std::collections::HashMap;

use std::path::PathBuf;

use egui::Key;
use egui_file_dialog::FileDialog;
use seqforge_core::{CutSite, FeatureId, Strand, ViewId};

use crate::command::AppCommand;

// ── Inline bar state ──────────────────────────────────────────────────────────

pub struct FindBar {
    pub pattern: String,
    pub mismatches: u8,
    needs_focus: bool,
}

impl Default for FindBar {
    fn default() -> Self {
        Self {
            pattern: String::new(),
            mismatches: 0,
            needs_focus: true,
        }
    }
}

pub struct GoToBar {
    pub input: String,
    needs_focus: bool,
}

impl Default for GoToBar {
    fn default() -> Self {
        Self {
            input: String::new(),
            needs_focus: true,
        }
    }
}

/// One occurrence of an enzyme in the sequence — enough to navigate to it.
#[derive(Clone, Copy)]
pub struct EnzymeSite {
    pub recognition_start: usize,
    pub recognition_end: usize,
}

/// One row in the enzyme overlay's results list: a currently-displayed enzyme,
/// its recognition pattern, and every site it cuts (ordered by position).
pub struct EnzymeRow {
    pub name: String,
    /// IUPAC recognition pattern (e.g. `"GGTCTC"`); empty for non-cutters.
    pub recognition: String,
    pub sites: Vec<EnzymeSite>,
}

/// Collapse the active view's enzyme state into display rows. Sites are grouped
/// in one pass over `cut_sites`, then joined against the active enzyme list so
/// non-cutting enzymes (e.g. preset members with zero sites) still appear.
/// Cutters sort first, then alphabetical; sites within a row sort by position.
pub fn enzyme_rows(active_enzymes: &[String], cut_sites: &[CutSite]) -> Vec<EnzymeRow> {
    let mut by: HashMap<&str, Vec<&CutSite>> = HashMap::new();
    for s in cut_sites {
        by.entry(s.enzyme.as_str()).or_default().push(s);
    }
    let mut rows: Vec<EnzymeRow> = active_enzymes
        .iter()
        .map(|name| {
            let group = by.get(name.as_str());
            let recognition = group
                .and_then(|v| v.first())
                .map(|s| s.pattern.clone())
                .unwrap_or_default();
            let mut sites: Vec<EnzymeSite> = group
                .map(|v| {
                    v.iter()
                        .map(|s| EnzymeSite {
                            recognition_start: s.recognition.start,
                            // Linear extent (== the old recognition_end; may exceed
                            // seq_len for an origin-spanning site).
                            recognition_end: s.recognition.start + s.recognition.len,
                        })
                        .collect()
                })
                .unwrap_or_default();
            sites.sort_by_key(|s| s.recognition_start);
            EnzymeRow {
                name: name.clone(),
                recognition,
                sites,
            }
        })
        .collect();
    rows.sort_by(|a, b| {
        (a.sites.is_empty())
            .cmp(&(b.sites.is_empty()))
            .then(a.name.cmp(&b.name))
    });
    rows
}

// ── Feature dialogs ───────────────────────────────────────────────────────────

/// GenBank feature-type strings offered in the New-Feature dropdown. The chosen
/// string is stored verbatim in `Feature.raw_kind`; display colour is derived
/// via `FeatureKind::classify` (never a stored enum — see editor.md conventions).
pub const FEATURE_KINDS: &[&str] = &[
    "CDS",
    "gene",
    "promoter",
    "terminator",
    "rep_origin",
    "misc_feature",
];

/// Unified add/edit feature modal. `id.is_none()` ⇒ **create** (`Tools → New
/// Feature from Selection…`, pre-filled from the selection; commits `AddFeature`);
/// `id.is_some()` ⇒ **edit** (double-click a feature or right-click → Edit…,
/// pre-filled from the feature; commits `UpdateFeature`). One form, one render
/// path — the create/edit split is a single `Option<FeatureId>`.
pub struct FeatureForm {
    /// `None` for a new feature, `Some` when editing an existing one.
    pub id: Option<FeatureId>,
    pub start: usize,
    pub end: usize,
    pub label: String,
    /// GenBank feature-type string (from [`FEATURE_KINDS`]).
    pub kind: String,
    /// `"+"`, `"-"`, or `"."` (unstranded).
    pub strand: String,
    pub needs_focus: bool,
}

impl FeatureForm {
    /// A create form over the selection range.
    pub fn create(start: usize, end: usize) -> Self {
        Self {
            id: None,
            start,
            end,
            label: String::new(),
            kind: "misc_feature".to_string(),
            strand: "+".to_string(),
            needs_focus: true,
        }
    }

    /// An edit form pre-filled from an existing feature.
    pub fn edit(
        id: FeatureId,
        label: String,
        kind: String,
        strand: String,
        start: usize,
        end: usize,
    ) -> Self {
        Self {
            id: Some(id),
            start,
            end,
            label,
            kind,
            strand,
            needs_focus: true,
        }
    }

    /// `true` when editing an existing feature.
    pub fn is_edit(&self) -> bool {
        self.id.is_some()
    }
}

/// Modal form for renaming an existing feature (right-click → Rename…).
pub struct RenameFeatureForm {
    pub id: FeatureId,
    pub input: String,
    pub needs_focus: bool,
}

impl RenameFeatureForm {
    pub fn new(id: FeatureId, current: String) -> Self {
        Self {
            id,
            input: current,
            needs_focus: true,
        }
    }
}

/// Read-only translation window: protein derived from DNA + strand + frame
/// (recomputed live from these fields each frame). Opened from the feature
/// context menu (prefilled from a CDS's strand + `/codon_start`) or from
/// `Tools → Translate Selection…` (frame user-adjustable, default 1).
pub struct TranslationView {
    /// Feature label or `"Selection"` — window subtitle.
    pub title: String,
    pub start: usize,
    pub end: usize,
    pub strand: Strand,
    /// GenBank codon_start convention: 1, 2, or 3.
    pub frame: usize,
    /// When `true`, show all six reading frames (+1/+2/+3, −1/−2/−3) at once
    /// instead of the single strand+frame selected above.
    pub all_frames: bool,
}

// ── Overlay variants ──────────────────────────────────────────────────────────

/// All transient UI that may be present at once. Variants are
/// open-ended for future palette / confirm / agent-prompt overlays.
pub enum Overlay {
    FindBar(FindBar),
    GoToBar(GoToBar),
    /// Boxed because `FileDialog` is ~2.4 KB — keeping it inline would
    /// blow up every `Overlay` value to that size.
    FileDialog(Box<FileDialog>),
    CliStatus(String),
    FeatureForm(FeatureForm),
    RenameFeature(RenameFeatureForm),
    Translation(TranslationView),
    /// Confirm modal shown when closing a tab or quitting the app with an
    /// unsaved buffer. `quitting` distinguishes "close this tab" from "quit
    /// the whole app" so the resolution re-issues the right follow-up.
    DirtyCloseConfirm {
        view_id: ViewId,
        quitting: bool,
    },
    /// Conflict modal shown when a `Save` is blocked because the file changed
    /// on disk since load (external-change guard). Offers Overwrite/Reload/Cancel.
    SaveConflict {
        view_id: ViewId,
        path: PathBuf,
    },
    /// Confirm modal for `File → Revert to Saved` (discards in-memory edits).
    ConfirmRevert {
        view_id: ViewId,
    },
}

impl Overlay {
    pub const TAG_FIND_BAR: &'static str = "FindBar";
    pub const TAG_GOTO_BAR: &'static str = "GoToBar";
    pub const TAG_FILE_DIALOG: &'static str = "FileDialog";
    pub const TAG_CLI_STATUS: &'static str = "CliStatus";
    pub const TAG_FEATURE_FORM: &'static str = "FeatureForm";
    pub const TAG_RENAME_FEATURE: &'static str = "RenameFeature";
    pub const TAG_TRANSLATION: &'static str = "Translation";
    pub const TAG_DIRTY_CLOSE: &'static str = "DirtyCloseConfirm";
    pub const TAG_SAVE_CONFLICT: &'static str = "SaveConflict";
    pub const TAG_CONFIRM_REVERT: &'static str = "ConfirmRevert";
    /// Generic "any overlay" marker — pushed onto the KeyContext stack
    /// whenever [`OverlayStack`] is non-empty. The keymap's Escape
    /// binding matches on this so one binding dismisses every kind.
    pub const TAG_ACTIVE: &'static str = "Overlay";

    pub fn tag(&self) -> &'static str {
        match self {
            Overlay::FindBar(_) => Self::TAG_FIND_BAR,
            Overlay::GoToBar(_) => Self::TAG_GOTO_BAR,
            Overlay::FileDialog(_) => Self::TAG_FILE_DIALOG,
            Overlay::CliStatus(_) => Self::TAG_CLI_STATUS,
            Overlay::FeatureForm(_) => Self::TAG_FEATURE_FORM,
            Overlay::RenameFeature(_) => Self::TAG_RENAME_FEATURE,
            Overlay::Translation(_) => Self::TAG_TRANSLATION,
            Overlay::DirtyCloseConfirm { .. } => Self::TAG_DIRTY_CLOSE,
            Overlay::SaveConflict { .. } => Self::TAG_SAVE_CONFLICT,
            Overlay::ConfirmRevert { .. } => Self::TAG_CONFIRM_REVERT,
        }
    }
}

// ── Stack ─────────────────────────────────────────────────────────────────────

/// Ordered set of currently-active overlays. Order is z-order (bottom
/// of the `Vec` is bottom of the visual stack); the most-recently
/// pushed overlay is the "top" and the target of `DismissOverlay`.
#[derive(Default)]
pub struct OverlayStack {
    overlays: Vec<Overlay>,
}

impl OverlayStack {
    pub fn is_empty(&self) -> bool {
        self.overlays.is_empty()
    }

    /// Push if no overlay of this kind is already present. Returns the
    /// tag that was pushed, or `None` if the kind was a duplicate.
    /// Callers (mostly `command::apply`) emit `OverlayPushed` events
    /// based on the returned tag.
    pub fn push_unique(&mut self, overlay: Overlay) -> Option<&'static str> {
        let tag = overlay.tag();
        if self.overlays.iter().any(|o| o.tag() == tag) {
            return None;
        }
        self.overlays.push(overlay);
        Some(tag)
    }

    /// Pop the topmost overlay. Returns its tag, or `None` if empty.
    pub fn pop(&mut self) -> Option<&'static str> {
        self.overlays.pop().map(|o| o.tag())
    }

    /// Remove the first overlay with the given tag (regardless of
    /// position). Used to dismiss a specific kind even when it isn't
    /// on top — e.g. `DismissCliStatus` doesn't care about z-order.
    pub fn pop_kind(&mut self, tag: &'static str) -> Option<&'static str> {
        let idx = self.overlays.iter().position(|o| o.tag() == tag)?;
        let removed = self.overlays.remove(idx);
        Some(removed.tag())
    }

    /// Tags to layer onto `KeyContext` before keymap dispatch. Emits
    /// every overlay's specific tag plus `"Overlay"` once if any
    /// overlay is present.
    pub fn context_tags(&self) -> impl Iterator<Item = &'static str> + '_ {
        let active = if self.overlays.is_empty() {
            None
        } else {
            Some(Overlay::TAG_ACTIVE)
        };
        self.overlays.iter().map(|o| o.tag()).chain(active)
    }

    /// Typed accessor: returns the active `FindBar` if any. The Find
    /// and GoTo bars are mutually exclusive — `push_unique` rejects a
    /// second bar of either kind.
    pub fn find_bar_mut(&mut self) -> Option<&mut FindBar> {
        self.overlays.iter_mut().find_map(|o| match o {
            Overlay::FindBar(b) => Some(b),
            _ => None,
        })
    }

    pub fn goto_bar_mut(&mut self) -> Option<&mut GoToBar> {
        self.overlays.iter_mut().find_map(|o| match o {
            Overlay::GoToBar(b) => Some(b),
            _ => None,
        })
    }

    pub fn file_dialog_mut(&mut self) -> Option<&mut FileDialog> {
        self.overlays.iter_mut().find_map(|o| match o {
            Overlay::FileDialog(d) => Some(d.as_mut()),
            _ => None,
        })
    }

    pub fn cli_status(&self) -> Option<&str> {
        self.overlays.iter().find_map(|o| match o {
            Overlay::CliStatus(m) => Some(m.as_str()),
            _ => None,
        })
    }

    pub fn feature_form_mut(&mut self) -> Option<&mut FeatureForm> {
        self.overlays.iter_mut().find_map(|o| match o {
            Overlay::FeatureForm(f) => Some(f),
            _ => None,
        })
    }

    pub fn rename_feature_mut(&mut self) -> Option<&mut RenameFeatureForm> {
        self.overlays.iter_mut().find_map(|o| match o {
            Overlay::RenameFeature(f) => Some(f),
            _ => None,
        })
    }

    pub fn translation_mut(&mut self) -> Option<&mut TranslationView> {
        self.overlays.iter_mut().find_map(|o| match o {
            Overlay::Translation(t) => Some(t),
            _ => None,
        })
    }

    /// `(view_id, quitting)` for an open dirty-close confirm modal, if any.
    pub fn dirty_close_confirm(&self) -> Option<(ViewId, bool)> {
        self.overlays.iter().find_map(|o| match o {
            Overlay::DirtyCloseConfirm { view_id, quitting } => Some((*view_id, *quitting)),
            _ => None,
        })
    }

    /// `(view_id, path)` for an open save-conflict modal, if any.
    pub fn save_conflict(&self) -> Option<(ViewId, PathBuf)> {
        self.overlays.iter().find_map(|o| match o {
            Overlay::SaveConflict { view_id, path } => Some((*view_id, path.clone())),
            _ => None,
        })
    }

    /// `view_id` for an open revert-confirm modal, if any.
    pub fn confirm_revert(&self) -> Option<ViewId> {
        self.overlays.iter().find_map(|o| match o {
            Overlay::ConfirmRevert { view_id } => Some(*view_id),
            _ => None,
        })
    }
}

// ── Rendering ─────────────────────────────────────────────────────────────────

/// Render whichever Find / GoTo bar is currently active, inline at
/// the top of the viewer pane. Returns the [`AppCommand`] produced
/// by the bar (submission or dismiss); `None` if no bar is active or
/// the user didn't interact this frame.
///
/// The bar's own keyboard handling covers Enter (submit from focused
/// input) but **no longer** handles Escape — that lives in the keymap
/// (`keymap::KEYMAP`) gated on the `"Overlay"` context tag, so Escape
/// works regardless of which widget has egui focus.
///
/// Only the transient one-shot verbs (Find / GoTo) live here; enzyme querying
/// moved into the Inspector's Cut-sites tab (decision 15 / Phase 1.5b).
pub fn show_inline_bar(stack: &mut OverlayStack, ui: &mut egui::Ui) -> Option<AppCommand> {
    if let Some(b) = stack.find_bar_mut() {
        return render_find_bar(b, ui);
    }
    if let Some(b) = stack.goto_bar_mut() {
        return render_goto_bar(b, ui);
    }
    None
}

fn bar_frame(ui: &egui::Ui) -> egui::Frame {
    egui::Frame::new()
        .fill(ui.visuals().extreme_bg_color)
        .inner_margin(egui::Margin::symmetric(8, 4))
}

fn render_find_bar(b: &mut FindBar, ui: &mut egui::Ui) -> Option<AppCommand> {
    let mut command: Option<AppCommand> = None;

    bar_frame(ui).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label("Find:");
            let text_resp = ui.add(
                egui::TextEdit::singleline(&mut b.pattern)
                    .hint_text("IUPAC pattern…")
                    .desired_width(200.0),
            );
            if b.needs_focus {
                text_resp.request_focus();
                b.needs_focus = false;
            }

            ui.label("Mismatches:");
            let mismatch_resp = ui.add(egui::DragValue::new(&mut b.mismatches).range(0..=5));

            if ui.button("Find").clicked() {
                command = Some(AppCommand::SubmitFind {
                    pattern: b.pattern.clone(),
                    mismatches: b.mismatches,
                });
            }
            if ui.button("Clear").clicked() {
                command = Some(AppCommand::SubmitFind {
                    pattern: String::new(),
                    mismatches: 0,
                });
            }

            // Enter submits. egui idiom: a singleline `TextEdit` gives up
            // focus on Enter, so by the time control returns to us the
            // field's `has_focus()` is already false. `lost_focus()` is
            // the one-frame transition that catches the exact moment;
            // gating on `key_pressed(Enter)` distinguishes Enter-dismiss
            // from click-away and Tab-out.
            let enter_pressed = ui.input(|i| i.key_pressed(Key::Enter));
            if enter_pressed && (text_resp.lost_focus() || mismatch_resp.lost_focus()) {
                command = Some(AppCommand::SubmitFind {
                    pattern: b.pattern.clone(),
                    mismatches: b.mismatches,
                });
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("✕").clicked() {
                    command = Some(AppCommand::DismissOverlay);
                }
            });
        });
    });

    ui.separator();
    command
}

fn render_goto_bar(b: &mut GoToBar, ui: &mut egui::Ui) -> Option<AppCommand> {
    let mut command: Option<AppCommand> = None;

    bar_frame(ui).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label("Go to position:");
            let text_resp = ui.add(
                egui::TextEdit::singleline(&mut b.input)
                    .hint_text("1-based…")
                    .desired_width(100.0),
            );
            if b.needs_focus {
                text_resp.request_focus();
                b.needs_focus = false;
            }

            if ui.button("Go").clicked() {
                if let Ok(pos) = b.input.trim().parse::<usize>() {
                    command = Some(AppCommand::SubmitGoTo { position: pos });
                }
            }

            // Enter submits. See `render_find_bar` for the lost_focus +
            // key_pressed(Enter) idiom rationale.
            let enter_pressed = ui.input(|i| i.key_pressed(Key::Enter));
            if enter_pressed && text_resp.lost_focus() {
                if let Ok(pos) = b.input.trim().parse::<usize>() {
                    command = Some(AppCommand::SubmitGoTo { position: pos });
                }
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("✕").clicked() {
                    command = Some(AppCommand::DismissOverlay);
                }
            });
        });
    });

    ui.separator();
    command
}
