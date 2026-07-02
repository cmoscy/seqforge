//! Translation domain for the sequence viewer — the memoized reading-frame /
//! CDS amino-acid lanes and the pure glyph builders behind them.
//!
//! This is **derived-on-demand** render data (ROADMAP decision 13): nothing here
//! is stored on the buffer. [`TranslationCache`] is rebuilt only when the
//! sequence version or the display toggles change (see `SequenceView::show`).
//! The [`TranslationTrack`](super::tracks::translation) paints these lanes;
//! this module owns only their computation.

use std::collections::HashSet;

use seqforge_core::{Annotations, Feature, FeatureId, FeatureKind, Selection, Strand};

use super::track::greedy_stack;

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

/// Memoized translation lanes for the whole buffer, rebuilt only when the
/// sequence version or the display toggles change.
#[derive(Debug, Clone)]
pub(crate) struct TranslationCache {
    pub version: u64,
    pub display: TranslationDisplay,
    /// Forward frame lanes then reverse frame lanes, in display order.
    pub frame_lanes: Vec<TransLane>,
    /// Feature-anchored lanes (auto-CDS + toggled features), each read from
    /// its own start/strand and packed so overlapping features never share a
    /// lane (same greedy interval stacking as the annotation bars).
    pub feature_lanes: Vec<TransLane>,
}

impl TranslationCache {
    /// Total lane count (frame lanes + feature lanes) — the band's row count.
    pub fn band_rows(&self) -> usize {
        self.frame_lanes.len() + self.feature_lanes.len()
    }

    /// Iterate every lane in paint order: frame lanes, then feature lanes.
    pub fn lanes(&self) -> impl Iterator<Item = &TransLane> {
        self.frame_lanes.iter().chain(self.feature_lanes.iter())
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
        Selection {
            anchor: anchor.start,
            focus: clicked.end,
        }
    } else {
        Selection {
            anchor: anchor.end,
            focus: clicked.start,
        }
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

    // Feature-anchored translation lanes: every feature the display wants
    // (auto-CDS + individually toggled), each read from its own start/strand.
    // Overlapping features are packed onto separate lanes via the same greedy
    // interval stacking the annotation bars use, so their residues never
    // collide in a shared row.
    let wanted: Vec<&Feature> = annotations
        .iter()
        .filter(|f| {
            let is_cds = matches!(FeatureKind::classify(&f.raw_kind), FeatureKind::Cds);
            display.wants_feature(f.id, is_cds)
        })
        .collect();
    let ranges: Vec<(usize, usize)> = wanted
        .iter()
        .map(|f| (f.range.start, f.range.end))
        .collect();
    let (rows, n_rows) = greedy_stack(&ranges);
    let mut feature_lanes: Vec<TransLane> = (0..n_rows)
        .map(|_| TransLane {
            label: "aa".to_string(),
            strand: Strand::Forward,
            glyphs: Vec::new(),
            orf_runs: Vec::new(),
        })
        .collect();
    for (i, f) in wanted.iter().enumerate() {
        let cs = f
            .qualifiers
            .get("codon_start")
            .and_then(|v| v.as_deref())
            .and_then(|s| s.trim().parse::<usize>().ok())
            .filter(|n| (1..=3).contains(n))
            .unwrap_or(1);
        feature_lanes[rows[i]]
            .glyphs
            .extend(cds_glyphs(seq, f.range.clone(), f.strand, cs));
    }

    TranslationCache {
        version,
        display,
        frame_lanes,
        feature_lanes,
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
        assert!(cache.feature_lanes.is_empty());
    }

    #[test]
    fn non_cds_feature_translates_when_toggled_on() {
        use seqforge_core::{Feature, Strand};
        let seq = b"ATGAAATAA";
        let mut ann = seqforge_core::Annotations::new(vec![]);
        let id = ann.add(Feature {
            id: Default::default(),
            range: 0..9,
            raw_kind: "misc_feature".to_string(),
            label: "region".to_string(),
            strand: Strand::Forward,
            qualifiers: Default::default(),
            provenance: None,
        });
        // show_cds is off; a misc_feature only translates when individually toggled.
        let mut d = TranslationDisplay::default();
        assert!(
            build_translation_cache(seq, &ann, 1, d.clone())
                .feature_lanes
                .is_empty()
        );
        d.features.insert(id);
        let cache = build_translation_cache(seq, &ann, 1, d);
        assert_eq!(
            cache.feature_lanes.len(),
            1,
            "toggled feature must translate"
        );
        // ATG AAA TAA anchored at the feature start → M K *.
        assert_eq!(
            cache.feature_lanes[0]
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
    fn overlapping_translated_features_get_separate_lanes() {
        use seqforge_core::{Feature, Strand};
        let seq = b"ATGAAATAAATGCCCTAA";
        let mut ann = seqforge_core::Annotations::new(vec![]);
        let mk = |range: std::ops::Range<usize>| Feature {
            id: Default::default(),
            range,
            raw_kind: "CDS".to_string(),
            label: "c".to_string(),
            strand: Strand::Forward,
            qualifiers: Default::default(),
            provenance: None,
        };
        // Two overlapping CDS features (0..12 and 6..18 share columns 6..12).
        ann.add(mk(0..12));
        ann.add(mk(6..18));
        let d = TranslationDisplay {
            show_cds: true,
            ..Default::default()
        };
        let cache = build_translation_cache(seq, &ann, 1, d);
        assert_eq!(
            cache.feature_lanes.len(),
            2,
            "overlapping features must be packed onto separate lanes"
        );
    }
}
