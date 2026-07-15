//! Cut-sites track — restriction-enzyme labels stacked above the ruler, each
//! with a short tick; on hover the full SnapGene-style staple (descender +
//! overhang step + wedge arrows) reveals, descending across the strand rows.
//!
//! Position-owned, but a **connector**: the hover staple reaches down into the
//! Sequence track's strand rows via `geom.strand_top_y` / `strand_bot_y`. It is
//! painted last in the stack's z-order so the staple lands on top of the
//! strands it crosses.

use egui::{Align2, Color32, Painter, Pos2, Rect, Stroke, Vec2};
use seqforge_core::MethylState;

use crate::viewer::track::{BlockCtx, BlockGeom, Hit, Track, paint_wedge_down, paint_wedge_up};

/// Cached verdict for site `idx`; `Cuttable` if the parallel `methyl_states`
/// cache is absent (e.g. while staging) so overlays never crash on a short slice.
fn site_methyl(ctx: &BlockCtx<'_>, idx: usize) -> MethylState {
    ctx.methyl_states
        .get(idx)
        .copied()
        .unwrap_or(MethylState::Cuttable)
}

fn methyl_tint_color(base: Color32, state: MethylState) -> Color32 {
    match state {
        MethylState::Cuttable => base,
        MethylState::Blocked => base.gamma_multiply(0.35),
        MethylState::Impaired => base.gamma_multiply(0.6),
    }
}

fn cut_site_label(enzyme: &str, state: MethylState) -> String {
    if state == MethylState::Cuttable {
        enzyme.to_string()
    } else {
        format!("{enzyme}*")
    }
}

fn worst_group_methyl(ctx: &BlockCtx<'_>, members: impl Iterator<Item = usize>) -> MethylState {
    // `MethylState` is ordered by severity, so the worst is the max.
    members
        .map(|idx| site_methyl(ctx, idx))
        .max()
        .unwrap_or(MethylState::Cuttable)
}

pub(crate) struct CutSitesTrack;

impl Track for CutSitesTrack {
    fn block_height(&self, ctx: &BlockCtx) -> f32 {
        ctx.layout.cut_band_lines as f32 * ctx.style.cut_label_row_h
    }

    fn hit_rects(&self, ctx: &BlockCtx, geom: &BlockGeom, hits: &mut Vec<(Rect, Hit)>) {
        // Click target is each label only — not the staple line through the
        // strands. Co-located members keep individual hit rects (one per name
        // line) so each enzyme stays addressable. (`cut_sites` is empty while
        // staging, so this is inert then.)
        let style = ctx.style;
        for group in &ctx.layout.cut_groups {
            let cx = geom.seq_x0 + (group.cut_pos - ctx.block_start) as f32 * style.char_width;
            for (k, &site_idx) in group.members.iter().enumerate() {
                let site = &ctx.cut_sites[site_idx];
                let line = group.base_line + k;
                let label_w = site.enzyme.len() as f32 * style.label_char_w + 8.0;
                let label_y = geom.y0 + line as f32 * style.cut_label_row_h;
                hits.push((
                    Rect::from_min_size(
                        Pos2::new(cx, label_y),
                        Vec2::new(label_w, style.cut_label_row_h),
                    ),
                    Hit::CutSite(site_idx),
                ));
            }
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
        let ruler_y = block_y + ctx.layout.cut_band_lines as f32 * cut_label_row_h;

        // ── Resting state: each group's names stacked over a single leader ──
        // Co-located enzymes share one tick (deduped) that descends from just
        // below the group's lowest name toward the ruler — the "one site, many
        // isoschizomers" cue.
        for group in &ctx.layout.cut_groups {
            let tcx = seq_x0 + (group.cut_pos - block_start) as f32 * char_width;
            for (k, &site_idx) in group.members.iter().enumerate() {
                let site = &ctx.cut_sites[site_idx];
                let methyl = site_methyl(ctx, site_idx);
                let color = methyl_tint_color(cut_site_color, methyl);
                let line = group.base_line + k;
                let label_y = block_y + line as f32 * cut_label_row_h;
                painter.text(
                    Pos2::new(tcx, label_y),
                    Align2::LEFT_TOP,
                    cut_site_label(&site.enzyme, methyl),
                    style.small_font.clone(),
                    color,
                );
            }
            // One leader tick per group, from below the last name to the ruler.
            let tick_top =
                block_y + (group.base_line + group.members.len()) as f32 * cut_label_row_h;
            let tick_color = methyl_tint_color(
                cut_site_color,
                worst_group_methyl(ctx, group.members.iter().copied()),
            );
            painter.line_segment(
                [Pos2::new(tcx, tick_top), Pos2::new(tcx, ruler_y)],
                Stroke::new(1.0, tick_color),
            );
        }

        // ── Hover-reveal: full staple + wedges for the hovered site ──
        // Only the topmost hovered site (resolved once, globally) promotes; it
        // renders in whichever block holds its `cut_pos`.
        let Some(idx) = ctx.hovered_cut_site else {
            return;
        };
        let site = &ctx.cut_sites[idx];
        let hover_methyl = site_methyl(ctx, idx);
        let cut_site_color = methyl_tint_color(cut_site_color, hover_methyl);
        let top_cut = site.cut_pos;
        let bot_cut = site.bottom_cut_pos;
        // Descender starts at the hovered site's group leader (below its names),
        // so the staple connects to the same tick shown at rest.
        let group_bottom_line = ctx
            .layout
            .cut_groups
            .iter()
            .find(|g| g.members.contains(&idx))
            .map(|g| g.base_line + g.members.len());
        let Some(bottom_line) =
            group_bottom_line.filter(|_| top_cut >= block_start && top_cut <= block_end)
        else {
            return;
        };

        let strand_h = style.strand_h;
        let bot_y = geom.strand_bot_y;
        let top_y = geom.strand_top_y;
        let tcx = seq_x0 + (top_cut - block_start) as f32 * char_width;
        let stroke = Stroke::new(1.5, cut_site_color);
        let line_top = block_y + bottom_line as f32 * cut_label_row_h;

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
