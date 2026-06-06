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

use egui::Key;
use egui_file_dialog::FileDialog;

use crate::command::AppCommand;

// ── Inline bar state ──────────────────────────────────────────────────────────

pub struct FindBar {
    pub pattern: String,
    pub mismatches: u8,
    needs_focus: bool,
}

impl Default for FindBar {
    fn default() -> Self {
        Self { pattern: String::new(), mismatches: 0, needs_focus: true }
    }
}

pub struct GoToBar {
    pub input: String,
    needs_focus: bool,
}

impl Default for GoToBar {
    fn default() -> Self {
        Self { input: String::new(), needs_focus: true }
    }
}

pub struct EnzymeBar {
    pub input: String,
    needs_focus: bool,
}

impl Default for EnzymeBar {
    fn default() -> Self {
        Self { input: String::new(), needs_focus: true }
    }
}

// ── Overlay variants ──────────────────────────────────────────────────────────

/// All transient UI that may be present at once. Variants are
/// open-ended for future palette / confirm / agent-prompt overlays.
pub enum Overlay {
    FindBar(FindBar),
    GoToBar(GoToBar),
    EnzymeBar(EnzymeBar),
    /// Boxed because `FileDialog` is ~2.4 KB — keeping it inline would
    /// blow up every `Overlay` value to that size.
    FileDialog(Box<FileDialog>),
    CliStatus(String),
}

impl Overlay {
    pub const TAG_FIND_BAR: &'static str = "FindBar";
    pub const TAG_GOTO_BAR: &'static str = "GoToBar";
    pub const TAG_ENZYME_BAR: &'static str = "EnzymeBar";
    pub const TAG_FILE_DIALOG: &'static str = "FileDialog";
    pub const TAG_CLI_STATUS: &'static str = "CliStatus";
    /// Generic "any overlay" marker — pushed onto the KeyContext stack
    /// whenever [`OverlayStack`] is non-empty. The keymap's Escape
    /// binding matches on this so one binding dismisses every kind.
    pub const TAG_ACTIVE: &'static str = "Overlay";

    pub fn tag(&self) -> &'static str {
        match self {
            Overlay::FindBar(_) => Self::TAG_FIND_BAR,
            Overlay::GoToBar(_) => Self::TAG_GOTO_BAR,
            Overlay::EnzymeBar(_) => Self::TAG_ENZYME_BAR,
            Overlay::FileDialog(_) => Self::TAG_FILE_DIALOG,
            Overlay::CliStatus(_) => Self::TAG_CLI_STATUS,
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

    pub fn enzyme_bar_mut(&mut self) -> Option<&mut EnzymeBar> {
        self.overlays.iter_mut().find_map(|o| match o {
            Overlay::EnzymeBar(b) => Some(b),
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
pub fn show_inline_bar(stack: &mut OverlayStack, ui: &mut egui::Ui) -> Option<AppCommand> {
    if let Some(b) = stack.find_bar_mut() {
        return render_find_bar(b, ui);
    }
    if let Some(b) = stack.goto_bar_mut() {
        return render_goto_bar(b, ui);
    }
    if let Some(b) = stack.enzyme_bar_mut() {
        return render_enzyme_bar(b, ui);
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

fn render_enzyme_bar(b: &mut EnzymeBar, ui: &mut egui::Ui) -> Option<AppCommand> {
    let mut command: Option<AppCommand> = None;

    bar_frame(ui).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label("Enzymes:");
            let text_resp = ui.add(
                egui::TextEdit::singleline(&mut b.input)
                    .hint_text("unique • unique+dual • non-cutters • type IIs • golden gate • moclo • EcoRI BamHI • none")
                    .desired_width(380.0),
            );
            if b.needs_focus {
                text_resp.request_focus();
                b.needs_focus = false;
            }

            if ui.button("Show").clicked() {
                command = Some(AppCommand::SubmitEnzymes { query: b.input.clone() });
            }
            if ui.button("Clear").clicked() {
                command = Some(AppCommand::SubmitEnzymes { query: String::new() });
            }

            // Enter submits — same lost_focus + key_pressed(Enter) idiom as
            // `render_find_bar`.
            let enter_pressed = ui.input(|i| i.key_pressed(Key::Enter));
            if enter_pressed && text_resp.lost_focus() {
                command = Some(AppCommand::SubmitEnzymes { query: b.input.clone() });
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
