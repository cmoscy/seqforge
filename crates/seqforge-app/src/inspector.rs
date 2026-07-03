//! The Inspector pane — a singleton dock pane (right side) that follows the
//! **active view** and surfaces its derived annotations as noun-collections
//! (`plans/primers.md` "Panels / Inspector", ROADMAP decision 10).
//!
//! Phase 1.3c makes the **Primers** tab interactive: a single click selects a
//! primer (`RevealPrimer` → `View.selected_primer` + reveal its footprint), the
//! selected row highlights, and an on-select detail block shows the full oligo +
//! QC. Editing stays a *launcher → modal* affair (double-click/Enter → the
//! noun's modal, Phase 2.1 for primers); the pane holds no draft state. Like
//! `Tab::FileBrowser`/`Tab::Terminal` it holds **no `ViewId`** — it reads
//! whatever view is active. The primer projection is the shared `PrimerInfo`
//! shape (same as the CLI `primers list`), memoized on `buffer.version`.

use seqforge_core::{PrimerId, PrimerInfo, PrimerState, Strand};

use crate::command::{AppCommand, PendingCommand};
use crate::workspace::Workspace;

/// Version-keyed `PrimerInfo` projection for the active view.
struct PrimerCache {
    view: seqforge_core::ViewId,
    version: u64,
    primers: Vec<PrimerInfo>,
}

/// Singleton Inspector state. Owns only the memoized projection + the current
/// panel selection; the pane reads the active view, so there is no per-view or
/// per-`ViewId` state to orphan.
#[derive(Default)]
pub struct InspectorState {
    cache: Option<PrimerCache>,
    /// The active view's `selected_primer`, mirrored each frame so the pane can
    /// highlight the row (source of truth stays on `View`).
    selected: Option<PrimerId>,
}

impl InspectorState {
    /// Rebuild the primer projection if the active view or its buffer version
    /// changed, and mirror the current panel selection. Called once per frame
    /// before the dock renders. Reuses the one `seqforge_bio::primer_infos`
    /// projection (same as the `ListPrimers` dispatch → CLI parity).
    pub fn refresh(&mut self, workspace: &mut Workspace) {
        let Some(view_id) = workspace.active_view else {
            self.cache = None;
            self.selected = None;
            return;
        };
        // Cheap peek (version + current selection) first; skip the projection
        // rebuild when the buffer version is unchanged.
        let peek = workspace.with_active_buffer(|v, buf, _a| (buf.version, v.selected_primer));
        let (version, selected) = match peek {
            Ok(p) => p,
            Err(_) => {
                self.cache = None;
                self.selected = None;
                return;
            }
        };
        self.selected = selected;

        if self
            .cache
            .as_ref()
            .is_some_and(|c| c.view == view_id && c.version == version)
        {
            return;
        }
        let projected = workspace.with_active_buffer(|_v, buf, ann| {
            let primers: Vec<&seqforge_core::Primer> = ann.primers().collect();
            seqforge_bio::primer_infos(&buf.text, &primers, buf.is_circular())
        });
        if let Ok(primers) = projected {
            self.cache = Some(PrimerCache {
                view: view_id,
                version,
                primers,
            });
        }
    }

    /// The projected primers for the active view (empty when no view is active).
    fn primers(&self) -> &[PrimerInfo] {
        self.cache.as_ref().map_or(&[], |c| c.primers.as_slice())
    }

    /// Render the Primers tab. Row interactions enqueue commands (never mutate
    /// state directly), preserving the single-applier contract.
    pub fn show(&self, ui: &mut egui::Ui, pending: &mut Vec<PendingCommand>) {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.add_space(6.0);
            ui.strong("Primers");
        });
        ui.separator();

        if self.cache.is_none() {
            ui.add_space(8.0);
            ui.vertical_centered(|ui| ui.weak("No file open."));
            return;
        }

        let primers = self.primers();
        if primers.is_empty() {
            ui.add_space(8.0);
            ui.vertical_centered(|ui| ui.weak("No primers on this sequence."));
            return;
        }

        // Attached first (sorted by binding position, top→bottom like the map),
        // then floating oligos in a trailing "Unattached" section (Benchling
        // idiom). Non-mutating derived ordering — the stored order is untouched.
        let mut attached: Vec<&PrimerInfo> =
            primers.iter().filter(|p| p.binding.is_some()).collect();
        attached.sort_by_key(|p| p.binding.as_ref().map(|b| b.start).unwrap_or(usize::MAX));
        let unattached: Vec<&PrimerInfo> =
            primers.iter().filter(|p| p.binding.is_none()).collect();

        egui::ScrollArea::vertical().show(ui, |ui| {
            for p in &attached {
                self.primer_row(ui, p, pending);
            }
            if !unattached.is_empty() {
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.add_space(6.0);
                    ui.weak("Unattached");
                });
                ui.separator();
                for p in &unattached {
                    self.primer_row(ui, p, pending);
                }
            }
        });
    }

    /// One clickable primer row + (when selected) an on-select detail block.
    /// Compact columns: strand glyph · state dot · name … binding · Tm · %GC.
    fn primer_row(&self, ui: &mut egui::Ui, p: &PrimerInfo, pending: &mut Vec<PendingCommand>) {
        let selected = self.selected == Some(p.id);
        let fill = if selected {
            ui.visuals().selection.bg_fill
        } else {
            egui::Color32::TRANSPARENT
        };

        let resp = egui::Frame::new()
            .fill(fill)
            .inner_margin(egui::Margin::symmetric(6, 2))
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.label(strand_glyph(p.strand));
                    ui.colored_label(state_color(ui, p.state), state_dot(p.state))
                        .on_hover_text(state_label(p.state));
                    ui.label(&p.name);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        match p.gc_display() {
                            Some(g) => ui.weak(format!("{g:.0} %")),
                            None => ui.weak("— %"),
                        };
                        ui.add_space(8.0);
                        match p.tm {
                            Some(t) => ui.weak(format!("{t:.1} °C")),
                            None => ui.weak("— °C"),
                        };
                        ui.add_space(8.0);
                        ui.weak(binding_label(p));
                    });
                });
            })
            .response
            .interact(egui::Sense::click());

        if resp.clicked() {
            pending.push((AppCommand::RevealPrimer { id: p.id }, None));
        }

        if selected {
            self.detail(ui, p);
        }
    }

    /// On-select detail: full oligo + the QC that doesn't fit the row.
    fn detail(&self, ui: &mut egui::Ui, p: &PrimerInfo) {
        egui::Frame::new()
            .inner_margin(egui::Margin {
                left: 22,
                right: 6,
                top: 2,
                bottom: 6,
            })
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.weak("5′");
                    ui.monospace(&p.sequence);
                    ui.weak("3′");
                });
                ui.horizontal(|ui| {
                    ui.weak(format!("{} nt", p.len));
                    if p.mismatches > 0 {
                        ui.separator();
                        let s = if p.mismatches == 1 { "" } else { "es" };
                        ui.colored_label(
                            state_color(ui, PrimerState::Drifted),
                            format!("{} mismatch{s}", p.mismatches),
                        );
                    }
                    if p.off_targets > 0 {
                        ui.separator();
                        let s = if p.off_targets == 1 { "" } else { "s" };
                        ui.weak(format!("{} off-target{s}", p.off_targets));
                    }
                });
                let dg = |ui: &mut egui::Ui, label: &str, v: Option<f64>| {
                    ui.horizontal(|ui| {
                        ui.weak(label);
                        match v {
                            Some(v) => ui.monospace(format!("{v:.1} kcal/mol")),
                            None => ui.weak("—"),
                        };
                    });
                };
                if let Some(at) = p.anneal_tm {
                    ui.horizontal(|ui| {
                        ui.weak("anneal Tm");
                        ui.monospace(format!("{at:.1} °C"));
                    });
                }
                dg(ui, "hairpin ΔG", p.hairpin_dg);
                dg(ui, "self-dimer ΔG", p.self_dimer_dg);
            });
    }
}

/// PrimerInfo helpers kept local (display-only).
trait GcDisplay {
    fn gc_display(&self) -> Option<f64>;
}
impl GcDisplay for PrimerInfo {
    fn gc_display(&self) -> Option<f64> {
        (self.len > 0).then_some(self.gc)
    }
}

fn strand_glyph(strand: Strand) -> &'static str {
    match strand {
        Strand::Forward => "→",
        Strand::Reverse => "←",
        Strand::Both => "↔",
        Strand::None => "·",
    }
}

fn state_dot(state: PrimerState) -> &'static str {
    match state {
        PrimerState::Confirmed => "●",
        PrimerState::Drifted => "◐",
        PrimerState::Detached => "○",
    }
}

fn state_label(state: PrimerState) -> &'static str {
    match state {
        PrimerState::Confirmed => "Confirmed",
        PrimerState::Drifted => "Drifted",
        PrimerState::Detached => "Detached",
    }
}

fn state_color(ui: &egui::Ui, state: PrimerState) -> egui::Color32 {
    match state {
        PrimerState::Confirmed => ui.visuals().weak_text_color(),
        PrimerState::Drifted => egui::Color32::from_rgb(0xE0, 0xA0, 0x30),
        PrimerState::Detached => ui.visuals().weak_text_color().gamma_multiply(0.6),
    }
}

/// 1-based inclusive display range, or `Unattached` for a floating oligo.
fn binding_label(p: &PrimerInfo) -> String {
    match &p.binding {
        Some(b) => format!("{}–{}", b.start + 1, b.end),
        None => "Unattached".to_string(),
    }
}

