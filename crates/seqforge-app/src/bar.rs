use egui::{Key, Modifiers};

use crate::command::AppCommand;

fn find_input_id() -> egui::Id { egui::Id::new("seqforge_find_input") }
fn goto_input_id() -> egui::Id { egui::Id::new("seqforge_goto_input") }

/// Returns true when a bar text field holds egui keyboard focus.
/// The terminal uses this to yield keyboard capture while the bar is active.
///
/// Note: the mismatches DragValue is not tracked here because egui 0.31's
/// DragValue does not expose a stable ID. When the DragValue is tab-focused,
/// the terminal may recapture keyboard events (arrow-key adjustment won't work).
/// Fix: add FocusOwner (see PLAN.md keyboard focus model) when editing lands.
pub fn bar_field_has_focus(ctx: &egui::Context) -> bool {
    ctx.memory(|m| {
        matches!(m.focused(), Some(id) if id == find_input_id() || id == goto_input_id())
    })
}

// ── Bar state ─────────────────────────────────────────────────────────────────

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

pub enum ActiveBar {
    Find(FindBar),
    GoTo(GoToBar),
}

// ── Rendering ─────────────────────────────────────────────────────────────────

/// Render the active inline bar at the top of the viewer pane.
///
/// Returns an [`AppCommand`] when the user submits (Enter or button
/// click) or dismisses (Escape, ✕): one of `SubmitFind`, `SubmitGoTo`,
/// or `DismissOverlay`. The bar does not mutate `*bar` itself any more —
/// `apply()` closes it in response to the returned command (Stage 2 of
/// the focus refactor — see `docs/focus-refactor.md`).
pub fn show_bar(bar: &mut Option<ActiveBar>, ui: &mut egui::Ui) -> Option<AppCommand> {
    let Some(active) = bar else { return None };

    let mut command: Option<AppCommand> = None;

    let frame = egui::Frame::new()
        .fill(ui.visuals().extreme_bg_color)
        .inner_margin(egui::Margin::symmetric(8, 4));

    frame.show(ui, |ui| {
        ui.horizontal(|ui| {
            // Track whether any bar widget has focus — used for Enter/Escape routing.
            let any_focused;

            match active {
                ActiveBar::Find(b) => {
                    ui.label("Find:");
                    let text_resp = ui.add(
                        egui::TextEdit::singleline(&mut b.pattern)
                            .id(find_input_id())
                            .hint_text("IUPAC pattern…")
                            .desired_width(200.0),
                    );
                    if b.needs_focus {
                        text_resp.request_focus();
                        b.needs_focus = false;
                    }

                    ui.label("Mismatches:");
                    let mismatch_resp =
                        ui.add(egui::DragValue::new(&mut b.mismatches).range(0..=5));

                    any_focused = text_resp.has_focus() || mismatch_resp.has_focus();

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

                    // Enter submits from any focused bar widget; consumed so it
                    // doesn't fall through to the terminal or other handlers.
                    if any_focused
                        && ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter))
                    {
                        command = Some(AppCommand::SubmitFind {
                            pattern: b.pattern.clone(),
                            mismatches: b.mismatches,
                        });
                    }
                }

                ActiveBar::GoTo(b) => {
                    ui.label("Go to position:");
                    let text_resp = ui.add(
                        egui::TextEdit::singleline(&mut b.input)
                            .id(goto_input_id())
                            .hint_text("1-based…")
                            .desired_width(100.0),
                    );
                    if b.needs_focus {
                        text_resp.request_focus();
                        b.needs_focus = false;
                    }

                    any_focused = text_resp.has_focus();

                    if ui.button("Go").clicked() {
                        if let Ok(pos) = b.input.trim().parse::<usize>() {
                            command = Some(AppCommand::SubmitGoTo { position: pos });
                        }
                    }

                    if any_focused
                        && ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter))
                    {
                        if let Ok(pos) = b.input.trim().parse::<usize>() {
                            command = Some(AppCommand::SubmitGoTo { position: pos });
                        }
                    }
                }
            }

            // Escape closes from any focused bar widget; consumed to prevent fallthrough.
            if any_focused && ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
                command = Some(AppCommand::DismissOverlay);
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
