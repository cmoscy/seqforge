//! The Inspector pane — a singleton dock pane (right side) that follows the
//! **active view** and surfaces its derived annotations as noun-collections
//! (`plans/primers.md` "Panels / Inspector", ROADMAP decision 10).
//!
//! Phase 1.3b lands the pane machinery + the **Primers** tab as a read-only
//! list; row interactions, columns/detail, and the Cut-sites/Features tabs are
//! Phase 1.3c+. Like `Tab::FileBrowser`/`Tab::Terminal` it holds **no
//! `ViewId`** — it reads whatever view is active, sidestepping the orphan-id
//! bug class. Its primer projection is the shared `PrimerInfo` shape (the same
//! one the CLI `primers list` returns in 1.4), memoized on `buffer.version` so
//! the seed-and-extend/QC pass stays change-scoped (mirrors the viewer's
//! `PrimerAnnealCache`).

use seqforge_core::{PrimerInfo, PrimerState, Strand};

use crate::workspace::Workspace;

/// Version-keyed `PrimerInfo` projection for the active view.
struct PrimerCache {
    view: seqforge_core::ViewId,
    version: u64,
    primers: Vec<PrimerInfo>,
}

/// Singleton Inspector state. Owns only the memoized projection; the pane reads
/// the active view, so there is no per-view or per-`ViewId` state to orphan.
#[derive(Default)]
pub struct InspectorState {
    cache: Option<PrimerCache>,
}

impl InspectorState {
    /// Rebuild the primer projection if the active view or its buffer version
    /// changed. Called once per frame before the dock renders. Reuses the one
    /// `seqforge_bio::primer_infos` projection (same as the `ListPrimers`
    /// dispatch → CLI parity) rather than a parallel GUI-only computation.
    pub fn refresh(&mut self, workspace: &mut Workspace) {
        let Some(view_id) = workspace.active_view else {
            self.cache = None;
            return;
        };
        // Cheap version peek first; skip the rebuild when nothing changed.
        let version = match workspace.with_active_buffer(|_v, buf, _a| buf.version) {
            Ok(v) => v,
            Err(_) => {
                self.cache = None;
                return;
            }
        };
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

    /// Render the Primers tab. Read-only in 1.3b (click-to-reveal + toggles land
    /// in 1.3c). Mouse-only, grabs no keyboard focus.
    pub fn show(&self, ui: &mut egui::Ui) {
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
                primer_row(ui, p);
            }
            if !unattached.is_empty() {
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.add_space(6.0);
                    ui.weak("Unattached");
                });
                ui.separator();
                for p in &unattached {
                    primer_row(ui, p);
                }
            }
        });
    }
}

/// One compact primer row: strand glyph · name · binding · Tm. Compact cues
/// (arrow glyph, state dot) over extra columns, per the "clean-look" rules.
fn primer_row(ui: &mut egui::Ui, p: &PrimerInfo) {
    ui.horizontal(|ui| {
        ui.add_space(6.0);
        ui.label(strand_glyph(p.strand));
        ui.colored_label(state_color(ui, p.state), state_dot(p.state));
        ui.label(&p.name);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add_space(6.0);
            match p.tm {
                Some(t) => ui.weak(format!("{t:.1} °C")),
                None => ui.weak("— °C"),
            };
            ui.add_space(10.0);
            ui.weak(binding_label(p));
        });
    });
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
