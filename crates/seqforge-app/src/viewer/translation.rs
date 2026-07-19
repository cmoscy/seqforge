//! Translation domain for the sequence viewer — the memoized reading-frame /
//! CDS amino-acid lanes and the pure glyph builders behind them.
//!
//! This is **derived-on-demand** render data (ROADMAP decision 13): nothing here
//! is stored on the buffer. [`TranslationCache`] is rebuilt only when the
//! sequence version or the display toggles change (see `SequenceView::show`).
//! The [`TranslationTrack`](super::tracks::translation) paints these lanes;
//! this module owns only their computation.

use std::collections::HashSet;

use seqforge_core::{Annotations, FeatureId, FeatureKind, Selection, Strand};

/// Which in-canvas translation lanes are active.
///
/// Two independent kinds of translation, matching the biology:
/// - **Feature translations** are *feature-anchored* — a feature's protein reads
///   from its own start in its own strand (`/codon_start`), so it never needs a
///   global frame. `show_cds` auto-enables this for every CDS; `features` adds
///   individually-toggled features (of any kind).
/// - **Global frame lanes** (`frames`, indexed `[+1, +2, +3, −1, −2, −3]`) are
///   the *frameless* whole-sequence scan, where a reading frame must be chosen
///   because there's no feature to anchor to.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TranslationDisplay {
    pub frames: [bool; 6],
    /// Auto-translate every CDS feature (anchored to its start/strand).
    pub show_cds: bool,
    /// Individually-toggled features to translate inline (any kind), by id.
    pub features: HashSet<FeatureId>,
    /// Emphasize ORFs in the frame lanes (stops red, Met green, Met→stop wash).
    pub show_orfs: bool,
}

impl TranslationDisplay {
    /// Any lane visible at all?
    pub fn is_active(&self) -> bool {
        self.show_cds || !self.features.is_empty() || self.frames.iter().any(|f| *f)
    }

    /// Should this feature get an inline translation lane? Auto for CDS when
    /// `show_cds`, plus any feature explicitly toggled on.
    fn wants_feature(&self, id: FeatureId, is_cds: bool) -> bool {
        (self.show_cds && is_cds) || self.features.contains(&id)
    }
}

/// Frame lane index → (strand, codon_start). `0..3` forward, `3..6` reverse.
fn frame_spec(i: usize) -> (Strand, usize) {
    if i < 3 {
        (Strand::Forward, i + 1)
    } else {
        (Strand::Reverse, i - 2)
    }
}

/// Human label for a frame-lane index (`+1`…`−3`).
fn frame_label(i: usize) -> &'static str {
    ["+1", "+2", "+3", "−1", "−2", "−3"][i]
}

/// How to colour an amino-acid glyph in a translation lane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AaKind {
    Normal,
    /// Start codon (Met) — green when ORFs are emphasized.
    Start,
    /// Stop codon (`*`) — red when ORFs are emphasized.
    Stop,
}

/// One amino acid placed under the sequence. `pos` is the **forward-strand
/// 0-based position of the codon's middle base**, so the glyph aligns under
/// that column regardless of strand.
#[derive(Debug, Clone)]
pub(crate) struct AaGlyph {
    pub pos: usize,
    pub ch: char,
    pub kind: AaKind,
}

/// One translation lane (a global frame, or the merged CDS lane).
#[derive(Debug, Clone)]
pub(crate) struct TransLane {
    pub label: String,
    /// The lane's strand — used when promoting one of its ORFs to a feature.
    pub strand: Strand,
    pub glyphs: Vec<AaGlyph>,
    /// Met→stop ORF spans (forward nt `[start, end)`) for the wash + promote;
    /// empty unless ORF emphasis is on.
    pub orf_runs: Vec<(usize, usize)>,
}

/// An ORF the user right-clicked in a frame lane, ready to annotate as a CDS.
#[derive(Debug, Clone, Copy)]
pub(crate) struct OrfPromote {
    pub start: usize,
    pub end: usize,
    pub strand: Strand,
}

/// A feature's own CDS translation — its amino-acid glyphs, painted by the
/// composite Features track directly under that feature's bar (T3 / editor 14e
/// C2). Unlike a global frame lane, this is *feature-owned*: it reads from the
/// feature's start/strand, so it needs no packing into a shared band.
#[derive(Debug, Clone)]
pub(crate) struct FeatureAa {
    pub id: FeatureId,
    pub glyphs: Vec<AaGlyph>,
}

/// Memoized translation for the whole buffer, rebuilt only when the sequence
/// version or the display toggles change.
///
/// The two kinds of translation live apart, matching where they're drawn: the
/// **global frame lanes** are a position-owned band hugging the sequence (the
/// Translation track), while each **feature CDS translation** rides under its
/// own bar (the Features track).
#[derive(Debug, Clone)]
pub(crate) struct TranslationCache {
    pub version: u64,
    pub display: TranslationDisplay,
    /// Forward frame lanes then reverse frame lanes, in display order.
    pub frame_lanes: Vec<TransLane>,
    /// Per-feature CDS translations (auto-CDS + toggled features), keyed by id;
    /// each read from its own start/strand. Drawn under the feature's bar.
    pub feature_glyphs: Vec<FeatureAa>,
}

impl TranslationCache {
    /// Number of global frame lanes — the position-owned band's row count.
    pub fn frame_band_rows(&self) -> usize {
        self.frame_lanes.len()
    }

    /// This feature's CDS amino-acid glyphs, if it is translated.
    pub fn feature_glyphs_for(&self, id: FeatureId) -> Option<&[AaGlyph]> {
        self.feature_glyphs
            .iter()
            .find(|fa| fa.id == id)
            .map(|fa| fa.glyphs.as_slice())
    }

    /// Whether this feature has a (non-empty) CDS sub-row to reserve space for.
    pub fn feature_has_aa(&self, id: FeatureId) -> bool {
        self.feature_glyphs_for(id).is_some_and(|g| !g.is_empty())
    }
}

/// Compute AA glyphs for a whole-sequence reading frame. Reverse frames read the
/// reverse complement but map each glyph back to its forward column.
pub(crate) fn frame_glyphs(seq: &[u8], strand: Strand, frame1: usize) -> Vec<AaGlyph> {
    let offset = frame1 - 1;
    let oriented = match strand {
        Strand::Reverse => seqforge_bio::reverse_complement(seq),
        _ => seq.to_vec(),
    };
    let protein = seqforge_bio::translate(&oriented, Strand::Forward, frame1);
    let l = seq.len();
    protein
        .chars()
        .enumerate()
        .filter_map(|(j, ch)| {
            let o_mid = offset + 3 * j + 1;
            if o_mid >= l {
                return None;
            }
            let pos = match strand {
                Strand::Reverse => l - 1 - o_mid,
                _ => o_mid,
            };
            let kind = match ch {
                '*' => AaKind::Stop,
                'M' => AaKind::Start,
                _ => AaKind::Normal,
            };
            Some(AaGlyph { pos, ch, kind })
        })
        .collect()
}

/// Glyphs for one CDS feature translated in its own frame/strand, placed at
/// forward columns within the feature span.
pub(crate) fn cds_glyphs(
    seq: &[u8],
    range: std::ops::Range<usize>,
    strand: Strand,
    codon_start: usize,
) -> Vec<AaGlyph> {
    let end = range.end.min(seq.len());
    if range.start >= end {
        return Vec::new();
    }
    let sub = &seq[range.start..end];
    let sublen = sub.len();
    let offset = codon_start.clamp(1, 3) - 1;
    let oriented = match strand {
        Strand::Reverse => seqforge_bio::reverse_complement(sub),
        _ => sub.to_vec(),
    };
    let protein = seqforge_bio::translate(&oriented, Strand::Forward, codon_start);
    protein
        .chars()
        .enumerate()
        .filter_map(|(j, ch)| {
            let o_mid = offset + 3 * j + 1;
            if o_mid >= sublen {
                return None;
            }
            let pos = match strand {
                Strand::Reverse => range.start + (sublen - 1 - o_mid),
                _ => range.start + o_mid,
            };
            let kind = match ch {
                '*' => AaKind::Stop,
                'M' => AaKind::Start,
                _ => AaKind::Normal,
            };
            Some(AaGlyph { pos, ch, kind })
        })
        .collect()
}

/// Extend a codon-anchored translation selection so both the origin codon
/// (`anchor`) and the newly clicked codon (`clicked`) stay whole, preserving
/// drag direction. Reaching right, the range runs from the origin's 5′ edge to
/// the clicked codon's 3′ edge; reaching left, from the origin's 3′ edge to the
/// clicked codon's 5′ edge — so a reverse (3′→5′) selection keeps the origin
/// residue's 3′ bases instead of clipping them. Matches whole-codon selection
/// in Benchling / SnapGene.
pub(crate) fn codon_extend(
    anchor: &std::ops::Range<usize>,
    clicked: &std::ops::Range<usize>,
) -> Selection {
    if clicked.start >= anchor.start {
        Selection::range(anchor.start, clicked.end)
    } else {
        Selection::range(anchor.end, clicked.start)
    }
}

/// Build the memoized translation lanes for the current display toggles.
pub(crate) fn build_translation_cache(
    seq: &[u8],
    annotations: &Annotations,
    version: u64,
    display: TranslationDisplay,
) -> TranslationCache {
    // ORF spans (per strand+frame) for the wash, computed once.
    let all_orfs = if display.show_orfs {
        seqforge_bio::find_orfs(seq, 1, true, true)
    } else {
        Vec::new()
    };

    let mut frame_lanes = Vec::new();
    for i in 0..6 {
        if !display.frames[i] {
            continue;
        }
        let (strand, frame1) = frame_spec(i);
        let glyphs = frame_glyphs(seq, strand, frame1);
        let orf_runs = all_orfs
            .iter()
            .filter(|o| o.strand == strand && o.frame == frame1)
            .map(|o| (o.start, o.end))
            .collect();
        frame_lanes.push(TransLane {
            label: frame_label(i).to_string(),
            strand,
            glyphs,
            orf_runs,
        });
    }

    // Per-feature CDS translations: every feature the display wants (auto-CDS +
    // individually toggled), each read from its own start/strand. No packing —
    // each rides under its own bar (the Features track owns that geometry).
    let feature_glyphs: Vec<FeatureAa> = annotations
        .iter()
        .filter(|f| {
            let is_cds = matches!(FeatureKind::classify(&f.raw_kind), FeatureKind::Cds);
            display.wants_feature(f.id, is_cds)
        })
        .filter_map(|f| {
            // Paint AA glyphs only for a single, non-wrapping CDS: a spliced
            // (`Join`) or origin-wrapping CDS has no single linear reading frame,
            // and flattening it to `bounds` (`0..len`) would render a wrong frame.
            // Omit it instead (`plans/span.md` P5a). Segment-aware CDS translation
            // is a feature-model follow-up.
            let span = f.location.as_span().filter(|s| !s.wraps(seq.len()))?;
            let cs = f
                .qualifiers
                .get("codon_start")
                .and_then(|v| v.as_deref())
                .and_then(|s| s.trim().parse::<usize>().ok())
                .filter(|n| (1..=3).contains(n))
                .unwrap_or(1);
            // Non-wrapping (filtered above) → linear extent is `start..start+len`
            // (not `end(len)`, which is `0` for a feature ending exactly at `len`).
            Some(FeatureAa {
                id: f.id,
                glyphs: cds_glyphs(seq, span.start..span.start + span.len, f.strand, cs),
            })
        })
        .collect();

    TranslationCache {
        version,
        display,
        frame_lanes,
        feature_glyphs,
    }
}

#[cfg(test)]
mod tests {
    use super::{AaKind, TranslationDisplay, build_translation_cache, codon_extend, frame_glyphs};
    use seqforge_core::Strand;

    #[test]
    fn frame_glyphs_forward_positions_and_kinds() {
        // ATG AAA TAA → M(start) K(normal) *(stop) at codon-middle columns 1,4,7.
        let g = frame_glyphs(b"ATGAAATAA", Strand::Forward, 1);
        assert_eq!(g.len(), 3);
        assert_eq!((g[0].pos, g[0].ch, g[0].kind), (1, 'M', AaKind::Start));
        assert_eq!((g[1].pos, g[1].ch), (4, 'K'));
        assert_eq!((g[2].pos, g[2].ch, g[2].kind), (7, '*', AaKind::Stop));
    }

    #[test]
    fn reverse_frame_glyphs_map_to_forward_columns() {
        // revcomp("TTATTTCAT") = "ATGAAATAA" (M K *). On the reverse lane the
        // glyphs anchor to forward columns (descending), still within bounds.
        let g = frame_glyphs(b"TTATTTCAT", Strand::Reverse, 1);
        assert_eq!(g.len(), 3);
        assert!(g.iter().all(|gl| gl.pos < 9));
        assert!(g.iter().any(|gl| gl.ch == 'M'));
    }

    #[test]
    fn translation_cache_builds_enabled_frames_only() {
        let seq = b"ATGAAATAAATGCCC";
        let ann = seqforge_core::Annotations::new(vec![]);
        let mut d = TranslationDisplay::default();
        d.frames[0] = true; // +1 only
        let cache = build_translation_cache(seq, &ann, 1, d);
        assert_eq!(cache.frame_lanes.len(), 1);
        assert!(cache.feature_glyphs.is_empty());
    }

    #[test]
    fn non_cds_feature_translates_when_toggled_on() {
        use seqforge_core::{Feature, Strand};
        let seq = b"ATGAAATAA";
        let mut ann = seqforge_core::Annotations::new(vec![]);
        let id = ann.add(Feature {
            id: Default::default(),
            location: seqforge_core::Location::simple(0..9),
            raw_kind: "misc_feature".to_string(),
            label: "region".to_string(),
            strand: Strand::Forward,
            qualifiers: Default::default(),
            lineage: None,
        });
        // show_cds is off; a misc_feature only translates when individually toggled.
        let mut d = TranslationDisplay::default();
        assert!(
            build_translation_cache(seq, &ann, 1, d.clone())
                .feature_glyphs
                .is_empty()
        );
        d.features.insert(id);
        let cache = build_translation_cache(seq, &ann, 1, d);
        assert_eq!(
            cache.feature_glyphs.len(),
            1,
            "toggled feature must translate"
        );
        // ATG AAA TAA anchored at the feature start → M K *.
        assert_eq!(
            cache.feature_glyphs[0]
                .glyphs
                .iter()
                .map(|g| g.ch)
                .collect::<String>(),
            "MK*"
        );
    }

    #[test]
    fn codon_extend_keeps_both_codons_whole_in_either_direction() {
        // Codons: A=[0,3), B=[3,6), C=[6,9).
        // Reaching right from A to C → [A.start, C.end) = 0..9.
        let s = codon_extend(&(0..3), &(6..9));
        assert_eq!(s.ordered(), (0, 9));
        // Reaching left from C to A → still 0..9; the origin codon C keeps its
        // 3′ base (index 8), which the old nt-level path clipped.
        let s = codon_extend(&(6..9), &(0..3));
        assert_eq!(s.ordered(), (0, 9));
        assert_eq!(
            s.anchor, 9,
            "reverse selection anchors at the origin's 3′ edge"
        );
        // Clicking the anchor codon itself selects exactly that codon.
        assert_eq!(codon_extend(&(3..6), &(3..6)).ordered(), (3, 6));
    }

    #[test]
    fn each_translated_feature_gets_its_own_glyphs() {
        use seqforge_core::{Feature, Strand};
        let seq = b"ATGAAATAAATGCCCTAA";
        let mut ann = seqforge_core::Annotations::new(vec![]);
        let mk = |range: std::ops::Range<usize>| Feature {
            id: Default::default(),
            location: seqforge_core::Location::simple(range),
            raw_kind: "CDS".to_string(),
            label: "c".to_string(),
            strand: Strand::Forward,
            qualifiers: Default::default(),
            lineage: None,
        };
        // Two overlapping CDS features (0..12 and 6..18 share columns 6..12).
        // No band packing now — each owns its glyphs, drawn under its own bar.
        let id0 = ann.add(mk(0..12));
        let id1 = ann.add(mk(6..18));
        let d = TranslationDisplay {
            show_cds: true,
            ..Default::default()
        };
        let cache = build_translation_cache(seq, &ann, 1, d);
        assert_eq!(cache.feature_glyphs.len(), 2);
        assert!(cache.feature_has_aa(id0));
        assert!(cache.feature_has_aa(id1));
    }

    #[test]
    fn wrapping_or_spliced_cds_is_omitted_not_mistranslated() {
        // P5a correct-by-omission: a spliced (`Join`) or origin-wrapping CDS has
        // no single linear reading frame, so it produces NO glyphs rather than a
        // wrong frame flattened through `bounds` (`0..len`).
        use seqforge_core::{Feature, Location, Span, Strand};
        let seq = b"ATGAAATAAATGCCCTAA"; // len 18
        let cds = |loc: Location| Feature {
            id: Default::default(),
            location: loc,
            raw_kind: "CDS".to_string(),
            label: "c".to_string(),
            strand: Strand::Forward,
            qualifiers: Default::default(),
            lineage: None,
        };
        let mut ann = seqforge_core::Annotations::new(vec![]);
        let linear = ann.add(cds(Location::simple(0..12)));
        let wrapping = ann.add(cds(Location::from_span(Span::new(15, 6)))); // 15..18 ∪ 0..3
        let spliced = ann.add(cds(Location::Join(vec![
            Location::simple(0..6),
            Location::simple(9..15),
        ])));
        let d = TranslationDisplay {
            show_cds: true,
            ..Default::default()
        };
        let cache = build_translation_cache(seq, &ann, 1, d);
        assert!(cache.feature_has_aa(linear), "single linear CDS translates");
        assert!(
            !cache.feature_has_aa(wrapping),
            "origin-wrapping CDS omitted"
        );
        assert!(!cache.feature_has_aa(spliced), "spliced Join CDS omitted");
        // Only the one translatable feature appears at all.
        assert_eq!(cache.feature_glyphs.len(), 1);
    }
}
