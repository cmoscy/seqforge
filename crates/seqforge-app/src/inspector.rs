//! The Inspector pane — a singleton dock pane (right side) that follows the
//! **active view** and surfaces its derived annotations as noun-collections
//! (`plans/primers.md` "Panels / Inspector", ROADMAP decisions 10 + 15).
//!
//! Horizontal sub-tabs (**Primers · Cut sites · Features**) share one generic
//! table renderer ([`render_collection`]) — the `Track` analog. Editing is
//! **inline-in-pane** (decision 15), not a launcher→center-modal: the selected
//! Features row expands to a read-only viewer, and an edit gesture (Edit button /
//! double-click) drops it into an inline field editor backed by a pane-local
//! [`FeatureDraft`]. Commit posts one `UpdateFeature` `ViewerRequest` (= the CLI
//! verb) through the single applier + history; the draft never mutates the
//! buffer. While a field has focus the pane contributes `Pane:Inspector:Editing`,
//! which suppresses single-key user bindings (keymap gate) — otherwise the pane
//! grabs no keys. Read-only nouns (cut-sites) stay non-editable.
//!
//! Like `Tab::FileBrowser`/`Tab::Terminal` it holds **no `ViewId`**. The primer
//! projection is the shared `PrimerInfo` shape (same as the CLI `primers list`),
//! memoized on `buffer.version`; features/cut-sites are cheap per-frame reads.

use std::collections::HashSet;
use std::ops::Range;

use seqforge_core::{
    CutSite, FeatureId, PrimerId, PrimerInfo, PrimerState, Strand, ViewId, ViewerRequest,
};

use crate::command::{AppCommand, PendingCommand};
use crate::overlay::{FEATURE_KINDS, enzyme_rows};
use crate::viewer::PrimerDisplay;
use crate::workspace::Workspace;

/// Which noun-collection the pane is showing. Default tab order (decision 15):
/// Features · Enzymes · Primers.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum InspectorTab {
    #[default]
    Features,
    /// Enzyme query + grouped cut sites (labelled "Enzymes" — 1.5b).
    CutSites,
    Primers,
}

/// Version-keyed `PrimerInfo` projection for the active view.
struct PrimerCache {
    view: ViewId,
    version: u64,
    primers: Vec<PrimerInfo>,
}

/// Owned per-frame projection of a feature (avoids holding an annotations borrow).
#[derive(Clone)]
struct FeatureRow {
    id: FeatureId,
    label: String,
    kind: String,
    range: Range<usize>,
    strand: Strand,
}

/// Live draft for the Inspector's inline feature editor (Phase 1.5a / decision 15).
/// Pane-local UI state: seeded from a row, mutated in place while editing, and
/// committed as one `UpdateFeature` verb (the same request the CLI posts). The
/// buffer never mutates until commit, so this holds no history/undo concern.
struct FeatureDraft {
    id: FeatureId,
    label: String,
    /// GenBank feature-type string (from [`FEATURE_KINDS`]).
    kind: String,
    /// `"+"`, `"-"`, or `"."`.
    strand: String,
    start: usize,
    end: usize,
    /// Grab keyboard focus for the label field on the first edit frame.
    needs_focus: bool,
}

impl FeatureDraft {
    fn from_row(f: &FeatureRow) -> Self {
        Self {
            id: f.id,
            label: f.label.clone(),
            kind: f.kind.clone(),
            strand: strand_flag(f.strand).to_string(),
            start: f.range.start,
            end: f.range.end,
            needs_focus: true,
        }
    }

    /// A half-open range is required (start < end); the applier also re-validates
    /// against the buffer length.
    fn is_valid(&self) -> bool {
        self.start < self.end
    }

    /// Map the draft to the one `UpdateFeature` verb — identical to the CLI
    /// `update-feature` request, so GUI and agent can't drift.
    fn to_request(&self) -> ViewerRequest {
        ViewerRequest::UpdateFeature {
            id: self.id,
            kind: Some(self.kind.clone()),
            label: Some(self.label.clone()),
            strand: Some(self.strand.clone()),
            start: Some(self.start),
            end: Some(self.end),
            view: None,
        }
    }
}

/// Singleton Inspector state. The active sub-tab + memoized/collected rows for
/// the active view; the pane reads whatever view is active, so there is no
/// per-`ViewId` state to orphan.
#[derive(Default)]
pub struct InspectorState {
    tab: InspectorTab,
    /// Expensive (folding) → version-gated.
    primer_cache: Option<PrimerCache>,
    /// Cheap reads, rebuilt each frame.
    features: Vec<FeatureRow>,
    cut_sites: Vec<CutSite>,
    /// The active view's displayed enzyme set (Cut-sites tab manages it — 1.5b).
    active_enzymes: Vec<String>,
    selected_primer: Option<PrimerId>,
    selected_feature: Option<FeatureId>,
    /// Active inline feature edit (Phase 1.5a). Pane-local; commit emits one
    /// `UpdateFeature`. Reconciled each frame against the live feature set.
    editing: Option<FeatureDraft>,
    /// Cut-sites tab: the pane-local enzyme query input (the ⌘E verb, re-homed).
    enzyme_query: String,
    /// Multi-site enzymes whose per-site sub-rows are expanded (pane-local).
    enzyme_expanded: HashSet<String>,
    /// One-shot: grab keyboard focus for the enzyme query next frame (set by ⌘E).
    focus_enzyme_query: bool,
    /// The active view's primer map-overlay display (mirrored for the header
    /// toggles; source of truth stays on the `SequenceView`).
    primer_display: PrimerDisplay,
    has_view: bool,
}

impl InspectorState {
    /// Rebuild the collections for the active view: primers version-gated
    /// (reuses `seqforge_bio::primer_infos` = the `ListPrimers`/CLI projection);
    /// features + cut-sites (cheap) + the panel selection every frame. Called
    /// once before the dock renders.
    pub fn refresh(&mut self, workspace: &mut Workspace) {
        let Some(view_id) = workspace.active_view else {
            self.clear();
            return;
        };
        let snap = workspace.with_active_buffer(|v, buf, ann| {
            let features: Vec<FeatureRow> = ann
                .iter()
                .map(|f| FeatureRow {
                    id: f.id,
                    label: f.label.clone(),
                    kind: f.raw_kind.clone(),
                    range: f.range.clone(),
                    strand: f.strand,
                })
                .collect();
            (
                buf.version,
                v.selected_primer,
                v.selected_feature,
                features,
                v.cut_sites.clone(),
                v.active_enzymes.clone(),
            )
        });
        let (version, sel_p, sel_f, features, cut_sites, active_enzymes) = match snap {
            Ok(t) => t,
            Err(_) => {
                self.clear();
                return;
            }
        };
        self.has_view = true;
        self.features = features;
        self.cut_sites = cut_sites;
        self.active_enzymes = active_enzymes;
        self.selected_primer = sel_p;
        self.selected_feature = sel_f;
        // Drop a stale edit draft if its feature was removed (or an edit/undo
        // elsewhere deleted it) — the draft can only outlive one frame if the
        // target still exists.
        if let Some(d) = &self.editing {
            if !self.features.iter().any(|f| f.id == d.id) {
                self.editing = None;
            }
        }
        self.primer_display = workspace
            .seq_views
            .get(&view_id)
            .map(|sv| sv.primer_display)
            .unwrap_or_default();

        let stale = self
            .primer_cache
            .as_ref()
            .is_none_or(|c| c.view != view_id || c.version != version);
        if stale {
            let projected = workspace.with_active_buffer(|_v, buf, ann| {
                let primers: Vec<&seqforge_core::Primer> = ann.primers().collect();
                seqforge_bio::primer_infos(&buf.text, &primers, buf.is_circular())
            });
            if let Ok(primers) = projected {
                self.primer_cache = Some(PrimerCache {
                    view: view_id,
                    version,
                    primers,
                });
            }
        }
    }

    fn clear(&mut self) {
        self.has_view = false;
        self.primer_cache = None;
        self.features.clear();
        self.cut_sites.clear();
        self.active_enzymes.clear();
        self.selected_primer = None;
        self.selected_feature = None;
        self.editing = None;
    }

    fn primers(&self) -> &[PrimerInfo] {
        self.primer_cache
            .as_ref()
            .map_or(&[], |c| c.primers.as_slice())
    }

    /// Whether the inline feature editor is capturing input this frame (drives
    /// the `Pane:Inspector:Editing` keymap gate).
    pub fn is_editing(&self) -> bool {
        self.editing.is_some()
    }

    /// Re-target ⌘E into the pane (Phase 1.5b): switch to the Cut-sites tab and
    /// request focus on its enzyme query field next frame. Called by
    /// `apply_open_enzymes` after ensuring the pane is docked.
    pub fn reveal_enzyme_query(&mut self) {
        self.tab = InspectorTab::CutSites;
        self.focus_enzyme_query = true;
    }

    /// Render the active sub-tab. Row interactions enqueue commands only
    /// (single-applier contract preserved).
    pub fn show(&mut self, ui: &mut egui::Ui, pending: &mut Vec<PendingCommand>) {
        if !self.has_view {
            ui.add_space(8.0);
            ui.vertical_centered(|ui| ui.weak("No file open."));
            return;
        }

        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.add_space(6.0);
            self.tab_button(ui, InspectorTab::Features, "Features", self.features.len());
            // "Enzymes" (the tab manages the enzyme set; count = active enzymes).
            self.tab_button(
                ui,
                InspectorTab::CutSites,
                "Enzymes",
                self.active_enzymes.len(),
            );
            self.tab_button(ui, InspectorTab::Primers, "Primers", self.primers().len());
        });
        ui.separator();

        // Primers-tab header toggles: show-on-map + arrows-vs-bases (drives the
        // PrimerTrack via `SetPrimerDisplay`; the source of truth is the view).
        if self.tab == InspectorTab::Primers {
            let mut d = self.primer_display;
            let mut changed = false;
            ui.horizontal(|ui| {
                ui.add_space(6.0);
                changed |= ui.checkbox(&mut d.show, "Show on map").changed();
                changed |= ui
                    .add_enabled_ui(d.show, |ui| ui.checkbox(&mut d.bases, "Bases").changed())
                    .inner;
            });
            if changed {
                pending.push((AppCommand::SetPrimerDisplay(d), None));
            }
            ui.separator();
        }

        match self.tab {
            InspectorTab::Primers => render_collection(
                ui,
                &PrimersCollection {
                    primers: self.primers(),
                    selected: self.selected_primer,
                },
                pending,
            ),
            InspectorTab::CutSites => self.show_cutsites(ui, pending),
            InspectorTab::Features => self.show_features(ui, pending),
        }
    }

    /// The Cut-sites tab: a query **header** (the re-homed ⌘E enzyme verb — sets
    /// the active enzyme set) over a grouped enzyme→sites **noun** list with
    /// per-enzyme remove + jump (Phase 1.5b / decision 15). All actions reuse the
    /// existing `SubmitEnzymes`/`AddEnzymes`/`RemoveEnzyme` commands — no new
    /// backend. Cut sites stay read-only (managed via the query, not row edits).
    fn show_cutsites(&mut self, ui: &mut egui::Ui, pending: &mut Vec<PendingCommand>) {
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
                .add_enabled(has_input, egui::Button::new("＋ Add"))
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
                    // Remove control pinned to the right edge — a close-style ✕
                    // (the "remove from view" affordance), not a leading checkbox.
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .small_button("✕")
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

    /// The Features tab: a read-only list that expands the selected row into an
    /// inline **viewer**, and — on an edit gesture — an inline **editor** (Phase
    /// 1.5a). Editing is pane-local until commit, which posts one `UpdateFeature`
    /// (the CLI verb) through the single applier + history. This replaces the
    /// former double-click→center-modal launcher (decision 15).
    fn show_features(&mut self, ui: &mut egui::Ui, pending: &mut Vec<PendingCommand>) {
        if self.features.is_empty() {
            ui.add_space(8.0);
            ui.vertical_centered(|ui| ui.weak("No features on this sequence."));
            return;
        }
        // Local, sorted snapshot so the loop can mutate `self.editing` freely.
        let mut feats = self.features.clone();
        feats.sort_by_key(|f| f.range.start);
        let selected = self.selected_feature;
        let editing = &mut self.editing;

        egui::ScrollArea::vertical().show(ui, |ui| {
            for f in &feats {
                let is_sel = selected == Some(f.id);
                let resp = row_shell(ui, &feature_display_row(f, is_sel));
                if resp.double_clicked() {
                    *editing = Some(FeatureDraft::from_row(f));
                    // Ensure the map selection follows the row being edited.
                    pending.push((AppCommand::RevealFeature { id: f.id }, None));
                } else if resp.clicked() {
                    // Selecting a different row cancels an in-flight edit.
                    if editing.as_ref().is_some_and(|d| d.id != f.id) {
                        *editing = None;
                    }
                    pending.push((AppCommand::RevealFeature { id: f.id }, None));
                }

                if is_sel {
                    let editing_this = editing.as_ref().is_some_and(|d| d.id == f.id);
                    if editing_this {
                        let d = editing.as_mut().expect("editing_this ⇒ Some");
                        if let Some(outcome) = feature_editor(ui, d) {
                            match outcome {
                                EditOutcome::Commit(req) => {
                                    pending.push((AppCommand::Viewer(req), None));
                                    *editing = None;
                                }
                                EditOutcome::Cancel => *editing = None,
                            }
                        }
                    } else if feature_viewer(ui, f) {
                        *editing = Some(FeatureDraft::from_row(f));
                        pending.push((AppCommand::RevealFeature { id: f.id }, None));
                    }
                }
            }
        });
    }

    fn tab_button(&mut self, ui: &mut egui::Ui, tab: InspectorTab, label: &str, count: usize) {
        let text = if count > 0 {
            format!("{label} ({count})")
        } else {
            label.to_string()
        };
        if ui.selectable_label(self.tab == tab, text).clicked() {
            self.tab = tab;
        }
    }
}

// ── The generic collection abstraction (Track analog) ─────────────────────────

/// A noun-collection rendered as a table of clickable rows. Implementations
/// project their model into [`Row`]s; the shared renderer owns the table shell
/// (scroll, clickable/highlighted rows, selection detail, activation dispatch).
trait InspectorCollection {
    fn empty_hint(&self) -> &'static str;
    fn rows(&self) -> Vec<Row>;
}

/// Tone for a row's state dot / name (mapped to a colour at render time).
enum Tone {
    Normal,
    Warn,
    Dim,
}

struct DetailLine {
    text: String,
    mono: bool,
}

/// A rendered row: compact columns + the commands its clicks enqueue.
struct Row {
    selected: bool,
    /// Strand arrow glyph (fwd/rev), if the noun is stranded.
    glyph: Option<&'static str>,
    /// State dot + tone (primers), if any.
    dot: Option<(&'static str, Tone)>,
    name: String,
    /// Render the name subdued (unnamed feature / detached oligo).
    dim_name: bool,
    /// Right-aligned compact cells, in visual left→right order.
    right: Vec<String>,
    /// Single-click → select + reveal.
    on_select: AppCommand,
    /// Double-click → edit modal (`None` = read-only noun).
    on_activate: Option<AppCommand>,
    /// Shown under the row while selected.
    detail: Vec<DetailLine>,
}

fn render_collection(
    ui: &mut egui::Ui,
    coll: &dyn InspectorCollection,
    pending: &mut Vec<PendingCommand>,
) {
    let rows = coll.rows();
    if rows.is_empty() {
        ui.add_space(8.0);
        ui.vertical_centered(|ui| ui.weak(coll.empty_hint()));
        return;
    }
    egui::ScrollArea::vertical().show(ui, |ui| {
        for row in rows {
            render_row(ui, row, pending);
        }
    });
}

/// Draw a row's compact shell (fill + glyph + dot + name + right cells) and
/// return its click-sensed response. Shared by the generic renderer and the
/// editable Features tab so their rows look identical.
fn row_shell(ui: &mut egui::Ui, row: &Row) -> egui::Response {
    let fill = if row.selected {
        ui.visuals().selection.bg_fill
    } else {
        egui::Color32::TRANSPARENT
    };

    egui::Frame::new()
        .fill(fill)
        .inner_margin(egui::Margin::symmetric(6, 2))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.horizontal(|ui| {
                if let Some(g) = row.glyph {
                    ui.weak(g);
                }
                if let Some((d, tone)) = &row.dot {
                    ui.colored_label(tone_color(ui, tone), *d);
                }
                if row.dim_name {
                    ui.weak(&row.name);
                } else {
                    ui.label(&row.name);
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    for cell in row.right.iter().rev() {
                        ui.weak(cell);
                        ui.add_space(8.0);
                    }
                });
            });
        })
        .response
        .interact(egui::Sense::click())
}

fn render_row(ui: &mut egui::Ui, row: Row, pending: &mut Vec<PendingCommand>) {
    let resp = row_shell(ui, &row);

    // Double-click activates (edit modal); a plain click selects/reveals.
    if resp.double_clicked() {
        if let Some(cmd) = row.on_activate {
            pending.push((cmd, None));
        }
    } else if resp.clicked() {
        pending.push((row.on_select, None));
    }

    if row.selected && !row.detail.is_empty() {
        render_detail(ui, &row.detail);
    }
}

/// The indented frame used for a selected row's detail / inline editor.
fn detail_frame() -> egui::Frame {
    egui::Frame::new().inner_margin(egui::Margin {
        left: 22,
        right: 6,
        top: 2,
        bottom: 6,
    })
}

fn render_detail(ui: &mut egui::Ui, lines: &[DetailLine]) {
    detail_frame().show(ui, |ui| {
        for line in lines {
            if line.mono {
                ui.horizontal_wrapped(|ui| ui.monospace(&line.text));
            } else {
                ui.weak(&line.text);
            }
        }
    });
}

// ── Collections ───────────────────────────────────────────────────────────────

struct PrimersCollection<'a> {
    primers: &'a [PrimerInfo],
    selected: Option<PrimerId>,
}

impl InspectorCollection for PrimersCollection<'_> {
    fn empty_hint(&self) -> &'static str {
        "No primers on this sequence."
    }
    fn rows(&self) -> Vec<Row> {
        // Attached-by-position first, then floating oligos (binding = MAX sort key).
        let mut order: Vec<&PrimerInfo> = self.primers.iter().collect();
        order.sort_by_key(|p| p.binding.as_ref().map_or(usize::MAX, |b| b.start));
        order
            .iter()
            .map(|p| {
                let (dot, tone) = match p.state {
                    PrimerState::Confirmed => ("●", Tone::Normal),
                    PrimerState::Drifted => ("◐", Tone::Warn),
                    PrimerState::Detached => ("○", Tone::Dim),
                };
                let right = vec![
                    binding_label(p),
                    p.tm.map_or_else(|| "— °C".into(), |t| format!("{t:.1} °C")),
                    if p.len > 0 {
                        format!("{:.0} %", p.gc)
                    } else {
                        "— %".into()
                    },
                ];

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

                Row {
                    selected: self.selected == Some(p.id),
                    glyph: Some(strand_glyph(p.strand)),
                    dot: Some((dot, tone)),
                    name: p.name.clone(),
                    dim_name: matches!(p.state, PrimerState::Detached),
                    right,
                    on_select: AppCommand::RevealPrimer { id: p.id },
                    on_activate: None, // primer edit modal = Phase 2.1
                    detail,
                }
            })
            .collect()
    }
}

/// Compact display row for a feature (the Features tab renders these directly so
/// it can layer the inline viewer/editor under the selected one — decision 15).
fn feature_display_row(f: &FeatureRow, selected: bool) -> Row {
    let unnamed = f.label.is_empty();
    Row {
        selected,
        glyph: Some(strand_glyph(f.strand)),
        dot: None,
        name: if unnamed {
            "(unnamed)".to_string()
        } else {
            f.label.clone()
        },
        dim_name: unnamed,
        right: vec![
            format!("{}–{}", f.range.start + 1, f.range.end),
            f.kind.clone(),
        ],
        // Unused by the Features tab (it drives select/edit off the raw row), but
        // kept coherent for any shared-renderer path.
        on_select: AppCommand::RevealFeature { id: f.id },
        on_activate: None,
        detail: vec![],
    }
}

/// Outcome of one frame of the inline feature editor.
enum EditOutcome {
    Commit(ViewerRequest),
    Cancel,
}

/// Read-only detail shown under a selected (non-editing) feature row, plus the
/// gesture into edit mode. Returns `true` when the user asks to edit (Edit button
/// — double-clicking the row is the other entry point). Kept keyless.
fn feature_viewer(ui: &mut egui::Ui, f: &FeatureRow) -> bool {
    let mut edit = false;
    detail_frame().show(ui, |ui| {
        ui.weak(format!(
            "{} · {}–{}",
            f.kind,
            f.range.start + 1,
            f.range.end
        ));
        ui.horizontal(|ui| {
            if ui.small_button("Edit").clicked() {
                edit = true;
            }
            ui.weak("or double-click");
        });
    });
    edit
}

/// The inline field editor for the selected feature (decision 15). Mutates the
/// pane-local draft; returns `Some(Commit)` on Save/Enter (draft valid),
/// `Some(Cancel)` on Cancel/Escape, else `None` (still editing). Enter/Escape are
/// handled at the widget level — the keymap has no plain-key bindings, and the
/// `Pane:Inspector:Editing` tag suppresses single-key user bindings while typing.
fn feature_editor(ui: &mut egui::Ui, d: &mut FeatureDraft) -> Option<EditOutcome> {
    let mut outcome = None;
    let mut submit_on_enter = false;
    detail_frame().show(ui, |ui| {
        egui::Grid::new(("feature_inline_editor", d.id.0))
            .num_columns(2)
            .spacing([10.0, 5.0])
            .show(ui, |ui| {
                ui.label("Label");
                let r = ui.text_edit_singleline(&mut d.label);
                if d.needs_focus {
                    r.request_focus();
                    d.needs_focus = false;
                }
                if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    submit_on_enter = true;
                }
                ui.end_row();

                ui.label("Kind");
                egui::ComboBox::from_id_salt(("feature_inline_kind", d.id.0))
                    .selected_text(&d.kind)
                    .show_ui(ui, |ui| {
                        for k in FEATURE_KINDS {
                            ui.selectable_value(&mut d.kind, (*k).to_string(), *k);
                        }
                    });
                ui.end_row();

                ui.label("Strand");
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut d.strand, "+".into(), "+ fwd");
                    ui.selectable_value(&mut d.strand, "-".into(), "− rev");
                    ui.selectable_value(&mut d.strand, ".".into(), ". none");
                });
                ui.end_row();

                ui.label("Start");
                ui.add(egui::DragValue::new(&mut d.start).range(0..=usize::MAX));
                ui.end_row();

                ui.label("End");
                ui.add(egui::DragValue::new(&mut d.end).range(d.start + 1..=usize::MAX));
                ui.end_row();
            });
        ui.add_space(4.0);
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
        });
    });
    // Escape cancels (no overlay is active, so the keymap leaves Escape for us).
    if outcome.is_none() && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        outcome = Some(EditOutcome::Cancel);
    }
    outcome
}

// ── Display helpers ───────────────────────────────────────────────────────────

fn strand_glyph(strand: Strand) -> &'static str {
    match strand {
        Strand::Forward => "→",
        Strand::Reverse => "←",
        Strand::Both => "↔",
        Strand::None => "·",
    }
}

fn strand_flag(strand: Strand) -> &'static str {
    match strand {
        Strand::Forward => "+",
        Strand::Reverse => "-",
        _ => ".",
    }
}

fn tone_color(ui: &egui::Ui, tone: &Tone) -> egui::Color32 {
    match tone {
        Tone::Normal => ui.visuals().text_color(),
        Tone::Warn => egui::Color32::from_rgb(0xE0, 0xA0, 0x30),
        Tone::Dim => ui.visuals().weak_text_color(),
    }
}

/// 1-based inclusive display range, or `Unattached` for a floating oligo.
fn binding_label(p: &PrimerInfo) -> String {
    match &p.binding {
        Some(b) => format!("{}–{}", b.start + 1, b.end),
        None => "Unattached".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row() -> FeatureRow {
        FeatureRow {
            id: FeatureId(7),
            label: "lacZ".into(),
            kind: "CDS".into(),
            range: 10..40,
            strand: Strand::Reverse,
        }
    }

    #[test]
    fn draft_seeds_from_row() {
        let d = FeatureDraft::from_row(&row());
        assert_eq!(d.id, FeatureId(7));
        assert_eq!(d.label, "lacZ");
        assert_eq!(d.kind, "CDS");
        assert_eq!(d.strand, "-"); // Reverse → "-"
        assert_eq!((d.start, d.end), (10, 40));
        assert!(d.needs_focus);
    }

    #[test]
    fn commit_maps_to_the_update_feature_verb() {
        // The inline editor must post exactly the CLI `update-feature` request,
        // with every field populated (all-Some), so GUI and agent can't drift.
        let d = FeatureDraft::from_row(&row());
        match d.to_request() {
            ViewerRequest::UpdateFeature {
                id,
                kind,
                label,
                strand,
                start,
                end,
                view,
            } => {
                assert_eq!(id, FeatureId(7));
                assert_eq!(kind.as_deref(), Some("CDS"));
                assert_eq!(label.as_deref(), Some("lacZ"));
                assert_eq!(strand.as_deref(), Some("-"));
                assert_eq!(start, Some(10));
                assert_eq!(end, Some(40));
                assert_eq!(view, None); // always the active view
            }
            other => panic!("expected UpdateFeature, got {other:?}"),
        }
    }

    #[test]
    fn draft_validity_requires_a_nonempty_range() {
        let mut d = FeatureDraft::from_row(&row());
        assert!(d.is_valid());
        d.end = d.start; // collapsed
        assert!(!d.is_valid(), "start == end must be rejected before commit");
        d.end = d.start.saturating_sub(1);
        assert!(!d.is_valid(), "end < start must be rejected");
    }

    #[test]
    fn reveal_enzyme_query_switches_to_cutsites_and_requests_focus() {
        // ⌘E (apply_open_enzymes) calls this after docking the pane — it must
        // land on the Cut-sites tab and arm the one-shot query focus.
        let mut st = InspectorState::default();
        st.tab = InspectorTab::Primers;
        st.reveal_enzyme_query();
        assert_eq!(st.tab, InspectorTab::CutSites);
        assert!(
            st.focus_enzyme_query,
            "query field should be focused next frame"
        );
    }
}
