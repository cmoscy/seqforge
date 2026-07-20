//! Shared Phosphor icon painting.
//!
//! Plain `Label`/`Button` strings tofu Phosphor codepoints because egui's default
//! font lacks those glyphs. Painting with `FontId::proportional` lets the
//! Phosphor Regular fallback (registered in [`crate::app`]) resolve.

/// Paint a Phosphor Regular glyph at `size` in the current text color.
pub fn phosphor_icon(ui: &mut egui::Ui, icon: &str, size: f32) -> egui::Response {
    phosphor_icon_colored(ui, icon, size, ui.visuals().text_color())
}

/// Paint a Phosphor Regular glyph at `size` in `color`.
pub fn phosphor_icon_colored(
    ui: &mut egui::Ui,
    icon: &str,
    size: f32,
    color: egui::Color32,
) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        icon,
        egui::FontId::proportional(size),
        color,
    );
    resp
}

/// Clickable Phosphor glyph (hover fill, like inspector remove controls).
pub fn phosphor_icon_button(ui: &mut egui::Ui, icon: &str, size: f32) -> egui::Response {
    let pad = 4.0;
    let (rect, resp) =
        ui.allocate_exact_size(egui::vec2(size + pad, size + pad), egui::Sense::click());
    if resp.hovered() {
        ui.painter()
            .rect_filled(rect, 3.0, ui.visuals().widgets.hovered.bg_fill);
    }
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        icon,
        egui::FontId::proportional(size),
        ui.visuals().text_color(),
    );
    resp
}

/// Icon + label in one horizontal strip (non-interactive label text).
pub fn phosphor_labeled(
    ui: &mut egui::Ui,
    icon: &str,
    text: impl Into<egui::RichText>,
    size: f32,
    color: egui::Color32,
) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        phosphor_icon_colored(ui, icon, size, color);
        ui.colored_label(color, text);
    });
}
