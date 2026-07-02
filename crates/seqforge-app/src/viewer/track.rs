//! Render-track infrastructure for the sequence viewer (T2 of the render-track
//! refactor — see `plans/render-tracks.md`).
//!
//! SeqForge wraps the sequence into **blocks** (one line-wrap each). A [`Track`]
//! is a per-block sub-lane that stacks vertically within every block. The
//! [`TrackStack`] runs **one** virtualized block loop: per visible block it sums
//! each track's `block_height` into that track's `y0`, then dispatches `paint`
//! and `hit_rects` at that offset. Because a track paints *and* hit-tests from
//! the same geometry, the two can't drift — the standing bug class the
//! abstraction removes (co-location invariant).
//!
//! This is **rendering/interaction only** — no domain-model change (decisions
//! 8/12/13). Sequence + Features stay "legacy core" paint calls until T3/T4.

use egui::{Align2, Color32, FontId, Painter, Pos2, Rect, Stroke, Vec2};
use seqforge_core::{
    Annotations, CutSite, Feature, FeatureId, FeatureKind, SearchHit, Selection, Strand,
};

use crate::command::AppCommand;
use crate::config::{LabelOverflow, Theme};

use super::translation::{AaGlyph, AaKind, OrfPromote, TranslationCache};

// ── Hit vocabulary ─────────────────────────────────────────────────────────────

/// Unified hit-test payload for one interactive rect in the sequence canvas
/// (T1 of the render-track refactor). Replaces the former five parallel
/// per-element hit vectors: one `Vec<(Rect, Hit)>`, resolved by variant in
/// priority order via [`find_hit`]. Each track emits its own hits from the same
/// geometry it paints (`Track::hit_rects`).
#[derive(Debug, Clone)]
pub(crate) enum Hit {
    /// Annotation bar — within-frame positional feature index (resolved to a
    /// `FeatureId` at click time via `render_ann.by_position`; a later phase can
    /// carry the `FeatureId` directly).
    Feature(usize),
    /// Search-hit highlight — index into `view.search_hits`.
    Search(usize),
    /// Cut-site label — index into `view.cut_sites`.
    CutSite(usize),
    /// ORF run in a frame lane (right-click → Annotate as CDS).
    Orf(OrfPromote),
    /// Translation codon cell — the codon's forward nt range.
    Codon(std::ops::Range<usize>),
}

impl Hit {
    pub fn as_feature(&self) -> Option<usize> {
        if let Hit::Feature(i) = self {
            Some(*i)
        } else {
            None
        }
    }
    pub fn as_search(&self) -> Option<usize> {
        if let Hit::Search(i) = self {
            Some(*i)
        } else {
            None
        }
    }
    pub fn as_cut_site(&self) -> Option<usize> {
        if let Hit::CutSite(i) = self {
            Some(*i)
        } else {
            None
        }
    }
    pub fn as_orf(&self) -> Option<OrfPromote> {
        if let Hit::Orf(o) = self {
            Some(*o)
        } else {
            None
        }
    }
    pub fn as_codon(&self) -> Option<std::ops::Range<usize>> {
        if let Hit::Codon(c) = self {
            Some(c.clone())
        } else {
            None
        }
    }
}

/// First hit whose rect contains `pos` and matches `extract`, in collection
/// order. Callers query by variant in priority order (feature → search → cut →
/// codon → seqpos), preserving the pre-unification resolution semantics.
pub(crate) fn find_hit<T>(
    hits: &[(Rect, Hit)],
    pos: Pos2,
    extract: impl Fn(&Hit) -> Option<T>,
) -> Option<T> {
    hits.iter()
        .find_map(|(r, h)| if r.contains(pos) { extract(h) } else { None })
}

// ── Stacking ─────────────────────────────────────────────────────────────────

/// Core greedy interval stacking (port of seqviz `stackElements`).
/// Sorts ranges by start, then packs each into the first row whose last
/// element ends at or before the current range's start.
/// Returns `(item_idx → row, n_rows)`.
pub(crate) fn greedy_stack(ranges: &[(usize, usize)]) -> (Vec<usize>, usize) {
    if ranges.is_empty() {
        return (vec![], 0);
    }
    let mut order: Vec<usize> = (0..ranges.len()).collect();
    order.sort_by_key(|&i| ranges[i].0);

    let mut result = vec![0usize; ranges.len()];
    let mut row_ends: Vec<usize> = Vec::new();

    for idx in order {
        let (start, end) = ranges[idx];
        match row_ends.iter().position(|&e| e <= start) {
            Some(r) => {
                row_ends[r] = end;
                result[idx] = r;
            }
            None => {
                row_ends.push(end);
                result[idx] = row_ends.len() - 1;
            }
        }
    }
    (result, row_ends.len())
}

// ── Per-block layout ─────────────────────────────────────────────────────────
//
// Each rendered block (one line-wrap of the sequence) sizes itself to the
// items actually present in it. Cut-label rows and feature rows are stacked
// *per block* rather than across the whole document, so a section with one
// enzyme doesn't leave blank space matching the heaviest stack elsewhere.

/// Layout decisions for one block.
#[derive(Debug, Default, Clone)]
pub(crate) struct BlockLayout {
    /// `(feat_idx, row_in_block)` for features overlapping this block.
    pub feat_rows: Vec<(usize, usize)>,
    /// Y offset (within the feature band) of each stack row's top. A row whose
    /// features carry a CDS translation is taller (bar + AA sub-row), so row
    /// offsets are a prefix sum of variable heights, not `row * annot_row_h`.
    pub feat_row_offsets: Vec<f32>,
    /// Total feature-band height (Σ row heights); the Features track's
    /// `block_height`.
    pub feat_band_h: f32,
    /// `(site_idx, row_in_block)` for cut sites whose `cut_pos` lies in
    /// `[block_start, block_end]`.
    pub cut_rows: Vec<(usize, usize)>,
    pub n_cut_rows: usize,
    /// Total height including ruler + both strands + gap.
    pub height: f32,
}

/// Build per-block layouts and the prefix-sum offsets used to convert block
/// indices to y-coordinates. `offsets[i]` is the y of the top of block `i`;
/// `offsets[n_blocks]` is the total content height.
///
/// The per-block height is the sum of every track's `block_height` plus the
/// trailing `block_gap`, in track order (CutLabels · Ruler · Sequence ·
/// Translation · Features); `TrackStack::y0s` re-derives the same prefix sums so
/// a track's painted position matches the offsets computed here.
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_block_layouts(
    annotations: &Annotations,
    cut_sites: &[CutSite],
    seq_len: usize,
    style: &Style,
    // Height of the global-frame translation band (0 when no frame lanes). Sits
    // between the bottom strand and the annotation bars in every block.
    trans_band_h: f32,
    // Memoized translation (`None` while staging / off): a feature with a CDS
    // sub-row makes its bar's stack row taller by `aa_row_h`.
    trans: Option<&TranslationCache>,
) -> (Vec<BlockLayout>, Vec<f32>) {
    let line_width = style.line_width;
    let n_blocks = seq_len.div_ceil(line_width).max(1);
    let mut layouts: Vec<BlockLayout> = Vec::with_capacity(n_blocks);
    let mut offsets: Vec<f32> = Vec::with_capacity(n_blocks + 1);
    offsets.push(0.0);

    for block_idx in 0..n_blocks {
        let block_start = block_idx * line_width;
        let block_end = (block_start + line_width).min(seq_len);

        // Features overlapping this block: clip ranges to the block for
        // the stacking pass so feature heights reflect what's actually
        // drawn in this row.
        let mut feat_idx_list: Vec<usize> = Vec::new();
        let mut feat_ranges: Vec<(usize, usize)> = Vec::new();
        for (i, f) in annotations.iter().enumerate() {
            if f.range.start < block_end && f.range.end > block_start {
                feat_idx_list.push(i);
                feat_ranges.push((f.range.start.max(block_start), f.range.end.min(block_end)));
            }
        }
        let (feat_local_rows, n_feat_rows) = greedy_stack(&feat_ranges);
        let feat_rows: Vec<(usize, usize)> = feat_idx_list
            .iter()
            .copied()
            .zip(feat_local_rows.iter().copied())
            .collect();

        // Variable feature-row heights: a row gains an AA sub-row iff one of its
        // features carries a (non-empty) CDS translation (T3 / editor 14e C2).
        let mut row_has_aa = vec![false; n_feat_rows];
        if let Some(tc) = trans {
            for (&idx, &row) in feat_idx_list.iter().zip(&feat_local_rows) {
                let id = annotations
                    .by_position(idx)
                    .expect("feat idx from this scan")
                    .id;
                if tc.feature_has_aa(id) {
                    row_has_aa[row] = true;
                }
            }
        }
        let mut feat_row_offsets = Vec::with_capacity(n_feat_rows);
        let mut fy = 0.0f32;
        for &has_aa in &row_has_aa {
            feat_row_offsets.push(fy);
            fy += style.annot_row_h + if has_aa { style.aa_row_h } else { 0.0 };
        }
        let feat_band_h = fy;

        // Cut sites whose top-strand cut sits in this block. Stacking
        // intervals use label half-width converted to base columns so
        // adjacent labels collide as the user expects.
        let mut cut_idx_list: Vec<usize> = Vec::new();
        let mut cut_ranges: Vec<(usize, usize)> = Vec::new();
        for (i, s) in cut_sites.iter().enumerate() {
            if s.cut_pos >= block_start && s.cut_pos <= block_end {
                cut_idx_list.push(i);
                let half_px = s.enzyme.len() as f32 * style.label_char_w * 0.5 + 4.0;
                let half_bases = (half_px / style.char_width).ceil() as usize + 1;
                cut_ranges.push((s.cut_pos.saturating_sub(half_bases), s.cut_pos + half_bases));
            }
        }
        let (cut_local_rows, n_cut_rows) = greedy_stack(&cut_ranges);
        let cut_rows: Vec<(usize, usize)> = cut_idx_list.into_iter().zip(cut_local_rows).collect();

        let cut_label_h = n_cut_rows as f32 * style.cut_label_row_h;
        let height = cut_label_h
            + style.ruler_h
            + style.strand_h * 2.0
            + trans_band_h
            + feat_band_h
            + style.block_gap;

        offsets.push(offsets.last().copied().unwrap_or(0.0) + height);
        layouts.push(BlockLayout {
            feat_rows,
            feat_row_offsets,
            feat_band_h,
            cut_rows,
            n_cut_rows,
            height,
        });
    }

    (layouts, offsets)
}

/// Locate the block containing a given y-coordinate (relative to the
/// allocated rect's top). Returns `None` if `rel_y` is negative.
pub(crate) fn y_to_block(rel_y: f32, offsets: &[f32]) -> Option<usize> {
    if rel_y < 0.0 || offsets.len() < 2 {
        return None;
    }
    // `offsets[i]` is the *top* of block i; total height is `offsets[n]`.
    // `partition_point` returns the first i where `offsets[i] > rel_y`, so
    // the containing block is one back.
    let idx = offsets.partition_point(|&o| o <= rel_y).saturating_sub(1);
    if idx >= offsets.len() - 1 {
        None
    } else {
        Some(idx)
    }
}

/// Clip a feature to the visible slice of a block and return its bar rect.
/// Returns `None` if the feature doesn't overlap this block at all.
pub(crate) fn annot_bar_rect(
    feat: &Feature,
    block_start: usize,
    block_end: usize,
    bar_row_y: f32, // top of the feature's stacked row
    seq_x0: f32,
    char_width: f32,
    row_h: f32,
) -> Option<Rect> {
    if feat.range.end <= block_start || feat.range.start >= block_end {
        return None;
    }
    let col_s = feat.range.start.max(block_start) - block_start;
    let col_e = feat.range.end.min(block_end) - block_start;
    Some(Rect::from_min_size(
        Pos2::new(seq_x0 + col_s as f32 * char_width, bar_row_y + 1.0),
        Vec2::new((col_e - col_s) as f32 * char_width, row_h - 2.0),
    ))
}

// ── Shared render style ────────────────────────────────────────────────────────

/// Resolved per-frame sizing, fonts, and colours shared by every track.
/// Built once in `SequenceView::show` from `Config` + `Theme` and borrowed
/// read-only through [`BlockCtx`].
#[derive(Debug, Clone)]
pub(crate) struct Style {
    // sizing
    pub char_width: f32,
    pub char_height: f32,
    pub label_char_w: f32,
    pub ruler_h: f32,
    pub strand_h: f32,
    pub annot_row_h: f32,
    pub cut_label_row_h: f32,
    pub aa_row_h: f32,
    pub block_gap: f32,
    pub line_width: usize,
    pub label_overflow: LabelOverflow,
    // fonts
    pub font_id: FontId,
    pub small_font: FontId,
    pub ruler_font: FontId,
    // colours
    pub text_color: Color32,
    pub selection_color: Color32,
    pub cursor_color: Color32,
    pub cut_site_color: Color32,
    pub label_text_light: Color32,
    pub label_text_dark: Color32,
    pub ruler_text: Color32,
    pub aa_stop: Color32,
    pub aa_start: Color32,
    pub orf_wash: Color32,
}

// ── Track trait + per-block context/geometry ───────────────────────────────────

/// Read-only per-block inputs shared by every track for one block. Cheap struct
/// of borrows rebuilt per block; the fixed-per-frame parts (`style`, `theme`,
/// caches) are the same references each time.
pub(crate) struct BlockCtx<'a> {
    pub block_idx: usize,
    pub block_start: usize,
    pub block_end: usize,
    /// The render sequence (speculative preview while staging, else committed).
    pub seq: &'a [u8],
    pub seq_len: usize,
    pub render_ann: &'a Annotations,
    /// Cut sites (empty while staging — derived overlays are suppressed then).
    pub cut_sites: &'a [CutSite],
    pub search_hits: &'a [SearchHit],
    pub trans_cache: Option<&'a TranslationCache>,
    pub show_orfs: bool,
    pub theme: &'a Theme,
    pub style: &'a Style,
    pub staging: bool,
    /// Realized-diff column ranges (preview space) — Sequence-track decorations.
    pub added: Option<(usize, usize)>,
    pub deleted: Option<(usize, usize)>,
    pub selection: Option<Selection>,
    pub selected_feature: Option<FeatureId>,
    /// Cursor blink phase (true = draw the cursor this frame).
    pub blink_on: bool,
    /// Cut site the pointer is hovering (full staple reveals), if any.
    pub hovered_cut_site: Option<usize>,
    /// Per-block stacking decisions (feature rows / cut rows).
    pub layout: &'a BlockLayout,
}

/// Per-track geometry within one block. Carries the Sequence track's strand
/// offsets so connector tracks (cut-site staples) can reach the sequence rows
/// below their own band.
pub(crate) struct BlockGeom {
    /// Top-y of this track's band.
    pub y0: f32,
    /// x of column 0 (left margin applied).
    pub seq_x0: f32,
    /// x of the allocated rect's left edge (for margin labels: 5'/3', lanes).
    pub rect_min_x: f32,
    /// Top-strand top-y for this block (Sequence track's `y0`), so connector
    /// tracks can reach the sequence rows.
    pub strand_top_y: f32,
    /// Bottom-strand top-y for this block.
    pub strand_bot_y: f32,
}

/// One block sub-lane. A track owns its **height**, **paint**, and **hit rects**
/// from a single geometry, so painting and hit-testing can't diverge.
pub(crate) trait Track {
    /// Vertical space this track occupies in `ctx`'s block.
    fn block_height(&self, ctx: &BlockCtx) -> f32;
    /// Paint this track's block at `geom.y0`.
    fn paint(&self, ctx: &BlockCtx, geom: &BlockGeom, painter: &Painter);
    /// Emit this track's interactive rects (same geometry `paint` uses).
    fn hit_rects(&self, ctx: &BlockCtx, geom: &BlockGeom, hits: &mut Vec<(Rect, Hit)>) {
        let _ = (ctx, geom, hits);
    }
}

/// Index of the Sequence track in the layout order — the strands, whose `y0`
/// every connector track (cut-site staples) reaches down to.
const SEQUENCE_TRACK: usize = 2;

/// The ordered set of tracks and the one block loop over them. Layout order
/// (top→bottom) is CutLabels · Ruler · Sequence · Translation · Features; the
/// paint order defers the CutSites track so its hover staple lands **on top of**
/// the strands it crosses (z-order preserved from the pre-refactor monolith).
pub(crate) struct TrackStack {
    tracks: Vec<Box<dyn Track>>,
    /// Indices into `tracks` in painting (z) order.
    paint_order: Vec<usize>,
}

impl TrackStack {
    /// The fixed T2 stack. Position-owned tracks (Ruler, CutSites, Translation)
    /// are migrated; Sequence + Features delegate to legacy core paint.
    pub fn new() -> Self {
        use super::tracks::{
            cut_sites::CutSitesTrack, features::FeaturesTrack, ruler::RulerTrack,
            sequence::SequenceTrack, translation::TranslationTrack,
        };
        let tracks: Vec<Box<dyn Track>> = vec![
            Box::new(CutSitesTrack), // 0
            Box::new(RulerTrack),    // 1
            Box::new(SequenceTrack), // 2 (== SEQUENCE_TRACK)
            Box::new(TranslationTrack),
            Box::new(FeaturesTrack),
        ];
        // Paint every track in layout order, then the cut-site staples last so
        // they overlay the strands / translation band they descend through.
        let paint_order = vec![1, 2, 3, 4, 0];
        Self {
            tracks,
            paint_order,
        }
    }

    /// Each track's `y0` within the block whose top is `block_top`, in layout
    /// order (prefix sum of `block_height`). Mirrors `build_block_layouts`.
    fn y0s(&self, ctx: &BlockCtx, block_top: f32) -> Vec<f32> {
        let mut y = block_top;
        let mut out = Vec::with_capacity(self.tracks.len());
        for t in &self.tracks {
            out.push(y);
            y += t.block_height(ctx);
        }
        out
    }

    fn geom_for(
        &self,
        y0s: &[f32],
        idx: usize,
        seq_x0: f32,
        rect_min_x: f32,
        strand_h: f32,
    ) -> BlockGeom {
        BlockGeom {
            y0: y0s[idx],
            seq_x0,
            rect_min_x,
            strand_top_y: y0s[SEQUENCE_TRACK],
            strand_bot_y: y0s[SEQUENCE_TRACK] + strand_h,
        }
    }

    /// Paint one block's tracks in z-order at the offsets `block_top` implies.
    pub fn paint_block(
        &self,
        ctx: &BlockCtx,
        block_top: f32,
        seq_x0: f32,
        rect_min_x: f32,
        painter: &Painter,
    ) {
        let y0s = self.y0s(ctx, block_top);
        let strand_h = ctx.style.strand_h;
        for &idx in &self.paint_order {
            let geom = self.geom_for(&y0s, idx, seq_x0, rect_min_x, strand_h);
            self.tracks[idx].paint(ctx, &geom, painter);
        }
    }

    /// Collect one block's interactive rects across all tracks.
    pub fn hit_block(
        &self,
        ctx: &BlockCtx,
        block_top: f32,
        seq_x0: f32,
        rect_min_x: f32,
        hits: &mut Vec<(Rect, Hit)>,
    ) {
        let y0s = self.y0s(ctx, block_top);
        let strand_h = ctx.style.strand_h;
        for idx in 0..self.tracks.len() {
            let geom = self.geom_for(&y0s, idx, seq_x0, rect_min_x, strand_h);
            self.tracks[idx].hit_rects(ctx, &geom, hits);
        }
    }
}

// ── Free paint helpers (shared across tracks) ──────────────────────────────────

/// Draw a small filled triangle pointing downward, with its apex at `(cx, y)`
/// and its base `height` units above (so the wedge sits *on top of* the
/// strand line and points into the cut). Used for the top-strand cut wedge.
pub(crate) fn paint_wedge_down(
    painter: &Painter,
    cx: f32,
    y: f32,
    half_w: f32,
    height: f32,
    color: Color32,
) {
    let points = vec![
        Pos2::new(cx - half_w, y - height),
        Pos2::new(cx + half_w, y - height),
        Pos2::new(cx, y),
    ];
    painter.add(egui::Shape::convex_polygon(
        points,
        color,
        egui::Stroke::NONE,
    ));
}

/// Draw a small filled triangle pointing upward, with its apex at `(cx, y)`
/// and its base `height` units below. Used for the bottom-strand cut wedge.
pub(crate) fn paint_wedge_up(
    painter: &Painter,
    cx: f32,
    y: f32,
    half_w: f32,
    height: f32,
    color: Color32,
) {
    let points = vec![
        Pos2::new(cx - half_w, y + height),
        Pos2::new(cx + half_w, y + height),
        Pos2::new(cx, y),
    ];
    painter.add(egui::Shape::convex_polygon(
        points,
        color,
        egui::Stroke::NONE,
    ));
}

pub(crate) fn search_hit_color(theme: &Theme, strand: Strand) -> Color32 {
    match strand {
        Strand::Forward => theme.strand.forward.0,
        Strand::Reverse => theme.strand.reverse.0,
        _ => theme.strand.unknown.0,
    }
}

pub(crate) fn build_strand_galley(
    painter: &Painter,
    bases: &[u8],
    font_id: &FontId,
    alpha: f32,
    theme: &Theme,
) -> std::sync::Arc<egui::Galley> {
    let mut job = egui::text::LayoutJob::default();
    for &b in bases {
        job.append(
            &(b.to_ascii_uppercase() as char).to_string(),
            0.0,
            egui::text::TextFormat {
                font_id: font_id.clone(),
                color: theme.bases.for_base(b).gamma_multiply(alpha),
                ..Default::default()
            },
        );
    }
    painter.layout_job(job)
}

/// Draw a feature label according to the `label_overflow` policy.
/// Contrast is guaranteed by the WCAG-aware text colour picker at the
/// call site, so no outline is needed.
pub(crate) fn paint_feature_label(
    painter: &Painter,
    font: &FontId,
    color: Color32,
    overflow: LabelOverflow,
    label_char_w: f32,
    bar: Rect,
    label: &str,
) {
    let bar_w = bar.width();
    let full_w = label.chars().count() as f32 * label_char_w;
    let text: Option<String> = if bar_w >= full_w {
        Some(label.to_string())
    } else {
        match overflow {
            LabelOverflow::Truncate => None,
            LabelOverflow::Extend => Some(label.to_string()),
            LabelOverflow::Ellipsis => {
                let usable = (bar_w - label_char_w).max(0.0);
                let n = (usable / label_char_w).floor() as usize;
                if n == 0 {
                    None
                } else {
                    let mut s: String = label.chars().take(n).collect();
                    s.push('…');
                    Some(s)
                }
            }
        }
    };
    let Some(text) = text else { return };
    painter.text(
        bar.center(),
        egui::Align2::CENTER_CENTER,
        &text,
        font.clone(),
        color,
    );
}

// ── Amino-acid lane rendering (shared by Translation + Features tracks) ─────────

/// Paint one AA lane's codon outlines + centred residue glyphs at `lane_y`.
/// Shared by the Translation track (global frame lanes) and the Features track
/// (per-CDS sub-rows). ORF wash + lane labels are the caller's concern.
#[allow(clippy::too_many_arguments)]
pub(crate) fn paint_aa_lane(
    painter: &Painter,
    style: &Style,
    block_start: usize,
    block_end: usize,
    seq_x0: f32,
    lane_y: f32,
    glyphs: &[AaGlyph],
    show_orfs: bool,
) {
    let text_color = style.text_color;
    let aa_normal = text_color.gamma_multiply(0.72);
    let char_width = style.char_width;
    let aa_row_h = style.aa_row_h;
    for g in glyphs {
        if g.pos < block_start || g.pos >= block_end {
            continue;
        }
        let color = if show_orfs {
            match g.kind {
                AaKind::Stop => style.aa_stop,
                AaKind::Start => style.aa_start,
                AaKind::Normal => aa_normal,
            }
        } else {
            aa_normal
        };
        // Codon cell spanning the residue's 3 nucleotides (clamped to the block
        // at a wrap). The faint outline groups the codon and marks the click
        // target (hit-rect emitted by `aa_codon_hits`).
        let ncs = g.pos.saturating_sub(1).max(block_start);
        let nce = (g.pos + 2).min(block_end);
        if ncs < nce {
            let cx = seq_x0 + (ncs - block_start) as f32 * char_width;
            let cw = (nce - ncs) as f32 * char_width;
            painter.rect_stroke(
                Rect::from_min_size(Pos2::new(cx, lane_y), Vec2::new(cw, aa_row_h)),
                2.0,
                Stroke::new(1.0, text_color.gamma_multiply(0.16)),
                egui::StrokeKind::Inside,
            );
        }
        let x = seq_x0 + (g.pos - block_start) as f32 * char_width + char_width * 0.5;
        painter.text(
            Pos2::new(x, lane_y),
            Align2::CENTER_TOP,
            g.ch,
            style.font_id.clone(),
            color,
        );
    }
}

/// Emit `Hit::Codon` rects for an AA lane's residues at `lane_y` (same geometry
/// `paint_aa_lane` outlines — the co-location invariant).
#[allow(clippy::too_many_arguments)]
pub(crate) fn aa_codon_hits(
    style: &Style,
    block_start: usize,
    block_end: usize,
    seq_len: usize,
    seq_x0: f32,
    lane_y: f32,
    glyphs: &[AaGlyph],
    hits: &mut Vec<(Rect, Hit)>,
) {
    let char_width = style.char_width;
    let aa_row_h = style.aa_row_h;
    for g in glyphs {
        if g.pos < block_start || g.pos >= block_end {
            continue;
        }
        let ncs = g.pos.saturating_sub(1).max(block_start);
        let nce = (g.pos + 2).min(block_end);
        if ncs < nce {
            let cx = seq_x0 + (ncs - block_start) as f32 * char_width;
            let cw = (nce - ncs) as f32 * char_width;
            hits.push((
                Rect::from_min_size(Pos2::new(cx, lane_y), Vec2::new(cw, aa_row_h)),
                Hit::Codon(g.pos.saturating_sub(1)..(g.pos + 2).min(seq_len)),
            ));
        }
    }
}

// ── Feature context menu plumbing (shared by show + Features track) ─────────────

/// Snapshot of a right-clicked feature, driving the annotation-bar context
/// menu. Captured by value so the menu closure needs no live annotation borrow.
#[derive(Debug, Clone)]
pub(crate) struct FeatureContext {
    pub id: FeatureId,
    pub start: usize,
    pub end: usize,
    pub strand: Strand,
    pub label: String,
    /// Verbatim GenBank feature-type string (for the Edit dialog).
    pub kind: String,
    /// `true` when the feature classifies as a CDS — the menu offers a CDS
    /// translation prefilled with its reading frame.
    pub is_cds: bool,
    /// Reading frame from `/codon_start` (1, 2, or 3; defaults to 1).
    pub codon_start: usize,
}

impl FeatureContext {
    /// Build a full context snapshot from a feature (right-click / secondary).
    pub fn from_feature(f: &Feature) -> Self {
        FeatureContext {
            id: f.id,
            start: f.range.start,
            end: f.range.end,
            strand: f.strand,
            kind: f.raw_kind.clone(),
            label: f.label.clone(),
            is_cds: matches!(FeatureKind::classify(&f.raw_kind), FeatureKind::Cds),
            codon_start: f
                .qualifiers
                .get("codon_start")
                .and_then(|v| v.as_deref())
                .and_then(|s| s.trim().parse::<usize>().ok())
                .filter(|n| (1..=3).contains(n))
                .unwrap_or(1),
        }
    }
}

/// GenBank-style strand flag for the feature forms (`+` / `-` / `.`).
pub(crate) fn strand_flag(strand: Strand) -> &'static str {
    match strand {
        Strand::Reverse => "-",
        Strand::None => ".",
        _ => "+",
    }
}

/// Build the edit-mode `OpenFeatureForm` command pre-filled from a right-clicked
/// / double-clicked feature.
pub(crate) fn open_edit_feature_cmd(fc: &FeatureContext) -> AppCommand {
    AppCommand::OpenFeatureForm {
        id: Some(fc.id),
        label: fc.label.clone(),
        kind: fc.kind.clone(),
        strand: strand_flag(fc.strand).to_string(),
        start: fc.start,
        end: fc.end,
    }
}

/// Screen → 0-based sequence offset. Returns positions in the closed range
/// `0..=seq_len` — the upper bound is the "insert-at-end" cursor.
pub(crate) fn screen_to_seq(
    pos: Pos2,
    rect: Rect,
    char_width: f32,
    line_width: usize,
    seq_len: usize,
    block_offsets: &[f32],
    left_margin: f32,
) -> Option<usize> {
    let rel_x = pos.x - rect.min.x - left_margin;
    let rel_y = pos.y - rect.min.y;
    if rel_x < 0.0 || rel_y < 0.0 {
        return None;
    }
    let block_idx = y_to_block(rel_y, block_offsets)?;
    let col = (rel_x / char_width) as usize;
    if col >= line_width {
        return None;
    }
    let p = block_idx * line_width + col;
    if p > seq_len { None } else { Some(p) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::LabelOverflow;
    use crate::viewer::tracks::{
        cut_sites::CutSitesTrack, features::FeaturesTrack, ruler::RulerTrack,
        sequence::SequenceTrack, translation::TranslationTrack,
    };
    use seqforge_core::{Annotations, Feature, Strand};

    /// A plausible `Style` for geometry tests (exact colours/fonts irrelevant).
    fn test_style() -> Style {
        let c = Color32::WHITE;
        Style {
            char_width: 8.0,
            char_height: 14.0,
            label_char_w: 6.0,
            ruler_h: 12.0,
            strand_h: 16.0,
            annot_row_h: 14.0,
            cut_label_row_h: 13.0,
            aa_row_h: 14.0,
            block_gap: 10.0,
            line_width: 20,
            label_overflow: LabelOverflow::Truncate,
            font_id: FontId::monospace(12.0),
            small_font: FontId::proportional(11.0),
            ruler_font: FontId::proportional(9.0),
            text_color: c,
            selection_color: c,
            cursor_color: c,
            cut_site_color: c,
            label_text_light: c,
            label_text_dark: c,
            ruler_text: c,
            aa_stop: c,
            aa_start: c,
            orf_wash: c,
        }
    }

    fn feat(range: std::ops::Range<usize>) -> Feature {
        Feature {
            id: Default::default(),
            range,
            raw_kind: "misc_feature".to_string(),
            label: "f".to_string(),
            strand: Strand::Forward,
            qualifiers: Default::default(),
            provenance: None,
        }
    }

    /// Co-location invariant: a track's `hit_rects` rect is the *same* geometry
    /// its `paint` uses. For the Features track that geometry is `annot_bar_rect`
    /// at the feature's stacked row — so the emitted hit rect must equal an
    /// independent `annot_bar_rect` of the same inputs.
    #[test]
    fn features_hit_rect_equals_painted_bar_rect() {
        let style = test_style();
        let theme = crate::config::Theme::default();
        let ann = Annotations::new(vec![feat(2..8)]);
        let layout = BlockLayout {
            feat_rows: vec![(0, 0)],
            feat_row_offsets: vec![0.0],
            feat_band_h: 14.0,
            cut_rows: vec![],
            n_cut_rows: 0,
            height: 0.0,
        };
        let ctx = BlockCtx {
            block_idx: 0,
            block_start: 0,
            block_end: 20,
            seq: b"ACGTACGTACGTACGTACGT",
            seq_len: 20,
            render_ann: &ann,
            cut_sites: &[],
            search_hits: &[],
            trans_cache: None,
            show_orfs: false,
            theme: &theme,
            style: &style,
            staging: false,
            added: None,
            deleted: None,
            selection: None,
            selected_feature: None,
            blink_on: false,
            hovered_cut_site: None,
            layout: &layout,
        };
        let geom = BlockGeom {
            y0: 100.0,
            seq_x0: 10.0,
            rect_min_x: 0.0,
            strand_top_y: 0.0,
            strand_bot_y: 0.0,
        };
        let mut hits = Vec::new();
        FeaturesTrack.hit_rects(&ctx, &geom, &mut hits);
        assert_eq!(hits.len(), 1);
        let expected = annot_bar_rect(
            &ann.by_position(0).unwrap().clone(),
            0,
            20,
            geom.y0, // row 0
            geom.seq_x0,
            style.char_width,
            style.annot_row_h,
        )
        .unwrap();
        assert_eq!(
            hits[0].0, expected,
            "hit rect must equal the painted bar rect"
        );
        assert!(matches!(hits[0].1, Hit::Feature(0)));
    }

    /// `TrackStack` block height == Σ track `block_height` + `block_gap`, i.e.
    /// the per-track heights the stack lays out reproduce `build_block_layouts`.
    #[test]
    fn stack_block_height_equals_build_block_layouts() {
        let style = test_style();
        let theme = crate::config::Theme::default();
        let ann = Annotations::new(vec![feat(1..30), feat(2..5)]);
        let (layouts, _off) = build_block_layouts(&ann, &[], 40, &style, 0.0, None);
        let layout = &layouts[0];
        let ctx = BlockCtx {
            block_idx: 0,
            block_start: 0,
            block_end: 20,
            seq: &[b'A'; 40],
            seq_len: 40,
            render_ann: &ann,
            cut_sites: &[],
            search_hits: &[],
            trans_cache: None,
            show_orfs: false,
            theme: &theme,
            style: &style,
            staging: false,
            added: None,
            deleted: None,
            selection: None,
            selected_feature: None,
            blink_on: false,
            hovered_cut_site: None,
            layout,
        };
        let tracks: Vec<Box<dyn Track>> = vec![
            Box::new(CutSitesTrack),
            Box::new(RulerTrack),
            Box::new(SequenceTrack),
            Box::new(TranslationTrack),
            Box::new(FeaturesTrack),
        ];
        let sum: f32 = tracks.iter().map(|t| t.block_height(&ctx)).sum::<f32>() + style.block_gap;
        assert!(
            (sum - layout.height).abs() < 1e-3,
            "Σ track heights + gap ({sum}) must equal build_block_layouts height ({})",
            layout.height
        );
    }

    #[test]
    fn find_hit_resolves_by_variant_then_order() {
        // Two overlapping rects at the same point: a Feature and a CutSite.
        // `find_hit` filters by the requested variant, and returns the first in
        // collection order — so callers get the intended type regardless of
        // which rect happens to overlap.
        let r = Rect::from_min_size(Pos2::new(0.0, 0.0), Vec2::new(10.0, 10.0));
        let p = Pos2::new(5.0, 5.0);
        let hits = vec![
            (r, Hit::CutSite(7)),
            (r, Hit::Feature(3)),
            (r, Hit::Codon(6..9)),
        ];
        assert_eq!(find_hit(&hits, p, Hit::as_feature), Some(3));
        assert_eq!(find_hit(&hits, p, Hit::as_cut_site), Some(7));
        assert_eq!(find_hit(&hits, p, Hit::as_codon), Some(6..9));
        assert_eq!(find_hit(&hits, p, Hit::as_search), None);
        // Outside every rect → no hit.
        assert_eq!(
            find_hit(&hits, Pos2::new(50.0, 50.0), Hit::as_feature),
            None
        );
    }
}
