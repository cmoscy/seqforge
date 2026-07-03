//! Primer tracks — directional arrows for authored primers (Phase 0.4 render +
//! Phase 1.1 decomposition; `plans/primers.md` "Rendering"). Two position-owned
//! bands straddle the sequence: **forward** primers above the top strand,
//! **reverse** below the bottom strand (the SnapGene/Benchling idiom). Each arrow
//! is an outlined body column-aligned to the annealed footprint, arrowhead at the
//! **3' end**, showing the oligo's **bases** per column — matches in the base
//! palette, **mismatches** on an amber cell (the `Drifted` cue). The 5' tail
//! (oligo bases beyond the footprint) peels **off** the grid as lifted letters,
//! long tails capped with a `+N` stub.
//!
//! The per-primer alignment (annealed / mismatch / tail, strand-correct) comes
//! from `seqforge_bio::decompose_primer`, carried in `BlockCtx::primer_decomps`.
//! A detached primer (`binding = None`) draws nowhere — panel-only (Phase 1.3);
//! full re-anneal drift detection (binding *moved*) is still Phase 1.1's find pass.
//!
//! Paint and hit-test share one geometry (`primer_body_rect`) — the co-location
//! invariant the Track abstraction exists to guarantee.

use egui::{Align2, Color32, Painter, Pos2, Rect, Stroke, Vec2};

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
        // the primer's own bases (drawn below) read clearly against it.
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

        // Annealed bases (Phase 1.1 decomposition): the oligo base at each
        // template column, column-aligned to the sequence row it abuts. Matches
        // read in the base palette; a **mismatch** gets an amber cell + amber
        // glyph (the `Drifted` cue). Reverse orientation/tail were resolved in
        // `decompose_primer`, so this loop is strand-agnostic.
        let decomp = ctx.primer_decomps.get(primer_idx);
        if let Some(decomp) = decomp {
            for ab in &decomp.annealed {
                if ab.column < block_start || ab.column >= block_end {
                    continue;
                }
                let cx = geom.seq_x0 + (ab.column - block_start) as f32 * char_width;
                let color = if ab.matches {
                    ctx.theme.bases.for_base(ab.base)
                } else {
                    let cell = Rect::from_min_size(
                        Pos2::new(cx, body.min.y),
                        Vec2::new(char_width, body.height()),
                    );
                    painter.rect_filled(cell, 0.0, style.primer_mismatch.gamma_multiply(0.55));
                    style.primer_mismatch
                };
                painter.text(
                    Pos2::new(cx + char_width * 0.5, mid_y),
                    Align2::CENTER_CENTER,
                    (ab.base as char).to_string(),
                    style.font_id.clone(),
                    color,
                );
            }
        }

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

        // 5' tail: oligo bases with no template column peel off the grid as
        // lifted letters (away from the strand), lighter. Drawn only in the block
        // holding the 5' end. Long tails are capped with a "+N" stub.
        let tail = decomp.map(|d| d.tail.as_slice()).unwrap_or(&[]);
        let five_prime_in_block = if reverse {
            binding.end <= block_end
        } else {
            binding.start >= block_start
        };
        if !tail.is_empty() && five_prime_in_block {
            let cap = 8usize;
            let shown = tail.len().min(cap);
            let lift = style.primer_row_h * 0.5 * if reverse { 1.0 } else { -1.0 };
            let lift_y = if reverse { body.max.y } else { body.min.y } + lift;
            let edge_x = if reverse { body.max.x } else { body.min.x };
            let dir = if reverse { 1.0 } else { -1.0 };
            // Kink connecting the body's 5' corner up to the lifted ribbon.
            painter.line_segment(
                [
                    Pos2::new(edge_x, mid_y),
                    Pos2::new(edge_x + dir * char_width * 0.5, lift_y),
                ],
                Stroke::new(1.5, tail_color),
            );
            // Tail bases nearest the annealed junction first (3'→5' of the tail).
            for k in 0..shown {
                let base = tail[tail.len() - 1 - k];
                let cx = edge_x + dir * (k as f32 + 0.5) * char_width;
                painter.text(
                    Pos2::new(cx, lift_y),
                    Align2::CENTER_CENTER,
                    (base as char).to_string(),
                    style.font_id.clone(),
                    tail_color,
                );
            }
            if tail.len() > cap {
                let cx = edge_x + dir * (shown as f32 + 0.5) * char_width;
                painter.text(
                    Pos2::new(cx, lift_y),
                    Align2::CENTER_CENTER,
                    format!("+{}", tail.len() - cap),
                    style.small_font.clone(),
                    tail_color,
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
