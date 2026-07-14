//! Translation track — the in-canvas **global reading-frame** band that hugs the
//! sequence (between the bottom strand and the annotation bars).
//!
//! Position-owned: global frame lanes are a frameless whole-sequence scan, so a
//! reading frame must be chosen (there's no feature to anchor to). Per-feature
//! CDS translations are *not* here — they ride under their own bar in the
//! composite Features track (T3 / editor 14e C2).

use egui::{Align2, Painter, Pos2, Rect, Vec2};

use crate::viewer::track::{BlockCtx, BlockGeom, Hit, Track, aa_codon_hits, paint_aa_lane};
use crate::viewer::translation::OrfPromote;

pub(crate) struct TranslationTrack;

impl Track for TranslationTrack {
    fn block_height(&self, ctx: &BlockCtx) -> f32 {
        ctx.trans_cache.map_or(0, |c| c.frame_band_rows()) as f32 * ctx.style.aa_row_h
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

        for (lane_i, lane) in tc.frame_lanes.iter().enumerate() {
            let lane_y = trans_y + lane_i as f32 * aa_row_h;
            // ORF-run click targets (each visible segment maps to the full ORF
            // for "Annotate as CDS").
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
            // Codon-cell click targets.
            aa_codon_hits(
                style,
                block_start,
                block_end,
                ctx.seq_len,
                seq_x0,
                lane_y,
                &lane.glyphs,
                hits,
            );
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
        let show_orfs = ctx.show_orfs;
        // Ordered selection range (suppressed while staging / at a bare cursor),
        // washed behind any residue whose codon overlaps it.
        let sel = ctx
            .selection
            .filter(|_| !ctx.staging)
            .filter(|s| !s.is_cursor())
            .map(|s| s.ordered());

        for (lane_i, lane) in tc.frame_lanes.iter().enumerate() {
            let lane_y = trans_y + lane_i as f32 * aa_row_h;
            // ORF wash behind the lane.
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
                    style.text_color.gamma_multiply(0.5),
                );
            }
            // Codon outlines + residue glyphs.
            paint_aa_lane(
                painter,
                style,
                block_start,
                block_end,
                seq_x0,
                lane_y,
                &lane.glyphs,
                show_orfs,
                sel,
            );
        }
    }
}
