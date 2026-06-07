use egui::{Align2, Color32, FontId, Pos2, Rect, Sense, Stroke, Vec2, text::LayoutJob};
use seqforge_core::{Annotations, Buffer, CutSite, Feature, Selection, Strand, View};

use crate::command::{AppCommand, PendingCommand};
use crate::config::{Config, LabelOverflow};

// ── Stacking ─────────────────────────────────────────────────────────────────

/// Core greedy interval stacking (port of seqviz `stackElements`).
/// Sorts ranges by start, then packs each into the first row whose last
/// element ends at or before the current range's start.
/// Returns `(item_idx → row, n_rows)`.
pub(crate) fn greedy_stack(ranges: &[(usize, usize)]) -> (Vec<usize>, usize) {
    if ranges.is_empty() {
        return (vec![], 0);
    }
    let mut order: Vec<usize> = (0..ranges.len()).collect();
    order.sort_by_key(|&i| ranges[i].0);

    let mut result = vec![0usize; ranges.len()];
    let mut row_ends: Vec<usize> = Vec::new();

    for idx in order {
        let (start, end) = ranges[idx];
        match row_ends.iter().position(|&e| e <= start) {
            Some(r) => {
                row_ends[r] = end;
                result[idx] = r;
            }
            None => {
                row_ends.push(end);
                result[idx] = row_ends.len() - 1;
            }
        }
    }
    (result, row_ends.len())
}

// ── Per-block layout ─────────────────────────────────────────────────────────
//
// Each rendered block (one line-wrap of the sequence) sizes itself to the
// items actually present in it. Cut-label rows and feature rows are stacked
// *per block* rather than across the whole document, so a section with one
// enzyme doesn't leave blank space matching the heaviest stack elsewhere.

/// Layout decisions for one block.
#[derive(Debug, Default, Clone)]
pub(crate) struct BlockLayout {
    /// `(feat_idx, row_in_block)` for features overlapping this block.
    pub feat_rows: Vec<(usize, usize)>,
    /// `(site_idx, row_in_block)` for cut sites whose `cut_pos` lies in
    /// `[block_start, block_end]`.
    pub cut_rows: Vec<(usize, usize)>,
    pub n_cut_rows: usize,
    /// Total height including ruler + both strands + gap.
    pub height: f32,
}

/// Build per-block layouts and the prefix-sum offsets used to convert block
/// indices to y-coordinates. `offsets[i]` is the y of the top of block `i`;
/// `offsets[n_blocks]` is the total content height.
#[allow(clippy::too_many_arguments)]
fn build_block_layouts(
    features: &[Feature],
    cut_sites: &[CutSite],
    seq_len: usize,
    line_width: usize,
    char_width: f32,
    label_char_w: f32,
    cut_label_row_h: f32,
    ruler_h: f32,
    strand_h: f32,
    annot_row_h: f32,
    block_gap: f32,
) -> (Vec<BlockLayout>, Vec<f32>) {
    let n_blocks = seq_len.div_ceil(line_width).max(1);
    let mut layouts: Vec<BlockLayout> = Vec::with_capacity(n_blocks);
    let mut offsets: Vec<f32> = Vec::with_capacity(n_blocks + 1);
    offsets.push(0.0);

    for block_idx in 0..n_blocks {
        let block_start = block_idx * line_width;
        let block_end = (block_start + line_width).min(seq_len);

        // Features overlapping this block: clip ranges to the block for
        // the stacking pass so feature heights reflect what's actually
        // drawn in this row.
        let mut feat_idx_list: Vec<usize> = Vec::new();
        let mut feat_ranges: Vec<(usize, usize)> = Vec::new();
        for (i, f) in features.iter().enumerate() {
            if f.range.start < block_end && f.range.end > block_start {
                feat_idx_list.push(i);
                feat_ranges.push((f.range.start.max(block_start), f.range.end.min(block_end)));
            }
        }
        let (feat_local_rows, n_feat_rows) = greedy_stack(&feat_ranges);
        let feat_rows: Vec<(usize, usize)> =
            feat_idx_list.into_iter().zip(feat_local_rows).collect();

        // Cut sites whose top-strand cut sits in this block. Stacking
        // intervals use label half-width converted to base columns so
        // adjacent labels collide as the user expects.
        let mut cut_idx_list: Vec<usize> = Vec::new();
        let mut cut_ranges: Vec<(usize, usize)> = Vec::new();
        for (i, s) in cut_sites.iter().enumerate() {
            if s.cut_pos >= block_start && s.cut_pos <= block_end {
                cut_idx_list.push(i);
                let half_px = s.enzyme.len() as f32 * label_char_w * 0.5 + 4.0;
                let half_bases = (half_px / char_width).ceil() as usize + 1;
                cut_ranges.push((s.cut_pos.saturating_sub(half_bases), s.cut_pos + half_bases));
            }
        }
        let (cut_local_rows, n_cut_rows) = greedy_stack(&cut_ranges);
        let cut_rows: Vec<(usize, usize)> = cut_idx_list.into_iter().zip(cut_local_rows).collect();

        let cut_label_h = n_cut_rows as f32 * cut_label_row_h;
        let annot_section_h = n_feat_rows as f32 * annot_row_h;
        let height = cut_label_h + ruler_h + strand_h * 2.0 + annot_section_h + block_gap;

        offsets.push(offsets.last().copied().unwrap_or(0.0) + height);
        layouts.push(BlockLayout {
            feat_rows,
            cut_rows,
            n_cut_rows,
            height,
        });
    }

    (layouts, offsets)
}

/// Locate the block containing a given y-coordinate (relative to the
/// allocated rect's top). Returns `None` if `rel_y` is negative.
fn y_to_block(rel_y: f32, offsets: &[f32]) -> Option<usize> {
    if rel_y < 0.0 || offsets.len() < 2 {
        return None;
    }
    // `offsets[i]` is the *top* of block i; total height is `offsets[n]`.
    // `partition_point` returns the first i where `offsets[i] > rel_y`, so
    // the containing block is one back.
    let idx = offsets.partition_point(|&o| o <= rel_y).saturating_sub(1);
    if idx >= offsets.len() - 1 {
        None
    } else {
        Some(idx)
    }
}

// ── Geometry helper ───────────────────────────────────────────────────────────

/// Clip a feature to the visible slice of a block and return its bar rect.
/// Returns `None` if the feature doesn't overlap this block at all.
fn annot_bar_rect(
    feat: &Feature,
    block_start: usize,
    block_end: usize,
    bar_row_y: f32, // top of the feature's stacked row
    seq_x0: f32,
    char_width: f32,
    row_h: f32,
) -> Option<Rect> {
    if feat.range.end <= block_start || feat.range.start >= block_end {
        return None;
    }
    let col_s = feat.range.start.max(block_start) - block_start;
    let col_e = feat.range.end.min(block_end) - block_start;
    Some(Rect::from_min_size(
        Pos2::new(seq_x0 + col_s as f32 * char_width, bar_row_y + 1.0),
        Vec2::new((col_e - col_s) as f32 * char_width, row_h - 2.0),
    ))
}

// ── Widget state ──────────────────────────────────────────────────────────────

/// Per-document state for the sequence viewer widget. With per-block
/// layouts there's no derived data worth caching across frames — each
/// block's stacking is O(items in block) and the total work is comparable
/// to a single pass over `features + cut_sites`.
#[derive(Debug, Default)]
pub struct SequenceView {
    drag_start: Option<usize>,
}

impl SequenceView {
    /// Reset transient interaction state on document change.
    pub fn reset(&mut self) {
        self.drag_start = None;
    }

    /// Render the sequence viewer. Caller must have already resolved an
    /// active view and locked its buffer for read; this widget is
    /// inert if there's no doc — the placeholder rendering lives in
    /// `tabs.rs::ui` so this function can assume a real buffer.
    ///
    /// Selection / feature highlight mutations go through
    /// `AppCommand::SetSelection` / `SelectFeature` (pushed to `cmds`)
    /// so the single-applier invariant from the focus refactor holds.
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        view: &mut View,
        buffer: &Buffer,
        annotations: &Annotations,
        cmds: &mut Vec<PendingCommand>,
        cfg: &Config,
    ) {
        let seq = &buffer.text;
        let seq_len = seq.len();

        if seq_len == 0 {
            ui.centered_and_justified(|ui| {
                ui.label("Empty sequence.");
            });
            return;
        }

        // ── Resolve runtime sizing from config ───────────────────────
        let font_size = cfg.settings.font.sequence_size;
        let label_size = cfg.settings.font.label_size;
        let ruler_size = cfg.settings.font.ruler_size;
        let annot_row_h = (label_size + 2.0 * cfg.settings.editor.label_padding)
            .max(cfg.settings.editor.min_annot_row_height);
        let ruler_h = cfg.settings.editor.ruler_height.max(ruler_size + 2.0);
        let strand_h = cfg.settings.editor.strand_bar_height;
        let block_gap = cfg.settings.editor.block_gap;
        let left_margin = cfg.settings.editor.left_margin;
        let right_margin = cfg.settings.editor.right_margin;
        // Cut-site labels render at `label_size` (same as feature labels) so
        // they stay legible on dense plasmids. Row height tracks the label
        // font + a small gap; label_char_w (measured from the same font
        // below) feeds the stacking math, keeping hit-rects accurate.
        let cut_label_row_h = label_size + 3.0;
        let selection_color = cfg.theme.ui.selection.0;
        let cursor_color = cfg.theme.ui.cursor.0;
        let cut_site_color = cfg.theme.ui.cut_site.0;
        let label_text_light = cfg.theme.ui.label_text.0;
        let label_text_dark = cfg.theme.ui.label_text_alt.0;
        let label_overflow = cfg.settings.editor.label_overflow;

        let comp = &buffer.complement;

        let font_id = FontId::monospace(font_size);
        let small_font = FontId::proportional(label_size);
        let ruler_font = FontId::proportional(ruler_size);

        // Measure char_width from an actual galley so feature bar positions
        // use the same per-character advance that LayoutJob renders, not the
        // single-glyph metric which can differ due to subpixel rounding.
        let (char_width, char_height, label_char_w) = ui.fonts(|f| {
            let probe = f.layout_no_wrap("A".repeat(64), font_id.clone(), Color32::BLACK);
            let label_probe = f.layout_no_wrap("A".repeat(32), small_font.clone(), Color32::BLACK);
            (
                probe.rect.width() / 64.0,
                f.row_height(&font_id),
                label_probe.rect.width() / 32.0,
            )
        });

        // Fit the line width to the available pane width.
        let avail = (ui.available_width() - left_margin - right_margin).max(char_width);
        let line_width = ((avail / char_width) as usize).max(10);
        let n_blocks = seq_len.div_ceil(line_width);

        // Per-block layout: each block sizes itself to the items it contains.
        // `block_offsets[i]` is the y-coord (within the allocated rect) of
        // the top of block i; `block_offsets[n_blocks]` is the total height.
        let (block_layouts, block_offsets) = build_block_layouts(
            &annotations.features,
            &view.cut_sites,
            seq_len,
            line_width,
            char_width,
            label_char_w,
            cut_label_row_h,
            ruler_h,
            strand_h,
            annot_row_h,
            block_gap,
        );
        let total_height = *block_offsets.last().unwrap_or(&0.0);
        let content_width = left_margin + line_width as f32 * char_width + right_margin;
        let alloc_width = content_width.max(ui.available_width());

        // Consume the one-shot scroll request: center the target block in
        // the viewport this frame, then clear so the user can scroll freely.
        let scroll_offset = view.scroll_to.take().map(|pos| {
            let block_idx = (pos / line_width).min(n_blocks.saturating_sub(1));
            let block_top = block_offsets[block_idx];
            let block_h = block_layouts[block_idx].height;
            let viewport_h = ui.available_height();
            (block_top - viewport_h / 2.0 + block_h / 2.0).max(0.0)
        });

        let mut scroll_area = egui::ScrollArea::vertical().auto_shrink([false, false]);
        if let Some(offset) = scroll_offset {
            scroll_area = scroll_area.vertical_scroll_offset(offset);
        }
        let mut computed_visible: Option<(usize, usize)> = None;
        scroll_area.show(ui, |ui| {
            let (response, painter) = ui.allocate_painter(
                Vec2::new(alloc_width, total_height),
                Sense::click_and_drag(),
            );
            let rect = response.rect;
            let clip = painter.clip_rect();
            let text_color = ui.visuals().text_color();
            let seq_x0 = rect.min.x + left_margin;

            // ── Pass 1: collect click-rects for all interactive elements ──

            let mut annot_hits: Vec<(Rect, usize)> = Vec::new();
            let mut search_hit_rects: Vec<(Rect, usize)> = Vec::new();
            // `Vec<(label_rect, site_idx)>` — label is the click + hover
            // target. The full staple only paints when this rect is
            // hovered (or the site is the persistent click selection).
            let mut cut_site_rects: Vec<(Rect, usize)> = Vec::new();
            for block_idx in 0..n_blocks {
                let block_y = rect.min.y + block_offsets[block_idx];
                let block_h = block_layouts[block_idx].height;
                if block_y + block_h < clip.min.y {
                    continue;
                }
                if block_y > clip.max.y {
                    break;
                }
                let block_start = block_idx * line_width;
                let block_end = (block_start + line_width).min(seq_len);
                let layout = &block_layouts[block_idx];
                let cut_label_h = layout.n_cut_rows as f32 * cut_label_row_h;
                let top_y = block_y + cut_label_h + ruler_h;
                let bot_y = top_y + strand_h;
                let annot_base_y = bot_y + strand_h;

                for &(feat_idx, row) in &layout.feat_rows {
                    let feat = &annotations.features[feat_idx];
                    let bar_row_y = annot_base_y + row as f32 * annot_row_h;
                    if let Some(r) = annot_bar_rect(
                        feat,
                        block_start,
                        block_end,
                        bar_row_y,
                        seq_x0,
                        char_width,
                        annot_row_h,
                    ) {
                        annot_hits.push((r, feat_idx));
                    }
                }
                for (hit_idx, hit) in view.search_hits.iter().enumerate() {
                    let vis_s = hit.start.max(block_start).min(block_end);
                    let vis_e = hit.end.min(block_end);
                    if vis_s < vis_e && vis_e > block_start {
                        let sx = seq_x0 + (vis_s - block_start) as f32 * char_width;
                        let sw = (vis_e - vis_s) as f32 * char_width;
                        search_hit_rects.push((
                            Rect::from_min_size(
                                Pos2::new(sx, top_y),
                                Vec2::new(sw, strand_h * 2.0),
                            ),
                            hit_idx,
                        ));
                    }
                }
                // Click target is the label only — not the staple line through the
                // strands. Clicking the strand near a cut site places a cursor as
                // expected; only the label is the intentional selection handle.
                for &(site_idx, row) in &layout.cut_rows {
                    let site = &view.cut_sites[site_idx];
                    let cx = seq_x0 + (site.cut_pos - block_start) as f32 * char_width;
                    let label_w = site.enzyme.len() as f32 * label_char_w + 8.0;
                    cut_site_rects.push((
                        Rect::from_center_size(
                            Pos2::new(cx, block_y + (row as f32 + 0.5) * cut_label_row_h),
                            Vec2::new(label_w, cut_label_row_h),
                        ),
                        site_idx,
                    ));
                }
            }

            // ── Interactions ──────────────────────────────────────────────

            let ptr = response.interact_pointer_pos();
            let ptr_seq = ptr.and_then(|p| {
                screen_to_seq(
                    p,
                    rect,
                    char_width,
                    line_width,
                    seq_len,
                    &block_offsets,
                    left_margin,
                )
            });

            let shift_held = ui.input(|i| i.modifiers.shift);

            // Helpers that close over `cmds` so each branch is one push.
            // The viewer never mutates `view.selection` / `selected_feature`
            // directly; one-frame visual lag is the documented trade-off
            // (see focus-refactor §2, "render never mutates state").
            let push_sel = |cmds: &mut Vec<PendingCommand>, sel: Option<Selection>| {
                cmds.push((AppCommand::SetSelection(sel), None));
            };
            let push_feat = |cmds: &mut Vec<PendingCommand>, feat: Option<usize>| {
                cmds.push((AppCommand::SelectFeature(feat), None));
            };

            if response.clicked() {
                if let Some(pos) = ptr {
                    if shift_held {
                        // Shift+click: extend focus while keeping existing anchor.
                        if let Some(seq_pos) = ptr_seq {
                            let new_sel = match view.selection {
                                Some(sel) => Selection {
                                    anchor: sel.anchor,
                                    focus: seq_pos,
                                },
                                None => Selection::cursor(seq_pos),
                            };
                            push_sel(cmds, Some(new_sel));
                        }
                    } else if let Some(&(_, feat_idx)) =
                        annot_hits.iter().find(|(r, _)| r.contains(pos))
                    {
                        let feat = &annotations.features[feat_idx];
                        push_sel(
                            cmds,
                            Some(Selection::range(feat.range.start, feat.range.end)),
                        );
                        push_feat(cmds, Some(feat_idx));
                    } else if let Some(&(_, hit_idx)) =
                        search_hit_rects.iter().find(|(r, _)| r.contains(pos))
                    {
                        let hit = &view.search_hits[hit_idx];
                        push_sel(cmds, Some(Selection::range(hit.start, hit.end)));
                        push_feat(cmds, None);
                    } else if let Some(&(_, site_idx)) =
                        cut_site_rects.iter().find(|(r, _)| r.contains(pos))
                    {
                        let site = &view.cut_sites[site_idx];
                        push_sel(
                            cmds,
                            Some(Selection::range(
                                site.recognition_start,
                                site.recognition_end,
                            )),
                        );
                        push_feat(cmds, None);
                    } else if let Some(seq_pos) = ptr_seq {
                        push_sel(cmds, Some(Selection::cursor(seq_pos)));
                        push_feat(cmds, None);
                    } else {
                        push_sel(cmds, None);
                        push_feat(cmds, None);
                    }
                }
            }

            if response.drag_started() {
                let on_annot = ptr.is_some_and(|p| annot_hits.iter().any(|(r, _)| r.contains(p)));
                let on_hit =
                    ptr.is_some_and(|p| search_hit_rects.iter().any(|(r, _)| r.contains(p)));
                let on_site =
                    ptr.is_some_and(|p| cut_site_rects.iter().any(|(r, _)| r.contains(p)));
                if !on_annot && !on_hit && !on_site {
                    // drag_start is view-local (not document state) so it
                    // stays on `self`.
                    self.drag_start = ptr_seq;
                    push_feat(cmds, None);
                    push_sel(cmds, ptr_seq.map(Selection::cursor));
                }
            }
            if response.dragged() {
                if let (Some(anchor), Some(focus)) = (self.drag_start, ptr_seq) {
                    push_sel(cmds, Some(Selection { anchor, focus }));
                }
            }
            if response.drag_stopped() {
                self.drag_start = None;
                // A zero-length drag stays as a cursor; non-zero stays as a range.
            }

            // ── Pass 2: render all visible blocks ─────────────────────────

            for block_idx in 0..n_blocks {
                let block_y = rect.min.y + block_offsets[block_idx];
                let block_h = block_layouts[block_idx].height;
                if block_y + block_h < clip.min.y {
                    continue;
                }
                if block_y > clip.max.y {
                    break;
                }

                let block_start = block_idx * line_width;
                let block_end = (block_start + line_width).min(seq_len);
                let block_len = block_end - block_start;
                let layout = &block_layouts[block_idx];
                let cut_label_h = layout.n_cut_rows as f32 * cut_label_row_h;

                // ── Ruler ─────────────────────────────────────────────────
                let ruler_y = block_y + cut_label_h;
                let ruler_text = cfg.theme.ui.ruler_text.0;
                for col in 0..block_len {
                    let abs = block_start + col;
                    if abs == 0 || (abs + 1) % 10 == 0 {
                        painter.text(
                            Pos2::new(seq_x0 + col as f32 * char_width, ruler_y),
                            Align2::LEFT_TOP,
                            format!("{}", abs + 1),
                            ruler_font.clone(),
                            ruler_text,
                        );
                    }
                }

                let top_y = ruler_y + ruler_h;
                let bot_y = top_y + strand_h;
                let annot_base_y = bot_y + strand_h;

                // ── Search hit highlights (behind selection and text) ──────
                for hit in &view.search_hits {
                    let vis_s = hit.start.max(block_start).min(block_end);
                    let vis_e = hit.end.min(block_end); // clamp wrap-arounds
                    if vis_s < vis_e && vis_e > block_start {
                        let sx = seq_x0 + (vis_s - block_start) as f32 * char_width;
                        let sw = (vis_e - vis_s) as f32 * char_width;
                        let color = search_hit_color(&cfg.theme, hit.strand);
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

                // ── Selection highlight / cursor (behind text) ────────────
                if let Some(sel) = view.selection {
                    if sel.is_cursor() {
                        // Thin vertical line between bases spanning both strands.
                        let pos = sel.anchor;
                        if pos >= block_start && pos <= block_end {
                            let cx = seq_x0 + (pos - block_start) as f32 * char_width;
                            painter.rect_filled(
                                Rect::from_min_size(
                                    Pos2::new(cx - 0.75, top_y),
                                    Vec2::new(1.5, strand_h * 2.0),
                                ),
                                0.0,
                                cursor_color,
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
                                Rect::from_min_size(
                                    Pos2::new(sx, top_y),
                                    Vec2::new(sw, char_height),
                                ),
                                0.0,
                                selection_color,
                            );
                            painter.rect_filled(
                                Rect::from_min_size(
                                    Pos2::new(sx, bot_y),
                                    Vec2::new(sw, char_height),
                                ),
                                0.0,
                                selection_color.gamma_multiply(0.7),
                            );
                        }
                    }
                }

                // ── 5'/3' labels (first block only) ───────────────────────
                if block_idx == 0 {
                    painter.text(
                        Pos2::new(rect.min.x, top_y),
                        Align2::LEFT_TOP,
                        "5'",
                        font_id.clone(),
                        text_color.gamma_multiply(0.45),
                    );
                    painter.text(
                        Pos2::new(rect.min.x, bot_y),
                        Align2::LEFT_TOP,
                        "3'",
                        font_id.clone(),
                        text_color.gamma_multiply(0.45),
                    );
                }

                // ── Strands ───────────────────────────────────────────────
                let top_galley = build_strand_galley(
                    ui,
                    &seq[block_start..block_end],
                    &font_id,
                    1.0,
                    &cfg.theme,
                );
                painter.galley(Pos2::new(seq_x0, top_y), top_galley, text_color);

                let bot_galley = build_strand_galley(
                    ui,
                    &comp[block_start..block_end],
                    &font_id,
                    0.65,
                    &cfg.theme,
                );
                painter.galley(Pos2::new(seq_x0, bot_y), bot_galley, text_color);

                // ── Annotation bars (below strands) ───────────────────────
                for &(feat_idx, row) in &layout.feat_rows {
                    let feat = &annotations.features[feat_idx];
                    let bar_row_y = annot_base_y + row as f32 * annot_row_h;
                    if let Some(bar) = annot_bar_rect(
                        feat,
                        block_start,
                        block_end,
                        bar_row_y,
                        seq_x0,
                        char_width,
                        annot_row_h,
                    ) {
                        let is_selected = view.selected_feature == Some(feat_idx);
                        painter.rect_filled(bar, 2.0, cfg.theme.feature_color(feat.kind));
                        if is_selected {
                            painter.rect_stroke(
                                bar,
                                2.0,
                                Stroke::new(1.5, Color32::WHITE),
                                egui::StrokeKind::Inside,
                            );
                        }
                        if !feat.label.is_empty() {
                            let swatch = cfg.theme.feature_color(feat.kind);
                            let fg = crate::config::theme::pick_contrast(
                                swatch,
                                label_text_light,
                                label_text_dark,
                            );
                            paint_feature_label(
                                &painter,
                                &small_font,
                                fg,
                                label_overflow,
                                label_char_w,
                                bar,
                                &feat.label,
                            );
                        }
                    }
                }

                // ── Cut site tick marks + labels (resting state) ──────────
                // SnapGene-style: at rest each site shows only a label in
                // its stacked row plus a short tick descending from the
                // label toward the ruler. The full staple — descender
                // through both strands, overhang step, and wedge arrows —
                // appears only when the user hovers the label (see the
                // hover-reveal pass below).
                let cut_label_font = FontId::proportional(label_size);
                for &(site_idx, row) in &layout.cut_rows {
                    let site = &view.cut_sites[site_idx];
                    let top_cut = site.cut_pos;
                    let tcx = seq_x0 + (top_cut - block_start) as f32 * char_width;
                    let label_y = block_y + row as f32 * cut_label_row_h;
                    painter.text(
                        Pos2::new(tcx, label_y),
                        Align2::CENTER_TOP,
                        &site.enzyme,
                        cut_label_font.clone(),
                        cut_site_color,
                    );
                    // Short tick from the bottom of the label row toward
                    // the ruler — just enough visual to read "site here"
                    // without committing to the full staple.
                    let tick_top = block_y + (row + 1) as f32 * cut_label_row_h;
                    let tick_bot = (tick_top + cut_label_row_h * 0.6).min(ruler_y);
                    painter.line_segment(
                        [Pos2::new(tcx, tick_top), Pos2::new(tcx, tick_bot)],
                        Stroke::new(1.0, cut_site_color),
                    );
                }

                // ── Hover-reveal: full staple + wedges for one site ──────
                // Hover hit-test uses the label rects collected in pass 1.
                // Only the topmost hovered site promotes; non-hovered
                // sites keep their tick + label.
                // `interact_pointer_pos` only fires during click/drag;
                // `hover_pos` covers idle mouse-over which is what we want.
                let hovered_site_idx = response.hover_pos().and_then(|p| {
                    cut_site_rects
                        .iter()
                        .find(|(r, _)| r.contains(p))
                        .map(|(_, idx)| *idx)
                });
                if let Some(idx) = hovered_site_idx {
                    let site = &view.cut_sites[idx];
                    let top_cut = site.cut_pos;
                    let bot_cut = site.bottom_cut_pos;
                    // Find the row this site occupies in *this* block's layout.
                    // Sites whose `cut_pos` is in this block were stacked here.
                    let row_in_block = layout
                        .cut_rows
                        .iter()
                        .find(|(i, _)| *i == idx)
                        .map(|(_, r)| *r);
                    if let Some(row) =
                        row_in_block.filter(|_| top_cut >= block_start && top_cut <= block_end)
                    {
                        let tcx = seq_x0 + (top_cut - block_start) as f32 * char_width;
                        let stroke = Stroke::new(1.5, cut_site_color);
                        let line_top = block_y + (row + 1) as f32 * cut_label_row_h;

                        // Descender from label row through the top strand.
                        painter.line_segment(
                            [Pos2::new(tcx, line_top), Pos2::new(tcx, bot_y)],
                            stroke,
                        );

                        // Strand connector + overhang step.
                        if top_cut == bot_cut {
                            painter.line_segment(
                                [Pos2::new(tcx, bot_y), Pos2::new(tcx, bot_y + strand_h)],
                                stroke,
                            );
                        } else if bot_cut >= block_start && bot_cut <= block_end {
                            let bcx = seq_x0 + (bot_cut - block_start) as f32 * char_width;
                            painter.line_segment(
                                [Pos2::new(tcx, bot_y), Pos2::new(bcx, bot_y)],
                                stroke,
                            );
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
                        // Two small filled triangles indicate cut points:
                        //   * top wedge on the top strand at `cut_pos`
                        //   * bottom wedge on the bottom strand at
                        //     `bottom_cut_pos`
                        // Triangles point *into* the cut from the strand
                        // they sit on (top wedge points down toward the
                        // top-strand cut line; bottom wedge points up).
                        // This matches the SnapGene convention of arrows
                        // marking the precise scissile bond on each strand.
                        let bcx_in_block = bot_cut >= block_start && bot_cut <= block_end;
                        let bcx = if bcx_in_block {
                            Some(seq_x0 + (bot_cut - block_start) as f32 * char_width)
                        } else {
                            None
                        };
                        let wedge_half = (char_width * 0.45).clamp(2.5, 5.0);
                        let wedge_h = (strand_h * 0.55).clamp(3.0, 7.0);
                        paint_wedge_down(&painter, tcx, top_y, wedge_half, wedge_h, cut_site_color);
                        if let Some(bcx) = bcx {
                            paint_wedge_up(
                                &painter,
                                bcx,
                                bot_y + strand_h,
                                wedge_half,
                                wedge_h,
                                cut_site_color,
                            );
                        }
                    }
                }

                // ── Block separator ───────────────────────────────────────
                if block_idx + 1 < n_blocks {
                    painter.hline(
                        rect.min.x..=rect.min.x + content_width,
                        block_y + block_h - block_gap * 0.5,
                        Stroke::new(0.5, text_color.gamma_multiply(0.08)),
                    );
                }
            }

            // Visible range for minimap viewport indicator. With
            // variable block heights we look up the first / last blocks
            // via the prefix-sum offsets rather than dividing by a
            // single block height.
            let scroll_top = (clip.min.y - rect.min.y).max(0.0);
            let scroll_bot = scroll_top + clip.height();
            let first_block = y_to_block(scroll_top, &block_offsets).unwrap_or(0);
            let last_block =
                y_to_block(scroll_bot, &block_offsets).unwrap_or(n_blocks.saturating_sub(1));
            computed_visible = Some((
                (first_block * line_width).min(seq_len),
                ((last_block + 1) * line_width).min(seq_len),
            ));
        });
        view.visible_range = computed_visible;
    }
}

// ── Free helpers ──────────────────────────────────────────────────────────────

/// Screen → 0-based sequence offset. Returns positions in the closed
/// range `0..=seq_len` — the upper bound is the "insert-at-end"
/// cursor (one past the last base), an editor-grade affordance that
/// view-only code doesn't strictly need but the edit path (Tier 3d)
/// does. Tier 2 #9.
///
/// Callers that want a strictly-on-base position (selection range
/// endpoints) should bound-check `< seq_len` themselves.
fn screen_to_seq(
    pos: Pos2,
    rect: Rect,
    char_width: f32,
    line_width: usize,
    seq_len: usize,
    block_offsets: &[f32],
    left_margin: f32,
) -> Option<usize> {
    let rel_x = pos.x - rect.min.x - left_margin;
    let rel_y = pos.y - rect.min.y;
    if rel_x < 0.0 || rel_y < 0.0 {
        return None;
    }
    let block_idx = y_to_block(rel_y, block_offsets)?;
    let col = (rel_x / char_width) as usize;
    if col >= line_width {
        return None;
    }
    let p = block_idx * line_width + col;
    if p > seq_len { None } else { Some(p) }
}

/// Draw a small filled triangle pointing downward, with its apex at `(cx, y)`
/// and its base `height` units above (so the wedge sits *on top of* the
/// strand line and points into the cut). Used for the top-strand cut wedge.
fn paint_wedge_down(
    painter: &egui::Painter,
    cx: f32,
    y: f32,
    half_w: f32,
    height: f32,
    color: Color32,
) {
    let points = vec![
        Pos2::new(cx - half_w, y - height),
        Pos2::new(cx + half_w, y - height),
        Pos2::new(cx, y),
    ];
    painter.add(egui::Shape::convex_polygon(
        points,
        color,
        egui::Stroke::NONE,
    ));
}

/// Draw a small filled triangle pointing upward, with its apex at `(cx, y)`
/// and its base `height` units below. Used for the bottom-strand cut wedge.
fn paint_wedge_up(
    painter: &egui::Painter,
    cx: f32,
    y: f32,
    half_w: f32,
    height: f32,
    color: Color32,
) {
    let points = vec![
        Pos2::new(cx - half_w, y + height),
        Pos2::new(cx + half_w, y + height),
        Pos2::new(cx, y),
    ];
    painter.add(egui::Shape::convex_polygon(
        points,
        color,
        egui::Stroke::NONE,
    ));
}

fn search_hit_color(theme: &crate::config::Theme, strand: Strand) -> Color32 {
    match strand {
        Strand::Forward => theme.strand.forward.0,
        Strand::Reverse => theme.strand.reverse.0,
        _ => theme.strand.unknown.0,
    }
}

fn build_strand_galley(
    ui: &egui::Ui,
    bases: &[u8],
    font_id: &FontId,
    alpha: f32,
    theme: &crate::config::Theme,
) -> std::sync::Arc<egui::Galley> {
    let mut job = LayoutJob::default();
    for &b in bases {
        job.append(
            &(b.to_ascii_uppercase() as char).to_string(),
            0.0,
            egui::text::TextFormat {
                font_id: font_id.clone(),
                color: theme.bases.for_base(b).gamma_multiply(alpha),
                ..Default::default()
            },
        );
    }
    ui.fonts(|f| f.layout_job(job))
}

/// Draw a feature label according to the `label_overflow` policy.
/// Contrast is guaranteed by the WCAG-aware text colour picker at the
/// call site, so no outline is needed.
fn paint_feature_label(
    painter: &egui::Painter,
    font: &FontId,
    color: Color32,
    overflow: LabelOverflow,
    label_char_w: f32,
    bar: Rect,
    label: &str,
) {
    let bar_w = bar.width();
    let full_w = label.chars().count() as f32 * label_char_w;
    let text: Option<String> = if bar_w >= full_w {
        Some(label.to_string())
    } else {
        match overflow {
            LabelOverflow::Truncate => None,
            LabelOverflow::Extend => Some(label.to_string()),
            LabelOverflow::Ellipsis => {
                let usable = (bar_w - label_char_w).max(0.0);
                let n = (usable / label_char_w).floor() as usize;
                if n == 0 {
                    None
                } else {
                    let mut s: String = label.chars().take(n).collect();
                    s.push('…');
                    Some(s)
                }
            }
        }
    };
    let Some(text) = text else { return };
    painter.text(
        bar.center(),
        Align2::CENTER_CENTER,
        &text,
        font.clone(),
        color,
    );
}
