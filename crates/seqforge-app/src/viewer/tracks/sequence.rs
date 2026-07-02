//! Sequence track — the two strands plus their column-aligned **decorations**:
//! search-hit wash, selection / cursor, the realized staged-edit diff (green
//! add / red delete wash + strikethrough), and the 5'/3' end labels.
//!
//! Decorations are Sequence-track paint, not standalone stacked tracks
//! (`plans/render-tracks.md`). Still a **legacy core** paint in T2 — T4 splits
//! the decorations into their own paint helpers and memoizes layout.

use egui::{Align2, Color32, Painter, Pos2, Rect, Stroke, Vec2};

use crate::viewer::track::{
    BlockCtx, BlockGeom, Hit, Track, build_strand_galley, search_hit_color,
};

/// Track-changes diff washes for the realized staged-edit preview (Phase 13.6).
/// A faint background channel painted *behind* the per-base glyphs so the
/// A/C/G/T foreground colours stay legible — the diff never recolours the bases.
const DIFF_ADD_BG: Color32 = Color32::from_rgba_premultiplied(40, 120, 75, 70); // added
const DIFF_DEL_BG: Color32 = Color32::from_rgba_premultiplied(120, 45, 45, 70); // removed
/// Strikethrough line struck through kept-but-deleted bases (drawn over the
/// glyphs, so opaque rather than a wash).
const DIFF_DEL_LINE: Color32 = Color32::from_rgb(214, 92, 92);

pub(crate) struct SequenceTrack;

impl Track for SequenceTrack {
    fn block_height(&self, ctx: &BlockCtx) -> f32 {
        ctx.style.strand_h * 2.0
    }

    fn hit_rects(&self, ctx: &BlockCtx, geom: &BlockGeom, hits: &mut Vec<(Rect, Hit)>) {
        // Search-hit wash is the only interactive Sequence decoration; it maps
        // to `Hit::Search`. Suppressed while staging (committed-space overlay).
        if ctx.staging {
            return;
        }
        let style = ctx.style;
        let top_y = geom.y0;
        for (hit_idx, hit) in ctx.search_hits.iter().enumerate() {
            let vis_s = hit.start.max(ctx.block_start).min(ctx.block_end);
            let vis_e = hit.end.min(ctx.block_end);
            if vis_s < vis_e && vis_e > ctx.block_start {
                let sx = geom.seq_x0 + (vis_s - ctx.block_start) as f32 * style.char_width;
                let sw = (vis_e - vis_s) as f32 * style.char_width;
                hits.push((
                    Rect::from_min_size(Pos2::new(sx, top_y), Vec2::new(sw, style.strand_h * 2.0)),
                    Hit::Search(hit_idx),
                ));
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
        let top_y = geom.y0;
        let bot_y = top_y + strand_h;
        let text_color = style.text_color;

        // ── Search hit highlights (behind selection and text) ──────
        // Suppressed while staging — derived overlays are anchored to
        // committed coordinates and refresh on commit.
        if !ctx.staging {
            for hit in ctx.search_hits {
                let vis_s = hit.start.max(block_start).min(block_end);
                let vis_e = hit.end.min(block_end); // clamp wrap-arounds
                if vis_s < vis_e && vis_e > block_start {
                    let sx = seq_x0 + (vis_s - block_start) as f32 * char_width;
                    let sw = (vis_e - vis_s) as f32 * char_width;
                    let color = search_hit_color(ctx.theme, hit.strand);
                    painter.rect_filled(
                        Rect::from_min_size(Pos2::new(sx, top_y), Vec2::new(sw, char_height)),
                        2.0,
                        color,
                    );
                    painter.rect_filled(
                        Rect::from_min_size(Pos2::new(sx, bot_y), Vec2::new(sw, char_height)),
                        2.0,
                        color,
                    );
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
                let (sel_s, sel_e) = sel.ordered();
                let vis_s = sel_s.max(block_start);
                let vis_e = sel_e.min(block_end);
                if vis_s < vis_e {
                    let sx = seq_x0 + (vis_s - block_start) as f32 * char_width;
                    let sw = (vis_e - vis_s) as f32 * char_width;
                    painter.rect_filled(
                        Rect::from_min_size(Pos2::new(sx, top_y), Vec2::new(sw, char_height)),
                        0.0,
                        style.selection_color,
                    );
                    painter.rect_filled(
                        Rect::from_min_size(Pos2::new(sx, bot_y), Vec2::new(sw, char_height)),
                        0.0,
                        style.selection_color.gamma_multiply(0.7),
                    );
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
                    DIFF_ADD_BG,
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
                    DIFF_DEL_BG,
                );
            }
        }

        // ── 5'/3' labels (first block only) ───────────────────────
        if ctx.block_idx == 0 {
            painter.text(
                Pos2::new(geom.rect_min_x, top_y),
                Align2::LEFT_TOP,
                "5'",
                style.font_id.clone(),
                text_color.gamma_multiply(0.45),
            );
            painter.text(
                Pos2::new(geom.rect_min_x, bot_y),
                Align2::LEFT_TOP,
                "3'",
                style.font_id.clone(),
                text_color.gamma_multiply(0.45),
            );
        }

        // ── Strands ───────────────────────────────────────────────
        let top_galley = build_strand_galley(
            painter,
            &seq[block_start..block_end],
            &style.font_id,
            1.0,
            ctx.theme,
        );
        painter.galley(Pos2::new(seq_x0, top_y), top_galley, text_color);

        // Bottom strand is the complement of the visible block, derived on
        // demand — never stored on the buffer.
        let block_comp = seqforge_bio::complement(&seq[block_start..block_end]);
        let bot_galley = build_strand_galley(painter, &block_comp, &style.font_id, 0.65, ctx.theme);
        painter.galley(Pos2::new(seq_x0, bot_y), bot_galley, text_color);

        // ── Delete strikethrough (Phase 13.6b) ────────────────────
        // Deleted bases are kept visible (verify-what's-leaving) with a
        // strikethrough struck through both strands, drawn *over* the glyphs.
        if let Some((rs, re)) = ctx.deleted {
            let vis_s = rs.max(block_start);
            let vis_e = re.min(block_end);
            if vis_s < vis_e {
                let sx = seq_x0 + (vis_s - block_start) as f32 * char_width;
                let ex = seq_x0 + (vis_e - block_start) as f32 * char_width;
                let stroke = Stroke::new(1.5, DIFF_DEL_LINE);
                for strand_top in [top_y, bot_y] {
                    let my = strand_top + char_height * 0.5;
                    painter.line_segment([Pos2::new(sx, my), Pos2::new(ex, my)], stroke);
                }
            }
        }
    }
}
