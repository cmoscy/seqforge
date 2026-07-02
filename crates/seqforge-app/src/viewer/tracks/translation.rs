//! Translation track — the in-canvas reading-frame / CDS amino-acid band that
//! hugs the sequence (between the bottom strand and the annotation bars).
//!
//! Position-owned: global frame lanes are a frameless whole-sequence scan. In
//! T2 the track paints the whole memoized band (frame lanes + feature lanes)
//! from [`TranslationCache`](crate::viewer::translation::TranslationCache); T3
//! moves the per-CDS `feature_lanes` out to the Features track, leaving the
//! global frame lanes here.

use egui::{Align2, Painter, Pos2, Rect, Stroke, Vec2};

use crate::viewer::track::{BlockCtx, BlockGeom, Hit, Track};
use crate::viewer::translation::{AaKind, OrfPromote};

pub(crate) struct TranslationTrack;

impl Track for TranslationTrack {
    fn block_height(&self, ctx: &BlockCtx) -> f32 {
        ctx.trans_cache.map_or(0, |c| c.band_rows()) as f32 * ctx.style.aa_row_h
    }

    fn hit_rects(&self, ctx: &BlockCtx, geom: &BlockGeom, hits: &mut Vec<(Rect, Hit)>) {
        let Some(tc) = ctx.trans_cache else {
            return;
        };
        let style = ctx.style;
        let aa_row_h = style.aa_row_h;
        let char_width = style.char_width;
        let block_start = ctx.block_start;
        let block_end = ctx.block_end;
        let seq_x0 = geom.seq_x0;
        let trans_y = geom.y0;

        // ORF-run click targets (frame lanes only; each visible segment maps to
        // the full ORF for "Annotate as CDS").
        for (lane_i, lane) in tc.frame_lanes.iter().enumerate() {
            let lane_y = trans_y + lane_i as f32 * aa_row_h;
            for &(rs, re) in &lane.orf_runs {
                let vis_s = rs.max(block_start);
                let vis_e = re.min(block_end);
                if vis_s < vis_e {
                    let sx = seq_x0 + (vis_s - block_start) as f32 * char_width;
                    let sw = (vis_e - vis_s) as f32 * char_width;
                    hits.push((
                        Rect::from_min_size(Pos2::new(sx, lane_y), Vec2::new(sw, aa_row_h)),
                        Hit::Orf(OrfPromote {
                            start: rs,
                            end: re,
                            strand: lane.strand,
                        }),
                    ));
                }
            }
        }
        // Codon-cell click targets across all lanes (frame + feature).
        for (lane_i, lane) in tc.lanes().enumerate() {
            let lane_y = trans_y + lane_i as f32 * aa_row_h;
            for g in &lane.glyphs {
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
                        Hit::Codon(g.pos.saturating_sub(1)..(g.pos + 2).min(ctx.seq_len)),
                    ));
                }
            }
        }
    }

    fn paint(&self, ctx: &BlockCtx, geom: &BlockGeom, painter: &Painter) {
        let Some(tc) = ctx.trans_cache else {
            return;
        };
        let style = ctx.style;
        let aa_row_h = style.aa_row_h;
        let char_width = style.char_width;
        let block_start = ctx.block_start;
        let block_end = ctx.block_end;
        let seq_x0 = geom.seq_x0;
        let trans_y = geom.y0;
        let text_color = style.text_color;
        let aa_normal = text_color.gamma_multiply(0.72);
        let show_orfs = ctx.show_orfs;

        for (lane_i, lane) in tc.lanes().enumerate() {
            let lane_y = trans_y + lane_i as f32 * aa_row_h;
            // ORF wash behind the lane (frame lanes only).
            if show_orfs {
                for &(rs, re) in &lane.orf_runs {
                    let vis_s = rs.max(block_start);
                    let vis_e = re.min(block_end);
                    if vis_s < vis_e {
                        let sx = seq_x0 + (vis_s - block_start) as f32 * char_width;
                        let sw = (vis_e - vis_s) as f32 * char_width;
                        painter.rect_filled(
                            Rect::from_min_size(Pos2::new(sx, lane_y), Vec2::new(sw, aa_row_h)),
                            0.0,
                            style.orf_wash,
                        );
                    }
                }
            }
            // Lane label in the left margin (first block only).
            if ctx.block_idx == 0 {
                painter.text(
                    Pos2::new(geom.rect_min_x, lane_y),
                    Align2::LEFT_TOP,
                    &lane.label,
                    style.small_font.clone(),
                    text_color.gamma_multiply(0.5),
                );
            }
            // Amino-acid glyphs whose codon midpoint falls in this block.
            for g in &lane.glyphs {
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
                // Codon cell spanning the residue's 3 nucleotides (clamped to
                // the block at a wrap). The faint outline groups the codon and
                // marks the click target (hit-rect emitted in `hit_rects`).
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
    }
}
