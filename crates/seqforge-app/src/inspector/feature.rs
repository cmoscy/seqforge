//! The Features tab — display row, read-only viewer, and inline field editor
//! (decision 15), plus the `InspectorState::show_features` render loop. Fields on
//! the row/draft are `pub(super)` because `mod.rs` (`refresh`,
//! `begin_feature_edit`) constructs them by struct literal; the methods and free
//! functions stay private to this module.

use seqforge_core::{FeatureId, FeatureKind, Span, Strand, Target, ViewerRequest};

use super::InspectorState;
use super::row::{
    EditOutcome, Row, Tone, detail_frame, row_shell, strand_flag, strand_glyph, visibility_button,
};
use crate::command::{AppCommand, PendingCommand};
use crate::config::Theme;
use crate::config::theme::{FEATURE_PRESETS, color_to_hex};
use crate::overlay::FEATURE_KINDS;

/// Owned per-frame projection of a feature (avoids holding an annotations borrow).
#[derive(Clone)]
pub(super) struct FeatureRow {
    pub(super) id: FeatureId,
    pub(super) label: String,
    pub(super) kind: String,
    /// Footprint as a wrap-aware [`Span`] (exact for a single-region feature,
    /// bounding span for a spliced `Join` — see `Feature::selection_span`).
    pub(super) span: Span,
    pub(super) strand: Strand,
    /// What the canvas paints: file colour (when honoured) → type → palette.
    pub(super) color: egui::Color32,
    /// The type/theme colour alone — what "Use type colour" falls back to.
    pub(super) type_color: egui::Color32,
    /// The feature's own colour, if it carries one (`/ApEinfo_*color`).
    pub(super) stored_color: Option<egui::Color32>,
}

/// Live draft for the Inspector's inline feature editor (Phase 1.5a / decision 15).
/// Pane-local UI state: seeded from a row, mutated in place while editing, and
/// committed as one `UpdateFeature` verb (the same request the CLI posts). The
/// buffer never mutates until commit, so this holds no history/undo concern.
pub(super) struct FeatureDraft {
    pub(super) id: FeatureId,
    pub(super) label: String,
    /// GenBank feature-type string (from [`FEATURE_KINDS`]).
    pub(super) kind: String,
    /// `"+"`, `"-"`, or `"."`.
    pub(super) strand: String,
    pub(super) start: usize,
    pub(super) end: usize,
    /// Own colour (`None` = follow the type colour). Only sent on commit when
    /// it differs from `original_color`, so an untouched named colour
    /// (`"green"`) is never rewritten as hex.
    pub(super) color: Option<egui::Color32>,
    pub(super) original_color: Option<egui::Color32>,
    pub(super) type_color: egui::Color32,
    /// Grab keyboard focus for the label field on the first edit frame.
    pub(super) needs_focus: bool,
    /// Delete is armed (two-step confirm): the Delete button became
    /// "Confirm delete?" and a second click commits the removal.
    pub(super) confirm_delete: bool,
}

impl FeatureDraft {
    fn from_row(f: &FeatureRow) -> Self {
        Self {
            id: f.id,
            label: f.label.clone(),
            kind: f.kind.clone(),
            strand: strand_flag(f.strand).to_string(),
            // The inline editor is a linear start/end form; a wrapping feature
            // shows its span's raw extent and, on commit, redefines to a crisp
            // linear region (the same path P5a's `UpdateFeature` gate allows when
            // both endpoints are given).
            start: f.span.start,
            end: f.span.start + f.span.len,
            color: f.stored_color,
            original_color: f.stored_color,
            type_color: f.type_color,
            needs_focus: true,
            confirm_delete: false,
        }
    }

    /// The `RemoveFeature` verb — identical to the CLI `remove-feature` request.
    fn to_delete_request(&self) -> ViewerRequest {
        ViewerRequest::RemoveFeature {
            id: self.id,
            target: Target::active(),
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
            // `""` clears; `None` leaves the stored colour alone.
            color: (self.color != self.original_color)
                .then(|| self.color.map(color_to_hex).unwrap_or_default()),
            target: Target::active(),
        }
    }
}

/// Compact display row for a feature (the Features tab renders these directly so
/// it can layer the inline viewer/editor under the selected one — decision 15).
fn feature_display_row(f: &FeatureRow, selected: bool) -> Row {
    let unnamed = f.label.is_empty();
    Row {
        selected,
        glyph: Some(strand_glyph(f.strand)),
        dot: Some(Tone::Swatch(f.color)),
        name: if unnamed {
            "(unnamed)".to_string()
        } else {
            f.label.clone()
        },
        dim_name: unnamed,
        right: vec![
            format!("{}–{}", f.span.start + 1, f.span.start + f.span.len),
            f.kind.clone(),
        ],
    }
}

/// Read-only detail shown under a selected (non-editing) feature row, plus the
/// gesture into edit mode. Returns `true` when the user asks to edit (Edit button
/// — double-clicking the row is the other entry point). Kept keyless.
fn feature_viewer(ui: &mut egui::Ui, f: &FeatureRow, _theme: &Theme) -> bool {
    let mut edit = false;
    detail_frame().show(ui, |ui| {
        ui.weak(format!(
            "{} · {}–{}",
            f.kind,
            f.span.start + 1,
            f.span.start + f.span.len
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
/// `Some(Cancel)` on Cancel/Escape, `Some(Delete)` when an armed delete is
/// confirmed, else `None` (still editing). **Enter always commits the current
/// primary action** — Save when editing, the delete when armed ("Confirm
/// delete?") — mirroring the canvas staging grammar (arm → Enter → commit).
/// Handled at the widget level: the keymap has no plain-key bindings, and the
/// `Pane:Inspector:Editing` tag suppresses single-key user bindings while typing.
fn feature_editor(ui: &mut egui::Ui, d: &mut FeatureDraft, theme: &Theme) -> Option<EditOutcome> {
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

                ui.label("Color");
                color_field(ui, d);
                ui.end_row();
            });
        ui.add_space(4.0);
        if d.confirm_delete {
            // The editor is one *staged* operation (decision 10 grammar): **Enter
            // commits the pending op, Esc/Cancel cancels the editor**. Arming just
            // re-stages the pending op from update → delete, so here Enter means
            // confirm-delete (not Save). A global Enter read covers the case where
            // focus isn't on the label field.
            let enter = submit_on_enter || ui.input(|i| i.key_pressed(egui::Key::Enter));
            ui.horizontal(|ui| {
                let btn = egui::Button::new(
                    egui::RichText::new(format!(
                        "{} Confirm delete?  (Enter)",
                        egui_phosphor::regular::TRASH
                    ))
                    .color(egui::Color32::WHITE),
                )
                .fill(theme.ui.danger.0);
                if ui.add(btn).clicked() || enter {
                    outcome = Some(EditOutcome::Delete(d.to_delete_request()));
                }
                // Cancel means the same thing in both states: cancel the editor.
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
                // Delete pinned right; arms the two-step confirm (modal-free).
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .button(egui::RichText::new(format!(
                            "{} Delete",
                            egui_phosphor::regular::TRASH
                        )))
                        .on_hover_text("Delete this feature")
                        .clicked()
                    {
                        d.confirm_delete = true;
                    }
                });
            });
        }
    });
    // Escape cancels the editor (armed or not — closing an armed delete never
    // deletes; use "Keep" to disarm without closing).
    if outcome.is_none() && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        outcome = Some(EditOutcome::Cancel);
    }
    outcome
}

/// The editor's colour row: the current colour as a picker button (editing
/// the type colour adopts it as the feature's own), the preset swatches, and
/// a reset back to the type colour.
fn color_field(ui: &mut egui::Ui, d: &mut FeatureDraft) {
    ui.vertical(|ui| {
        ui.horizontal(|ui| {
            let mut c = d.color.unwrap_or(d.type_color);
            if egui::color_picker::color_edit_button_srgba(
                ui,
                &mut c,
                egui::color_picker::Alpha::Opaque,
            )
            .changed()
            {
                d.color = Some(c);
            }
            if d.color.is_some() {
                if ui.small_button("Use type colour").clicked() {
                    d.color = None;
                }
            } else {
                ui.weak("type colour");
            }
        });
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(3.0, 3.0);
            for (name, c) in FEATURE_PRESETS {
                let swatch = egui::Button::new("")
                    .fill(*c)
                    .min_size(egui::vec2(14.0, 14.0));
                if ui.add(swatch).on_hover_text(*name).clicked() {
                    d.color = Some(*c);
                }
            }
        });
    });
}

impl InspectorState {
    pub(super) fn show_features(
        &mut self,
        ui: &mut egui::Ui,
        pending: &mut Vec<PendingCommand>,
        theme: &Theme,
    ) {
        if self.features.is_empty() {
            ui.add_space(8.0);
            ui.vertical_centered(|ui| ui.weak("No features on this sequence."));
            return;
        }
        // Local, sorted snapshot so the loop can mutate `self.editing` freely.
        let mut feats = self.features.clone();
        feats.sort_by_key(|f| f.span.start);
        let selected = match &self.selected {
            Some(super::SelectedNoun::Feature(id)) => Some(*id),
            _ => None,
        };
        let editing = &mut self.editing;

        let visibility = self.feature_visibility.clone();
        egui::ScrollArea::vertical().show(ui, |ui| {
            for f in &feats {
                let is_sel = selected == Some(f.id);
                // Per-row map-visibility eye (authoritative session hide/show; the
                // row stays listed regardless). Hidden-by-kind (`source`) reads as
                // hidden here too; un-hiding an individual row also lifts its kind
                // rule so the click is intuitive.
                let kind = FeatureKind::classify(&f.kind);
                let visible = visibility.visible(kind, f.id);
                let resp = ui
                    .horizontal(|ui| {
                        if visibility_button(ui, visible).clicked() {
                            let mut v = visibility.clone();
                            if visible {
                                v.hidden_ids.insert(f.id);
                            } else {
                                v.hidden_ids.remove(&f.id);
                                v.hidden_kinds.remove(&kind);
                            }
                            pending.push((AppCommand::SetFeatureVisibility(v), None));
                        }
                        row_shell(ui, &feature_display_row(f, is_sel), theme)
                    })
                    .inner;
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
                        if let Some(outcome) = feature_editor(ui, d, theme) {
                            match outcome {
                                EditOutcome::Commit(req) => {
                                    pending.push((AppCommand::Viewer(req), None));
                                    *editing = None;
                                }
                                EditOutcome::Delete(req) => {
                                    pending.push((AppCommand::Viewer(req), None));
                                    *editing = None;
                                }
                                EditOutcome::Cancel => *editing = None,
                            }
                        }
                    } else if feature_viewer(ui, f, theme) {
                        *editing = Some(FeatureDraft::from_row(f));
                        pending.push((AppCommand::RevealFeature { id: f.id }, None));
                    }
                }
            }
        });
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
            span: Span::from_range(10..40),
            strand: Strand::Reverse,
            color: egui::Color32::RED,
            type_color: egui::Color32::GREEN,
            stored_color: Some(egui::Color32::RED),
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
                color,
                target,
            } => {
                assert_eq!(color, None, "untouched colour is not re-sent");
                assert_eq!(id, FeatureId(7));
                assert_eq!(kind.as_deref(), Some("CDS"));
                assert_eq!(label.as_deref(), Some("lacZ"));
                assert_eq!(strand.as_deref(), Some("-"));
                assert_eq!(start, Some(10));
                assert_eq!(end, Some(40));
                assert_eq!(target, Target::active()); // always the active view
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
    fn colour_change_and_reset_map_to_the_color_field() {
        let colour_of = |d: &FeatureDraft| match d.to_request() {
            ViewerRequest::UpdateFeature { color, .. } => color,
            other => panic!("expected UpdateFeature, got {other:?}"),
        };
        let mut d = FeatureDraft::from_row(&row());
        d.color = Some(egui::Color32::from_rgb(0x84, 0xb0, 0xdc));
        assert_eq!(colour_of(&d).as_deref(), Some("#84b0dc"));
        d.color = None;
        assert_eq!(colour_of(&d).as_deref(), Some(""), "reset clears");
    }
}
