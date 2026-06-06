//! Theme palette: pure colour data, swappable per file.
//!
//! Colours parse from `#RRGGBB` or `#RRGGBBAA` hex strings into
//! [`egui::Color32`]. Any palette section can be omitted; missing keys
//! use the built-in defaults from the corresponding `Default` impl.
//!
//! [`Theme::default()`] is the canonical source of truth for the dark
//! palette — it parses `defaults/default-dark.toml` once via
//! [`std::sync::LazyLock`] so the embedded TOML and the runtime defaults
//! can never diverge.

use std::collections::HashMap;
use std::sync::LazyLock;

use egui::Color32;
use seqforge_core::FeatureKind;
use serde::{Deserialize, Deserializer};

use super::defaults::DEFAULT_DARK;

static DEFAULT_DARK_THEME: LazyLock<Theme> = LazyLock::new(|| {
    toml::from_str(DEFAULT_DARK).expect("embedded default-dark.toml is valid TOML")
});

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Theme {
    #[serde(default)]
    pub bases: BaseColors,
    #[serde(default)]
    pub ui: UiColors,
    #[serde(default)]
    pub features: HashMap<String, HexColor>,
    #[serde(default)]
    pub strand: StrandColors,
    #[serde(default)]
    pub minimap: MinimapColors,
}

impl Default for Theme {
    fn default() -> Self {
        DEFAULT_DARK_THEME.clone()
    }
}

impl Theme {
    /// Resolve a feature kind to a colour, preferring a theme override
    /// and falling back to the legacy palette baked into the code.
    pub fn feature_color(&self, kind: FeatureKind) -> Color32 {
        let key = match kind {
            FeatureKind::Gene => "gene",
            FeatureKind::Cds => "cds",
            FeatureKind::Promoter => "promoter",
            FeatureKind::Terminator => "terminator",
            FeatureKind::Rep => "rep",
            FeatureKind::Source => "source",
            FeatureKind::Misc => "misc",
            FeatureKind::Other => "other",
        };
        if let Some(c) = self.features.get(key) {
            return c.0;
        }
        // Built-in fallback identical to the pre-config viewer palette.
        match kind {
            FeatureKind::Gene => Color32::from_rgb(100, 149, 237),
            FeatureKind::Cds => Color32::from_rgb(72, 201, 176),
            FeatureKind::Promoter => Color32::from_rgb(241, 196, 15),
            FeatureKind::Terminator => Color32::from_rgb(231, 76, 60),
            FeatureKind::Rep => Color32::from_rgb(155, 89, 182),
            FeatureKind::Source => Color32::from_rgb(149, 165, 166),
            FeatureKind::Misc | FeatureKind::Other => Color32::from_rgb(189, 195, 199),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct BaseColors {
    #[serde(rename = "A")]
    pub a: HexColor,
    #[serde(rename = "T")]
    pub t: HexColor,
    #[serde(rename = "G")]
    pub g: HexColor,
    #[serde(rename = "C")]
    pub c: HexColor,
    /// Catch-all for ambiguous / non-ACGT bases.
    pub other: HexColor,
}

impl Default for BaseColors {
    fn default() -> Self {
        Self {
            a: HexColor(Color32::from_rgb(0, 150, 64)),
            t: HexColor(Color32::from_rgb(200, 30, 60)),
            g: HexColor(Color32::from_rgb(220, 120, 0)),
            c: HexColor(Color32::from_rgb(50, 100, 220)),
            other: HexColor(Color32::DARK_GRAY),
        }
    }
}

/// Pick whichever of `light` / `dark` gives the higher WCAG contrast
/// ratio against `bg`. The ratio is `(L_lighter + 0.05) /
/// (L_darker + 0.05)` where `L` is the relative luminance computed
/// from sRGB-linearised RGB components. This is the canonical method
/// from WCAG 2.x for "should this text be light or dark?" and handles
/// mid-tone swatches correctly without a hand-tuned threshold.
pub fn pick_contrast(bg: Color32, light: Color32, dark: Color32) -> Color32 {
    let l_bg = relative_luminance(bg);
    if contrast_ratio(l_bg, relative_luminance(dark))
        >= contrast_ratio(l_bg, relative_luminance(light))
    {
        dark
    } else {
        light
    }
}

fn relative_luminance(c: Color32) -> f32 {
    let to_linear = |x: u8| {
        let v = x as f32 / 255.0;
        if v <= 0.03928 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
    };
    0.2126 * to_linear(c.r()) + 0.7152 * to_linear(c.g()) + 0.0722 * to_linear(c.b())
}

fn contrast_ratio(l1: f32, l2: f32) -> f32 {
    let (lo, hi) = if l1 < l2 { (l1, l2) } else { (l2, l1) };
    (hi + 0.05) / (lo + 0.05)
}

impl BaseColors {
    pub fn for_base(&self, base: u8) -> Color32 {
        match base.to_ascii_uppercase() {
            b'A' => self.a.0,
            b'T' => self.t.0,
            b'G' => self.g.0,
            b'C' => self.c.0,
            _ => self.other.0,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct UiColors {
    pub selection: HexColor,
    pub cursor: HexColor,
    pub cut_site: HexColor,
    /// Feature label colour used on *dark* feature swatches.
    pub label_text: HexColor,
    /// Feature label colour used on *light* feature swatches. The
    /// viewer picks between `label_text` and `label_text_alt` per
    /// feature based on the swatch's relative luminance.
    pub label_text_alt: HexColor,
    /// Ruler tick label colour (a single base colour, the viewer applies
    /// its own gamma-multiply for the dim look).
    pub ruler_text: HexColor,
}

impl Default for UiColors {
    fn default() -> Self {
        Self {
            selection: HexColor(Color32::from_rgb(173, 214, 255)),
            cursor: HexColor(Color32::from_rgb(50, 120, 255)),
            cut_site: HexColor(Color32::from_rgb(156, 168, 184)),
            label_text: HexColor(Color32::WHITE),
            label_text_alt: HexColor(Color32::from_rgb(20, 20, 20)),
            ruler_text: HexColor(Color32::from_rgb(160, 160, 160)),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct StrandColors {
    pub forward: HexColor,
    pub reverse: HexColor,
    pub unknown: HexColor,
}

impl Default for StrandColors {
    fn default() -> Self {
        Self {
            forward: HexColor(Color32::from_rgba_unmultiplied(255, 190, 0, 110)),
            reverse: HexColor(Color32::from_rgba_unmultiplied(0, 190, 255, 110)),
            unknown: HexColor(Color32::from_rgba_unmultiplied(200, 200, 200, 90)),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct MinimapColors {
    /// Viewport indicator drawn behind features on the spine.
    pub viewport: HexColor,
    /// Linear minimap selection highlight.
    pub selection: HexColor,
    /// Linear minimap viewport border / cursor indicator.
    pub cursor: HexColor,
}

impl Default for MinimapColors {
    fn default() -> Self {
        Self {
            viewport: HexColor(Color32::from_rgba_unmultiplied(255, 255, 255, 28)),
            selection: HexColor(Color32::from_rgba_unmultiplied(173, 214, 255, 90)),
            cursor: HexColor(Color32::WHITE),
        }
    }
}

/// Hex colour wrapper that deserialises from `"#RRGGBB"` or `"#RRGGBBAA"`.
#[derive(Debug, Clone, Copy)]
pub struct HexColor(pub Color32);

impl Default for HexColor {
    fn default() -> Self {
        HexColor(Color32::WHITE)
    }
}

impl<'de> Deserialize<'de> for HexColor {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s: String = String::deserialize(d)?;
        parse_hex(&s)
            .map(HexColor)
            .ok_or_else(|| serde::de::Error::custom(format!("invalid hex colour: {s}")))
    }
}

fn parse_hex(s: &str) -> Option<Color32> {
    let s = s.strip_prefix('#').unwrap_or(s);
    match s.len() {
        6 => {
            let r = u8::from_str_radix(&s[0..2], 16).ok()?;
            let g = u8::from_str_radix(&s[2..4], 16).ok()?;
            let b = u8::from_str_radix(&s[4..6], 16).ok()?;
            Some(Color32::from_rgb(r, g, b))
        }
        8 => {
            let r = u8::from_str_radix(&s[0..2], 16).ok()?;
            let g = u8::from_str_radix(&s[2..4], 16).ok()?;
            let b = u8::from_str_radix(&s[4..6], 16).ok()?;
            let a = u8::from_str_radix(&s[6..8], 16).ok()?;
            Some(Color32::from_rgba_unmultiplied(r, g, b, a))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn default_dark_theme_parses() {
        let t = Theme::default();
        assert!(t.features.contains_key("gene"));
    }
}
