//! Cut-sites track — restriction-enzyme labels stacked above the ruler, each
//! with a short tick; on hover the full SnapGene-style staple (descender +
//! overhang step + wedge arrows) reveals, descending across the strand rows.
//!
//! Position-owned, but a **connector**: the hover staple reaches down into the
//! Sequence track's strand rows via `geom.strand_top_y` / `strand_bot_y`. It is
//! painted last in the stack's z-order so the staple lands on top of the
//! strands it crosses.

use egui::{Align2, Painter, Pos2, Rect, Stroke, Vec2};

use crate::viewer::track::{BlockCtx, BlockGeom, Hit, Track, paint_wedge_down, paint_wedge_up};

pub(crate) struct CutSitesTrack;

impl Track for CutSitesTrack {
    fn block_height(&self, ctx: &BlockCtx) -> f32 {
        ctx.layout.n_cut_rows as f32 * ctx.style.cut_label_row_h
    }

    fn hit_rects(&self, ctx: &BlockCtx, geom: &BlockGeom, hits: &mut Vec<(Rect, Hit)>) {
        // Click target is the label only — not the staple line through the
        // strands. (`cut_sites` is empty while staging, so this is inert then.)
        let style = ctx.style;
        for &(site_idx, row) in &ctx.layout.cut_rows {
            let site = &ctx.cut_sites[site_idx];
            let cx = geom.seq_x0 + (site.cut_pos - ctx.block_start) as f32 * style.char_width;
            let label_w = site.enzyme.len() as f32 * style.label_char_w + 8.0;
            hits.push((
                Rect::from_center_size(
                    Pos2::new(cx, geom.y0 + (row as f32 + 0.5) * style.cut_label_row_h),
                    Vec2::new(label_w, style.cut_label_row_h),
                ),
                Hit::CutSite(site_idx),
            ));
        }
    }

    fn paint(&self, ctx: &BlockCtx, geom: &BlockGeom, painter: &Painter) {
        let style = ctx.style;
        let char_width = style.char_width;
        let cut_label_row_h = style.cut_label_row_h;
        let block_start = ctx.block_start;
        let block_end = ctx.block_end;
        let seq_x0 = geom.seq_x0;
        let block_y = geom.y0;
        let cut_site_color = style.cut_site_color;
        // Bottom of this block's cut-label band == top of the ruler.
        let ruler_y = block_y + ctx.layout.n_cut_rows as f32 * cut_label_row_h;

        // ── Resting state: label + short tick descending toward the ruler ──
        for &(site_idx, row) in &ctx.layout.cut_rows {
            let site = &ctx.cut_sites[site_idx];
            let top_cut = site.cut_pos;
            let tcx = seq_x0 + (top_cut - block_start) as f32 * char_width;
            let label_y = block_y + row as f32 * cut_label_row_h;
            painter.text(
                Pos2::new(tcx, label_y),
                Align2::CENTER_TOP,
                &site.enzyme,
                style.small_font.clone(),
                cut_site_color,
            );
            let tick_top = block_y + (row + 1) as f32 * cut_label_row_h;
            let tick_bot = (tick_top + cut_label_row_h * 0.6).min(ruler_y);
            painter.line_segment(
                [Pos2::new(tcx, tick_top), Pos2::new(tcx, tick_bot)],
                Stroke::new(1.0, cut_site_color),
            );
        }

        // ── Hover-reveal: full staple + wedges for the hovered site ──
        // Only the topmost hovered site (resolved once, globally) promotes; it
        // renders in whichever block holds its `cut_pos`.
        let Some(idx) = ctx.hovered_cut_site else {
            return;
        };
        let row_in_block = ctx
            .layout
            .cut_rows
            .iter()
            .find(|(i, _)| *i == idx)
            .map(|(_, r)| *r);
        let site = &ctx.cut_sites[idx];
        let top_cut = site.cut_pos;
        let bot_cut = site.bottom_cut_pos;
        let Some(row) = row_in_block.filter(|_| top_cut >= block_start && top_cut <= block_end)
        else {
            return;
        };

        let strand_h = style.strand_h;
        let bot_y = geom.strand_bot_y;
        let top_y = geom.strand_top_y;
        let tcx = seq_x0 + (top_cut - block_start) as f32 * char_width;
        let stroke = Stroke::new(1.5, cut_site_color);
        let line_top = block_y + (row + 1) as f32 * cut_label_row_h;

        // Descender from label row through the top strand.
        painter.line_segment([Pos2::new(tcx, line_top), Pos2::new(tcx, bot_y)], stroke);

        // Strand connector + overhang step.
        if top_cut == bot_cut {
            painter.line_segment(
                [Pos2::new(tcx, bot_y), Pos2::new(tcx, bot_y + strand_h)],
                stroke,
            );
        } else if bot_cut >= block_start && bot_cut <= block_end {
            let bcx = seq_x0 + (bot_cut - block_start) as f32 * char_width;
            painter.line_segment([Pos2::new(tcx, bot_y), Pos2::new(bcx, bot_y)], stroke);
            painter.line_segment(
                [Pos2::new(bcx, bot_y), Pos2::new(bcx, bot_y + strand_h)],
                stroke,
            );
        } else {
            // Bottom cut in another block — stub the top half.
            painter.line_segment(
                [
                    Pos2::new(tcx, bot_y),
                    Pos2::new(tcx, bot_y + strand_h * 0.5),
                ],
                Stroke::new(1.5, cut_site_color.gamma_multiply(0.4)),
            );
        }

        // ── Wedge arrows ─────────────────────────────────
        // Two small filled triangles marking the scissile bond on each strand:
        // top wedge points down into the top-strand cut; bottom wedge points up.
        let bcx_in_block = bot_cut >= block_start && bot_cut <= block_end;
        let bcx = bcx_in_block.then(|| seq_x0 + (bot_cut - block_start) as f32 * char_width);
        let wedge_half = (char_width * 0.45).clamp(2.5, 5.0);
        let wedge_h = (strand_h * 0.55).clamp(3.0, 7.0);
        paint_wedge_down(painter, tcx, top_y, wedge_half, wedge_h, cut_site_color);
        if let Some(bcx) = bcx {
            paint_wedge_up(
                painter,
                bcx,
                bot_y + strand_h,
                wedge_half,
                wedge_h,
                cut_site_color,
            );
        }
    }
}
