//! Ruler track — the base-position numbers above each block's strands.
//! Position-owned, no interaction.

use egui::{Align2, Painter, Pos2};

use crate::viewer::track::{BlockCtx, BlockGeom, Track};

pub(crate) struct RulerTrack;

impl Track for RulerTrack {
    fn block_height(&self, ctx: &BlockCtx) -> f32 {
        ctx.style.ruler_h
    }

    fn paint(&self, ctx: &BlockCtx, geom: &BlockGeom, painter: &Painter) {
        let style = ctx.style;
        let block_len = ctx.block_end - ctx.block_start;
        for col in 0..block_len {
            let abs = ctx.block_start + col;
            if abs == 0 || (abs + 1) % 10 == 0 {
                painter.text(
                    Pos2::new(geom.seq_x0 + col as f32 * style.char_width, geom.y0),
                    Align2::LEFT_TOP,
                    format!("{}", abs + 1),
                    style.ruler_font.clone(),
                    style.ruler_text,
                );
            }
        }
    }
}
