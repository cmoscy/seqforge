//! The Inspector pane — a singleton dock pane (right side) that follows the
//! **active view** and surfaces its derived annotations as noun-collections
//! (`plans/primers.md` "Panels / Inspector", ROADMAP decisions 10 + 15).
//!
//! Horizontal sub-tabs (**Primers · Cut sites · Features**) each render a
//! noun-collection table. Editing is
//! **inline-in-pane** (decision 15), not a launcher→center-modal: the selected
//! Features/Primers row expands to a read-only viewer, and an edit gesture (Edit
//! button / double-click) drops it into an inline field editor backed by a
//! pane-local [`FeatureDraft`] / [`PrimerDraft`]. Commit posts one
//! `UpdateFeature` / `UpdatePrimer` (or `AddPrimer`) `ViewerRequest` (= the CLI
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
    CutSite, CutSiteKey, FeatureId, MethylContext, MethylState, PrimerId, PrimerInfo, ViewId,
};

use crate::command::{AppCommand, PendingCommand};
use crate::viewer::PrimerDisplay;
use crate::workspace::Workspace;

mod cutsite;
mod feature;
mod primer;
mod row;
use feature::*;
use primer::*;

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

/// The active view's selected *object*, projected from `ViewSelection` (decision
/// 17). One field replaces the former three parallel `selected_*` fields — they
/// were mutually-exclusive projections of the one selection. Each tab derives its
/// row highlight from this; drives follow-selection (`inspector_tab`).
#[derive(Debug, Clone, PartialEq)]
pub(super) enum SelectedNoun {
    Feature(FeatureId),
    Primer(PrimerId),
    CutSite(CutSiteKey),
}

impl SelectedNoun {
    /// The tab that owns this noun — for follow-selection tab switching.
    fn inspector_tab(&self) -> InspectorTab {
        match self {
            SelectedNoun::Feature(_) => InspectorTab::Features,
            SelectedNoun::Primer(_) => InspectorTab::Primers,
            SelectedNoun::CutSite(_) => InspectorTab::CutSites,
        }
    }
}

/// Version-keyed `PrimerInfo` projection for the active view.
struct PrimerCache {
    view: ViewId,
    version: u64,
    primers: Vec<PrimerInfo>,
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
    /// Methylation verdict per site, parallel to `cut_sites` (cached on the
    /// `View`; the tab just reads it — no per-frame evaluation).
    methyl_states: Vec<MethylState>,
    /// The active view's displayed enzyme set (Cut-sites tab manages it — 1.5b).
    active_enzymes: Vec<String>,
    /// Host methylation toggles (Dam/Dcm/CpG) shown as the tab's checkboxes.
    methylation: MethylContext,
    /// The active view's selected object (feature / primer / cut site), synced
    /// from `ViewSelection` each frame. One field (decision 17); each tab derives
    /// its highlight, and it drives follow-selection tab switching.
    selected: Option<SelectedNoun>,
    /// Active inline feature edit (Phase 1.5a). Pane-local; commit emits one
    /// `UpdateFeature`. Reconciled each frame against the live feature set.
    editing: Option<FeatureDraft>,
    /// Active inline primer edit/create (Phase 2.1). Pane-local; commit emits one
    /// `UpdatePrimer` (edit) or `AddPrimer` (create). Edit drafts are reconciled
    /// each frame against the live primer set; a create draft (`id = None`) has no
    /// target to outlive.
    editing_primer: Option<PrimerDraft>,
    /// The active view's current range selection (0-based half-open) + its
    /// template slice, captured each frame — seeds create-from-selection for a new
    /// primer. `None` when there's no range selection (a bare cursor / nothing).
    selection_seed: Option<(Range<usize>, String)>,
    /// A unique default primer name for the next create (decision 9), captured
    /// each frame from `Annotations::suggest_primer_name()`.
    suggested_primer_name: String,
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
    pub fn refresh(&mut self, workspace: &mut Workspace, follow_selection: bool) {
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
            // Create-from-selection seed: a range selection + its template slice.
            let selection_seed = v
                .selection
                .text_range()
                .filter(|s| !s.is_cursor())
                .map(|s| s.ordered())
                .and_then(|(a, b)| {
                    buf.text.get(a..b).map(|slice| {
                        let oligo: String = slice
                            .iter()
                            .map(|&c| c.to_ascii_uppercase() as char)
                            .collect();
                        (a..b, oligo)
                    })
                });
            // Project the one selection to a single object (mutually exclusive).
            let selected = v
                .selection
                .selected_feature()
                .map(SelectedNoun::Feature)
                .or_else(|| v.selection.selected_primer().map(SelectedNoun::Primer))
                .or_else(|| {
                    v.selection
                        .selected_cut_site()
                        .cloned()
                        .map(SelectedNoun::CutSite)
                });
            (
                buf.version,
                selected,
                features,
                v.cut_sites.clone(),
                v.methyl_states.clone(),
                v.active_enzymes.clone(),
                v.methylation,
                selection_seed,
                ann.suggest_primer_name(),
            )
        });
        let (
            version,
            selected,
            features,
            cut_sites,
            methyl_states,
            active_enzymes,
            methylation,
            selection_seed,
            suggested_primer_name,
        ) = match snap {
            Ok(t) => t,
            Err(_) => {
                self.clear();
                return;
            }
        };
        self.selection_seed = selection_seed;
        self.suggested_primer_name = suggested_primer_name;
        self.has_view = true;
        self.features = features;
        self.cut_sites = cut_sites;
        self.methyl_states = methyl_states;
        self.active_enzymes = active_enzymes;
        self.methylation = methylation;
        self.apply_follow_selection(&selected, follow_selection);
        self.selected = selected;
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

        // Drop a stale primer *edit* draft if its primer was removed (undo / a
        // delete elsewhere). A *create* draft (`id = None`) has no target to
        // outlive, so it survives until committed or cancelled.
        if let Some(id) = self.editing_primer.as_ref().and_then(|d| d.id) {
            if !self.primers().iter().any(|p| p.id == id) {
                self.editing_primer = None;
            }
        }

        // Live anneal Tm for an attached draft (①): needs the template, which only
        // `refresh` holds (zero-copy). O(footprint) NN — cheap like the selection
        // Tm readout, so recomputed each frame the editor is open (no memo).
        if let Some(d) = &mut self.editing_primer {
            d.anneal_tm = draft_anneal_tm(d, workspace);
        }
    }

    fn clear(&mut self) {
        self.has_view = false;
        self.primer_cache = None;
        self.features.clear();
        self.cut_sites.clear();
        self.methyl_states.clear();
        self.active_enzymes.clear();
        self.methylation = MethylContext::default();
        self.selected = None;
        self.editing = None;
        self.editing_primer = None;
        self.selection_seed = None;
    }

    fn primers(&self) -> &[PrimerInfo] {
        self.primer_cache
            .as_ref()
            .map_or(&[], |c| c.primers.as_slice())
    }

    /// Whether an inline editor (feature *or* primer) is capturing input this
    /// frame (drives the `Pane:Inspector:Editing` keymap gate).
    pub fn is_editing(&self) -> bool {
        self.editing.is_some() || self.editing_primer.is_some()
    }

    /// Re-target ⌘E into the pane (Phase 1.5b): switch to the Cut-sites tab and
    /// request focus on its enzyme query field next frame. Called by
    /// `apply_open_enzymes` after ensuring the pane is docked.
    pub fn reveal_enzyme_query(&mut self) {
        self.tab = InspectorTab::CutSites;
        self.focus_enzyme_query = true;
    }

    /// Enter the inline feature editor for `id`, pre-filled — the entry point for
    /// canvas edit/delete gestures routing into the pane (decision 15,
    /// tab-exclusive editing). `arm_delete` opens with the two-step delete
    /// pre-armed (from a Delete gesture / context-menu Delete). Called by
    /// `apply_edit_feature_in_inspector` after docking the pane.
    #[allow(clippy::too_many_arguments)]
    pub fn begin_feature_edit(
        &mut self,
        id: FeatureId,
        label: String,
        kind: String,
        strand: String,
        start: usize,
        end: usize,
        arm_delete: bool,
    ) {
        self.tab = InspectorTab::Features;
        self.editing = Some(FeatureDraft {
            id,
            label,
            kind,
            strand,
            start,
            end,
            needs_focus: true,
            confirm_delete: arm_delete,
        });
    }

    /// Enter the inline **primer** editor for `id`, pre-filled from the primer's
    /// authored fields — the entry point for canvas primer edit/delete gestures
    /// routing into the pane (Phase 2.1, mirroring [`Self::begin_feature_edit`]).
    /// `arm_delete` opens with the two-step delete pre-armed. Seeded from the
    /// authored `Primer` (not the derived projection) so it works even before the
    /// pane's cache is warm.
    pub fn begin_primer_edit(
        &mut self,
        id: PrimerId,
        name: String,
        sequence: String,
        strand: String,
        binding: Option<Range<usize>>,
        arm_delete: bool,
    ) {
        self.tab = InspectorTab::Primers;
        let (attached, start, end) = match binding {
            Some(b) => (true, b.start, b.end),
            None => (false, 0, 0),
        };
        self.editing_primer = Some(PrimerDraft {
            id: Some(id),
            name,
            sequence,
            strand,
            attached,
            start,
            end,
            needs_focus: true,
            confirm_delete: arm_delete,
            qc_cache: None,
            anneal_tm: None,
            insert: Default::default(),
        });
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

        // Primers-tab header: show-on-map + arrows-vs-bases toggles (drive the
        // PrimerTrack via `SetPrimerDisplay`; the source of truth is the view) +
        // the create-a-primer affordance (inline editor, decision 15).
        if self.tab == InspectorTab::Primers {
            let mut d = self.primer_display;
            let mut changed = false;
            ui.horizontal(|ui| {
                ui.add_space(6.0);
                changed |= ui.checkbox(&mut d.show, "Show on map").changed();
                changed |= ui
                    .add_enabled_ui(d.show, |ui| ui.checkbox(&mut d.bases, "Bases").changed())
                    .inner;
                // ＋ Add primer — inline create (create-from-selection when a range
                // is selected, else a floating oligo). Opens the same inline editor
                // that edits, so the form is inline from day one, never a modal.
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let from_sel = self.selection_seed.is_some();
                    let hint = if from_sel {
                        "New primer from the selected range"
                    } else {
                        "New floating oligo (no binding)"
                    };
                    if ui
                        .button(format!("{} Add primer", egui_phosphor::regular::PLUS))
                        .on_hover_text(hint)
                        .clicked()
                    {
                        self.begin_primer_create(pending);
                    }
                });
            });
            if changed {
                pending.push((AppCommand::SetPrimerDisplay(d), None));
            }
            ui.separator();
        }

        match self.tab {
            InspectorTab::Primers => self.show_primers(ui, pending),
            InspectorTab::CutSites => self.show_cutsites(ui, pending),
            InspectorTab::Features => self.show_features(ui, pending),
        }
    }

    /// Follow-selection: switch the active tab when the selected *object* changes
    /// (not every frame), so a manual tab switch sticks until the next selection.
    /// `follow == false` → highlight-only (the tab never changes here). Compares
    /// `new` against the still-current `self.selected` (call *before* overwriting).
    fn apply_follow_selection(&mut self, new: &Option<SelectedNoun>, follow: bool) {
        if follow && new.is_some() && *new != self.selected {
            if let Some(n) = new {
                self.tab = n.inspector_tab();
            }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reveal_enzyme_query_switches_to_cutsites_and_requests_focus() {
        // ⌘E (apply_open_enzymes) calls this after docking the pane — it must
        // land on the Cut-sites tab and arm the one-shot query focus.
        let mut st = InspectorState {
            tab: InspectorTab::Primers,
            ..Default::default()
        };
        st.reveal_enzyme_query();
        assert_eq!(st.tab, InspectorTab::CutSites);
        assert!(
            st.focus_enzyme_query,
            "query field should be focused next frame"
        );
    }

    #[test]
    fn follow_selection_switches_tab_on_object_change() {
        let mut st = InspectorState {
            tab: InspectorTab::Features,
            ..Default::default()
        };
        // Selecting a primer follows to the Primers tab.
        st.apply_follow_selection(&Some(SelectedNoun::Primer(PrimerId(1))), true);
        assert_eq!(st.tab, InspectorTab::Primers);
        st.selected = Some(SelectedNoun::Primer(PrimerId(1)));

        // A cut-site selection follows to Cut-sites.
        let key = seqforge_core::CutSiteKey {
            enzyme: "EcoRI".into(),
            recognition_start: 4,
        };
        st.apply_follow_selection(&Some(SelectedNoun::CutSite(key.clone())), true);
        assert_eq!(st.tab, InspectorTab::CutSites);
        st.selected = Some(SelectedNoun::CutSite(key));
    }

    #[test]
    fn follow_selection_does_not_retrap_manual_tab_switch() {
        // Same object as last frame → no switch, so a manual tab change sticks.
        let mut st = InspectorState {
            tab: InspectorTab::Features, // user manually parked here
            selected: Some(SelectedNoun::Primer(PrimerId(1))),
            ..Default::default()
        };
        st.apply_follow_selection(&Some(SelectedNoun::Primer(PrimerId(1))), true);
        assert_eq!(
            st.tab,
            InspectorTab::Features,
            "unchanged object must not re-yank"
        );
    }

    #[test]
    fn follow_selection_off_is_highlight_only() {
        let mut st = InspectorState {
            tab: InspectorTab::Features,
            ..Default::default()
        };
        st.apply_follow_selection(&Some(SelectedNoun::Primer(PrimerId(1))), false);
        assert_eq!(st.tab, InspectorTab::Features, "follow off never switches");
    }
}
