//! The Cut-sites tab — a query **header** (the re-homed ⌘E enzyme verb) over a
//! grouped enzyme→sites **noun** list (Phase 1.5b / decision 15). Read-only: it
//! reuses the existing `SubmitEnzymes`/`AddEnzymes`/`RemoveEnzyme`/`RevealRange`
//! commands and touches only the enzyme-related `InspectorState` fields, so it
//! extracts cleanly as a split `impl` block.

use super::InspectorState;
use super::row::remove_button;
use crate::command::{AppCommand, PendingCommand};
use crate::overlay::enzyme_rows;

impl InspectorState {
    pub(super) fn show_cutsites(&mut self, ui: &mut egui::Ui, pending: &mut Vec<PendingCommand>) {
        // ── Query header (verb) ──────────────────────────────────────────
        ui.add_space(2.0);
        let mut submit_show = false;
        ui.horizontal(|ui| {
            ui.add_space(6.0);
            let resp = ui.add(
                egui::TextEdit::singleline(&mut self.enzyme_query)
                    .hint_text("unique • type IIs • golden gate • EcoRI BamHI • none")
                    .desired_width(ui.available_width() - 6.0),
            );
            if self.focus_enzyme_query {
                resp.request_focus();
                self.focus_enzyme_query = false;
            }
            // Enter submits as Show (replace the set) — the Find-bar idiom.
            if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                submit_show = true;
            }
        });
        let has_input = !self.enzyme_query.trim().is_empty();
        ui.horizontal(|ui| {
            ui.add_space(6.0);
            if ui.button("Show").clicked() {
                submit_show = true;
            }
            if ui
                .add_enabled(
                    has_input,
                    egui::Button::new(format!("{} Add", egui_phosphor::regular::PLUS)),
                )
                .on_hover_text("Add these enzymes to the current set")
                .clicked()
            {
                pending.push((
                    AppCommand::AddEnzymes {
                        query: self.enzyme_query.clone(),
                    },
                    None,
                ));
            }
            if ui
                .add_enabled(!self.active_enzymes.is_empty(), egui::Button::new("Clear"))
                .clicked()
            {
                pending.push((
                    AppCommand::SubmitEnzymes {
                        query: String::new(),
                    },
                    None,
                ));
            }
        });
        if submit_show {
            pending.push((
                AppCommand::SubmitEnzymes {
                    query: self.enzyme_query.clone(),
                },
                None,
            ));
        }
        ui.separator();

        // ── Grouped enzyme→site list (noun) ──────────────────────────────
        let rows = enzyme_rows(&self.active_enzymes, &self.cut_sites);
        if rows.is_empty() {
            ui.add_space(8.0);
            ui.vertical_centered(|ui| ui.weak("No enzymes shown — type a query above (or ⌘E)."));
            return;
        }
        let total: usize = rows.iter().map(|r| r.sites.len()).sum();
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            ui.add_space(6.0);
            ui.weak(format!(
                "{} enzyme{}, {} site{}",
                rows.len(),
                if rows.len() == 1 { "" } else { "s" },
                total,
                if total == 1 { "" } else { "s" },
            ));
        });

        let expanded = &mut self.enzyme_expanded;
        egui::ScrollArea::vertical().show(ui, |ui| {
            for r in &rows {
                let n = r.sites.len();
                let is_expanded = expanded.contains(&r.name);
                ui.horizontal(|ui| {
                    ui.add_space(6.0);
                    // ▸/▾ for multi-site (expandable); spacer otherwise.
                    let prefix = match n {
                        0 | 1 => "   ",
                        _ if is_expanded => "▾ ",
                        _ => "▸ ",
                    };
                    let name = egui::RichText::new(format!("{prefix}{}", r.name)).monospace();
                    let name = if n == 0 { name.weak() } else { name };
                    let hover = match n {
                        0 => "No sites",
                        1 => "Jump to site",
                        _ if is_expanded => "Collapse",
                        _ => "Show sites",
                    };
                    let resp = ui
                        .add_enabled(n > 0, egui::SelectableLabel::new(is_expanded, name))
                        .on_hover_text(hover);
                    if resp.clicked() {
                        if n == 1 {
                            let s = &r.sites[0];
                            pending.push((
                                AppCommand::RevealRange {
                                    start: s.recognition_start,
                                    end: s.recognition_end,
                                },
                                None,
                            ));
                        } else if n > 1 {
                            if is_expanded {
                                expanded.remove(&r.name);
                            } else {
                                expanded.insert(r.name.clone());
                            }
                        }
                    }
                    ui.label(egui::RichText::new(format!("×{n}")).small().weak());
                    if !r.recognition.is_empty() {
                        ui.label(egui::RichText::new(&r.recognition).monospace().small());
                    }
                    // Shared remove control, pinned right. Enzymes use the ✕ icon:
                    // this drops the enzyme from the displayed set (reversible via
                    // re-query), not a destructive delete — hence ✕, not trash.
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if remove_button(ui, egui_phosphor::regular::X)
                            .on_hover_text(format!("Remove {} from view", r.name))
                            .clicked()
                        {
                            pending.push((
                                AppCommand::RemoveEnzyme {
                                    name: r.name.clone(),
                                },
                                None,
                            ));
                        }
                    });
                });
                // Per-site sub-rows for an expanded multi-site enzyme.
                if n > 1 && is_expanded {
                    for s in &r.sites {
                        ui.horizontal(|ui| {
                            ui.add_space(28.0);
                            if ui
                                .small_button(format!("@ {}", s.recognition_start + 1))
                                .clicked()
                            {
                                pending.push((
                                    AppCommand::RevealRange {
                                        start: s.recognition_start,
                                        end: s.recognition_end,
                                    },
                                    None,
                                ));
                            }
                        });
                    }
                }
            }
        });
    }
}
