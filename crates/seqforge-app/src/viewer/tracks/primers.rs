//! Primer tracks — directional half-arrows for authored primers (Phase 0.4;
//! `plans/primers.md` "Rendering"). Two position-owned bands straddle the
//! sequence: **forward** primers above the top strand, **reverse** below the
//! bottom strand (the SnapGene/Benchling idiom). Each arrow is column-aligned to
//! its annealed footprint with the **arrowhead at the 3' end** (extension
//! direction); a 5' tail (oligo bases beyond the footprint) peels **off** the
//! grid, lighter, since it has no template column.
//!
//! Read-only for 0.4: every attached primer draws as a plain arrow. Mismatch
//! marks / drifted-vs-detached state come with annealing (Phase 1.1). A detached
//! primer (`binding = None`) draws nowhere — it is panel-only (Phase 1.3).
//!
//! Paint and hit-test share one geometry (`primer_body_rect`) — the co-location
//! invariant the Track abstraction exists to guarantee.

use egui::{Color32, Painter, Pos2, Rect, Stroke};

use crate::viewer::track::{BlockCtx, BlockGeom, Hit, Track, primer_body_rect};

/// Forward-primer band (above the top strand); arrowhead points 3'→right.
pub(crate) struct PrimerForwardTrack;
/// Reverse-primer band (below the bottom strand); arrowhead points 3'→left.
pub(crate) struct PrimerReverseTrack;

impl Track for PrimerForwardTrack {
    fn block_height(&self, ctx: &BlockCtx) -> f32 {
        ctx.layout.primer_fwd_band_h
    }
    fn paint(&self, ctx: &BlockCtx, geom: &BlockGeom, painter: &Painter) {
        paint_band(ctx, geom, painter, &ctx.layout.primer_fwd_rows, false);
    }
    fn hit_rects(&self, ctx: &BlockCtx, geom: &BlockGeom, hits: &mut Vec<(Rect, Hit)>) {
        hit_band(ctx, geom, &ctx.layout.primer_fwd_rows, hits);
    }
}

impl Track for PrimerReverseTrack {
    fn block_height(&self, ctx: &BlockCtx) -> f32 {
        ctx.layout.primer_rev_band_h
    }
    fn paint(&self, ctx: &BlockCtx, geom: &BlockGeom, painter: &Painter) {
        paint_band(ctx, geom, painter, &ctx.layout.primer_rev_rows, true);
    }
    fn hit_rects(&self, ctx: &BlockCtx, geom: &BlockGeom, hits: &mut Vec<(Rect, Hit)>) {
        hit_band(ctx, geom, &ctx.layout.primer_rev_rows, hits);
    }
}

/// Strip a strand colour's alpha so an arrow reads solid over the strand wash.
fn opaque(c: Color32) -> Color32 {
    Color32::from_rgb(c.r(), c.g(), c.b())
}

/// Emit `Hit::Primer(id)` across each primer's footprint body — the same rect
/// `paint_band` fills.
fn hit_band(
    ctx: &BlockCtx,
    geom: &BlockGeom,
    rows: &[(usize, usize)],
    hits: &mut Vec<(Rect, Hit)>,
) {
    let style = ctx.style;
    for &(primer_idx, row) in rows {
        let Some(primer) = ctx.render_ann.primer_by_position(primer_idx) else {
            continue;
        };
        let Some(binding) = &primer.binding else {
            continue;
        };
        let row_y = geom.y0 + row as f32 * style.primer_row_h;
        if let Some(rect) = primer_body_rect(
            binding,
            ctx.block_start,
            ctx.block_end,
            row_y,
            geom.seq_x0,
            style.char_width,
            style.primer_row_h,
        ) {
            hits.push((rect, Hit::Primer(primer.id)));
        }
    }
}

fn paint_band(
    ctx: &BlockCtx,
    geom: &BlockGeom,
    painter: &Painter,
    rows: &[(usize, usize)],
    reverse: bool,
) {
    let style = ctx.style;
    let char_width = style.char_width;
    let block_start = ctx.block_start;
    let block_end = ctx.block_end;
    let base = if reverse {
        ctx.theme.strand.reverse.0
    } else {
        ctx.theme.strand.forward.0
    };
    let body_color = opaque(base);
    // Faint wash inside the outlined body — enough to read as a shape without the
    // heavy opaque bar; the annealed bases show on the adjacent sequence track.
    let fill_color = base.gamma_multiply(0.4);
    let tail_color = base.gamma_multiply(0.6);

    for &(primer_idx, row) in rows {
        let Some(primer) = ctx.render_ann.primer_by_position(primer_idx) else {
            continue;
        };
        let Some(binding) = &primer.binding else {
            continue;
        };
        let row_y = geom.y0 + row as f32 * style.primer_row_h;
        let Some(body) = primer_body_rect(
            binding,
            block_start,
            block_end,
            row_y,
            geom.seq_x0,
            char_width,
            style.primer_row_h,
        ) else {
            continue;
        };

        // Body: outlined arrow aligned to the annealed footprint (SnapGene /
        // Benchling idiom) — a faint fill + solid outline, not an opaque bar, so
        // it frames the corresponding bases on the adjacent sequence row rather
        // than hiding them.
        painter.rect_filled(body, 2.0, fill_color);
        painter.rect_stroke(
            body,
            2.0,
            Stroke::new(1.5, body_color),
            egui::StrokeKind::Inside,
        );

        let mid_y = body.center().y;
        let head_len = (char_width * 0.8).clamp(4.0, 10.0);
        let head_half = (body.height() * 0.55 + 2.0).min(style.primer_row_h * 0.5);

        // Arrowhead at the 3' terminus, if it falls in this block. Forward's 3'
        // is `binding.end` (right edge); reverse's is `binding.start` (left).
        if reverse {
            if binding.start >= block_start {
                let tip_x = body.min.x - head_len;
                filled_triangle(
                    painter,
                    Pos2::new(body.min.x, mid_y - head_half),
                    Pos2::new(body.min.x, mid_y + head_half),
                    Pos2::new(tip_x, mid_y),
                    body_color,
                );
            }
        } else if binding.end <= block_end {
            let tip_x = body.max.x + head_len;
            filled_triangle(
                painter,
                Pos2::new(body.max.x, mid_y - head_half),
                Pos2::new(body.max.x, mid_y + head_half),
                Pos2::new(tip_x, mid_y),
                body_color,
            );
        }

        // 5' tail: oligo bases beyond the annealed footprint have no template
        // column, so the ribbon peels off the grid (rises away from the strand).
        // Forward's 5' is `binding.start`; reverse's is `binding.end`.
        let oligo_len = primer.sequence.chars().count();
        let footprint = binding.end.saturating_sub(binding.start);
        let tail_len = oligo_len.saturating_sub(footprint);
        if tail_len > 0 {
            let tail_px = (tail_len as f32 * char_width).min(char_width * 6.0);
            // Lift up for the forward band (away from the strand below), down for
            // the reverse band (away from the strand above).
            let lift = style.primer_row_h * 0.45 * if reverse { 1.0 } else { -1.0 };
            let lift_y = if reverse { body.max.y } else { body.min.y } + lift;
            let stroke = Stroke::new(2.0, tail_color);
            if reverse {
                if binding.end <= block_end {
                    // peel to the right of the 5' end, kinking off the grid
                    let x = body.max.x;
                    painter.line_segment(
                        [Pos2::new(x, mid_y), Pos2::new(x + head_len, lift_y)],
                        stroke,
                    );
                    painter.line_segment(
                        [
                            Pos2::new(x + head_len, lift_y),
                            Pos2::new(x + tail_px, lift_y),
                        ],
                        stroke,
                    );
                }
            } else if binding.start >= block_start {
                let x = body.min.x;
                painter.line_segment(
                    [Pos2::new(x, mid_y), Pos2::new(x - head_len, lift_y)],
                    stroke,
                );
                painter.line_segment(
                    [
                        Pos2::new(x - head_len, lift_y),
                        Pos2::new(x - tail_px, lift_y),
                    ],
                    stroke,
                );
            }
        }
    }
}

fn filled_triangle(painter: &Painter, a: Pos2, b: Pos2, c: Pos2, color: Color32) {
    painter.add(egui::Shape::convex_polygon(
        vec![a, b, c],
        color,
        Stroke::NONE,
    ));
}
