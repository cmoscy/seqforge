//! The Inspector pane — a singleton dock pane (right side) that follows the
//! **active view** and surfaces its derived annotations as noun-collections
//! (`plans/primers.md` "Panels / Inspector", ROADMAP decision 10).
//!
//! Horizontal sub-tabs (**Primers · Cut sites · Features**) share one generic
//! table via the [`InspectorCollection`] trait — the `Track` analog: templatize
//! **display + selection + activation dispatch**, *not* the edit forms. Editing
//! is a **launcher → modal** affair: a row's double-click opens the noun's
//! existing modal (`OpenFeatureForm` today; `OpenPrimerForm` at Phase 2.1),
//! whose Submit is one `ViewerRequest` = the CLI verb. The pane holds no draft
//! state and (beyond the click gesture) grabs no keys. Read-only nouns
//! (cut-sites) have no modal — `on_activate` is `None`.
//!
//! Like `Tab::FileBrowser`/`Tab::Terminal` it holds **no `ViewId`**. The primer
//! projection is the shared `PrimerInfo` shape (same as the CLI `primers list`),
//! memoized on `buffer.version`; features/cut-sites are cheap per-frame reads.

use std::ops::Range;

use seqforge_core::{CutSite, FeatureId, PrimerId, PrimerInfo, PrimerState, Strand, ViewId};

use crate::command::{AppCommand, PendingCommand};
use crate::workspace::Workspace;

/// Which noun-collection the pane is showing.
#[derive(Default, Clone, Copy, PartialEq, Eq)]
enum InspectorTab {
    #[default]
    Primers,
    CutSites,
    Features,
}

/// Version-keyed `PrimerInfo` projection for the active view.
struct PrimerCache {
    view: ViewId,
    version: u64,
    primers: Vec<PrimerInfo>,
}

/// Owned per-frame projection of a feature (avoids holding an annotations borrow).
struct FeatureRow {
    id: FeatureId,
    label: String,
    kind: String,
    range: Range<usize>,
    strand: Strand,
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
    selected_primer: Option<PrimerId>,
    selected_feature: Option<FeatureId>,
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
            )
        });
        let (version, sel_p, sel_f, features, cut_sites) = match snap {
            Ok(t) => t,
            Err(_) => {
                self.clear();
                return;
            }
        };
        self.has_view = true;
        self.features = features;
        self.cut_sites = cut_sites;
        self.selected_primer = sel_p;
        self.selected_feature = sel_f;

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
        self.selected_primer = None;
        self.selected_feature = None;
    }

    fn primers(&self) -> &[PrimerInfo] {
        self.primer_cache.as_ref().map_or(&[], |c| c.primers.as_slice())
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
            self.tab_button(ui, InspectorTab::Primers, "Primers", self.primers().len());
            self.tab_button(ui, InspectorTab::CutSites, "Cut sites", self.cut_sites.len());
            self.tab_button(ui, InspectorTab::Features, "Features", self.features.len());
        });
        ui.separator();

        match self.tab {
            InspectorTab::Primers => render_collection(
                ui,
                &PrimersCollection {
                    primers: self.primers(),
                    selected: self.selected_primer,
                },
                pending,
            ),
            InspectorTab::CutSites => {
                render_collection(ui, &CutSitesCollection { sites: &self.cut_sites }, pending)
            }
            InspectorTab::Features => render_collection(
                ui,
                &FeaturesCollection {
                    features: &self.features,
                    selected: self.selected_feature,
                },
                pending,
            ),
        }
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

fn render_row(ui: &mut egui::Ui, row: Row, pending: &mut Vec<PendingCommand>) {
    let fill = if row.selected {
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
        .interact(egui::Sense::click());

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

fn render_detail(ui: &mut egui::Ui, lines: &[DetailLine]) {
    egui::Frame::new()
        .inner_margin(egui::Margin {
            left: 22,
            right: 6,
            top: 2,
            bottom: 6,
        })
        .show(ui, |ui| {
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
                detail.push(DetailLine { text: meta, mono: false });
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

struct FeaturesCollection<'a> {
    features: &'a [FeatureRow],
    selected: Option<FeatureId>,
}

impl InspectorCollection for FeaturesCollection<'_> {
    fn empty_hint(&self) -> &'static str {
        "No features on this sequence."
    }
    fn rows(&self) -> Vec<Row> {
        let mut order: Vec<&FeatureRow> = self.features.iter().collect();
        order.sort_by_key(|f| f.range.start);
        order
            .iter()
            .map(|f| {
                let unnamed = f.label.is_empty();
                Row {
                    selected: self.selected == Some(f.id),
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
                    on_select: AppCommand::RevealFeature { id: f.id },
                    // The payoff: double-click opens the *existing* feature modal.
                    on_activate: Some(AppCommand::OpenFeatureForm {
                        id: Some(f.id),
                        label: f.label.clone(),
                        kind: f.kind.clone(),
                        strand: strand_flag(f.strand).to_string(),
                        start: f.range.start,
                        end: f.range.end,
                    }),
                    detail: vec![DetailLine {
                        text: format!("{} · {}–{}", f.kind, f.range.start + 1, f.range.end),
                        mono: false,
                    }],
                }
            })
            .collect()
    }
}

struct CutSitesCollection<'a> {
    sites: &'a [CutSite],
}

impl InspectorCollection for CutSitesCollection<'_> {
    fn empty_hint(&self) -> &'static str {
        "No cut sites — set enzymes with the ⌘E bar."
    }
    fn rows(&self) -> Vec<Row> {
        let mut order: Vec<&CutSite> = self.sites.iter().collect();
        order.sort_by_key(|s| s.recognition_start);
        order
            .iter()
            .map(|s| Row {
                // Cut sites are derived, with no persistent selection id.
                selected: false,
                glyph: None,
                dot: None,
                name: s.enzyme.clone(),
                dim_name: false,
                right: vec![format!("cut @ {}", s.cut_pos + 1), s.recognition.clone()],
                on_select: AppCommand::RevealRange {
                    start: s.recognition_start,
                    end: s.recognition_end,
                },
                on_activate: None, // read-only
                detail: vec![],
            })
            .collect()
    }
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
