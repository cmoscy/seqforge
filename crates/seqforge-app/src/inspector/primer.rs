//! The Primers tab — the `PrimerDraft`, display row, read-only viewer (with the
//! binding-site list + Attach/Rescan actions), inline editor, and the
//! `InspectorState::{show_primers, begin_primer_create}` render loop. This is
//! where Phase 2.2 tail-composition (insertion tools) grows. `PrimerDraft` fields
//! are `pub(super)` because `mod.rs` (`refresh`, `begin_primer_edit`) constructs
//! and reconciles the draft by struct literal; the methods and most free
//! functions stay private (only `draft_anneal_tm`, used by `refresh`, is shared).

use std::sync::OnceLock;

use seqforge_bio::EnzymeSpec;
use seqforge_core::{
    PrimerId, PrimerInfo, PrimerState, Span, Strand, ViewSelection, ViewerRequest,
};

use super::row::{
    DetailLine, EditOutcome, Row, binding_label, detail_frame, row_shell, strand_flag, strand_glyph,
};
use super::{InspectorState, InspectorTab};
use crate::command::{AppCommand, PendingCommand};
use crate::workspace::Workspace;

/// The enzyme catalog (name + Type IIs / overhang-length projection), built once.
/// The app doesn't link `seqforge-restriction`; this reaches enzyme geometry
/// through the bio seam.
fn enzyme_catalog() -> &'static [EnzymeSpec] {
    static CATALOG: OnceLock<Vec<EnzymeSpec>> = OnceLock::new();
    CATALOG.get_or_init(seqforge_bio::enzyme_catalog)
}

/// Live draft for the Inspector's inline **primer** editor (Phase 2.1 / decision
/// 15) — the sibling of [`super::feature::FeatureDraft`]. `id = Some` edits an
/// existing primer (commits `UpdatePrimer`); `id = None` creates one (`AddPrimer`,
/// create-from-selection). Pane-local until commit; the buffer never mutates
/// until then. The oligo `sequence` is the authored reagent (5'→3', tail incl.);
/// `attached` reflects whether the primer has an annealing footprint.
pub(super) struct PrimerDraft {
    pub(super) id: Option<PrimerId>,
    pub(super) name: String,
    pub(super) sequence: String,
    /// `"+"`, `"-"`, or `"."`.
    pub(super) strand: String,
    /// Whether this primer has a binding footprint (drives whether start/end are
    /// editable + sent). A detached/floating oligo keeps `attached = false`.
    pub(super) attached: bool,
    pub(super) start: usize,
    pub(super) end: usize,
    pub(super) needs_focus: bool,
    pub(super) confirm_delete: bool,
    /// Memoized self-structure QC, keyed on the exact oligo string it was computed
    /// for. egui is immediate-mode, so `primer_editor` re-runs every frame; the
    /// O(n³) fold behind [`primer_qc`](seqforge_bio::primer_qc) recomputes only
    /// when `sequence` actually changes (compute-on-change, not per-frame).
    pub(super) qc_cache: Option<(String, seqforge_bio::PrimerQc)>,
    /// Live primer:template annealing Tm (°C) for the draft's current footprint,
    /// recomputed in [`InspectorState::refresh`] (which has the template) whenever
    /// the draft is attached with a valid binding. `None` when floating/invalid.
    pub(super) anneal_tm: Option<f64>,
    /// Transient state for the "Insert" tail-composition affordance (Phase 2.2a):
    /// the picked enzyme, an overhang buffer (Type IIs), a filler-bases buffer,
    /// and the last compose error. Never committed — drives the editor only.
    pub(super) insert: InsertState,
}

/// Editor-transient state for the tail-composition insert tools (Phase 2.2a).
#[derive(Default)]
pub(super) struct InsertState {
    enzyme: String,
    overhang: String,
    bases: String,
    error: Option<String>,
}

impl PrimerDraft {
    /// Seed an edit draft from the projection row (`UpdatePrimer` on commit).
    fn from_info(p: &PrimerInfo) -> Self {
        let (attached, start, end) = match &p.binding {
            Some(b) => (true, b.start, b.start + b.len),
            None => (false, 0, 0),
        };
        Self {
            id: Some(p.id),
            name: p.name.clone(),
            sequence: p.sequence.clone(),
            strand: strand_flag(p.strand).to_string(),
            attached,
            start,
            end,
            needs_focus: true,
            confirm_delete: false,
            qc_cache: None,
            anneal_tm: None,
            insert: InsertState::default(),
        }
    }

    /// Seed a create draft (`AddPrimer` on commit). `binding` + `oligo` come from
    /// the current selection when creating from a range (else a floating oligo).
    pub(super) fn create(name: String, oligo: String, binding: Option<Span>) -> Self {
        let (attached, start, end) = match binding {
            Some(b) => (true, b.start, b.start + b.len),
            None => (false, 0, 0),
        };
        Self {
            id: None,
            name,
            sequence: oligo,
            strand: "+".to_string(),
            attached,
            start,
            end,
            needs_focus: true,
            confirm_delete: false,
            qc_cache: None,
            anneal_tm: None,
            insert: InsertState::default(),
        }
    }

    /// A valid draft needs a non-empty oligo and (when attached) a non-empty
    /// half-open footprint.
    fn is_valid(&self) -> bool {
        !self.sequence.trim().is_empty() && (!self.attached || self.start < self.end)
    }

    /// Map the draft to its one CLI verb — `UpdatePrimer` (edit) or `AddPrimer`
    /// (create) — so GUI and agent can't drift. When attached, the footprint is
    /// sent as `start`/`end`; when floating, an edit sends `detach: true` to
    /// *clear* any existing binding (the `(None, None) = keep` ambiguity), while a
    /// create simply omits the footprint.
    fn to_request(&self) -> ViewerRequest {
        let (start, end) = if self.attached {
            (Some(self.start), Some(self.end))
        } else {
            (None, None)
        };
        match self.id {
            Some(id) => ViewerRequest::UpdatePrimer {
                id,
                name: Some(self.name.clone()),
                sequence: Some(self.sequence.clone()),
                strand: Some(self.strand.clone()),
                start,
                end,
                detach: !self.attached,
                view: None,
            },
            None => ViewerRequest::AddPrimer {
                name: Some(self.name.clone()),
                sequence: self.sequence.clone(),
                start,
                end,
                strand: self.strand.clone(),
                view: None,
            },
        }
    }

    /// The `RemovePrimer` verb (only meaningful for an existing primer).
    fn to_delete_request(&self) -> Option<ViewerRequest> {
        self.id
            .map(|id| ViewerRequest::RemovePrimer { id, view: None })
    }
}

/// Compact display row for a primer (the Primers tab renders these directly so it
/// can layer the inline viewer/editor under the selected one — decision 15).
fn primer_display_row(p: &PrimerInfo, selected: bool) -> Row {
    let tone = match p.state {
        PrimerState::Confirmed => super::row::Tone::Normal,
        PrimerState::Drifted => super::row::Tone::Warn,
        PrimerState::Detached => super::row::Tone::Dim,
    };
    Row {
        selected,
        glyph: Some(strand_glyph(p.strand)),
        dot: Some(tone),
        name: p.name.clone(),
        dim_name: matches!(p.state, PrimerState::Detached),
        right: vec![
            binding_label(p),
            p.tm.map_or_else(|| "— °C".into(), |t| format!("{t:.1} °C")),
            if p.len > 0 {
                format!("{:.0} %", p.gc)
            } else {
                "— %".into()
            },
        ],
    }
}

/// The on-select detail lines for a primer (full oligo + QC), shown under a
/// selected row in the read-only viewer.
fn primer_detail_lines(p: &PrimerInfo) -> Vec<DetailLine> {
    let mut detail = vec![DetailLine {
        text: format!("5′ {} 3′", p.sequence),
        mono: true,
    }];
    let mut meta = format!("{} nt", p.len);
    if p.mismatches > 0 {
        let s = if p.mismatches == 1 { "" } else { "es" };
        meta += &format!(" · {} mismatch{s}", p.mismatches);
    }
    if p.off_targets > 0 {
        let s = if p.off_targets == 1 { "" } else { "s" };
        meta += &format!(" · {} off-target{s}", p.off_targets);
    }
    detail.push(DetailLine {
        text: meta,
        mono: false,
    });
    if let Some(at) = p.anneal_tm {
        detail.push(DetailLine {
            text: format!("anneal Tm {at:.1} °C"),
            mono: false,
        });
    }
    if let Some(h) = p.hairpin_dg {
        detail.push(DetailLine {
            text: format!("hairpin ΔG {h:.1} kcal/mol"),
            mono: false,
        });
    }
    if let Some(d) = p.self_dimer_dg {
        detail.push(DetailLine {
            text: format!("self-dimer ΔG {d:.1} kcal/mol"),
            mono: false,
        });
    }
    detail
}

/// Apply one frame's editor outcome to the pane-local primer draft: commit posts
/// the one `ViewerRequest` (`AddPrimer`/`UpdatePrimer`/`RemovePrimer`) and clears
/// the draft; cancel just clears it. Shared by the create + edit render paths.
fn commit_primer_outcome(
    outcome: EditOutcome,
    editing: &mut Option<PrimerDraft>,
    pending: &mut Vec<PendingCommand>,
) {
    match outcome {
        EditOutcome::Commit(req) | EditOutcome::Delete(req) => {
            pending.push((AppCommand::Viewer(req), None));
            *editing = None;
        }
        EditOutcome::Cancel => *editing = None,
    }
}

/// Read-only detail shown under a selected (non-editing) primer row: full oligo +
/// QC (the [`primer_detail_lines`]), the list of every place the oligo anneals
/// (attached + off-target, each with an anneal Tm and — for the unattached ones —
/// an **Attach** action), a **Rescan** re-anchor for a drifted/detached primer,
/// plus the gesture into edit mode. Returns `true` when the user asks to edit
/// (Edit button; double-click is the other entry point). Enqueues its own
/// site/rescan commands. Mirror of [`super::feature`]'s `feature_viewer`.
fn primer_viewer(ui: &mut egui::Ui, p: &PrimerInfo, pending: &mut Vec<PendingCommand>) -> bool {
    let mut edit = false;
    detail_frame().show(ui, |ui| {
        for line in primer_detail_lines(p) {
            if line.mono {
                ui.horizontal_wrapped(|ui| ui.monospace(&line.text));
            } else {
                ui.weak(&line.text);
            }
        }

        if !p.sites.is_empty() {
            // Header names the reality: a floating oligo shows *candidate* sites;
            // an anchored one shows its footprint + any off-targets.
            let header = if p.binding.is_some() {
                "Binding sites"
            } else {
                "Candidate sites"
            };
            ui.add_space(2.0);
            ui.weak(header);
            for site in &p.sites {
                ui.horizontal(|ui| {
                    let tm = site
                        .anneal_tm
                        .map_or_else(|| "— °C".into(), |t| format!("{t:.1} °C"));
                    let mm = if site.mismatches == 0 {
                        String::new()
                    } else {
                        format!(" · {} mm", site.mismatches)
                    };
                    let label = format!(
                        "{} {}–{} · {tm}{mm}",
                        strand_glyph(site.strand),
                        site.span.start + 1,
                        site.span.start + site.span.len,
                    );
                    if site.attached {
                        ui.strong(label);
                        ui.weak("attached");
                    } else {
                        ui.weak(label);
                        if ui.small_button("Attach").clicked() {
                            pending.push((
                                AppCommand::Viewer(ViewerRequest::UpdatePrimer {
                                    id: p.id,
                                    name: None,
                                    sequence: None,
                                    strand: Some(strand_flag(site.strand).to_string()),
                                    start: Some(site.span.start),
                                    end: Some(site.span.start + site.span.len),
                                    detach: false,
                                    view: None,
                                }),
                                None,
                            ));
                        }
                    }
                });
            }
        }

        ui.add_space(2.0);
        ui.horizontal(|ui| {
            if ui.small_button("Edit").clicked() {
                edit = true;
            }
            // Re-anchor to the best site — offered when the primer isn't cleanly
            // attached (drifted footprint or floating oligo) and something binds.
            if !matches!(p.state, PrimerState::Confirmed) && !p.sites.is_empty() {
                if ui.small_button("Rescan").clicked() {
                    pending.push((
                        AppCommand::Viewer(ViewerRequest::RescanPrimer {
                            id: p.id,
                            view: None,
                        }),
                        None,
                    ));
                }
            } else {
                ui.weak("or double-click");
            }
        });
    });
    edit
}

/// The "Insert" tail-composition affordance (Phase 2.2a). Prepends a restriction
/// site (`restriction_tail` via the bio seam) or filler bases to the draft oligo
/// — a **staged** string mutation, so the QC readout / site list update live and
/// commit still rides the existing `UpdatePrimer`. The tail leaves the binding
/// footprint untouched (it's 5'), so `decompose_primer` treats it as tail.
fn insert_tools(ui: &mut egui::Ui, d: &mut PrimerDraft) {
    let salt = d.id.map_or(0, |id| id.0.wrapping_add(1));
    ui.horizontal_wrapped(|ui| {
        ui.label("Insert");
        let selected = if d.insert.enzyme.is_empty() {
            "enzyme…".to_string()
        } else {
            d.insert.enzyme.clone()
        };
        egui::ComboBox::from_id_salt(("primer_insert_enzyme", salt))
            .selected_text(selected)
            .show_ui(ui, |ui| {
                for spec in enzyme_catalog() {
                    ui.selectable_value(&mut d.insert.enzyme, spec.name.clone(), &spec.name);
                }
            });
        let spec = enzyme_catalog().iter().find(|s| s.name == d.insert.enzyme);
        let overhang_len = spec.filter(|s| s.type_iis).and_then(|s| s.overhang_len);
        if let Some(n) = overhang_len {
            ui.add(
                egui::TextEdit::singleline(&mut d.insert.overhang)
                    .desired_width(64.0)
                    .hint_text(format!("{n} nt")),
            );
        }
        if ui
            .add_enabled(!d.insert.enzyme.is_empty(), egui::Button::new("Add site"))
            .clicked()
        {
            let overhang = overhang_len.map(|_| d.insert.overhang.clone());
            match seqforge_bio::restriction_tail(&d.insert.enzyme, overhang.as_deref(), None) {
                Ok(tail) => {
                    d.sequence.insert_str(0, &tail);
                    d.insert.overhang.clear();
                    d.insert.error = None;
                }
                Err(e) => d.insert.error = Some(e.to_string()),
            }
        }
        // Filler bases (validated at commit by `parse_oligo`).
        ui.add(
            egui::TextEdit::singleline(&mut d.insert.bases)
                .desired_width(72.0)
                .hint_text("5′ bases"),
        );
        if ui
            .add_enabled(
                !d.insert.bases.trim().is_empty(),
                egui::Button::new("Prepend"),
            )
            .clicked()
        {
            let bases = d.insert.bases.trim().to_ascii_uppercase();
            d.sequence.insert_str(0, &bases);
            d.insert.bases.clear();
        }
    });
    if let Some(err) = &d.insert.error {
        ui.colored_label(egui::Color32::from_rgb(0xE0, 0x60, 0x60), err);
    }
}

/// The inline field editor for a primer draft (create or edit — decision 15).
/// Mutates the pane-local draft; returns `Some(Commit)` on Save/Enter (draft
/// valid), `Some(Cancel)` on Cancel/Escape, `Some(Delete)` when an armed delete
/// is confirmed, else `None`. **Enter always commits the current primary action**
/// (Save when editing, the delete when armed) — the canvas staging grammar.
/// Carries a live Tm/%GC/self-structure QC readout off the draft oligo.
fn primer_editor(ui: &mut egui::Ui, d: &mut PrimerDraft) -> Option<EditOutcome> {
    let mut outcome = None;
    let mut submit_on_enter = false;
    let grid_salt = d.id.map_or(0, |id| id.0.wrapping_add(1));
    detail_frame().show(ui, |ui| {
        egui::Grid::new(("primer_inline_editor", grid_salt))
            .num_columns(2)
            .spacing([10.0, 5.0])
            .show(ui, |ui| {
                ui.label("Name");
                let r = ui.text_edit_singleline(&mut d.name);
                if d.needs_focus {
                    r.request_focus();
                    d.needs_focus = false;
                }
                if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    submit_on_enter = true;
                }
                ui.end_row();

                ui.label("Oligo 5′→3′");
                if ui
                    .add(
                        egui::TextEdit::singleline(&mut d.sequence)
                            .font(egui::TextStyle::Monospace),
                    )
                    .lost_focus()
                    && ui.input(|i| i.key_pressed(egui::Key::Enter))
                {
                    submit_on_enter = true;
                }
                ui.end_row();

                ui.label("Strand");
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut d.strand, "+".into(), "+ fwd");
                    ui.selectable_value(&mut d.strand, "-".into(), "− rev");
                    ui.selectable_value(&mut d.strand, ".".into(), ". none");
                });
                ui.end_row();

                // Binding: an Attached ⇄ Floating toggle (③). Attaching seeds a
                // default footprint (oligo-length at the current start) if none is
                // set; a commit sends the footprint or `detach: true` accordingly.
                ui.label("Binding");
                ui.horizontal(|ui| {
                    if ui.selectable_label(d.attached, "Attached").clicked() && !d.attached {
                        d.attached = true;
                        if d.start >= d.end {
                            d.end = d.start + d.sequence.chars().count().max(1);
                        }
                    }
                    if ui.selectable_label(!d.attached, "Floating").clicked() {
                        d.attached = false;
                    }
                });
                ui.end_row();
                if d.attached {
                    ui.label("Start");
                    ui.add(egui::DragValue::new(&mut d.start).range(0..=usize::MAX));
                    ui.end_row();
                    ui.label("End");
                    ui.add(egui::DragValue::new(&mut d.end).range(d.start + 1..=usize::MAX));
                    ui.end_row();
                }
            });

        ui.add_space(2.0);
        insert_tools(ui, d);

        // Live QC readout off the draft oligo (Phase 0.5 thermo). The evaluation
        // layer is total + self-describing (decision 12): Tm is a `Result` whose
        // `Err` *is* the "not meaningful yet" signal, GC is always defined, and the
        // structure ΔGs are `Ok(0.0)` when nothing can fold. So the view just renders
        // what QC returns — no length threshold. egui re-runs this closure every
        // frame, so the O(n³) fold is memoized on the exact oligo string.
        if d.sequence.is_empty() {
            d.qc_cache = None;
            ui.weak("Tm — · —% GC"); // nothing typed yet
        } else {
            // Anneal Tm (①) leads when the draft is attached — it's the number that
            // governs annealing to *this* footprint; self-Tm stays as context.
            let anneal = d.anneal_tm.filter(|_| d.attached);
            if d.qc_cache.as_ref().map(|(seq, _)| seq) != Some(&d.sequence) {
                let qc = seqforge_bio::primer_qc(&d.sequence);
                d.qc_cache = Some((d.sequence.clone(), qc));
            }
            let qc = &d.qc_cache.as_ref().expect("qc_cache set above").1;
            let self_tm = qc
                .tm
                .as_ref()
                .map_or_else(|_| "—".to_string(), |t| format!("{t:.1} °C"));
            let head = match anneal {
                Some(at) => format!("anneal Tm {at:.1} °C · self {self_tm}"),
                None => format!("Tm {self_tm}"),
            };
            ui.horizontal_wrapped(|ui| {
                ui.weak(format!("{head} · {:.0}% GC", qc.gc));
                // Structure ΔG lines appear only when *destabilizing* (< 0): a
                // ΔG ≥ 0 is the healthy primer (and the short-oligo) case, which
                // needs no line. Value-driven — the readout surfaces problems.
                if let Ok(h) = &qc.hairpin_dg {
                    if *h < 0.0 {
                        ui.weak(format!("· hairpin ΔG {h:.1}"));
                    }
                }
                if let Ok(sd) = &qc.self_dimer_dg {
                    if *sd < 0.0 {
                        ui.weak(format!("· self-dimer ΔG {sd:.1}"));
                    }
                }
            });
        }

        ui.add_space(4.0);
        if d.confirm_delete {
            // Armed delete (existing primer only): Enter confirms the delete.
            let enter = submit_on_enter || ui.input(|i| i.key_pressed(egui::Key::Enter));
            ui.horizontal(|ui| {
                let btn = egui::Button::new(
                    egui::RichText::new(format!(
                        "{} Confirm delete?  (Enter)",
                        egui_phosphor::regular::TRASH
                    ))
                    .color(egui::Color32::WHITE),
                )
                .fill(egui::Color32::from_rgb(0xB0, 0x30, 0x30));
                if (ui.add(btn).clicked() || enter) && d.to_delete_request().is_some() {
                    outcome = Some(EditOutcome::Delete(d.to_delete_request().expect("checked")));
                }
                if ui.button("Cancel").clicked() {
                    outcome = Some(EditOutcome::Cancel);
                }
            });
        } else {
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(d.is_valid(), egui::Button::new("Save"))
                    .clicked()
                    || (submit_on_enter && d.is_valid())
                {
                    outcome = Some(EditOutcome::Commit(d.to_request()));
                }
                if ui.button("Cancel").clicked() {
                    outcome = Some(EditOutcome::Cancel);
                }
                // Delete pinned right — only for an existing primer (create has
                // nothing to remove). Arms the two-step confirm (modal-free).
                if d.id.is_some() {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .button(egui::RichText::new(format!(
                                "{} Delete",
                                egui_phosphor::regular::TRASH
                            )))
                            .on_hover_text("Delete this primer")
                            .clicked()
                        {
                            d.confirm_delete = true;
                        }
                    });
                }
            });
        }
    });
    if outcome.is_none() && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        outcome = Some(EditOutcome::Cancel);
    }
    outcome
}

/// Primer:template annealing Tm (°C) for a draft's current footprint, or `None`
/// when the draft is floating / the footprint is invalid or out of bounds. Only
/// Forward/Reverse primers anneal (an unstranded oligo has no duplex sense).
pub(super) fn draft_anneal_tm(d: &PrimerDraft, workspace: &mut Workspace) -> Option<f64> {
    if !d.attached || d.start >= d.end {
        return None;
    }
    let strand = match d.strand.as_str() {
        "+" => Strand::Forward,
        "-" => Strand::Reverse,
        _ => return None,
    };
    workspace
        .with_active_buffer(|_v, buf, _ann| {
            (d.end <= buf.text.len())
                .then(|| seqforge_bio::anneal_tm(&d.sequence, &(d.start..d.end), strand, &buf.text))
                .and_then(Result::ok)
        })
        .ok()
        .flatten()
}

impl InspectorState {
    /// The Primers tab: the read-only list expands the selected row into an
    /// inline **viewer** (full oligo + QC) and, on an edit gesture, an inline
    /// **editor** (Phase 2.1). A create draft (`id = None`, from ＋ Add primer)
    /// renders at the top. Editing/creating is pane-local until commit, which
    /// posts one `UpdatePrimer`/`AddPrimer` (the CLI verb) through the single
    /// applier + history. Mirrors `show_features`.
    pub(super) fn show_primers(&mut self, ui: &mut egui::Ui, pending: &mut Vec<PendingCommand>) {
        // Attached-first, floating oligos last (list mirrors the map top→bottom).
        let mut primers = self.primers().to_vec();
        primers.sort_by_key(|p| p.binding.as_ref().map_or(usize::MAX, |b| b.start));
        let pair = match &self.selected {
            Some(super::SelectedNoun::PrimerPair { fwd, rev }) => Some((*fwd, *rev)),
            _ => None,
        };
        let selected = match &self.selected {
            Some(super::SelectedNoun::Primer(id)) => Some(*id),
            _ => None,
        };
        // Pane-local: whether Run PCR labels the product (opt-in, off by default).
        // A local so it stays disjoint from the `editing` field borrow below.
        let mut pcr_label = self.pcr_label;
        let editing = &mut self.editing_primer;

        egui::ScrollArea::vertical().show(ui, |ui| {
            // A create draft has no row to sit under → render it at the top.
            if editing.as_ref().is_some_and(|d| d.id.is_none()) {
                let d = editing.as_mut().expect("checked is_none ⇒ Some");
                ui.add_space(2.0);
                ui.horizontal(|ui| {
                    ui.add_space(6.0);
                    ui.strong("New primer");
                });
                if let Some(outcome) = primer_editor(ui, d) {
                    commit_primer_outcome(outcome, editing, pending);
                }
                ui.separator();
            }

            // PCR primer-pair banner (Phase 3.1b): Cmd-click two primers to form a
            // pair, then Run PCR. Orientation is derived from strand (fwd = the
            // top-strand binder), so there is no swap. One `Pcr` op — the same
            // `seqforge pcr --fwd --rev` the CLI/agent drives.
            if let Some((fwd, rev)) = pair {
                let name_of = |id: PrimerId| {
                    primers
                        .iter()
                        .find(|p| p.id == id)
                        .map_or_else(|| format!("primer {id}"), |p| p.name.clone())
                };
                detail_frame().show(ui, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.strong("PCR pair");
                        ui.weak(format!("{} → {}", name_of(fwd), name_of(rev)));
                    });
                    ui.horizontal(|ui| {
                        if ui.button("Run PCR").clicked() {
                            pending.push((
                                AppCommand::Viewer(ViewerRequest::Pcr {
                                    fwd,
                                    rev,
                                    name: None,
                                    product_feature: pcr_label,
                                    view: None,
                                }),
                                None,
                            ));
                        }
                        ui.checkbox(&mut pcr_label, "Label product")
                            .on_hover_text("Add a whole-product feature labelling the amplicon");
                    });
                });
                ui.separator();
            }

            if primers.is_empty() {
                ui.add_space(8.0);
                ui.vertical_centered(|ui| ui.weak("No primers on this sequence."));
                return;
            }

            let cmd_held = ui.input(|i| i.modifiers.command);
            for p in &primers {
                let in_pair = pair.is_some_and(|(f, r)| f == p.id || r == p.id);
                let is_sel = selected == Some(p.id) || in_pair;
                let resp = row_shell(ui, &primer_display_row(p, is_sel));
                if resp.double_clicked() {
                    *editing = Some(PrimerDraft::from_info(p));
                    pending.push((AppCommand::RevealPrimer { id: p.id }, None));
                } else if resp.clicked() {
                    // Cmd-click builds / edits the PCR pair (bounded multi-select);
                    // a plain click selects the single primer as before.
                    if cmd_held {
                        pending.push((AppCommand::PromotePrimerPair { id: p.id }, None));
                    } else {
                        // Selecting a different row cancels an in-flight edit.
                        if editing.as_ref().is_some_and(|d| d.id != Some(p.id)) {
                            *editing = None;
                        }
                        pending.push((AppCommand::RevealPrimer { id: p.id }, None));
                    }
                }

                // The inline viewer/editor expands only under the single selection
                // (a pair is a run action, not a per-primer detail).
                if selected == Some(p.id) {
                    let editing_this = editing.as_ref().is_some_and(|d| d.id == Some(p.id));
                    if editing_this {
                        let d = editing.as_mut().expect("editing_this ⇒ Some");
                        if let Some(outcome) = primer_editor(ui, d) {
                            commit_primer_outcome(outcome, editing, pending);
                        }
                    } else if primer_viewer(ui, p, pending) {
                        *editing = Some(PrimerDraft::from_info(p));
                        pending.push((AppCommand::RevealPrimer { id: p.id }, None));
                    }
                }
            }
        });
        self.pcr_label = pcr_label;
    }

    /// Begin a create-from-selection primer draft: binding = the current range
    /// selection when present (binding = selection, oligo = template slice), else
    /// a floating oligo; the name defaults to the shared `suggest_primer_name()`
    /// (decision 9, editable before commit). Clears any in-flight edit.
    pub(super) fn begin_primer_create(&mut self, pending: &mut Vec<PendingCommand>) {
        self.tab = InspectorTab::Primers;
        let (oligo, binding) = match &self.selection_seed {
            Some((range, oligo)) => (oligo.clone(), Some(Span::from_range(range.clone()))),
            None => (String::new(), None),
        };
        self.editing_primer = Some(PrimerDraft::create(
            self.suggested_primer_name.clone(),
            oligo,
            binding,
        ));
        // A create draft has no selected row; clear the panel selection so the
        // editor renders at the top, not under a stale row.
        pending.push((AppCommand::Select(ViewSelection::None), None));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn primer_info(binding: Option<std::ops::Range<usize>>, strand: Strand) -> PrimerInfo {
        PrimerInfo {
            id: PrimerId(4),
            name: "P1".into(),
            sequence: "ATGCGT".into(),
            binding: binding.map(Span::from_range),
            strand,
            len: 6,
            tm: Some(42.0),
            gc: 50.0,
            hairpin_dg: None,
            self_dimer_dg: None,
            anneal_tm: None,
            state: PrimerState::Confirmed,
            mismatches: 0,
            off_targets: 0,
            sites: vec![],
        }
    }

    #[test]
    fn primer_draft_seeds_from_attached_info() {
        let d = PrimerDraft::from_info(&primer_info(Some(2..8), Strand::Reverse));
        assert_eq!(d.id, Some(PrimerId(4)));
        assert_eq!(d.sequence, "ATGCGT");
        assert_eq!(d.strand, "-");
        assert!(d.attached);
        assert_eq!((d.start, d.end), (2, 8));
        assert!(d.needs_focus);
    }

    #[test]
    fn primer_draft_seeds_from_floating_info() {
        let d = PrimerDraft::from_info(&primer_info(None, Strand::Forward));
        assert!(!d.attached, "no binding → floating oligo");
    }

    #[test]
    fn primer_edit_commits_to_update_primer_verb() {
        // An attached edit sends both binding ends (Some) + all authored fields.
        let d = PrimerDraft::from_info(&primer_info(Some(2..8), Strand::Forward));
        match d.to_request() {
            ViewerRequest::UpdatePrimer {
                id,
                name,
                sequence,
                strand,
                start,
                end,
                detach,
                view,
            } => {
                assert_eq!(id, PrimerId(4));
                assert_eq!(name.as_deref(), Some("P1"));
                assert_eq!(sequence.as_deref(), Some("ATGCGT"));
                assert_eq!(strand.as_deref(), Some("+"));
                assert_eq!((start, end), (Some(2), Some(8)));
                assert!(!detach, "an attached edit keeps the binding");
                assert_eq!(view, None);
            }
            other => panic!("expected UpdatePrimer, got {other:?}"),
        }
    }

    #[test]
    fn floating_primer_update_detaches() {
        // A floating oligo edit sends no footprint + detach = true → the binding
        // is cleared (③: the editor now attaches/detaches via the detach flag).
        let d = PrimerDraft::from_info(&primer_info(None, Strand::Forward));
        match d.to_request() {
            ViewerRequest::UpdatePrimer {
                start, end, detach, ..
            } => {
                assert_eq!((start, end), (None, None));
                assert!(detach, "floating edit clears the binding");
            }
            other => panic!("expected UpdatePrimer, got {other:?}"),
        }
    }

    #[test]
    fn primer_create_commits_to_add_primer_verb() {
        let d = PrimerDraft::create(
            "Primer 1".into(),
            "ATGC".into(),
            Some(Span::from_range(0..4)),
        );
        match d.to_request() {
            ViewerRequest::AddPrimer {
                name,
                sequence,
                start,
                end,
                strand,
                ..
            } => {
                assert_eq!(name.as_deref(), Some("Primer 1"));
                assert_eq!(sequence, "ATGC");
                assert_eq!((start, end), (Some(0), Some(4)));
                assert_eq!(strand, "+");
            }
            other => panic!("expected AddPrimer, got {other:?}"),
        }
        // A create draft has no delete verb.
        assert!(d.to_delete_request().is_none());
    }

    #[test]
    fn primer_draft_validity() {
        let mut d = PrimerDraft::create("p".into(), "ATGC".into(), Some(Span::from_range(0..4)));
        assert!(d.is_valid());
        d.sequence = "   ".into();
        assert!(!d.is_valid(), "empty oligo is invalid");
        d.sequence = "ATGC".into();
        d.end = d.start; // collapsed footprint while attached
        assert!(!d.is_valid(), "attached needs start < end");
        // A floating oligo ignores the (unused) start/end.
        d.attached = false;
        assert!(d.is_valid());
    }

    #[test]
    fn edit_primer_delete_verb() {
        let d = PrimerDraft::from_info(&primer_info(Some(0..4), Strand::Forward));
        assert!(matches!(
            d.to_delete_request(),
            Some(ViewerRequest::RemovePrimer {
                id: PrimerId(4),
                ..
            })
        ));
    }
}
