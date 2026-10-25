//! Writing config back to disk — the GUI's half of a config that is
//! otherwise hand-edited TOML.
//!
//! Edits go through `toml_edit` at the key level rather than re-serializing
//! [`super::Theme`]: a user's theme file is theirs, and a round trip through
//! `toml` would drop every comment and reorder every table. Only the keys the
//! user actually changed are touched; their trailing comments survive too.
//!
//! The pure `apply_*` functions do the document surgery (and are what the
//! tests exercise); the `write_*` wrappers add the disk I/O, materializing a
//! built-in theme or the settings template on first write exactly as the
//! "Open … File" menu commands do.

use std::path::PathBuf;

use egui::Color32;
use toml_edit::{DocumentMut, Item, Table, Value};

use super::{defaults, ensure_file_exists, paths};

/// The embedded body of a built-in theme, if `name` is one.
pub fn embedded_theme(name: &str) -> Option<&'static str> {
    match name {
        "default-dark" => Some(defaults::DEFAULT_DARK),
        "default-light" => Some(defaults::DEFAULT_LIGHT),
        _ => None,
    }
}

/// Theme files spell colours as uppercase `#RRGGBB` / `#RRGGBBAA`.
fn theme_hex(c: Color32) -> String {
    super::theme::color_to_hex(c).to_ascii_uppercase()
}

/// Set `key = value`, keeping the existing entry's decor (alignment padding,
/// trailing comment) when the key is already present.
fn set_preserving(table: &mut Table, key: &str, value: &str) {
    if let Some(v) = table.get_mut(key).and_then(Item::as_value_mut) {
        let decor = v.decor().clone();
        *v = Value::from(value);
        *v.decor_mut() = decor;
    } else {
        table.insert(key, toml_edit::value(value));
    }
}

/// Apply `[features]` edits to a theme document: `Some` sets the key, `None`
/// removes it. Keys are stored lowercase, matching how they are looked up.
pub fn apply_feature_colors(
    body: &str,
    edits: &[(String, Option<Color32>)],
) -> Result<String, String> {
    let mut doc: DocumentMut = body.parse().map_err(|e| format!("parse theme: {e}"))?;
    let item = doc
        .entry("features")
        .or_insert_with(|| Item::Table(Table::new()));
    let table = item
        .as_table_mut()
        .ok_or("theme `features` is not a table")?;
    for (key, color) in edits {
        let key = key.to_ascii_lowercase();
        match color {
            Some(c) => set_preserving(table, &key, &theme_hex(*c)),
            None => {
                table.remove(&key);
            }
        }
    }
    Ok(doc.to_string())
}

/// Point `settings.toml` at another theme.
pub fn apply_active_theme(body: &str, name: &str) -> Result<String, String> {
    let mut doc: DocumentMut = body.parse().map_err(|e| format!("parse settings: {e}"))?;
    set_preserving(doc.as_table_mut(), "theme", name);
    Ok(doc.to_string())
}

/// Write `[features]` edits into `themes/<name>.toml`, seeding it from the
/// built-in theme first if it is not on disk yet.
pub fn write_feature_colors(
    theme_name: &str,
    edits: &[(String, Option<Color32>)],
) -> Result<PathBuf, String> {
    let path = paths::theme_path(theme_name);
    if !path.exists() {
        let template = embedded_theme(theme_name)
            .ok_or_else(|| format!("theme {theme_name:?} has no file at {}", path.display()))?;
        ensure_file_exists(&path, template).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    rewrite(&path, |body| apply_feature_colors(body, edits))?;
    Ok(path)
}

/// Switch the active theme in `settings.toml`, seeding it from the template
/// first if needed.
pub fn write_active_theme(name: &str) -> Result<PathBuf, String> {
    let path = paths::settings_path();
    ensure_file_exists(&path, defaults::SETTINGS_TEMPLATE)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    rewrite(&path, |body| apply_active_theme(body, name))?;
    Ok(path)
}

fn rewrite(
    path: &std::path::Path,
    f: impl FnOnce(&str) -> Result<String, String>,
) -> Result<(), String> {
    let body = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let out = f(&body)?;
    std::fs::write(path, out).map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edits_touch_only_the_named_keys_and_keep_comments() {
        let out = apply_feature_colors(
            defaults::DEFAULT_DARK,
            &[
                ("CDS".into(), Some(Color32::from_rgb(0x12, 0xab, 0xef))),
                ("enhancer".into(), None),
                ("my_type".into(), Some(Color32::from_rgb(1, 2, 3))),
            ],
        )
        .unwrap();
        assert!(
            out.contains("cds        = \"#12ABEF\""),
            "alignment kept:\n{out}"
        );
        assert!(!out.contains("enhancer"), "removed");
        assert!(out.contains("my_type = \"#010203\""));
        // Comments elsewhere in the file survive, and the result still loads.
        assert!(out.contains("# used on dark feature swatches"));
        assert!(out.contains("# Keys are matched against the verbatim GenBank"));
        let theme: super::super::Theme = toml::from_str(&out).unwrap();
        assert_eq!(theme.type_color("CDS"), Color32::from_rgb(0x12, 0xab, 0xef));
    }

    #[test]
    fn a_trailing_comment_on_an_edited_key_survives() {
        let body = "[features]\ncds = \"#000000\"  # mine\n";
        let out = apply_feature_colors(body, &[("cds".into(), Some(Color32::WHITE))]).unwrap();
        assert_eq!(out, "[features]\ncds = \"#FFFFFF\"  # mine\n");
    }

    #[test]
    fn a_missing_features_table_is_created() {
        let out = apply_feature_colors(
            "[bases]\nA = \"#FFFFFF\"\n",
            &[("gene".into(), Some(Color32::BLACK))],
        )
        .unwrap();
        let theme: super::super::Theme = toml::from_str(&out).unwrap();
        assert_eq!(theme.type_color("gene"), Color32::BLACK);
    }

    #[test]
    fn switching_theme_keeps_the_settings_template_comments() {
        let out = apply_active_theme(defaults::SETTINGS_TEMPLATE, "default-light").unwrap();
        let s: super::super::Settings = toml::from_str(&out).unwrap();
        assert_eq!(s.theme, "default-light");
        let comments = |t: &str| {
            t.lines()
                .filter(|l| l.trim_start().starts_with('#'))
                .count()
        };
        assert_eq!(comments(&out), comments(defaults::SETTINGS_TEMPLATE));
    }
}
