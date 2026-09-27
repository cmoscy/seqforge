//! Primer tracks — directional arrows for authored primers (Phase 0.4 render +
//! Phase 1.1 decomposition + attachment-state; `plans/primers.md` "Rendering").
//! Two position-owned bands straddle the sequence: **forward** primers above the
//! top strand, **reverse** below the bottom strand (the SnapGene/Benchling idiom).
//! Each arrow is an outlined body column-aligned to the annealed footprint,
//! arrowhead at the **3' end**, showing the oligo's **bases** per column — matches
//! in the base palette, **mismatches** on an amber cell (the `Drifted` cue). The
//! 5' tail (oligo bases beyond the footprint) continues the arrow **inline on
//! the primer's own row**, dimmed, with a notch at the anneal boundary — it is
//! part of the primer's shape, not a base-rendering extra, so it draws in arrow
//! mode too (as a dimmed rule + length). It stays on-row deliberately: the band
//! reserves `n_rows × primer_row_h`, so anything raised above the top row would
//! collide with cut labels / the previous wrap. A **moved** badge marks
//! [`AttachmentState::Drifted`] primers; an **×N** badge counts off-target sites.
//!
//! The per-primer alignment (annealed / mismatch / tail, strand-correct) comes
//! from `seqforge_bio::decompose_primer`, carried in `BlockCtx::primer_decomps`.
//! Attachment state + off-targets come from `BlockCtx::primer_states` (memoized
//! find + classify). A detached primer (`binding = None`) draws nowhere — panel-
//! only (Phase 1.3).
//!
//! Paint and hit-test share one geometry (`primer_body_rect`) — the co-location
//! invariant the Track abstraction exists to guarantee.

use egui::{Align2, Color32, Painter, Pos2, Rect, Stroke, Vec2};
use seqforge_bio::AttachmentState;

use crate::viewer::track::{BlockCtx, BlockGeom, Hit, Track, primer_body_rect};

/// Forward-primer band (above the top strand); arrowhead points 3'→right.
pub(crate) struct PrimerForwardTrack;
/// Reverse-primer band (below the bottom strand); arrowhead points 3'→left.
pub(crate) struct PrimerReverseTrack;

impl Track for PrimerForwardTrack {
    fn block_height(&self, ctx: &BlockCtx) -> f32 {
        // Hidden (Inspector toggle) → collapse the band so the stack closes up.
        if !ctx.primer_display.show {
            return 0.0;
        }
        ctx.layout.primer_fwd_band_h
    }
    fn paint(&self, ctx: &BlockCtx, geom: &BlockGeom, painter: &Painter) {
        if !ctx.primer_display.show {
            return;
        }
        paint_band(ctx, geom, painter, &ctx.layout.primer_fwd_rows, false);
    }
    fn hit_rects(&self, ctx: &BlockCtx, geom: &BlockGeom, hits: &mut Vec<(Rect, Hit)>) {
        if !ctx.primer_display.show {
            return;
        }
        hit_band(ctx, geom, &ctx.layout.primer_fwd_rows, hits);
    }
}

impl Track for PrimerReverseTrack {
    fn block_height(&self, ctx: &BlockCtx) -> f32 {
        if !ctx.primer_display.show {
            return 0.0;
        }
        ctx.layout.primer_rev_band_h
    }
    fn paint(&self, ctx: &BlockCtx, geom: &BlockGeom, painter: &Painter) {
        if !ctx.primer_display.show {
            return;
        }
        paint_band(ctx, geom, painter, &ctx.layout.primer_rev_rows, true);
    }
    fn hit_rects(&self, ctx: &BlockCtx, geom: &BlockGeom, hits: &mut Vec<(Rect, Hit)>) {
        if !ctx.primer_display.show {
            return;
        }
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

        // Selection = highlight the oligo *object* (Phase 1.5e), not the template
        // row. A primer is a single-strand reagent, so selecting it emphasises its
        // own drawn bases — annealed body here + the lifted 5' tail below — rather
        // than a `view.selection` on the template (wrong strand for a reverse
        // primer; a 5' tail has no template column). Keyed on `selected_primer`,
        // the counterpart of the Features track's `selected_feature` pass.
        let is_selected = ctx.selected_primer == Some(primer.id);
        if is_selected {
            painter.rect_filled(body, 2.0, body_color.gamma_multiply(0.28));
        }

        // Body: an outline only (no fill) aligned to the annealed footprint
        // (SnapGene / Benchling idiom) — the strand-coloured stroke + arrowhead
        // carry identity; the primer's own bases fill the interior. A selected
        // oligo gets a brighter, heavier outline.
        let body_stroke = if is_selected {
            Stroke::new(2.0, Color32::WHITE)
        } else {
            Stroke::new(1.5, body_color)
        };
        painter.rect_stroke(body, 2.0, body_stroke, egui::StrokeKind::Inside);

        let mid_y = body.center().y;
        let head_len = (char_width * 0.8).clamp(4.0, 10.0);
        let head_half = (body.height() * 0.55 + 2.0).min(style.primer_row_h * 0.5);

        // Annealed bases (Phase 1.1 decomposition): the oligo base at each
        // template column, column-aligned to the sequence row it abuts. Matched
        // bases share **one** neutral colour so the primer reads as a distinct
        // block (not the multi-colour base palette of the sequence below); a
        // **mismatch** pops in the amber accent + cell (the `Drifted` cue).
        // Reverse orientation/tail were resolved in `decompose_primer`, so this
        // loop is strand-agnostic.
        let decomp = ctx.primer_decomps.get(primer_idx);
        // Arrows-vs-bases (Inspector toggle): in arrow mode we draw only the
        // outline + arrowhead + badges; the per-base letters and tail ribbon are
        // suppressed.
        if ctx.primer_display.bases {
            if let Some(decomp) = decomp {
                for ab in &decomp.annealed {
                    if ab.column < block_start || ab.column >= block_end {
                        continue;
                    }
                    let cx = geom.seq_x0 + (ab.column - block_start) as f32 * char_width;
                    let color = if ab.matches {
                        style.text_color
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
        } else if binding.start + binding.len <= block_end {
            let tip_x = body.max.x + head_len;
            filled_triangle(
                painter,
                Pos2::new(body.max.x, mid_y - head_half),
                Pos2::new(body.max.x, mid_y + head_half),
                Pos2::new(tip_x, mid_y),
                body_color,
            );
        }

        // Attachment-state badges (Phase 1.1): drifted "moved" cue + off-target count.
        if let Some(att) = ctx.primer_states.get(primer_idx) {
            paint_state_badges(painter, body, mid_y, reverse, char_width, style, att);
        }

        // 5' tail: oligo bases with no template column. Drawn **inline on the
        // primer's own row**, continuing the body away from the 3' end in a
        // dimmed hue with a thin notch at the junction — the SnapGene/Benchling
        // idiom. It deliberately does not lift into a separate row: the band
        // reserves `n_rows × primer_row_h` and nothing more, so a raised ribbon
        // on the top row would collide with cut labels / the previous wrap.
        // Keeping it on-row also means a tail can never collide with a
        // neighbouring primer that `stack_primers` packed beside it. Drawn only
        // in the block holding the 5' end.
        //
        // Every tail base is lettered. This used to cap at 8 and collapse the
        // rest into a `+N` stub, which hid exactly the bases a tail exists to
        // carry — a restriction site, an overhang, a homology arm. A tail runs
        // *outward* from the 5' edge and a tailed primer usually sits at
        // position 0, so an uncapped tail would reach into the margin where the
        // lane labels live; the painter is clipped to the block's columns
        // instead, so it degrades by running out of room rather than by
        // overdrawing or by lying about its length.
        let tail = decomp.map(|d| d.tail.as_slice()).unwrap_or(&[]);
        let five_prime_in_block = if reverse {
            binding.start + binding.len <= block_end
        } else {
            binding.start >= block_start
        };
        if !tail.is_empty() && five_prime_in_block {
            let shown = tail.len();
            let edge_x = if reverse { body.max.x } else { body.min.x };
            let dir = if reverse { 1.0 } else { -1.0 };
            let span_w = dir * shown as f32 * char_width;

            // Keep the tail inside the block's own columns. Without this an
            // uncapped tail paints over the left margin's 5'/3' lane labels.
            let cols = (block_end - block_start) as f32;
            let painter =
                &painter.with_clip_rect(painter.clip_rect().intersect(Rect::from_min_max(
                    Pos2::new(geom.seq_x0, painter.clip_rect().min.y),
                    Pos2::new(geom.seq_x0 + cols * char_width, painter.clip_rect().max.y),
                )));

            // Selected-emphasis pass (Phase 1.5e): one wash over body + tail so
            // the whole oligo reads as a single selected object.
            if is_selected {
                let wash = Rect::from_two_pos(
                    Pos2::new(edge_x, body.min.y),
                    Pos2::new(edge_x + span_w, body.max.y),
                );
                painter.rect_filled(wash, 2.0, tail_color.gamma_multiply(0.28));
            }

            // The notch: a short vertical tick at the anneal boundary, marking
            // where the oligo stops touching the template.
            painter.line_segment(
                [
                    Pos2::new(edge_x, body.min.y + 1.0),
                    Pos2::new(edge_x, body.max.y - 1.0),
                ],
                Stroke::new(1.0, tail_color),
            );

            if ctx.primer_display.bases {
                // Tail bases nearest the junction first (3'→5' of the tail).
                for k in 0..shown {
                    let base = tail[tail.len() - 1 - k];
                    let cx = edge_x + dir * (k as f32 + 0.5) * char_width;
                    painter.text(
                        Pos2::new(cx, mid_y),
                        Align2::CENTER_CENTER,
                        (base as char).to_string(),
                        style.font_id.clone(),
                        tail_color,
                    );
                }
            } else {
                // Arrow mode: no letters, but the tail is part of the primer's
                // *shape*, not a base-rendering extra. A dimmed rule along the
                // row, labelled with its length.
                painter.line_segment(
                    [Pos2::new(edge_x, mid_y), Pos2::new(edge_x + span_w, mid_y)],
                    Stroke::new(1.5, tail_color),
                );
                painter.text(
                    Pos2::new(edge_x + span_w, mid_y),
                    if reverse {
                        Align2::LEFT_CENTER
                    } else {
                        Align2::RIGHT_CENTER
                    },
                    format!("{} nt", tail.len()),
                    style.small_font.clone(),
                    tail_color,
                );
            }
        }
    }
}

/// Amber drift badge + off-target count past the 3' arrowhead.
fn paint_state_badges(
    painter: &Painter,
    body: Rect,
    mid_y: f32,
    reverse: bool,
    char_width: f32,
    style: &crate::viewer::track::Style,
    att: &seqforge_bio::PrimerAttachment,
) {
    let badge_color = style.primer_mismatch;
    let mut badge_x = if reverse {
        body.min.x - char_width * 1.2
    } else {
        body.max.x + char_width * 0.4
    };

    if att.state == AttachmentState::Drifted {
        painter.text(
            Pos2::new(badge_x, mid_y),
            Align2::CENTER_CENTER,
            "moved",
            style.small_font.clone(),
            badge_color,
        );
        badge_x += if reverse {
            -char_width * 2.8
        } else {
            char_width * 2.8
        };
    }

    let n = att.off_target_sites.len();
    if n > 0 {
        painter.text(
            Pos2::new(badge_x, mid_y),
            Align2::CENTER_CENTER,
            format!("×{n}"),
            style.small_font.clone(),
            badge_color.gamma_multiply(0.85),
        );
    }
}

fn filled_triangle(painter: &Painter, a: Pos2, b: Pos2, c: Pos2, color: Color32) {
    painter.add(egui::Shape::convex_polygon(
        vec![a, b, c],
        color,
        Stroke::NONE,
    ));
}
