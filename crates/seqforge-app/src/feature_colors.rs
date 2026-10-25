//! Settings → Feature Colors…: edit the active theme's `[features]` table.
//!
//! The palette a feature falls back to when it carries no colour of its own
//! (see [`Theme::feature_color`]). Edits preview live — the window swaps a
//! patched [`Config`] into `AppState` — and are only written to
//! `themes/<name>.toml` on Apply, via the comment-preserving
//! [`crate::config::write`]. Cancel restores the config the window opened on.

use std::collections::BTreeMap;
use std::sync::Arc;

use egui::Color32;

use crate::app::AppState;
use crate::command::AppCommand;
use crate::config::theme::{FEATURE_PRESETS, HexColor};
use crate::config::write::{embedded_theme, write_feature_colors};
use crate::config::{Config, Theme};
use crate::overlay::FEATURE_KINDS;

/// The eight coarse kind keys, listed first since every type falls back to one.
const BUCKETS: &[&str] = &[
    "gene",
    "cds",
    "promoter",
    "terminator",
    "rep",
    "source",
    "misc",
    "other",
];

pub struct FeatureColorsEditor {
    /// The config the window opened on (or last applied); Cancel restores it.
    base: Arc<Config>,
    /// Every listed type key → its explicit theme colour (`None` = no key in
    /// the theme; the type falls back to its bucket).
    rows: BTreeMap<String, Option<Color32>>,
    /// The built-in theme's own values, for per-row Reset.
    builtin: BTreeMap<String, Color32>,
    /// `epoch` of the config this window last pushed or adopted. A different
    /// epoch on `AppState` means someone else reloaded — re-seed from it.
    seen_epoch: u64,
    new_key: String,
}

fn explicit(theme: &Theme) -> BTreeMap<String, Option<Color32>> {
    theme
        .features
        .iter()
        .map(|(k, v)| (k.to_ascii_lowercase(), Some(v.0)))
        .collect()
}

impl FeatureColorsEditor {
    /// Seed from the live config, listing the theme's keys plus the common
    /// GenBank types and every type in the active sequence.
    pub fn open(state: &mut AppState) -> Self {
        let mut types: Vec<String> = FEATURE_KINDS
            .iter()
            .map(|k| k.to_ascii_lowercase())
            .collect();
        if let Ok(present) = state.workspace.with_active_buffer(|_, _, ann| {
            ann.iter()
                .map(|f| f.raw_kind.to_ascii_lowercase())
                .collect::<Vec<_>>()
        }) {
            types.extend(present);
        }
        Self::seeded(state.config.clone(), types)
    }

    fn seeded(base: Arc<Config>, extra_types: Vec<String>) -> Self {
        let mut rows = explicit(&base.theme);
        for k in BUCKETS.iter().map(|k| k.to_string()).chain(extra_types) {
            rows.entry(k).or_insert(None);
        }
        let builtin = embedded_theme(&base.settings.theme)
            .and_then(|body| toml::from_str::<Theme>(body).ok())
            .map(|t| {
                explicit(&t)
                    .into_iter()
                    .filter_map(|(k, v)| Some((k, v?)))
                    .collect()
            })
            .unwrap_or_default();
        Self {
            seen_epoch: base.epoch,
            base,
            rows,
            builtin,
            new_key: String::new(),
        }
    }

    /// The theme as currently edited.
    fn preview_theme(&self) -> Theme {
        let mut t = self.base.theme.clone();
        t.features = self
            .rows
            .iter()
            .filter_map(|(k, v)| Some((k.clone(), HexColor((*v)?))))
            .collect();
        t
    }

    /// The `[features]` writes that turn the base theme into the edited one.
    fn edits(&self) -> Vec<(String, Option<Color32>)> {
        let before = explicit(&self.base.theme);
        let mut out: Vec<_> = self
            .rows
            .iter()
            .filter(|(k, v)| before.get(*k).copied().flatten() != **v)
            .map(|(k, v)| (k.clone(), *v))
            .collect();
        // A theme key whose row was dropped entirely (not just un-set).
        out.extend(
            before
                .keys()
                .filter(|k| !self.rows.contains_key(*k))
                .map(|k| (k.clone(), None)),
        );
        out
    }

    fn is_dirty(&self) -> bool {
        !self.edits().is_empty()
    }
}

/// Swap a config carrying `theme` into the app, bumping `epoch` so
/// epoch-keyed caches (the minimap tessellation) rebuild.
fn push_preview(state: &mut AppState, theme: Theme) -> u64 {
    let mut cfg = (*state.config).clone();
    cfg.theme = theme;
    cfg.epoch = cfg.epoch.wrapping_add(1);
    let epoch = cfg.epoch;
    state.config = Arc::new(cfg);
    epoch
}

/// Draw the window if open. Returns commands for the caller to enqueue.
pub fn show(ctx: &egui::Context, state: &mut AppState) -> Vec<AppCommand> {
    let mut cmds = Vec::new();
    let Some(mut ed) = state.feature_colors.take() else {
        return cmds;
    };
    // A reload from elsewhere (menu, theme switch) replaces our base.
    if state.config.epoch != ed.seen_epoch {
        let extra = ed.rows.keys().cloned().collect();
        ed = FeatureColorsEditor::seeded(state.config.clone(), extra);
    }

    let mut open = true;
    let mut close = false;
    let mut apply = false;
    let before = ed.rows.clone();
    let preview = ed.preview_theme();
    let theme_name = ed.base.settings.theme.clone();

    egui::Window::new("Feature Colors")
        .collapsible(false)
        .resizable(true)
        .default_width(380.0)
        .open(&mut open)
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label("Theme");
                let mut chosen = theme_name.clone();
                egui::ComboBox::from_id_salt("feature_colors_theme")
                    .selected_text(&chosen)
                    .show_ui(ui, |ui| {
                        for name in theme_names(&theme_name) {
                            ui.selectable_value(&mut chosen, name.clone(), name);
                        }
                    });
                if chosen != theme_name {
                    cmds.push(AppCommand::SetActiveTheme(chosen));
                }
            });
            ui.weak(
                "Used for features without a colour of their own. \
                 Type keys beat the kind bucket they fall into.",
            );
            ui.separator();

            egui::ScrollArea::vertical()
                .max_height(420.0)
                .show(ui, |ui| {
                    let (buckets, types): (Vec<String>, Vec<String>) = ed
                        .rows
                        .keys()
                        .cloned()
                        .partition(|k| BUCKETS.contains(&k.as_str()));
                    ui.strong("Kinds");
                    for k in &buckets {
                        color_row(ui, &mut ed, &preview, k);
                    }
                    ui.add_space(6.0);
                    ui.strong("GenBank types");
                    for k in &types {
                        color_row(ui, &mut ed, &preview, k);
                    }
                });

            ui.horizontal(|ui| {
                let r = ui.add(
                    egui::TextEdit::singleline(&mut ed.new_key)
                        .hint_text("add a type, e.g. polyA_signal")
                        .desired_width(200.0),
                );
                let key = ed.new_key.trim().to_ascii_lowercase();
                let ok = !key.is_empty() && !ed.rows.contains_key(&key);
                let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if ui.add_enabled(ok, egui::Button::new("Add")).clicked() || (ok && enter) {
                    let c = preview.type_color(&key);
                    ed.rows.insert(key, Some(c));
                    ed.new_key.clear();
                }
            });
            ui.separator();
            ui.horizontal(|ui| {
                let dirty = ed.is_dirty();
                if ui.add_enabled(dirty, egui::Button::new("Apply")).clicked() {
                    apply = true;
                }
                if ui.button(if dirty { "Cancel" } else { "Close" }).clicked() {
                    close = true;
                }
                if dirty {
                    ui.weak(format!("writes themes/{theme_name}.toml"));
                }
            });
        });

    if apply {
        match write_feature_colors(&theme_name, &ed.edits()) {
            Ok(path) => {
                state.toasts.success(format!("Saved {}", path.display()));
                // Re-read from disk so what's shown is what was written.
                cmds.push(AppCommand::ReloadConfig);
            }
            Err(e) => {
                state.toasts.error(format!("Feature colours: {e}"));
            }
        }
    } else if ed.rows != before {
        ed.seen_epoch = push_preview(state, ed.preview_theme());
    }

    if !open || close {
        // Dropping unapplied edits: put the opening config back (with a
        // fresh epoch, so caches built from the preview are invalidated).
        if ed.is_dirty() {
            let mut cfg = (*ed.base).clone();
            cfg.epoch = state.config.epoch.wrapping_add(1);
            state.config = Arc::new(cfg);
        }
    } else {
        state.feature_colors = Some(ed);
    }
    cmds
}

/// One type row: swatch picker, preset menu, and Reset/Remove.
fn color_row(ui: &mut egui::Ui, ed: &mut FeatureColorsEditor, preview: &Theme, key: &str) {
    ui.horizontal(|ui| {
        let current = ed.rows.get(key).copied().flatten();
        let mut c = current.unwrap_or_else(|| preview.type_color(key));
        if egui::color_picker::color_edit_button_srgba(
            ui,
            &mut c,
            egui::color_picker::Alpha::Opaque,
        )
        .changed()
        {
            ed.rows.insert(key.to_string(), Some(c));
        }
        ui.menu_button("▾", |ui| {
            egui::Grid::new(("feature_colors_presets", key))
                .spacing([4.0, 4.0])
                .show(ui, |ui| {
                    for (i, (name, p)) in FEATURE_PRESETS.iter().enumerate() {
                        let b = egui::Button::new("")
                            .fill(*p)
                            .min_size(egui::vec2(20.0, 20.0));
                        if ui.add(b).on_hover_text(*name).clicked() {
                            ed.rows.insert(key.to_string(), Some(*p));
                            ui.close_menu();
                        }
                        if i % 6 == 5 {
                            ui.end_row();
                        }
                    }
                });
        });
        if current.is_some() {
            ui.label(key);
        } else {
            ui.weak(key)
                .on_hover_text("No key in the theme — uses its kind's colour");
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            // A built-in key resets to the shipped value; anything else (bar
            // the buckets, which always exist) un-sets back to its kind.
            let reset = match ed.builtin.get(key).copied() {
                Some(b) => (current != Some(b)).then_some(("Reset", Some(b))),
                None => (current.is_some() && !BUCKETS.contains(&key))
                    .then_some(("Use kind colour", None)),
            };
            if let Some((text, to)) = reset {
                if ui.small_button(text).clicked() {
                    ed.rows.insert(key.to_string(), to);
                }
            }
        });
    });
}

/// Built-in themes plus any `themes/*.toml` on disk, with `current` included
/// even if it is neither.
fn theme_names(current: &str) -> Vec<String> {
    let mut names = vec!["default-dark".to_string(), "default-light".to_string()];
    if let Ok(dir) = std::fs::read_dir(crate::config::paths::themes_dir()) {
        for e in dir.flatten() {
            let p = e.path();
            if p.extension().is_some_and(|x| x == "toml") {
                if let Some(stem) = p.file_stem().and_then(|s| s.to_str()) {
                    names.push(stem.to_string());
                }
            }
        }
    }
    names.push(current.to_string());
    names.sort();
    names.dedup();
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    fn editor() -> FeatureColorsEditor {
        FeatureColorsEditor::seeded(Arc::new(Config::default()), vec!["primer_bind".into()])
    }

    #[test]
    fn untouched_editor_writes_nothing() {
        let ed = editor();
        assert!(ed.edits().is_empty());
        assert!(!ed.is_dirty());
        // Buckets and the extra type are listed; the built-ins are known.
        assert!(ed.rows.contains_key("cds") && ed.rows.contains_key("primer_bind"));
        assert!(ed.builtin.contains_key("misc_difference"));
    }

    #[test]
    fn edits_are_the_minimal_diff() {
        let mut ed = editor();
        ed.rows.insert("cds".into(), Some(Color32::RED));
        ed.rows.insert("enhancer".into(), None);
        ed.rows.insert("my_type".into(), Some(Color32::BLUE));
        let mut e = ed.edits();
        e.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(
            e,
            vec![
                ("cds".into(), Some(Color32::RED)),
                ("enhancer".into(), None),
                ("my_type".into(), Some(Color32::BLUE)),
            ]
        );
    }

    #[test]
    fn preview_theme_reflects_rows() {
        let mut ed = editor();
        ed.rows.insert("misc_difference".into(), Some(Color32::RED));
        ed.rows.insert("enhancer".into(), None);
        let t = ed.preview_theme();
        assert_eq!(t.type_color("misc_difference"), Color32::RED);
        // Un-set → falls back to its bucket (`other`).
        assert_eq!(
            t.type_color("enhancer"),
            t.kind_color(seqforge_core::FeatureKind::Other)
        );
    }
}
