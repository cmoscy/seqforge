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

use egui::{Color32, Painter, Rect, Stroke};
use seqforge_core::FeatureKind;

use crate::viewer::track::{
    BlockCtx, BlockGeom, Hit, Track, aa_codon_hits, annot_bar_rect, paint_aa_lane,
    paint_feature_label,
};

pub(crate) struct FeaturesTrack;

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
            if let Some(r) = annot_bar_rect(
                feat,
                ctx.block_start,
                ctx.block_end,
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
                );
            }

            let Some(bar) = annot_bar_rect(
                feat,
                ctx.block_start,
                ctx.block_end,
                bar_row_y,
                geom.seq_x0,
                style.char_width,
                style.annot_row_h,
            ) else {
                continue;
            };
            // Resolve the selected *id* to this frame's position for the
            // highlight — position never leaves the frame.
            let is_selected = ctx.selected_feature == Some(feat.id);
            let swatch = ctx
                .theme
                .feature_color(FeatureKind::classify(&feat.raw_kind));
            painter.rect_filled(bar, 2.0, swatch);
            if is_selected {
                painter.rect_stroke(
                    bar,
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
                    bar,
                    &feat.label,
                );
            }
        }
    }
}
