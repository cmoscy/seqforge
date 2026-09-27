//! Sequence track — the two strands plus their column-aligned **decorations**:
//! search-hit wash, selection / cursor, the realized staged-edit diff (green
//! add / red delete wash + strikethrough), and the 5'/3' end labels.
//!
//! Decorations are Sequence-track paint, not standalone stacked tracks
//! (`plans/render-tracks.md`). Still a **legacy core** paint in T2 — T4 splits
//! the decorations into their own paint helpers and memoizes layout.

use egui::{Align2, Painter, Pos2, Rect, Stroke, Vec2};

use crate::viewer::track::{
    BlockCtx, BlockGeom, Hit, Track, build_strand_galley, search_hit_color,
};

pub(crate) struct SequenceTrack;

impl Track for SequenceTrack {
    fn block_height(&self, ctx: &BlockCtx) -> f32 {
        ctx.style.strand_h * 2.0 + ctx.layout.seq_top_pad
    }

    fn hit_rects(&self, ctx: &BlockCtx, geom: &BlockGeom, hits: &mut Vec<(Rect, Hit)>) {
        // Search-hit wash is the only interactive Sequence decoration; it maps
        // to `Hit::Search`. Suppressed while staging (committed-space overlay).
        if ctx.staging {
            return;
        }
        let style = ctx.style;
        // Letter band starts below `seq_top_pad` (geom.strand_top_y).
        let top_y = geom.strand_top_y;
        for (hit_idx, hit) in ctx.search_hits.iter().enumerate() {
            // One hit rect per linear run — an origin-spanning hit is clickable on
            // either arm (`Span::linear_pieces`, the shared geometry primitive).
            for run in hit.span.linear_pieces(ctx.seq_len).iter() {
                let vis_s = run.start.max(ctx.block_start).min(ctx.block_end);
                let vis_e = run.end.min(ctx.block_end);
                if vis_s < vis_e && vis_e > ctx.block_start {
                    let sx = geom.seq_x0 + (vis_s - ctx.block_start) as f32 * style.char_width;
                    let sw = (vis_e - vis_s) as f32 * style.char_width;
                    hits.push((
                        Rect::from_min_size(
                            Pos2::new(sx, top_y),
                            Vec2::new(sw, style.strand_h * 2.0),
                        ),
                        Hit::Search(hit_idx),
                    ));
                }
            }
        }
    }

    fn paint(&self, ctx: &BlockCtx, geom: &BlockGeom, painter: &Painter) {
        let style = ctx.style;
        let seq = ctx.seq;
        let block_start = ctx.block_start;
        let block_end = ctx.block_end;
        let seq_x0 = geom.seq_x0;
        let char_width = style.char_width;
        let char_height = style.char_height;
        let strand_h = style.strand_h;
        // Letter band below seq_top_pad; mid-rail at the half-band boundary.
        let top_y = geom.strand_top_y;
        let spine_gap = (strand_h - char_height).max(0.0);
        let top_text_y = top_y;
        let mid_y = top_y + strand_h;
        let bot_text_y = mid_y + spine_gap;
        let text_color = style.text_color;

        // ── Search hit highlights (behind selection and text) ──────
        // Suppressed while staging — derived overlays are anchored to
        // committed coordinates and refresh on commit.
        if !ctx.staging {
            for hit in ctx.search_hits {
                let color = search_hit_color(ctx.theme, hit.strand);
                // Paint each linear run — both arms of an origin-spanning hit.
                for run in hit.span.linear_pieces(ctx.seq_len).iter() {
                    let vis_s = run.start.max(block_start).min(block_end);
                    let vis_e = run.end.min(block_end);
                    if vis_s < vis_e && vis_e > block_start {
                        let sx = seq_x0 + (vis_s - block_start) as f32 * char_width;
                        let sw = (vis_e - vis_s) as f32 * char_width;
                        painter.rect_filled(
                            Rect::from_min_size(
                                Pos2::new(sx, top_text_y),
                                Vec2::new(sw, char_height),
                            ),
                            2.0,
                            color,
                        );
                        painter.rect_filled(
                            Rect::from_min_size(
                                Pos2::new(sx, bot_text_y),
                                Vec2::new(sw, char_height),
                            ),
                            2.0,
                            color,
                        );
                    }
                }
            }
        }

        // ── Hover footprint wash (behind selection and text) ──────
        // A hovered primer's annealed range (single-stranded, on its own band)
        // or enzyme's recognition site (both strands), in the neutral
        // `hover_wash` grey — ephemeral, paint-time only. Drawn under the
        // selection so a real selection still dominates.
        if let Some((hs, he, strands)) = ctx.hover_footprint.filter(|_| !ctx.staging) {
            let vis_s = hs.max(block_start);
            let vis_e = he.min(block_end);
            if vis_s < vis_e {
                let sx = seq_x0 + (vis_s - block_start) as f32 * char_width;
                let sw = (vis_e - vis_s) as f32 * char_width;
                let wash = |y: f32| {
                    painter.rect_filled(
                        Rect::from_min_size(Pos2::new(sx, y), Vec2::new(sw, char_height)),
                        2.0,
                        style.hover_wash,
                    );
                };
                if strands.top() {
                    wash(top_text_y);
                }
                if strands.bottom() {
                    wash(bot_text_y);
                }
            }
        }

        // ── Selection highlight / cursor (behind text) ────────────
        // Suppressed while staging — the realized diff wash (below) is
        // the active visual; selection coords are committed-space and
        // would mislead against the speculative buffer.
        if let Some(sel) = ctx.selection.filter(|_| !ctx.staging) {
            if sel.is_cursor() {
                // Thin vertical line between bases spanning both strands.
                let pos = sel.anchor;
                if ctx.blink_on && pos >= block_start && pos <= block_end {
                    let cx = seq_x0 + (pos - block_start) as f32 * char_width;
                    painter.rect_filled(
                        Rect::from_min_size(
                            Pos2::new(cx - 0.75, top_y),
                            Vec2::new(1.5, strand_h * 2.0),
                        ),
                        0.0,
                        style.cursor_color,
                    );
                }
            } else {
                // Wrap-aware: a selection that crosses the origin paints as its
                // two linear arms (`Span::linear_pieces`), each clipped to this
                // block — the same geometry primitive features render from.
                for run in sel.to_span(ctx.seq_len).linear_pieces(ctx.seq_len).iter() {
                    let vis_s = run.start.max(block_start);
                    let vis_e = run.end.min(block_end);
                    if vis_s < vis_e {
                        let sx = seq_x0 + (vis_s - block_start) as f32 * char_width;
                        let sw = (vis_e - vis_s) as f32 * char_width;
                        painter.rect_filled(
                            Rect::from_min_size(
                                Pos2::new(sx, top_text_y),
                                Vec2::new(sw, char_height),
                            ),
                            0.0,
                            style.selection_color,
                        );
                        painter.rect_filled(
                            Rect::from_min_size(
                                Pos2::new(sx, bot_text_y),
                                Vec2::new(sw, char_height),
                            ),
                            0.0,
                            style.selection_color.gamma_multiply(0.7),
                        );
                    }
                }
            }
        }

        // ── Realized diff wash (Phase 13.6) ───────────────────────
        // Drawn over the *preview* bytes, behind the strand glyphs so the
        // per-base A/C/G/T colours stay legible. `added`/`deleted` are
        // render-space column ranges on the preview.
        if let Some((rs, re)) = ctx.added {
            let vis_s = rs.max(block_start);
            let vis_e = re.min(block_end);
            if vis_s < vis_e {
                let sx = seq_x0 + (vis_s - block_start) as f32 * char_width;
                let sw = (vis_e - vis_s) as f32 * char_width;
                painter.rect_filled(
                    Rect::from_min_size(Pos2::new(sx, top_y), Vec2::new(sw, strand_h * 2.0)),
                    0.0,
                    style.diff_add_bg,
                );
            }
        }
        if let Some((rs, re)) = ctx.deleted {
            let vis_s = rs.max(block_start);
            let vis_e = re.min(block_end);
            if vis_s < vis_e {
                let sx = seq_x0 + (vis_s - block_start) as f32 * char_width;
                let sw = (vis_e - vis_s) as f32 * char_width;
                painter.rect_filled(
                    Rect::from_min_size(Pos2::new(sx, top_y), Vec2::new(sw, strand_h * 2.0)),
                    0.0,
                    style.diff_del_bg,
                );
            }
        }

        // ── 5'/3' labels (first block only) ───────────────────────
        if ctx.block_idx == 0 {
            painter.text(
                Pos2::new(geom.rect_min_x, top_text_y),
                Align2::LEFT_TOP,
                "5'",
                style.font_id.clone(),
                text_color.gamma_multiply(0.45),
            );
            painter.text(
                Pos2::new(geom.rect_min_x, bot_text_y),
                Align2::LEFT_TOP,
                "3'",
                style.font_id.clone(),
                text_color.gamma_multiply(0.45),
            );
        }

        // ── Mid-strand rail (spine + per-base / decade ticks) ─────
        // SnapGene-style column guide between forward and reverse.
        // Behind glyphs; cut staples / cursor remain the stronger verticals.
        let block_len = block_end.saturating_sub(block_start);
        if block_len > 0 {
            let rail = Stroke::new(1.25, text_color.gamma_multiply(0.30));
            let spine_x1 = seq_x0 + block_len as f32 * char_width;
            painter.line_segment([Pos2::new(seq_x0, mid_y), Pos2::new(spine_x1, mid_y)], rail);
            for col in 0..block_len {
                let pos = block_start + col + 1; // 1-based
                let half = if pos % 10 == 0 {
                    4.5
                } else if pos % 5 == 0 {
                    3.0
                } else {
                    2.0
                };
                let cx = seq_x0 + col as f32 * char_width + char_width * 0.5;
                painter.line_segment(
                    [Pos2::new(cx, mid_y - half), Pos2::new(cx, mid_y + half)],
                    rail,
                );
            }
        }

        // ── Strands ───────────────────────────────────────────────
        let top_galley = build_strand_galley(
            painter,
            &seq[block_start..block_end],
            &style.font_id,
            1.0,
            ctx.theme,
        );
        painter.galley(Pos2::new(seq_x0, top_text_y), top_galley, text_color);

        // Bottom strand is the complement of the visible block, derived on
        // demand — never stored on the buffer.
        let block_comp = seqforge_bio::complement(&seq[block_start..block_end]);
        let bot_galley = build_strand_galley(painter, &block_comp, &style.font_id, 0.65, ctx.theme);
        painter.galley(Pos2::new(seq_x0, bot_text_y), bot_galley, text_color);

        // ── Delete strikethrough (Phase 13.6b) ────────────────────
        // Deleted bases are kept visible (verify-what's-leaving) with a
        // strikethrough struck through both strands, drawn *over* the glyphs.
        if let Some((rs, re)) = ctx.deleted {
            let vis_s = rs.max(block_start);
            let vis_e = re.min(block_end);
            if vis_s < vis_e {
                let sx = seq_x0 + (vis_s - block_start) as f32 * char_width;
                let ex = seq_x0 + (vis_e - block_start) as f32 * char_width;
                let stroke = Stroke::new(1.5, style.diff_del_line);
                for strand_top in [top_text_y, bot_text_y] {
                    let my = strand_top + char_height * 0.5;
                    painter.line_segment([Pos2::new(sx, my), Pos2::new(ex, my)], stroke);
                }
            }
        }
    }
}
