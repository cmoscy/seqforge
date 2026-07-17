//! Shared display atoms for the Inspector tabs — the "Track analog".
//!
//! All three tabs (Features · Enzymes · Primers) are bespoke — each layers its
//! own inline editor / query / expansion under a shared **compact row** shell.
//! The generic `InspectorCollection` renderer was retired once every noun grew an
//! interaction (decision 15); `Row` + `row_shell` remain the shared display atom.
//! Items are `pub(super)` so the sibling noun modules (`feature`, `primer`,
//! `cutsite`) and the parent (`mod`) can reuse them.

use seqforge_core::{PrimerInfo, Strand};

/// Tone for a row's state dot / name (mapped to a colour at render time).
pub(super) enum Tone {
    Normal,
    Warn,
    Dim,
}

pub(super) struct DetailLine {
    pub(super) text: String,
    pub(super) mono: bool,
}

/// A rendered row's compact display fields (fill + glyph + dot + name + right
/// cells). Interaction (click/double-click) and the selected-row detail/editor
/// are owned by each tab's bespoke loop. Rows carry no remove control —
/// deletion of authored data (features/primers) lives in the edit interface with
/// confirmation (decision 15).
pub(super) struct Row {
    pub(super) selected: bool,
    /// Strand arrow glyph (fwd/rev), if the noun is stranded.
    pub(super) glyph: Option<&'static str>,
    /// Primer state indicator (painted dot), if any. The tone maps 1:1 to state
    /// (Normal = Confirmed, Warn = Drifted, Dim = Detached).
    pub(super) dot: Option<Tone>,
    pub(super) name: String,
    /// Render the name subdued (unnamed feature / detached oligo).
    pub(super) dim_name: bool,
    /// Right-aligned compact cells, in visual left→right order.
    pub(super) right: Vec<String>,
}

/// Outcome of one frame of an inline editor (feature or primer).
pub(super) enum EditOutcome {
    Commit(seqforge_core::ViewerRequest),
    Delete(seqforge_core::ViewerRequest),
    Cancel,
}

/// Shared right-edge remove/close control, rendered from the Phosphor icon font
/// (the bundled egui font lacks ✕/trash glyphs — they tofu). One glyph, one
/// placement, one logic across Inspector tabs. `icon` conveys intent (`X` =
/// reversible remove-from-view; `TRASH` = destructive delete). Red-tinted on
/// hover. Returns its response so callers add hover text / read `clicked()`.
pub(super) fn remove_button(ui: &mut egui::Ui, icon: &str) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(18.0, 18.0), egui::Sense::click());
    let hovered = resp.hovered();
    if hovered {
        ui.painter()
            .rect_filled(rect, 3.0, ui.visuals().widgets.hovered.bg_fill);
    }
    let color = if hovered {
        egui::Color32::from_rgb(0xE0, 0x60, 0x60)
    } else {
        ui.visuals().weak_text_color()
    };
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        icon,
        egui::FontId::proportional(14.0),
        color,
    );
    resp
}

/// A per-row map-visibility toggle (Phosphor eye / eye-slash). `visible` drives
/// the icon; the caller flips the state on `clicked()`. Mirrors [`remove_button`]
/// — one glyph, one placement, one logic — but is reversible view state, not a
/// remove. A hidden row shows the crossed-out eye, subdued.
pub(super) fn visibility_button(ui: &mut egui::Ui, visible: bool) -> egui::Response {
    use egui_phosphor::regular;
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(18.0, 18.0), egui::Sense::click());
    let hovered = resp.hovered();
    if hovered {
        ui.painter()
            .rect_filled(rect, 3.0, ui.visuals().widgets.hovered.bg_fill);
    }
    let icon = if visible {
        regular::EYE
    } else {
        regular::EYE_SLASH
    };
    let color = if visible {
        ui.visuals().text_color()
    } else {
        ui.visuals().weak_text_color()
    };
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        icon,
        egui::FontId::proportional(14.0),
        color,
    );
    resp
}

/// Paint a primer state dot (filled for Confirmed/Drifted, hollow ring for
/// Detached), coloured by tone. Painted rather than a font glyph because the
/// bundled font tofus ●◐○, and colour is the primary signal for a status dot.
fn state_dot(ui: &mut egui::Ui, tone: &Tone) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(12.0, 14.0), egui::Sense::hover());
    let color = tone_color(ui, tone);
    let c = rect.center();
    match tone {
        Tone::Dim => {
            ui.painter()
                .circle_stroke(c, 3.5, egui::Stroke::new(1.2, color));
        }
        _ => {
            ui.painter().circle_filled(c, 3.5, color);
        }
    }
}

/// Draw a row's compact shell (fill + glyph + dot + name + right cells) and return
/// its click-sensed response. Shared by every tab so their rows look and behave
/// identically.
pub(super) fn row_shell(ui: &mut egui::Ui, row: &Row) -> egui::Response {
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
                if let Some(tone) = &row.dot {
                    state_dot(ui, tone);
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

/// The indented frame used for a selected row's detail / inline editor.
pub(super) fn detail_frame() -> egui::Frame {
    egui::Frame::new().inner_margin(egui::Margin {
        left: 22,
        right: 6,
        top: 2,
        bottom: 6,
    })
}

/// Phosphor strand arrows (the bundled font tofus →/←/↔).
pub(super) fn strand_glyph(strand: Strand) -> &'static str {
    use egui_phosphor::regular;
    match strand {
        Strand::Forward => regular::ARROW_RIGHT,
        Strand::Reverse => regular::ARROW_LEFT,
        Strand::Both => regular::ARROWS_LEFT_RIGHT,
        Strand::None => regular::MINUS,
    }
}

pub(super) fn strand_flag(strand: Strand) -> &'static str {
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
pub(super) fn binding_label(p: &PrimerInfo) -> String {
    match &p.binding {
        Some(b) => format!("{}–{}", b.start + 1, b.start + b.len),
        None => "Unattached".to_string(),
    }
}
