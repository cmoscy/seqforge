//! Features track — the annotation bars below the strands (+ their labels).
//!
//! **Legacy core** in T2: it paints one flat greedy-stacked bar row band using
//! the per-block `layout.feat_rows`. T3 turns this into the composite,
//! feature-owned track (bar + per-CDS AA sub-row, variable row heights) and
//! lands the deferred editor 14e C2.

use egui::{Color32, Painter, Rect, Stroke};
use seqforge_core::FeatureKind;

use crate::viewer::track::{BlockCtx, BlockGeom, Hit, Track, annot_bar_rect, paint_feature_label};

pub(crate) struct FeaturesTrack;

impl Track for FeaturesTrack {
    fn block_height(&self, ctx: &BlockCtx) -> f32 {
        ctx.layout.n_feat_rows as f32 * ctx.style.annot_row_h
    }

    fn hit_rects(&self, ctx: &BlockCtx, geom: &BlockGeom, hits: &mut Vec<(Rect, Hit)>) {
        let style = ctx.style;
        for &(feat_idx, row) in &ctx.layout.feat_rows {
            let feat = ctx
                .render_ann
                .by_position(feat_idx)
                .expect("feat_idx from this frame's layout");
            let bar_row_y = geom.y0 + row as f32 * style.annot_row_h;
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
        }
    }

    fn paint(&self, ctx: &BlockCtx, geom: &BlockGeom, painter: &Painter) {
        let style = ctx.style;
        for &(feat_idx, row) in &ctx.layout.feat_rows {
            let feat = ctx
                .render_ann
                .by_position(feat_idx)
                .expect("feat_idx from this frame's layout");
            let bar_row_y = geom.y0 + row as f32 * style.annot_row_h;
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
