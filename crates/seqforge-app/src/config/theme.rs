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
use seqforge_core::{Feature, FeatureKind};
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
    #[serde(default)]
    pub translation: TranslationColors,
    #[serde(default)]
    pub diff: DiffColors,
    #[serde(default)]
    pub chrome: ChromeColors,
}

impl Default for Theme {
    fn default() -> Self {
        DEFAULT_DARK_THEME.clone()
    }
}

impl Theme {
    /// Resolve the colour of one feature.
    ///
    /// Precedence, highest first:
    ///   1. the colour stored *on the feature* (`/ApEinfo_fwdcolor`, or
    ///      `/ApEinfo_revcolor` on a reverse-strand feature) — skipped when
    ///      `use_file_colors` is off,
    ///   2. a theme key matching the verbatim GenBank type (`misc_difference`,
    ///      `enhancer`, …), which is how a palette addresses types finer than
    ///      the eight [`FeatureKind`] buckets,
    ///   3. a theme key for the bucket,
    ///   4. the palette baked into the code.
    ///
    /// SnapGene, Benchling and ApE all treat the per-feature colour as
    /// authoritative and the type default as merely what a *new* feature is
    /// born with; (1) is what puts SeqForge on that footing. A file colour we
    /// cannot parse falls through to (2) rather than erroring — a malformed
    /// qualifier should not blank out a feature.
    pub fn feature_color(&self, feat: &Feature, use_file_colors: bool) -> Color32 {
        if use_file_colors {
            if let Some(c) = feat.stored_color().and_then(parse_color) {
                return c;
            }
        }
        self.type_color(&feat.raw_kind)
    }

    /// The colour for a verbatim GenBank type — steps 2–4 of
    /// [`Self::feature_color`], i.e. what a feature with no colour of its own
    /// gets.
    pub fn type_color(&self, raw_kind: &str) -> Color32 {
        if let Some(c) = self.features.get(&raw_kind.to_ascii_lowercase()) {
            return c.0;
        }
        self.kind_color(FeatureKind::classify(raw_kind))
    }

    /// Resolve a feature *kind* to a colour, preferring a theme override
    /// and falling back to the legacy palette baked into the code.
    pub fn kind_color(&self, kind: FeatureKind) -> Color32 {
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
        // Mirrors default-dark.toml [bases] (mono ink; other dimmer).
        Self {
            a: HexColor(Color32::from_rgb(0xE8, 0xE8, 0xE8)),
            t: HexColor(Color32::from_rgb(0xE8, 0xE8, 0xE8)),
            g: HexColor(Color32::from_rgb(0xE8, 0xE8, 0xE8)),
            c: HexColor(Color32::from_rgb(0xE8, 0xE8, 0xE8)),
            other: HexColor(Color32::from_rgb(0x8A, 0x8A, 0x8A)),
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
        if v <= 0.03928 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
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
    /// Amber accent for a primer's mismatched annealed columns (the visual
    /// counterpart of the `Drifted` state; Phase 1.1).
    pub mismatch: HexColor,
    /// Neutral, semi-transparent grey washed over a hovered primer's annealed
    /// footprint / a hovered enzyme's recognition site. Deliberately *achromatic*
    /// so it reads as a transient "here it sits" cue, distinct from the chromatic
    /// (blue) `selection` — hue, not just alpha, separates the two states.
    pub hover_wash: HexColor,
    /// Red tint for destructive action fills (delete confirm buttons).
    pub danger: HexColor,
    /// Red tint for danger button hover state.
    pub danger_hover: HexColor,
    /// Amber accent for warning indicators (primer drifted state dot).
    pub warn: HexColor,
    /// Red for error text / labels.
    pub error: HexColor,
    /// Grey for methylated / blocked enzyme state text.
    pub methyl_blocked: HexColor,
    /// Outline around the selected feature / primer (canvas and minimap).
    pub selected_outline: HexColor,
}

impl Default for UiColors {
    fn default() -> Self {
        Self {
            selection: HexColor(Color32::from_rgba_unmultiplied(0x3A, 0x6E, 0xA8, 0xCC)),
            cursor: HexColor(Color32::from_rgb(50, 120, 255)),
            cut_site: HexColor(Color32::from_rgb(156, 168, 184)),
            label_text: HexColor(Color32::WHITE),
            label_text_alt: HexColor(Color32::from_rgb(20, 20, 20)),
            ruler_text: HexColor(Color32::from_rgb(160, 160, 160)),
            mismatch: HexColor(Color32::from_rgb(235, 155, 40)),
            hover_wash: HexColor(Color32::from_rgba_unmultiplied(150, 155, 165, 64)),
            danger: HexColor(Color32::from_rgb(0xB0, 0x30, 0x30)),
            danger_hover: HexColor(Color32::from_rgb(0xE0, 0x60, 0x60)),
            warn: HexColor(Color32::from_rgb(0xE0, 0xA0, 0x30)),
            error: HexColor(Color32::from_rgb(0xE0, 0x60, 0x60)),
            methyl_blocked: HexColor(Color32::GRAY),
            selected_outline: HexColor(Color32::WHITE),
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

/// The app chrome — panels, windows, widget text and separators — as opposed
/// to the sequence canvas. Mapped onto egui's [`egui::Visuals`] by
/// [`Theme::visuals`]: `dark` picks egui's stock dark or light base, and each
/// colour set here overrides it. Unset colours keep the stock value.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ChromeColors {
    pub dark: bool,
    /// Side/top/bottom panel background.
    pub panel: Option<HexColor>,
    /// Floating window and menu background.
    pub window: Option<HexColor>,
    /// Deepest background: text fields, scroll troughs.
    pub extreme: Option<HexColor>,
    /// Non-interactive text (labels, weak text derives from it).
    pub text: Option<HexColor>,
    /// Separators and window outlines.
    pub stroke: Option<HexColor>,
}

impl Default for ChromeColors {
    fn default() -> Self {
        Self {
            dark: true,
            panel: None,
            window: None,
            extreme: None,
            text: None,
            stroke: None,
        }
    }
}

impl Theme {
    /// The egui visuals for this theme's chrome.
    pub fn visuals(&self) -> egui::Visuals {
        let c = &self.chrome;
        let mut v = if c.dark {
            egui::Visuals::dark()
        } else {
            egui::Visuals::light()
        };
        if let Some(HexColor(x)) = c.panel {
            v.panel_fill = x;
            v.widgets.noninteractive.bg_fill = x;
            v.widgets.noninteractive.weak_bg_fill = x;
        }
        if let Some(HexColor(x)) = c.window {
            v.window_fill = x;
        }
        if let Some(HexColor(x)) = c.extreme {
            v.extreme_bg_color = x;
        }
        if let Some(HexColor(x)) = c.text {
            v.widgets.noninteractive.fg_stroke.color = x;
        }
        if let Some(HexColor(x)) = c.stroke {
            v.widgets.noninteractive.bg_stroke.color = x;
            v.window_stroke.color = x;
        }
        v
    }
}

/// Colours for the in-canvas translation band (frame lanes + feature CDS
/// lanes) and the ORF emphasis painted over the frame lanes (Phase 14e).
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct TranslationColors {
    /// Stop codons (`*`) in a frame/CDS lane.
    pub stop: HexColor,
    /// Start codons (`M`) in a frame/CDS lane.
    pub start: HexColor,
    /// Faint wash behind an ORF's Met→stop run when "Show ORFs" is on.
    pub orf_wash: HexColor,
}

impl Default for TranslationColors {
    fn default() -> Self {
        Self {
            stop: HexColor(Color32::from_rgb(210, 70, 70)),
            start: HexColor(Color32::from_rgb(70, 175, 90)),
            orf_wash: HexColor(Color32::from_rgba_unmultiplied(90, 175, 110, 26)),
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

/// Colours for the staged-edit diff preview (Phase 13.6).
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct DiffColors {
    /// Green wash behind added bases.
    pub add_bg: HexColor,
    /// Red wash behind deleted bases.
    pub del_bg: HexColor,
    /// Strikethrough line on deleted bases.
    pub del_line: HexColor,
}

impl Default for DiffColors {
    fn default() -> Self {
        Self {
            add_bg: HexColor(Color32::from_rgba_premultiplied(40, 120, 75, 70)),
            del_bg: HexColor(Color32::from_rgba_premultiplied(120, 45, 45, 70)),
            del_line: HexColor(Color32::from_rgb(214, 92, 92)),
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

/// Parse a colour written either as hex or as a CSS/X11 name.
///
/// Theme files are hex-only (see [`HexColor`]), but feature qualifiers are
/// not ours to dictate: ApE writes `/ApEinfo_fwdcolor="green"` as readily as
/// `"#00ff00"`, so a reader that only understood hex would silently ignore a
/// large share of real-world files.
pub fn parse_color(s: &str) -> Option<Color32> {
    let s = s.trim();
    if let Some(c) = parse_hex(s) {
        return Some(c);
    }
    // The X11/CSS basic set, which covers what ApE and SnapGene emit by name.
    let named: &[(&str, (u8, u8, u8))] = &[
        ("black", (0, 0, 0)),
        ("white", (255, 255, 255)),
        ("red", (255, 0, 0)),
        ("green", (0, 128, 0)),
        ("lime", (0, 255, 0)),
        ("blue", (0, 0, 255)),
        ("yellow", (255, 255, 0)),
        ("cyan", (0, 255, 255)),
        ("aqua", (0, 255, 255)),
        ("magenta", (255, 0, 255)),
        ("fuchsia", (255, 0, 255)),
        ("silver", (192, 192, 192)),
        ("gray", (128, 128, 128)),
        ("grey", (128, 128, 128)),
        ("maroon", (128, 0, 0)),
        ("olive", (128, 128, 0)),
        ("purple", (128, 0, 128)),
        ("teal", (0, 128, 128)),
        ("navy", (0, 0, 128)),
        ("orange", (255, 165, 0)),
        ("pink", (255, 192, 203)),
        ("brown", (165, 42, 42)),
        ("gold", (255, 215, 0)),
        ("violet", (238, 130, 238)),
        ("turquoise", (64, 224, 208)),
        ("salmon", (250, 128, 114)),
        ("khaki", (240, 230, 140)),
        ("plum", (221, 160, 221)),
        ("orchid", (218, 112, 214)),
        ("tan", (210, 180, 140)),
    ];
    let lower = s.to_ascii_lowercase();
    named
        .iter()
        .find(|(n, _)| *n == lower)
        .map(|(_, (r, g, b))| Color32::from_rgb(*r, *g, *b))
}

fn parse_hex(s: &str) -> Option<Color32> {
    let s = s.strip_prefix('#').unwrap_or(s);
    match s.len() {
        // `#RGB` shorthand: each nibble doubled, per CSS.
        3 => {
            let n = |i: usize| u8::from_str_radix(&s[i..i + 1], 16).ok().map(|v| v * 17);
            Some(Color32::from_rgb(n(0)?, n(1)?, n(2)?))
        }
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
    use seqforge_core::{Feature, FeatureId, Location, Strand};

    /// The embedded themes must define the same keys in the same tables — a
    /// key under the wrong table is silently ignored (most tables are not
    /// `deny_unknown_fields`), which is exactly how a misplaced block hides.
    #[test]
    fn embedded_themes_share_one_layout() {
        let tables = |body: &str| -> Vec<(String, Vec<String>)> {
            let v: toml::Table = toml::from_str(body).unwrap();
            v.iter()
                .map(|(t, inner)| {
                    let mut keys: Vec<String> = inner.as_table().unwrap().keys().cloned().collect();
                    keys.sort();
                    (t.clone(), keys)
                })
                .collect()
        };
        assert_eq!(
            tables(crate::config::defaults::DEFAULT_DARK),
            tables(crate::config::defaults::DEFAULT_LIGHT)
        );
        let light: Theme = toml::from_str(crate::config::defaults::DEFAULT_LIGHT).unwrap();
        assert!(light.features.contains_key("misc_difference"));
    }

    #[test]
    fn chrome_maps_onto_egui_visuals() {
        let dark = Theme::default();
        let light: Theme = toml::from_str(crate::config::defaults::DEFAULT_LIGHT).unwrap();
        // The dark theme spells out egui's stock values, so it changes nothing.
        let (v, stock) = (dark.visuals(), egui::Visuals::dark());
        assert!(v.dark_mode);
        assert_eq!(v.panel_fill, stock.panel_fill);
        assert_eq!(v.extreme_bg_color, stock.extreme_bg_color);
        assert_eq!(
            v.widgets.noninteractive.fg_stroke.color,
            stock.widgets.noninteractive.fg_stroke.color
        );
        let v = light.visuals();
        assert!(!v.dark_mode);
        assert_eq!(v.panel_fill, Color32::from_gray(248));
        assert_eq!(
            v.widgets.noninteractive.fg_stroke.color,
            Color32::from_gray(80)
        );
        // A theme with no [chrome] table keeps today's dark chrome.
        let bare: Theme = toml::from_str("[bases]\nA = \"#FFFFFF\"\n").unwrap();
        assert!(bare.visuals().dark_mode);
    }

    #[test]
    fn default_dark_theme_parses() {
        let t = Theme::default();
        assert!(t.features.contains_key("gene"));
    }

    fn feat(raw_kind: &str, strand: Strand, quals: &[(&str, &str)]) -> Feature {
        Feature {
            id: FeatureId(1),
            location: Location::simple(0..10),
            raw_kind: raw_kind.into(),
            label: "f".into(),
            strand,
            qualifiers: quals
                .iter()
                .map(|(k, v)| ((*k).to_string(), Some((*v).to_string())))
                .collect(),
            lineage: None,
        }
    }

    #[test]
    fn file_color_outranks_theme() {
        let t = Theme::default();
        let f = feat("CDS", Strand::Forward, &[("ApEinfo_fwdcolor", "#a6d9a6")]);
        assert_eq!(
            t.feature_color(&f, true),
            Color32::from_rgb(0xa6, 0xd9, 0xa6)
        );
    }

    #[test]
    fn use_file_colors_off_ignores_the_qualifier() {
        let t = Theme::default();
        let f = feat("CDS", Strand::Forward, &[("ApEinfo_fwdcolor", "#a6d9a6")]);
        assert_eq!(t.feature_color(&f, false), t.kind_color(FeatureKind::Cds));
    }

    /// ApE paints a reverse-strand feature from `revcolor`.
    #[test]
    fn reverse_strand_prefers_revcolor() {
        let t = Theme::default();
        let quals = [
            ("ApEinfo_fwdcolor", "#ff0000"),
            ("ApEinfo_revcolor", "#0000ff"),
        ];
        let fwd = feat("CDS", Strand::Forward, &quals);
        let rev = feat("CDS", Strand::Reverse, &quals);
        assert_eq!(t.feature_color(&fwd, true), Color32::from_rgb(255, 0, 0));
        assert_eq!(t.feature_color(&rev, true), Color32::from_rgb(0, 0, 255));
    }

    /// A theme key naming the verbatim GenBank type beats the coarse bucket —
    /// this is what lets `misc_difference` differ from `misc_feature`.
    #[test]
    fn raw_kind_key_outranks_bucket_key() {
        let mut t = Theme::default();
        t.features.insert(
            "misc_difference".into(),
            HexColor(Color32::from_rgb(1, 2, 3)),
        );
        let f = feat("misc_difference", Strand::Forward, &[]);
        assert_eq!(t.feature_color(&f, true), Color32::from_rgb(1, 2, 3));
        // ...and a sibling type still falls back to the bucket.
        let other = feat("misc_feature", Strand::Forward, &[]);
        assert_eq!(
            t.feature_color(&other, true),
            t.kind_color(FeatureKind::Misc)
        );
    }

    /// A malformed qualifier must not blank the feature out.
    #[test]
    fn unparseable_file_color_falls_back_to_palette() {
        let t = Theme::default();
        let f = feat("CDS", Strand::Forward, &[("ApEinfo_fwdcolor", "banana")]);
        assert_eq!(t.feature_color(&f, true), t.kind_color(FeatureKind::Cds));
    }

    #[test]
    fn parse_color_accepts_hex_forms_and_css_names() {
        assert_eq!(parse_color("#f00"), Some(Color32::from_rgb(255, 0, 0)));
        assert_eq!(parse_color("#ff0000"), Some(Color32::from_rgb(255, 0, 0)));
        assert_eq!(
            parse_color("#ff000080"),
            Some(Color32::from_rgba_unmultiplied(255, 0, 0, 0x80))
        );
        // ApE writes names as readily as hex.
        assert_eq!(parse_color("green"), Some(Color32::from_rgb(0, 128, 0)));
        assert_eq!(parse_color("  CYAN "), Some(Color32::from_rgb(0, 255, 255)));
        assert_eq!(parse_color("not-a-color"), None);
        assert_eq!(parse_color(""), None);
    }
}
