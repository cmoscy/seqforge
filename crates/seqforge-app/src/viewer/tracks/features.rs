//! Features track — the annotation bars below the strands, each with its own
//! **CDS translation sub-row** directly beneath it (T3 / editor 14e C2).
//!
//! This is the one **composite / feature-owned** track: a translated feature's
//! protein rides under its bar (proximity to source; the SnapGene/Benchling
//! idiom that disambiguates feature-dense plasmids), rather than pooling into
//! the global-frame band. Row heights are variable — a stack row with a
//! translated feature is taller by one AA row (`layout.feat_row_offsets`, sized
//! in `build_block_layouts`). Global frame translation stays position-owned in
//! the Translation track; `cds_glyphs` is reused unchanged.

use egui::{Color32, Painter, Pos2, Rect, Stroke};
use seqforge_core::{Feature, FeatureKind, Strand};

use crate::viewer::track::{
    BlockCtx, BlockGeom, Hit, Track, aa_codon_hits, clip_range_rect, paint_aa_lane,
    paint_feature_label,
};

pub(crate) struct FeaturesTrack;

/// The clipped bar rects for a feature within one block — **one per segment**
/// (`Location::segments()`), so a `Join` (spliced CDS, or an origin-spanning
/// feature whose arms sit at opposite ends of the linear layout) yields a rect
/// per arm and **nothing** in the blocks between them. A plain `Simple` feature
/// yields its single clipped bar. This is the geometry both `paint` and
/// `hit_rects` derive from, so they can't drift, and neither ever falls back to
/// the feature's linear *hull* (the source of the full-width-bar / mis-click
/// bugs on origin-spanning features).
fn feature_segment_rects(
    feat: &Feature,
    len_total: usize,
    block: std::ops::Range<usize>,
    bar_row_y: f32,
    seq_x0: f32,
    char_width: f32,
    row_h: f32,
) -> Vec<Rect> {
    feat.location
        .pieces(len_total)
        .into_iter()
        .filter_map(|seg| {
            clip_range_rect(
                &seg,
                block.start,
                block.end,
                bar_row_y,
                seq_x0,
                char_width,
                row_h,
            )
        })
        .collect()
}

/// Bounding rect of a feature's in-block segment rects (they share a row, so
/// only the x-extent varies) — the anchor for the label, selection outline, and
/// strand/torn markers within this block. `None` if the slice is empty.
fn union_rect(rects: &[Rect]) -> Option<Rect> {
    let first = rects.first()?;
    let min_x = rects.iter().map(|r| r.min.x).fold(first.min.x, f32::min);
    let max_x = rects.iter().map(|r| r.max.x).fold(first.max.x, f32::max);
    Some(Rect::from_min_max(
        Pos2::new(min_x, first.min.y),
        Pos2::new(max_x, first.max.y),
    ))
}

impl Track for FeaturesTrack {
    fn block_height(&self, ctx: &BlockCtx) -> f32 {
        ctx.layout.feat_band_h
    }

    fn hit_rects(&self, ctx: &BlockCtx, geom: &BlockGeom, hits: &mut Vec<(Rect, Hit)>) {
        let style = ctx.style;
        for &(feat_idx, row) in &ctx.layout.feat_rows {
            let feat = ctx
                .render_ann
                .by_position(feat_idx)
                .expect("feat_idx from this frame's layout");
            let bar_row_y = geom.y0 + ctx.layout.feat_row_offsets[row];
            // One hit rect per in-block segment — clicking any arm selects the
            // feature; the empty middle blocks of an origin-spanning feature emit
            // no hit rect, so a click there no longer selects it.
            for r in feature_segment_rects(
                feat,
                ctx.seq_len,
                ctx.block_start..ctx.block_end,
                bar_row_y,
                geom.seq_x0,
                style.char_width,
                style.annot_row_h,
            ) {
                hits.push((r, Hit::Feature(feat_idx)));
            }
            // Codon click targets for this feature's CDS sub-row (if any).
            if let Some(glyphs) = ctx
                .trans_cache
                .and_then(|tc| tc.feature_glyphs_for(feat.id))
            {
                aa_codon_hits(
                    style,
                    ctx.block_start,
                    ctx.block_end,
                    ctx.seq_len,
                    geom.seq_x0,
                    bar_row_y + style.annot_row_h,
                    glyphs,
                    hits,
                );
            }
        }
    }

    fn paint(&self, ctx: &BlockCtx, geom: &BlockGeom, painter: &Painter) {
        let style = ctx.style;
        // Ordered selection range (suppressed while staging / at a bare cursor),
        // washed behind any CDS residue whose codon overlaps it.
        let sel = ctx
            .selection
            .filter(|_| !ctx.staging)
            .filter(|s| !s.is_cursor())
            .map(|s| s.ordered());
        for &(feat_idx, row) in &ctx.layout.feat_rows {
            let feat = ctx
                .render_ann
                .by_position(feat_idx)
                .expect("feat_idx from this frame's layout");
            let bar_row_y = geom.y0 + ctx.layout.feat_row_offsets[row];

            // ── CDS translation sub-row, directly under this feature's bar ──
            // Painted before the bar so a wrapped feature's residues never sit
            // over an adjacent bar in the same row; clamped to the block.
            if let Some(glyphs) = ctx
                .trans_cache
                .and_then(|tc| tc.feature_glyphs_for(feat.id))
            {
                paint_aa_lane(
                    painter,
                    style,
                    ctx.block_start,
                    ctx.block_end,
                    geom.seq_x0,
                    bar_row_y + style.annot_row_h,
                    glyphs,
                    ctx.show_orfs,
                    sel,
                );
            }

            // Segment rects for this block. Empty ⇒ the feature has no presence
            // here (e.g. a middle block between the two arms of an origin-spanning
            // feature) ⇒ draw nothing — no fill, no label, no selection outline.
            let segments = feature_segment_rects(
                feat,
                ctx.seq_len,
                ctx.block_start..ctx.block_end,
                bar_row_y,
                geom.seq_x0,
                style.char_width,
                style.annot_row_h,
            );
            let Some(bounds) = union_rect(&segments) else {
                continue;
            };
            // Resolve the selected *id* to this frame's position for the
            // highlight — position never leaves the frame.
            let is_selected = ctx.selected_feature == Some(feat.id);
            let swatch = ctx
                .theme
                .feature_color(FeatureKind::classify(&feat.raw_kind));

            // A plain single-segment, non-fuzzy feature keeps exactly today's
            // one-rectangle look. A `Join` (spliced / origin-spanning) or a fuzzy
            // (`<`/`>`) feature draws one bar per segment, linked by a thin intron
            // connector, with a strand arrow + torn edges anchored on this block's
            // segment bounds.
            let (fuzzy_left, fuzzy_right) = feat.location.fuzzy_ends();
            let decorated =
                feat.location.pieces(ctx.seq_len).len() > 1 || fuzzy_left || fuzzy_right;
            if !decorated {
                painter.rect_filled(segments[0], 2.0, swatch);
            } else {
                // Intron connectors: link consecutive segments left-to-right. A
                // backward jump in x is the circular origin wrap (arms at opposite
                // ends of the linear layout), never a real intron, so it's skipped.
                let cy = bounds.center().y;
                for pair in segments.windows(2) {
                    let (a, b) = (pair[0], pair[1]);
                    if b.min.x >= a.max.x {
                        painter.line_segment(
                            [Pos2::new(a.max.x, cy), Pos2::new(b.min.x, cy)],
                            Stroke::new(1.0, swatch),
                        );
                    }
                }
                for seg in &segments {
                    painter.rect_filled(*seg, 2.0, swatch);
                }
                paint_torn_and_arrow(
                    painter,
                    feat.strand,
                    bounds,
                    swatch,
                    fuzzy_left,
                    fuzzy_right,
                );
            }
            if is_selected {
                painter.rect_stroke(
                    bounds,
                    2.0,
                    Stroke::new(1.5, Color32::WHITE),
                    egui::StrokeKind::Inside,
                );
            }
            if !feat.label.is_empty() {
                let fg = crate::config::theme::pick_contrast(
                    swatch,
                    style.label_text_light,
                    style.label_text_dark,
                );
                paint_feature_label(
                    painter,
                    &style.small_font,
                    fg,
                    style.label_overflow,
                    style.label_char_w,
                    bounds,
                    &feat.label,
                );
            }
        }
    }
}

/// Draw the torn-edge markers (a zigzag at a fuzzy `<`/`>` bound) and a strand
/// arrowhead at the feature's 3' terminus, over the clipped hull `bar`. Only
/// invoked for multi-segment / fuzzy features — a plain `Simple` feature keeps
/// its today's flat rectangle.
fn paint_torn_and_arrow(
    painter: &Painter,
    strand: Strand,
    bar: Rect,
    color: Color32,
    fuzzy_left: bool,
    fuzzy_right: bool,
) {
    if fuzzy_left {
        paint_torn_edge(painter, bar.min.x, bar, color);
    }
    if fuzzy_right {
        paint_torn_edge(painter, bar.max.x, bar, color);
    }

    // Arrowhead at the 3' end: Forward points right (3' = hull max.x), Reverse
    // left (3' = hull min.x). Mirrors the primer-track convention.
    let half = bar.height() * 0.5;
    let mid_y = bar.center().y;
    let head = half.min(bar.width());
    match strand {
        Strand::Forward => {
            let tip = Pos2::new(bar.max.x + head, mid_y);
            painter.add(egui::Shape::convex_polygon(
                vec![
                    Pos2::new(bar.max.x, mid_y - half),
                    Pos2::new(bar.max.x, mid_y + half),
                    tip,
                ],
                color,
                Stroke::NONE,
            ));
        }
        Strand::Reverse => {
            let tip = Pos2::new(bar.min.x - head, mid_y);
            painter.add(egui::Shape::convex_polygon(
                vec![
                    Pos2::new(bar.min.x, mid_y - half),
                    Pos2::new(bar.min.x, mid_y + half),
                    tip,
                ],
                color,
                Stroke::NONE,
            ));
        }
        Strand::Both | Strand::None => {}
    }
}

/// A small vertical zigzag at column `x` spanning the bar height — the ragged
/// silhouette that marks a GenBank `<`/`>` partial boundary.
fn paint_torn_edge(painter: &Painter, x: f32, bar: Rect, color: Color32) {
    let teeth = 4;
    let amp = 2.0;
    let step = bar.height() / teeth as f32;
    let mut pts = Vec::with_capacity(teeth + 1);
    for i in 0..=teeth {
        let dx = if i % 2 == 0 { -amp } else { amp };
        pts.push(Pos2::new(x + dx, bar.min.y + step * i as f32));
    }
    painter.add(egui::Shape::line(pts, Stroke::new(1.5, color)));
}
