//! The Cut-sites tab — a query **header** (the re-homed ⌘E enzyme verb) over a
//! grouped enzyme→sites **noun** list (Phase 1.5b / decision 15). Read-only: it
//! reuses the `SubmitEnzymes`/`AddEnzymes`/`RemoveEnzyme` commands + emits
//! `RevealCutSite` (single-site object selection, map↔panel sync) and touches
//! only the enzyme-related `InspectorState` fields, so it
//! extracts cleanly as a split `impl` block.

use seqforge_core::{CutSite, CutSiteKey, MethylState};

use super::InspectorState;
use super::row::remove_button;
use crate::command::{AppCommand, PendingCommand};
use crate::overlay::enzyme_rows;

/// Cached methylation verdict for the site keyed by (enzyme, recognition_start),
/// read from the `methyl_states` cache parallel to `cut_sites`. `Cuttable` if the
/// site or its cached verdict is absent.
fn cutsite_state(
    cut_sites: &[CutSite],
    methyl_states: &[MethylState],
    enzyme: &str,
    start: usize,
) -> MethylState {
    cut_sites
        .iter()
        .position(|s| s.enzyme == enzyme && s.recognition.start == start)
        .and_then(|i| methyl_states.get(i).copied())
        .unwrap_or(MethylState::Cuttable)
}

fn worst_methyl_state(states: impl IntoIterator<Item = MethylState>) -> MethylState {
    // `MethylState` is ordered by severity, so the worst is the max.
    states.into_iter().max().unwrap_or(MethylState::Cuttable)
}

fn enzyme_row_methyl(
    row: &crate::overlay::EnzymeRow,
    cut_sites: &[CutSite],
    methyl_states: &[MethylState],
) -> MethylState {
    worst_methyl_state(
        row.sites
            .iter()
            .map(|s| cutsite_state(cut_sites, methyl_states, &row.name, s.recognition_start)),
    )
}

fn methyl_label_suffix(state: MethylState) -> &'static str {
    match state {
        MethylState::Cuttable => "",
        MethylState::Blocked => " (blocked)",
        MethylState::Impaired => " (impaired)",
    }
}

const PRESET_MENU: &[(&str, &str)] = &[
    ("Unique cutters", "unique"),
    ("Unique and dual", "unique and dual"),
    ("Non-cutters", "non-cutters"),
    ("Type IIs", "type iis"),
    ("Golden Gate", "golden gate"),
    ("MoClo", "moclo"),
    ("All enzymes", "all"),
];

fn preset_label_for_query(query: &str) -> Option<&'static str> {
    let q = query.trim().to_ascii_lowercase();
    PRESET_MENU.iter().find(|(_, qs)| *qs == q).map(|(l, _)| *l)
}

fn methyl_rich_text(text: String, state: MethylState) -> egui::RichText {
    let rt = egui::RichText::new(text);
    match state {
        MethylState::Cuttable => rt,
        MethylState::Blocked => rt.weak().color(egui::Color32::GRAY),
        MethylState::Impaired => rt.weak(),
    }
}

impl InspectorState {
    pub(super) fn show_cutsites(&mut self, ui: &mut egui::Ui, pending: &mut Vec<PendingCommand>) {
        // ── Query header (verb) ──────────────────────────────────────────
        ui.add_space(2.0);
        let mut submit_show = false;
        ui.horizontal(|ui| {
            ui.add_space(6.0);
            // Preset dropdown — auto-Show on selection.
            let selected_label =
                preset_label_for_query(&self.enzyme_query).unwrap_or("Presets\u{2026}");
            let combo = egui::ComboBox::from_id_salt("cutsite_presets")
                .selected_text(selected_label)
                .width(120.0);
            combo.show_ui(ui, |ui| {
                for &(label, query) in PRESET_MENU {
                    if ui
                        .selectable_label(selected_label == label, label)
                        .clicked()
                    {
                        self.enzyme_query = query.to_string();
                        submit_show = true;
                    }
                }
            });
            let resp = ui.add(
                egui::TextEdit::singleline(&mut self.enzyme_query)
                    .hint_text("EcoRI BamHI \u{2022} HindIII PstI \u{2022} none")
                    .desired_width(ui.available_width() - 6.0),
            );
            if self.focus_enzyme_query {
                resp.request_focus();
                self.focus_enzyme_query = false;
            }
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
        // Host methylation toggles — verdicts derive at read time; sites stay put.
        let mut methyl = self.methylation;
        let mut methyl_changed = false;
        ui.horizontal(|ui| {
            ui.add_space(6.0);
            methyl_changed |= ui.checkbox(&mut methyl.dam, "Dam").changed();
            methyl_changed |= ui.checkbox(&mut methyl.dcm, "Dcm").changed();
            methyl_changed |= ui.checkbox(&mut methyl.cpg, "CpG").changed();
        });
        if methyl_changed {
            pending.push((
                AppCommand::SetMethylation {
                    dam: methyl.dam,
                    dcm: methyl.dcm,
                    cpg: methyl.cpg,
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

        // The selected cut site (map↔panel sync) — read before the mutable
        // `expanded` borrow. A selected multi-site enzyme reveals its sub-rows
        // (so the specific site shows) without fighting the manual expand toggle.
        let sel_cut = match &self.selected {
            Some(super::SelectedNoun::CutSite(k)) => Some(k.clone()),
            _ => None,
        };
        let owns_selected = |r: &crate::overlay::EnzymeRow| {
            sel_cut.as_ref().is_some_and(|k| {
                k.enzyme == r.name
                    && r.sites
                        .iter()
                        .any(|s| s.recognition_start == k.recognition_start)
            })
        };
        let expanded = &mut self.enzyme_expanded;
        egui::ScrollArea::vertical().show(ui, |ui| {
            for r in &rows {
                let n = r.sites.len();
                let is_expanded = expanded.contains(&r.name);
                let row_selected = owns_selected(r);
                // Show sub-rows when manually expanded OR when this enzyme owns
                // the selected site (so the chosen site is always visible).
                let show_sites = n > 1 && (is_expanded || row_selected);
                let row_methyl = enzyme_row_methyl(r, &self.cut_sites, &self.methyl_states);
                let methyl_star = if row_methyl != MethylState::Cuttable {
                    "*"
                } else {
                    ""
                };
                ui.horizontal(|ui| {
                    ui.add_space(6.0);
                    // ▸/▾ for multi-site (expandable); spacer otherwise.
                    let prefix = match n {
                        0 | 1 => "   ",
                        _ if show_sites => "▾ ",
                        _ => "▸ ",
                    };
                    let display_name = format!("{prefix}{}{methyl_star}", r.name);
                    let name = if n == 0 {
                        egui::RichText::new(display_name).monospace().weak()
                    } else {
                        methyl_rich_text(display_name, row_methyl).monospace()
                    };
                    let hover = match n {
                        0 => "No sites",
                        1 => "Jump to site",
                        _ if show_sites => "Collapse",
                        _ => "Show sites",
                    };
                    // Highlight the row when it owns the selected site (single-site)
                    // or is expanded (multi-site affordance).
                    let highlighted = row_selected || (show_sites && n > 1);
                    let resp = ui
                        .add_enabled(n > 0, egui::SelectableLabel::new(highlighted, name))
                        .on_hover_text(hover);
                    if resp.clicked() {
                        if n == 1 {
                            let s = &r.sites[0];
                            pending.push((
                                AppCommand::RevealCutSite {
                                    key: CutSiteKey {
                                        enzyme: r.name.clone(),
                                        recognition_start: s.recognition_start,
                                    },
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
                // Per-site sub-rows for an expanded / selected multi-site enzyme.
                if show_sites {
                    for s in &r.sites {
                        let site_selected = sel_cut.as_ref().is_some_and(|k| {
                            k.enzyme == r.name && k.recognition_start == s.recognition_start
                        });
                        let site_methyl = cutsite_state(
                            &self.cut_sites,
                            &self.methyl_states,
                            &r.name,
                            s.recognition_start,
                        );
                        let label = format!(
                            "@ {}{}",
                            s.recognition_start + 1,
                            methyl_label_suffix(site_methyl)
                        );
                        ui.horizontal(|ui| {
                            ui.add_space(28.0);
                            if ui
                                .add(egui::SelectableLabel::new(
                                    site_selected,
                                    methyl_rich_text(label, site_methyl),
                                ))
                                .clicked()
                            {
                                pending.push((
                                    AppCommand::RevealCutSite {
                                        key: CutSiteKey {
                                            enzyme: r.name.clone(),
                                            recognition_start: s.recognition_start,
                                        },
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
