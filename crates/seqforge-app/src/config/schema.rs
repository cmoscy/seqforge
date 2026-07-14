//! Strongly-typed user preferences deserialised from `settings.toml`.
//!
//! Every field uses `#[serde(default)]` against the type's `Default`
//! impl, so a missing key or a missing whole section falls back to the
//! built-in value. The `Default` impls are the canonical source of
//! truth for the runtime — the embedded `defaults/settings.toml` is a
//! commented template for users, not a parse target.

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    /// Name of the theme file under `themes/<name>.toml` (without the
    /// `.toml` suffix). Falls back to the built-in default-dark if the
    /// named theme cannot be found.
    pub theme: String,
    pub font: FontSettings,
    pub editor: EditorSettings,
    pub minimap: MinimapSettings,
    pub layout: LayoutSettings,
    pub inspector: InspectorSettings,
    pub terminal: TerminalSettings,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: "default-dark".into(),
            font: FontSettings::default(),
            editor: EditorSettings::default(),
            minimap: MinimapSettings::default(),
            layout: LayoutSettings::default(),
            inspector: InspectorSettings::default(),
            terminal: TerminalSettings::default(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct FontSettings {
    /// DNA base letters in the viewer (monospace).
    pub sequence_size: f32,
    /// Feature name text drawn on annotation bars.
    pub label_size: f32,
    /// Ruler tick labels.
    pub ruler_size: f32,
    /// General UI text (header rows, etc).
    pub ui_size: f32,
}

impl Default for FontSettings {
    fn default() -> Self {
        Self {
            sequence_size: 13.0,
            label_size: 12.0,
            ruler_size: 11.0,
            ui_size: 13.0,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct EditorSettings {
    pub left_margin: f32,
    pub right_margin: f32,
    /// Minimum ruler height; grows to fit `font.ruler_size + 2`.
    pub ruler_height: f32,
    pub strand_bar_height: f32,
    pub block_gap: f32,
    /// Padding above + below the label text inside each annotation row.
    pub label_padding: f32,
    /// Behaviour when a feature label is wider than its bar.
    pub label_overflow: LabelOverflow,
    /// Floor on annotation row height; the runtime uses
    /// `max(this, label_size + 2 * label_padding)`.
    pub min_annot_row_height: f32,
}

impl Default for EditorSettings {
    fn default() -> Self {
        Self {
            left_margin: 30.0,
            right_margin: 20.0,
            ruler_height: 14.0,
            strand_bar_height: 17.0,
            block_gap: 14.0,
            label_padding: 2.5,
            label_overflow: LabelOverflow::Truncate,
            min_annot_row_height: 16.0,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LabelOverflow {
    /// Hide the label entirely when it doesn't fit the bar.
    #[default]
    Truncate,
    /// Truncate with a trailing ellipsis when the bar can fit at least
    /// one character plus the ellipsis.
    Ellipsis,
    /// Allow the label to draw at full width on top of the bar even if
    /// it overflows the feature span (centered on the bar).
    Extend,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct MinimapSettings {
    pub spine_stroke: f32,
    pub feature_arc_width: f32,
    pub selected_border: f32,
    pub cursor_tick_length: f32,
    pub min_arc_degrees: f32,
    pub min_bar_width: f32,
    pub linear_spine_height: f32,
    pub linear_feature_row_height: f32,
    pub spine_feature_gap: f32,
}

impl Default for MinimapSettings {
    fn default() -> Self {
        Self {
            spine_stroke: 2.5,
            feature_arc_width: 7.0,
            selected_border: 2.0,
            cursor_tick_length: 9.0,
            min_arc_degrees: 2.5,
            min_bar_width: 2.0,
            linear_spine_height: 8.0,
            linear_feature_row_height: 6.0,
            spine_feature_gap: 3.0,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct LayoutSettings {
    /// Width fraction given to the file browser in a fresh layout.
    pub file_browser_fraction: f32,
    /// Height fraction given to the viewer above the terminal in a fresh layout.
    pub terminal_fraction: f32,
    /// Height fraction given to the file browser within its tab (the
    /// minimap takes the remainder).
    pub minimap_browser_fraction: f32,
    /// Width fraction given to the Inspector pane (right dock) in a fresh
    /// layout; the central viewer takes the remainder.
    pub inspector_fraction: f32,
}

impl Default for LayoutSettings {
    fn default() -> Self {
        Self {
            file_browser_fraction: 0.20,
            terminal_fraction: 0.70,
            minimap_browser_fraction: 0.62,
            inspector_fraction: 0.22,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct InspectorSettings {
    /// When `true` (default), selecting an object on the map switches the
    /// Inspector's active tab to follow it (properties-panel convention). The
    /// switch fires only when the selected *object changes*, so a manual tab
    /// switch sticks until the next selection. `false` → highlight-only (the row
    /// lights up on its tab, but the active tab never changes).
    pub follow_selection: bool,
}

impl Default for InspectorSettings {
    fn default() -> Self {
        Self {
            follow_selection: true,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct TerminalSettings {
    /// Shell to spawn. Empty string = use `$SHELL` (or `/bin/bash` as a fallback).
    pub shell: String,
}
