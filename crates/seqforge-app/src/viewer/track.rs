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

use std::hash::{Hash, Hasher};

use egui::{Align2, Color32, FontId, Painter, Pos2, Rect, Stroke, Vec2};
use seqforge_core::{
    Annotations, CutSite, Feature, FeatureId, FeatureKind, MethylState, PrimerId, SearchHit,
    Selection, Strand,
};

use crate::command::AppCommand;
use crate::config::{LabelOverflow, Theme};

use super::translation::{AaGlyph, AaKind, OrfPromote, TranslationCache, TranslationDisplay};

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
    /// Primer arrow — carries the stable [`PrimerId`] directly (decision 12/14;
    /// primers are greenfield, so no positional-index legacy to mirror).
    Primer(PrimerId),
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
    pub fn as_primer(&self) -> Option<PrimerId> {
        if let Hit::Primer(id) = self {
            Some(*id)
        } else {
            None
        }
    }
}

/// Which strand row(s) a hover footprint wash covers. A primer represents one
/// strand (its arrow band); an enzyme recognition site spans both.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FootprintStrands {
    Top,
    Bottom,
    Both,
}

impl FootprintStrands {
    pub fn top(self) -> bool {
        matches!(self, Self::Top | Self::Both)
    }
    pub fn bottom(self) -> bool {
        matches!(self, Self::Bottom | Self::Both)
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

/// One cut-label group: co-located restriction sites (shared `cut_pos`) that
/// share a single leader tick and stack their names vertically. `members` are
/// indices into `cut_sites`, kept individually addressable (hover/click) even
/// though they render as a set. `base_line` is the group's first label line
/// within the cut band (line 0 = topmost).
#[derive(Debug, Clone)]
pub(crate) struct CutGroup {
    pub cut_pos: usize,
    pub members: Vec<usize>,
    pub base_line: usize,
}

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
    /// Cut-label groups whose `cut_pos` lies in `[block_start, block_end]`.
    /// Co-located sites (isoschizomers sharing a `cut_pos`) are one group —
    /// their names stack vertically under a single leader tick instead of each
    /// taking its own scattered row.
    pub cut_groups: Vec<CutGroup>,
    /// Total label lines across all groups (the cut-band height, in rows).
    pub cut_band_lines: usize,
    /// `(primer_idx, row_in_block)` for **forward** primers overlapping this
    /// block (band above the top strand). `primer_idx` is a within-frame
    /// positional index into `Annotations::primers()`.
    pub primer_fwd_rows: Vec<(usize, usize)>,
    /// Forward-primer band height (Σ rows × `primer_row_h`); 0 when none.
    pub primer_fwd_band_h: f32,
    /// As above for **reverse** primers (band below the bottom strand).
    pub primer_rev_rows: Vec<(usize, usize)>,
    pub primer_rev_band_h: f32,
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
    // Session-scoped feature-visibility filter: hidden features (by kind/id, e.g.
    // `source` by default) are excluded here so they reserve no stack row and are
    // neither painted nor hit-tested.
    visibility: &super::FeatureVisibility,
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
            // Hidden features (source by default, or user-toggled) reserve no
            // stack row and are thus neither painted nor hit-tested downstream.
            if !visibility.visible(FeatureKind::classify(&f.raw_kind), f.id) {
                continue;
            }
            let hull = f.hull(seq_len);
            if hull.start < block_end && hull.end > block_start {
                feat_idx_list.push(i);
                feat_ranges.push((hull.start.max(block_start), hull.end.min(block_end)));
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

        // Cut sites whose top-strand cut sits in this block, **grouped by
        // `cut_pos`**: isoschizomers at one position become a single group that
        // stacks its names under one leader (decision 4 still holds — each stays
        // a distinct, individually-addressable entity). Groups are stacked
        // horizontally by their widest member's label width; a row's height is
        // its tallest group, and line offsets are the prefix sum of row heights.
        let mut groups: Vec<(usize, Vec<usize>)> = Vec::new();
        for (i, s) in cut_sites.iter().enumerate() {
            if s.cut_pos >= block_start && s.cut_pos <= block_end {
                match groups.iter_mut().find(|(pos, _)| *pos == s.cut_pos) {
                    Some((_, members)) => members.push(i),
                    None => groups.push((s.cut_pos, vec![i])),
                }
            }
        }
        let group_ranges: Vec<(usize, usize)> = groups
            .iter()
            .map(|(pos, members)| {
                let widest = members
                    .iter()
                    .map(|&i| cut_sites[i].enzyme.len())
                    .max()
                    .unwrap_or(0);
                let full_px = widest as f32 * style.label_char_w + 8.0;
                let full_bases = (full_px / style.char_width).ceil() as usize + 1;
                (pos.saturating_sub(1), *pos + full_bases)
            })
            .collect();
        let (group_rows, n_group_rows) = greedy_stack(&group_ranges);
        // Per-row height = tallest group in that row; line offset = prefix sum.
        let mut row_heights = vec![0usize; n_group_rows];
        for (gi, &row) in group_rows.iter().enumerate() {
            row_heights[row] = row_heights[row].max(groups[gi].1.len());
        }
        let mut row_line_offset = vec![0usize; n_group_rows];
        let mut acc = 0usize;
        for (r, h) in row_heights.iter().enumerate() {
            row_line_offset[r] = acc;
            acc += h;
        }
        let cut_band_lines = acc;
        let cut_groups: Vec<CutGroup> = groups
            .into_iter()
            .enumerate()
            .map(|(gi, (cut_pos, members))| CutGroup {
                cut_pos,
                members,
                base_line: row_line_offset[group_rows[gi]],
            })
            .collect();

        // Primers overlapping this block, stacked **per strand** into their own
        // band: forward above the top strand, reverse below the bottom strand.
        // A detached primer (`binding = None`) draws nowhere (panel-only), so it
        // is skipped here. `primer_idx` is the positional index into `primers()`.
        let (primer_fwd_rows, n_fwd_rows) =
            stack_primers(annotations, block_start, block_end, Strand::Forward);
        let (primer_rev_rows, n_rev_rows) =
            stack_primers(annotations, block_start, block_end, Strand::Reverse);
        let primer_fwd_band_h = n_fwd_rows as f32 * style.primer_row_h;
        let primer_rev_band_h = n_rev_rows as f32 * style.primer_row_h;

        let cut_label_h = cut_band_lines as f32 * style.cut_label_row_h;
        let height = cut_label_h
            + style.ruler_h
            + primer_fwd_band_h
            + style.strand_h * 2.0
            + primer_rev_band_h
            + trans_band_h
            + feat_band_h
            + style.block_gap;

        offsets.push(offsets.last().copied().unwrap_or(0.0) + height);
        layouts.push(BlockLayout {
            feat_rows,
            feat_row_offsets,
            feat_band_h,
            cut_groups,
            cut_band_lines,
            primer_fwd_rows,
            primer_fwd_band_h,
            primer_rev_rows,
            primer_rev_band_h,
            height,
        });
    }

    (layouts, offsets)
}

/// Stack the primers of one band (forward or reverse) overlapping a block into
/// non-overlapping rows. A primer joins the **reverse** band iff its strand is
/// `Reverse`; every other strand (Forward / Both / None) joins the **forward**
/// band. Detached primers (`binding = None`) draw nowhere and are skipped.
/// Returns `(primer_idx → row, n_rows)`; `primer_idx` is the positional index
/// into `Annotations::primers()`.
fn stack_primers(
    annotations: &Annotations,
    block_start: usize,
    block_end: usize,
    band: Strand,
) -> (Vec<(usize, usize)>, usize) {
    let want_rev = matches!(band, Strand::Reverse);
    let mut idx_list: Vec<usize> = Vec::new();
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    for (i, p) in annotations.primers().enumerate() {
        let Some(b) = &p.binding else { continue };
        if (p.strand == Strand::Reverse) != want_rev {
            continue;
        }
        if b.start < block_end && b.end > block_start {
            idx_list.push(i);
            ranges.push((b.start.max(block_start), b.end.min(block_end)));
        }
    }
    let (rows, n_rows) = greedy_stack(&ranges);
    (idx_list.into_iter().zip(rows).collect(), n_rows)
}

// ── Layout memoization (T4 perf) ───────────────────────────────────────────────
//
// `build_block_layouts` is O(blocks × features). The pre-refactor monolith ran it
// every frame for every block; this memoizes the whole result on a fingerprint of
// its inputs, so it rebuilds only when one changes (typically an edit, a resize,
// or a translation toggle) — not on every repaint / scroll / cursor blink.

/// Fingerprint of every input `build_block_layouts` reads. `buffer.version`
/// captures sequence **and** annotation edits (both bump it — the cache-key
/// contract); the rest capture the wrap width, layout dimensions, cut-site set,
/// and translation display (which features get a CDS sub-row + how many frame
/// lanes the band has).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct LayoutKey {
    version: u64,
    seq_len: usize,
    line_width: usize,
    dims: u64,
    cut_fp: u64,
    display: TranslationDisplay,
    /// Feature visibility is view-derived (an enzyme/primer-style session
    /// toggle), so it doesn't bump `version` — captured here so a show/hide
    /// rebuilds the memoized layout.
    visibility: super::FeatureVisibility,
}

impl LayoutKey {
    pub fn new(
        version: u64,
        seq_len: usize,
        style: &Style,
        cut_sites: &[CutSite],
        display: &TranslationDisplay,
        visibility: &super::FeatureVisibility,
    ) -> Self {
        LayoutKey {
            version,
            seq_len,
            line_width: style.line_width,
            dims: dims_fingerprint(style),
            cut_fp: cut_fingerprint(cut_sites),
            display: display.clone(),
            visibility: visibility.clone(),
        }
    }
}

/// Hash the layout-affecting `Style` dimensions (row heights, char/label
/// advances) so a font/settings change invalidates the memo.
fn dims_fingerprint(style: &Style) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for v in [
        style.char_width,
        style.label_char_w,
        style.ruler_h,
        style.strand_h,
        style.annot_row_h,
        style.cut_label_row_h,
        style.aa_row_h,
        style.primer_row_h,
        style.block_gap,
    ] {
        v.to_bits().hash(&mut h);
    }
    h.finish()
}

/// Hash the cut-site set (positions + label widths drive the stacking). Cut
/// sites are view-derived from the enzyme selection, so an enzyme toggle doesn't
/// bump `version` — this catches it.
fn cut_fingerprint(cut_sites: &[CutSite]) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    cut_sites.len().hash(&mut h);
    for s in cut_sites {
        s.cut_pos.hash(&mut h);
        s.enzyme.len().hash(&mut h);
    }
    h.finish()
}

/// Memoized per-block layout: the block layouts + prefix-sum offsets, valid as
/// long as `key` matches the current frame's inputs.
#[derive(Debug)]
pub(crate) struct LayoutCache {
    pub key: LayoutKey,
    pub layouts: Vec<BlockLayout>,
    pub offsets: Vec<f32>,
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

/// Clip one range to a block's visible slice and return its bar rect, or `None`
/// if it doesn't overlap. The per-segment primitive behind [`annot_bar_rect`].
#[allow(clippy::too_many_arguments)]
pub(crate) fn clip_range_rect(
    range: &std::ops::Range<usize>,
    block_start: usize,
    block_end: usize,
    bar_row_y: f32,
    seq_x0: f32,
    char_width: f32,
    row_h: f32,
) -> Option<Rect> {
    if range.end <= block_start || range.start >= block_end {
        return None;
    }
    let col_s = range.start.max(block_start) - block_start;
    let col_e = range.end.min(block_end) - block_start;
    Some(Rect::from_min_size(
        Pos2::new(seq_x0 + col_s as f32 * char_width, bar_row_y + 1.0),
        Vec2::new((col_e - col_s) as f32 * char_width, row_h - 2.0),
    ))
}

/// On-grid body rect for a primer's annealed footprint, clipped to a block —
/// the arrow's column-aligned body (arrowhead + 5' tail are painted *outside*
/// this rect). `None` if the binding doesn't overlap the block. The Primer
/// tracks paint **and** hit-test from this one rect (co-location invariant).
pub(crate) fn primer_body_rect(
    binding: &std::ops::Range<usize>,
    block_start: usize,
    block_end: usize,
    row_y: f32,
    seq_x0: f32,
    char_width: f32,
    row_h: f32,
) -> Option<Rect> {
    if binding.end <= block_start || binding.start >= block_end {
        return None;
    }
    let col_s = binding.start.max(block_start) - block_start;
    let col_e = binding.end.min(block_end) - block_start;
    Some(Rect::from_min_size(
        Pos2::new(seq_x0 + col_s as f32 * char_width, row_y + 1.0),
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
    /// Height of one primer-arrow stack row (arrow body + tail lift-off).
    pub primer_row_h: f32,
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
    /// Neutral grey wash for a hovered primer/enzyme footprint (see
    /// `UiColors::hover_wash`); distinct from `selection_color` by hue.
    pub hover_wash: Color32,
    pub cursor_color: Color32,
    pub cut_site_color: Color32,
    pub label_text_light: Color32,
    pub label_text_dark: Color32,
    pub ruler_text: Color32,
    pub aa_stop: Color32,
    pub aa_start: Color32,
    pub orf_wash: Color32,
    /// Amber accent for a primer's mismatched annealed bases.
    pub primer_mismatch: Color32,
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
    /// Per-primer template decomposition (annealed bases + mismatches + 5'
    /// tail), aligned positionally with `render_ann.primers()`. Recomputed each
    /// frame against the render sequence (cheap — a handful of primers × footprint).
    pub primer_decomps: &'a [seqforge_bio::PrimerDecomposition],
    /// Per-primer attachment state (Confirmed / Drifted / Detached + off-targets),
    /// aligned positionally with `render_ann.primers()`. Memoized on buffer version.
    pub primer_states: &'a [seqforge_bio::PrimerAttachment],
    /// Which primer overlays to draw (show/hide + arrows-vs-bases).
    pub primer_display: super::PrimerDisplay,
    /// Cut sites (empty while staging — derived overlays are suppressed then).
    pub cut_sites: &'a [CutSite],
    /// Methylation verdict per site, parallel to `cut_sites` (cached on the
    /// `View`, recomputed only on enzyme/context change — not per frame).
    pub methyl_states: &'a [MethylState],
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
    /// The selected primer *object* (id), if any. Drives the PrimerTrack's
    /// selected-emphasis pass over the oligo's own drawn bases (annealed body +
    /// lifted 5' tail) — the object-vs-range counterpart of `selected_feature`
    /// (Phase 1.5e). A primer's tail/overhang has no template column, so the
    /// highlight lives on the track, not on `selection` (which stays `None` for
    /// a selected primer).
    pub selected_primer: Option<PrimerId>,
    /// Cursor blink phase (true = draw the cursor this frame).
    pub blink_on: bool,
    /// Cut site the pointer is hovering (full staple reveals), if any.
    pub hovered_cut_site: Option<usize>,
    /// Ordered template nt-range to wash on hover — a hovered primer's annealed
    /// footprint or a hovered enzyme's recognition site — plus which strand
    /// row(s) to wash. Paint-time only, ephemeral (no `Selection`, no command,
    /// no undo); the Sequence track washes it in the neutral `style.hover_wash`
    /// grey, behind the strand glyphs and any real selection. A primer is
    /// single-stranded (`Top` for Forward, `Bottom` for Reverse — matching its
    /// arrow band); an enzyme recognition site is genuinely double-stranded
    /// (`Both`). One wash path serves both nouns.
    pub hover_footprint: Option<(usize, usize, FootprintStrands)>,
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
/// every connector track (cut-site staples, primer bands) reaches to. Must match
/// the Sequence track's position in [`TrackStack::new`].
const SEQUENCE_TRACK: usize = 3;

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
            cut_sites::CutSitesTrack,
            features::FeaturesTrack,
            primers::{PrimerForwardTrack, PrimerReverseTrack},
            ruler::RulerTrack,
            sequence::SequenceTrack,
            translation::TranslationTrack,
        };
        // Layout order (top→bottom): forward primers above the top strand,
        // reverse primers below the bottom strand — straddling the Sequence track
        // (decision 14 render; SnapGene/Benchling idiom). Below the strand the
        // codon-aligned Translation band hugs the bases (innermost), then reverse
        // primers, then Features outermost — distance from the bases tracks how
        // base-level each lane is.
        let tracks: Vec<Box<dyn Track>> = vec![
            Box::new(CutSitesTrack),      // 0
            Box::new(RulerTrack),         // 1
            Box::new(PrimerForwardTrack), // 2
            Box::new(SequenceTrack),      // 3 (== SEQUENCE_TRACK)
            Box::new(TranslationTrack),   // 4 — codon band hugs the bases
            Box::new(PrimerReverseTrack), // 5
            Box::new(FeaturesTrack),      // 6
        ];
        // Paint every track in layout order, then the cut-site staples last so
        // they overlay the strands / translation band they descend through.
        let paint_order = vec![1, 2, 3, 4, 5, 6, 0];
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
///
/// `sel` is the ordered, non-cursor selection nt-range (caller-filtered for
/// staging). Three tiers, keyed only on how the codon sits in the range
/// (source-agnostic — bases vs. residue click render identically):
/// - **fully inside** → full wash + full-strength glyph + brighter outline
///   ("cleanly selected, in frame"). Wash + glyph reinforce rather than a faint
///   letter on a faint wash.
/// - **partially inside** → faded wash, glyph/outline unchanged ("edge —
///   carrying part of an adjacent codon"). At most two per selection (the ragged
///   ends), so it doubles as a frame-alignment cue.
/// - **untouched** → nothing.
///
/// The glyph stays a clean binary — bright ⟺ the whole residue is captured — so
/// brightness reads as "in frame" independent of the wash's "touched".
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
    sel: Option<(usize, usize)>,
) {
    let text_color = style.text_color;
    let aa_normal = text_color.gamma_multiply(0.72);
    let char_width = style.char_width;
    let aa_row_h = style.aa_row_h;
    for g in glyphs {
        if g.pos < block_start || g.pos >= block_end {
            continue;
        }
        // Full codon nt span (`g.pos` is the middle base) for the containment
        // test; the block-clamped span (`ncs..nce`) for drawing at a wrap.
        let codon_s = g.pos.saturating_sub(1);
        let codon_e = g.pos + 2;
        let ncs = codon_s.max(block_start);
        let nce = codon_e.min(block_end);
        // Selected ⟺ the whole codon is inside the range; partial ⟺ it overlaps
        // but isn't wholly contained (a ragged / out-of-frame edge).
        let selected = sel.is_some_and(|(s, e)| s <= codon_s && codon_e <= e);
        let partial = !selected && sel.is_some_and(|(s, e)| codon_s < e && s < codon_e);
        // Selection wash behind the residue — full when contained, faded when
        // only partially in.
        if (selected || partial) && ncs < nce {
            let cx = seq_x0 + (ncs - block_start) as f32 * char_width;
            let cw = (nce - ncs) as f32 * char_width;
            let wash = if selected {
                style.selection_color
            } else {
                style.selection_color.gamma_multiply(0.35)
            };
            painter.rect_filled(
                Rect::from_min_size(Pos2::new(cx, lane_y), Vec2::new(cw, aa_row_h)),
                2.0,
                wash,
            );
        }
        // Residue color: full strength when selected (reinforces the wash),
        // dimmed otherwise. ORF start/stop keep their semantic hue either way.
        let color = if show_orfs {
            match g.kind {
                AaKind::Stop => style.aa_stop,
                AaKind::Start => style.aa_start,
                AaKind::Normal if selected => text_color,
                AaKind::Normal => aa_normal,
            }
        } else if selected {
            text_color
        } else {
            aa_normal
        };
        // Codon cell spanning the residue's 3 nucleotides (clamped to the block
        // at a wrap). The outline groups the codon and marks the click target
        // (hit-rect emitted by `aa_codon_hits`); brighter on a selected cell so
        // it reads as a discrete block.
        if ncs < nce {
            let cx = seq_x0 + (ncs - block_start) as f32 * char_width;
            let cw = (nce - ncs) as f32 * char_width;
            let outline = if selected { 0.5 } else { 0.16 };
            painter.rect_stroke(
                Rect::from_min_size(Pos2::new(cx, lane_y), Vec2::new(cw, aa_row_h)),
                2.0,
                Stroke::new(1.0, text_color.gamma_multiply(outline)),
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
    /// `true` when the feature classifies as a CDS — the menu offers a CDS
    /// translation prefilled with its reading frame.
    pub is_cds: bool,
    /// Reading frame from `/codon_start` (1, 2, or 3; defaults to 1).
    pub codon_start: usize,
}

impl FeatureContext {
    /// Build a full context snapshot from a feature (right-click / secondary).
    pub fn from_feature(f: &Feature, len: usize) -> Self {
        let span = f.hull(len);
        FeatureContext {
            id: f.id,
            start: span.start,
            end: span.end,
            strand: f.strand,
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

/// Route a right-clicked / double-clicked feature into the Inspector's inline
/// editor (decision 15, tab-exclusive editing) rather than a center modal.
pub(crate) fn open_edit_feature_cmd(fc: &FeatureContext) -> AppCommand {
    AppCommand::EditFeatureInInspector {
        id: fc.id,
        arm_delete: false,
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
        cut_sites::CutSitesTrack,
        features::FeaturesTrack,
        primers::{PrimerForwardTrack, PrimerReverseTrack},
        ruler::RulerTrack,
        sequence::SequenceTrack,
        translation::TranslationTrack,
    };
    use seqforge_core::{Annotations, Feature, Primer, Strand};

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
            primer_row_h: 14.0,
            block_gap: 10.0,
            line_width: 20,
            label_overflow: LabelOverflow::Truncate,
            font_id: FontId::monospace(12.0),
            small_font: FontId::proportional(11.0),
            ruler_font: FontId::proportional(9.0),
            text_color: c,
            selection_color: c,
            hover_wash: c,
            cursor_color: c,
            cut_site_color: c,
            label_text_light: c,
            label_text_dark: c,
            ruler_text: c,
            aa_stop: c,
            aa_start: c,
            orf_wash: c,
            primer_mismatch: c,
        }
    }

    fn feat(range: std::ops::Range<usize>) -> Feature {
        Feature {
            id: Default::default(),
            location: seqforge_core::Location::simple(range),
            raw_kind: "misc_feature".to_string(),
            label: "f".to_string(),
            strand: Strand::Forward,
            qualifiers: Default::default(),
            provenance: None,
        }
    }

    fn feat_kind(range: std::ops::Range<usize>, raw_kind: &str) -> Feature {
        Feature {
            raw_kind: raw_kind.to_string(),
            ..feat(range)
        }
    }

    /// Feature-visibility filter: `source` is excluded from the layout by default
    /// (reserves no stack row), and returns when shown.
    #[test]
    fn build_block_layouts_hides_source_by_default() {
        let style = test_style();
        // One `source` (whole molecule) + one real feature.
        let ann = Annotations::new(vec![feat_kind(0..20, "source"), feat_kind(2..8, "CDS")]);

        let default_vis = crate::viewer::FeatureVisibility::default();
        let (layouts, _) = build_block_layouts(&ann, &[], 20, &style, &default_vis, 0.0, None);
        assert_eq!(
            layouts[0].feat_rows.len(),
            1,
            "source hidden by default — only the CDS is laid out"
        );

        // Un-hide source (remove the kind rule) → both features are laid out.
        let show_all = crate::viewer::FeatureVisibility {
            hidden_kinds: Default::default(),
            ..Default::default()
        };
        let (layouts, _) = build_block_layouts(&ann, &[], 20, &style, &show_all, 0.0, None);
        assert_eq!(layouts[0].feat_rows.len(), 2, "source shown when un-hidden");
    }

    /// Co-location invariant: a track's `hit_rects` rect is the *same* geometry
    /// its `paint` uses. For the Features track that geometry is one
    /// `clip_range_rect` per `Location` segment — for a plain single-segment
    /// feature, the emitted hit rect equals a `clip_range_rect` of its span.
    #[test]
    fn features_hit_rect_equals_painted_bar_rect() {
        let style = test_style();
        let theme = crate::config::Theme::default();
        let ann = Annotations::new(vec![feat(2..8)]);
        let layout = BlockLayout {
            feat_rows: vec![(0, 0)],
            feat_row_offsets: vec![0.0],
            feat_band_h: 14.0,
            ..Default::default()
        };
        let ctx = BlockCtx {
            block_idx: 0,
            block_start: 0,
            block_end: 20,
            seq: b"ACGTACGTACGTACGTACGT",
            seq_len: 20,
            render_ann: &ann,
            primer_decomps: &[],
            primer_states: &[],
            primer_display: crate::viewer::PrimerDisplay::default(),
            cut_sites: &[],
            methyl_states: &[],
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
            selected_primer: None,
            blink_on: false,
            hovered_cut_site: None,
            hover_footprint: None,
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
        let expected = clip_range_rect(
            &ann.by_position(0).unwrap().hull(20),
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

    /// An origin-spanning `Join` (arms at the two ends of the linear layout)
    /// emits a hit rect **per arm** and nothing across the middle — so a click
    /// between the arms no longer selects it, and it never hit-tests full-width.
    #[test]
    fn features_hit_rects_are_per_segment_not_hull() {
        let style = test_style();
        let theme = crate::config::Theme::default();
        // ori-shaped: join(16..20, 0..4) on a length-20 molecule, one block wide.
        let ori = Feature {
            location: seqforge_core::Location::Join(vec![
                seqforge_core::Location::simple(16..20),
                seqforge_core::Location::simple(0..4),
            ]),
            ..feat(0..20)
        };
        let ann = Annotations::new(vec![ori]);
        let layout = BlockLayout {
            feat_rows: vec![(0, 0)],
            feat_row_offsets: vec![0.0],
            feat_band_h: 14.0,
            ..Default::default()
        };
        let ctx = BlockCtx {
            block_idx: 0,
            block_start: 0,
            block_end: 20,
            seq: b"ACGTACGTACGTACGTACGT",
            seq_len: 20,
            render_ann: &ann,
            primer_decomps: &[],
            primer_states: &[],
            primer_display: crate::viewer::PrimerDisplay::default(),
            cut_sites: &[],
            methyl_states: &[],
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
            selected_primer: None,
            blink_on: false,
            hovered_cut_site: None,
            hover_footprint: None,
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
        // Two arms → two hit rects; neither spans the whole 20-col block.
        assert_eq!(hits.len(), 2, "one hit rect per arm");
        let full_width = 20.0 * style.char_width;
        for (r, _) in &hits {
            assert!(
                r.width() < full_width,
                "no arm hit rect spans the whole molecule (was {})",
                r.width()
            );
        }
    }

    fn primer(binding: std::ops::Range<usize>, strand: Strand) -> Primer {
        Primer {
            id: Default::default(),
            name: "p".to_string(),
            sequence: "ACGTAC".to_string(),
            binding: Some(binding),
            strand,
            qualifiers: Default::default(),
        }
    }

    fn primer_ctx<'a>(
        ann: &'a Annotations,
        layout: &'a BlockLayout,
        style: &'a Style,
        theme: &'a crate::config::Theme,
    ) -> BlockCtx<'a> {
        BlockCtx {
            block_idx: 0,
            block_start: 0,
            block_end: 20,
            seq: &[b'A'; 20],
            seq_len: 20,
            render_ann: ann,
            primer_decomps: &[],
            primer_states: &[],
            primer_display: crate::viewer::PrimerDisplay::default(),
            cut_sites: &[],
            methyl_states: &[],
            search_hits: &[],
            trans_cache: None,
            show_orfs: false,
            theme,
            style,
            staging: false,
            added: None,
            deleted: None,
            selection: None,
            selected_feature: None,
            selected_primer: None,
            blink_on: false,
            hovered_cut_site: None,
            hover_footprint: None,
            layout,
        }
    }

    /// Co-location invariant for the forward Primer track: the emitted hit rect
    /// equals an independent `primer_body_rect` of the same footprint, and the
    /// hit carries the primer's id.
    #[test]
    fn primer_hit_rect_equals_painted_body_rect() {
        let style = test_style();
        let theme = crate::config::Theme::default();
        let mut ann = Annotations::new(vec![]);
        let pid = ann.add_primer(primer(2..8, Strand::Forward));
        let (layouts, _off) =
            build_block_layouts(&ann, &[], 20, &style, &Default::default(), 0.0, None);
        let ctx = primer_ctx(&ann, &layouts[0], &style, &theme);
        let geom = BlockGeom {
            y0: 100.0,
            seq_x0: 10.0,
            rect_min_x: 0.0,
            strand_top_y: 0.0,
            strand_bot_y: 0.0,
        };
        let mut hits = Vec::new();
        PrimerForwardTrack.hit_rects(&ctx, &geom, &mut hits);
        assert_eq!(hits.len(), 1);
        let expected = primer_body_rect(
            &(2..8),
            0,
            20,
            geom.y0,
            geom.seq_x0,
            style.char_width,
            style.primer_row_h,
        )
        .unwrap();
        assert_eq!(
            hits[0].0, expected,
            "hit rect must equal the painted body rect"
        );
        assert!(matches!(hits[0].1, Hit::Primer(id) if id == pid));
    }

    /// Forward primers land in the forward band and reverse in the reverse band;
    /// a detached primer (`binding = None`) draws in neither.
    #[test]
    fn primer_bands_partition_by_strand_and_skip_detached() {
        let style = test_style();
        let theme = crate::config::Theme::default();
        let mut ann = Annotations::new(vec![]);
        ann.add_primer(primer(0..6, Strand::Forward));
        ann.add_primer(primer(2..9, Strand::Reverse));
        ann.add_primer(Primer {
            binding: None,
            ..primer(0..6, Strand::Forward)
        });
        let (layouts, _off) =
            build_block_layouts(&ann, &[], 20, &style, &Default::default(), 0.0, None);
        let ctx = primer_ctx(&ann, &layouts[0], &style, &theme);
        let geom = BlockGeom {
            y0: 0.0,
            seq_x0: 0.0,
            rect_min_x: 0.0,
            strand_top_y: 0.0,
            strand_bot_y: 0.0,
        };
        let mut fwd = Vec::new();
        PrimerForwardTrack.hit_rects(&ctx, &geom, &mut fwd);
        let mut rev = Vec::new();
        PrimerReverseTrack.hit_rects(&ctx, &geom, &mut rev);
        assert_eq!(fwd.len(), 1, "one attached forward primer");
        assert_eq!(rev.len(), 1, "one reverse primer");
    }

    /// `TrackStack` block height == Σ track `block_height` + `block_gap`, i.e.
    /// the per-track heights the stack lays out reproduce `build_block_layouts`.
    #[test]
    fn stack_block_height_equals_build_block_layouts() {
        let style = test_style();
        let theme = crate::config::Theme::default();
        let ann = Annotations::new(vec![feat(1..30), feat(2..5)]);
        let (layouts, _off) =
            build_block_layouts(&ann, &[], 40, &style, &Default::default(), 0.0, None);
        let layout = &layouts[0];
        let ctx = BlockCtx {
            block_idx: 0,
            block_start: 0,
            block_end: 20,
            seq: &[b'A'; 40],
            seq_len: 40,
            render_ann: &ann,
            primer_decomps: &[],
            primer_states: &[],
            primer_display: crate::viewer::PrimerDisplay::default(),
            cut_sites: &[],
            methyl_states: &[],
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
            selected_primer: None,
            blink_on: false,
            hovered_cut_site: None,
            hover_footprint: None,
            layout,
        };
        let tracks: Vec<Box<dyn Track>> = vec![
            Box::new(CutSitesTrack),
            Box::new(RulerTrack),
            Box::new(PrimerForwardTrack),
            Box::new(SequenceTrack),
            Box::new(TranslationTrack),
            Box::new(PrimerReverseTrack),
            Box::new(FeaturesTrack),
        ];
        let sum: f32 = tracks.iter().map(|t| t.block_height(&ctx)).sum::<f32>() + style.block_gap;
        assert!(
            (sum - layout.height).abs() < 1e-3,
            "Σ track heights + gap ({sum}) must equal build_block_layouts height ({})",
            layout.height
        );
    }

    /// The layout memo rebuilds only when an input changes: equal inputs →
    /// equal key (cache hit); a version / cut-site / wrap / display change →
    /// different key (rebuild).
    #[test]
    fn layout_key_invalidates_on_each_input() {
        use seqforge_core::CutSite;
        let style = test_style();
        let display = TranslationDisplay::default();
        let cuts = vec![CutSite {
            enzyme: "EcoRI".into(),
            pattern: "GAATTC".into(),
            recognition: seqforge_core::Span::new(0, 6),
            cut_pos: 1,
            bottom_cut_pos: 5,
        }];
        let vis = crate::viewer::FeatureVisibility::default();
        let base = LayoutKey::new(3, 100, &style, &cuts, &display, &vis);
        // Identical inputs → equal (cache hit).
        assert_eq!(base, LayoutKey::new(3, 100, &style, &cuts, &display, &vis));
        // Version bump (a sequence or annotation edit).
        assert_ne!(base, LayoutKey::new(4, 100, &style, &cuts, &display, &vis));
        // Cut-site set change (enzyme toggle — doesn't bump version).
        assert_ne!(base, LayoutKey::new(3, 100, &style, &[], &display, &vis));
        // Wrap width change (resize).
        let mut narrow = test_style();
        narrow.line_width = 10;
        assert_ne!(base, LayoutKey::new(3, 100, &narrow, &cuts, &display, &vis));
        // Translation toggle (affects which features get a CDS sub-row).
        let mut d2 = TranslationDisplay::default();
        d2.frames[0] = true;
        assert_ne!(base, LayoutKey::new(3, 100, &style, &cuts, &d2, &vis));
        // Feature-visibility toggle (show/hide — doesn't bump version).
        let vis2 = crate::viewer::FeatureVisibility {
            show_all: false,
            ..Default::default()
        };
        assert_ne!(base, LayoutKey::new(3, 100, &style, &cuts, &display, &vis2));
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
