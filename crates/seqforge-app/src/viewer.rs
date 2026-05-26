use egui::{text::LayoutJob, Align2, Color32, FontId, Pos2, Rect, Sense, Stroke, Vec2};
use seqforge_core::{
    Annotations, Buffer, BufferId, Feature, Selection, Strand, View,
};

use crate::cache::Cache;
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

fn stack_features(features: &[Feature]) -> (Vec<usize>, usize) {
    let ranges: Vec<(usize, usize)> =
        features.iter().map(|f| (f.range.start, f.range.end)).collect();
    greedy_stack(&ranges)
}

fn stack_cut_labels(
    sites: &[seqforge_core::CutSite],
    char_width: f32,
    label_char_w: f32,
) -> (Vec<usize>, usize) {
    let ranges: Vec<(usize, usize)> = sites
        .iter()
        .map(|s| {
            let half_px = s.enzyme.len() as f32 * label_char_w * 0.5 + 4.0;
            let half_bases = (half_px / char_width).ceil() as usize + 1;
            (s.cut_pos.saturating_sub(half_bases), s.cut_pos + half_bases)
        })
        .collect();
    greedy_stack(&ranges)
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

/// Stacked-row assignment for a set of intervals (features or cut
/// labels). Both viewer caches share the same shape.
#[derive(Debug, Default, Clone)]
pub(crate) struct StackLayout {
    pub(crate) row: Vec<usize>,
    pub(crate) n_rows: usize,
}

/// Rendering caches and interaction state for the sequence viewer widget.
/// Document data lives on [`Buffer`]; selection / scroll / search live
/// on [`View`]. Caches use the generic [`Cache`] helper and key on the
/// version-stable inputs each one depends on — this is the contract
/// the rest of the codebase follows for derived data (Stage 2.5e).
#[derive(Debug, Default)]
pub struct SequenceView {
    drag_start: Option<usize>,
    /// Feature stacking: keyed by `(buffer_id, buffer.version)`. Two
    /// views of the same buffer at the same version share the layout
    /// conceptually; sharing the cache itself is a future Tier 4 win
    /// (today each `SequenceView` has its own).
    feature_cache: Cache<(BufferId, u64), StackLayout>,
    /// Cut-label stacking: keyed by `(sorted_cut_positions, char_width_q)`
    /// where `char_width_q` is `char_width × 4` rounded to integer so
    /// sub-quarter-pixel font changes don't thrash the cache. Pane
    /// resize is the only realistic trigger for re-stacking.
    cut_label_cache: Cache<(Vec<usize>, u32), StackLayout>,
}

impl SequenceView {
    /// Clear caches so the next `show()` recomputes from scratch.
    /// Called by `command::apply` on Open / Close so per-doc derived
    /// data doesn't leak across document boundaries.
    pub fn reset(&mut self) {
        self.drag_start = None;
        self.feature_cache.invalidate();
        self.cut_label_cache.invalidate();
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
        let ruler_h = cfg
            .settings
            .editor
            .ruler_height
            .max(ruler_size + 2.0);
        let strand_h = cfg.settings.editor.strand_bar_height;
        let block_gap = cfg.settings.editor.block_gap;
        let left_margin = cfg.settings.editor.left_margin;
        let right_margin = cfg.settings.editor.right_margin;
        let cut_label_row_h = ruler_size + 3.0;
        let selection_color = cfg.theme.ui.selection.0;
        let cursor_color = cfg.theme.ui.cursor.0;
        let cut_site_color = cfg.theme.ui.cut_site.0;
        let label_text_light = cfg.theme.ui.label_text.0;
        let label_text_dark = cfg.theme.ui.label_text_alt.0;
        let label_overflow = cfg.settings.editor.label_overflow;

        // Feature stacking keyed by (buffer_id, buffer.version). The
        // `Cache` helper handles version-driven invalidation; producer
        // runs at most once per distinct key.
        let feature_layout = self
            .feature_cache
            .get_or_compute((view.buffer_id, buffer.version), || {
                let (row, n_rows) = stack_features(&annotations.features);
                StackLayout { row, n_rows }
            })
            .clone();

        // Complement lives on Buffer (computed at load time, recomputed
        // on edit by Buffer's mutator). View-side caching no longer needed.
        let comp = &buffer.complement;
        let feat_row = &feature_layout.row;
        let n_annot_rows = feature_layout.n_rows;

        let font_id = FontId::monospace(font_size);
        let small_font = FontId::proportional(label_size);
        let ruler_font = FontId::proportional(ruler_size);

        // Measure char_width from an actual galley so feature bar positions
        // use the same per-character advance that LayoutJob renders, not the
        // single-glyph metric which can differ due to subpixel rounding.
        let (char_width, char_height, label_char_w) = ui.fonts(|f| {
            let probe = f.layout_no_wrap("A".repeat(64), font_id.clone(), Color32::BLACK);
            let label_probe =
                f.layout_no_wrap("A".repeat(32), small_font.clone(), Color32::BLACK);
            (
                probe.rect.width() / 64.0,
                f.row_height(&font_id),
                label_probe.rect.width() / 32.0,
            )
        });

        // Fit the line width to the available pane width.
        let avail = (ui.available_width() - left_margin - right_margin).max(char_width);
        let line_width = ((avail / char_width) as usize).max(10);

        let annot_section_h = n_annot_rows as f32 * annot_row_h;

        // Cut-label stacking. Key on sorted cut positions (catches
        // enzyme swaps that don't change count) and quantized char
        // width (pane resize invalidates).
        let cut_site_key: Vec<usize> = {
            let mut positions: Vec<usize> =
                view.cut_sites.iter().map(|s| s.cut_pos).collect();
            positions.sort_unstable();
            positions
        };
        let char_width_q = (char_width * 4.0).round() as u32;
        let sites = view.cut_sites.clone(); // small, used inside closure
        let cut_layout = self
            .cut_label_cache
            .get_or_compute((cut_site_key, char_width_q), || {
                let (row, n_rows) = stack_cut_labels(&sites, char_width, label_char_w);
                StackLayout { row, n_rows }
            })
            .clone();
        let cut_label_row = &cut_layout.row;
        let n_cut_label_rows = cut_layout.n_rows;
        let cut_label_h = n_cut_label_rows as f32 * cut_label_row_h;

        let block_h = cut_label_h + ruler_h + strand_h * 2.0 + annot_section_h + block_gap;
        let n_blocks = seq_len.div_ceil(line_width);
        let total_height = n_blocks as f32 * block_h;
        let content_width = left_margin + line_width as f32 * char_width + right_margin;
        let alloc_width = content_width.max(ui.available_width());

        // Consume the one-shot scroll request: center the target block in the
        // viewport this frame, then clear so the user can scroll freely.
        let scroll_offset = view.scroll_to.take().map(|pos| {
            let block_top = (pos / line_width) as f32 * block_h;
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
                let mut cut_site_rects: Vec<(Rect, usize)> = Vec::new();
                for block_idx in 0..n_blocks {
                    let block_y = rect.min.y + block_idx as f32 * block_h;
                    if block_y + block_h < clip.min.y {
                        continue;
                    }
                    if block_y > clip.max.y {
                        break;
                    }
                    let block_start = block_idx * line_width;
                    let block_end = (block_start + line_width).min(seq_len);
                    let top_y = block_y + cut_label_h + ruler_h;
                    let bot_y = top_y + strand_h;
                    let annot_base_y = bot_y + strand_h;

                    for (feat_idx, feat) in annotations.features.iter().enumerate() {
                        let bar_row_y = annot_base_y + feat_row[feat_idx] as f32 * annot_row_h;
                        if let Some(r) = annot_bar_rect(
                            feat, block_start, block_end, bar_row_y, seq_x0, char_width, annot_row_h,
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
                    for (site_idx, site) in view.cut_sites.iter().enumerate() {
                        if site.cut_pos >= block_start && site.cut_pos <= block_end {
                            let cx = seq_x0 + (site.cut_pos - block_start) as f32 * char_width;
                            let label_w = site.enzyme.len() as f32 * label_char_w + 8.0;
                            let row = cut_label_row[site_idx];
                            cut_site_rects.push((
                                Rect::from_center_size(
                                    Pos2::new(cx, block_y + (row as f32 + 0.5) * cut_label_row_h),
                                    Vec2::new(label_w, cut_label_row_h),
                                ),
                                site_idx,
                            ));
                        }
                    }
                }

                // ── Interactions ──────────────────────────────────────────────

                let ptr = response.interact_pointer_pos();
                let ptr_seq = ptr.and_then(|p| {
                    screen_to_seq(p, rect, char_width, line_width, seq_len, block_h, left_margin)
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
                                    Some(sel) => Selection { anchor: sel.anchor, focus: seq_pos },
                                    None => Selection::cursor(seq_pos),
                                };
                                push_sel(cmds, Some(new_sel));
                            }
                        } else if let Some(&(_, feat_idx)) =
                            annot_hits.iter().find(|(r, _)| r.contains(pos))
                        {
                            let feat = &annotations.features[feat_idx];
                            push_sel(cmds, Some(Selection::range(feat.range.start, feat.range.end)));
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
                    let on_hit = ptr.is_some_and(|p| search_hit_rects.iter().any(|(r, _)| r.contains(p)));
                    let on_site = ptr.is_some_and(|p| cut_site_rects.iter().any(|(r, _)| r.contains(p)));
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
                    let block_y = rect.min.y + block_idx as f32 * block_h;
                    if block_y + block_h < clip.min.y {
                        continue;
                    }
                    if block_y > clip.max.y {
                        break;
                    }

                    let block_start = block_idx * line_width;
                    let block_end = (block_start + line_width).min(seq_len);
                    let block_len = block_end - block_start;

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
                        ui, &seq[block_start..block_end], &font_id, 1.0, &cfg.theme,
                    );
                    painter.galley(Pos2::new(seq_x0, top_y), top_galley, text_color);

                    let bot_galley = build_strand_galley(
                        ui, &comp[block_start..block_end], &font_id, 0.65, &cfg.theme,
                    );
                    painter.galley(Pos2::new(seq_x0, bot_y), bot_galley, text_color);

                    // ── Annotation bars (below strands) ───────────────────────
                    for (feat_idx, feat) in annotations.features.iter().enumerate() {
                        let bar_row_y = annot_base_y + feat_row[feat_idx] as f32 * annot_row_h;
                        if let Some(bar) = annot_bar_rect(
                            feat, block_start, block_end, bar_row_y, seq_x0, char_width, annot_row_h,
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
                                    swatch, label_text_light, label_text_dark,
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

                    // ── Cut site staples ──────────────────────────────────────
                    // Label sits in its stacked row above the ruler. The vertical
                    // line descends from the label through both strands at inter-base
                    // x positions (same coordinate system as the cursor). The horizontal
                    // connector at bot_y encodes the overhang; blunt = straight line.
                    for (site_idx, site) in view.cut_sites.iter().enumerate() {
                        let top_cut = site.cut_pos;
                        let bot_cut = site.bottom_cut_pos;
                        if top_cut < block_start || top_cut > block_end {
                            continue;
                        }
                        let tcx = seq_x0 + (top_cut - block_start) as f32 * char_width;
                        let stroke = Stroke::new(1.5, cut_site_color);

                        // Label in its assigned stacking row.
                        let row = cut_label_row[site_idx];
                        let label_y = block_y + row as f32 * cut_label_row_h;
                        painter.text(
                            Pos2::new(tcx, label_y),
                            Align2::CENTER_TOP,
                            &site.enzyme,
                            FontId::proportional((ruler_size - 1.0).max(8.0)),
                            cut_site_color,
                        );

                        // Vertical from bottom of label row down through the top strand.
                        let line_top = block_y + (row + 1) as f32 * cut_label_row_h;
                        painter.line_segment(
                            [Pos2::new(tcx, line_top), Pos2::new(tcx, bot_y)],
                            stroke,
                        );

                        if top_cut == bot_cut {
                            // Blunt: straight line through both strands.
                            painter.line_segment(
                                [Pos2::new(tcx, bot_y), Pos2::new(tcx, bot_y + strand_h)],
                                stroke,
                            );
                        } else if bot_cut >= block_start && bot_cut <= block_end {
                            // Staggered: horizontal step at inter-strand boundary + bottom vertical.
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
                            // Bottom cut in a different block — stub the top half.
                            painter.line_segment(
                                [Pos2::new(tcx, bot_y),
                                 Pos2::new(tcx, bot_y + strand_h * 0.5)],
                                Stroke::new(1.5, cut_site_color.gamma_multiply(0.4)),
                            );
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

                // Visible range for minimap viewport indicator.
                let scroll_top = (clip.min.y - rect.min.y).max(0.0);
                let first_block = (scroll_top / block_h) as usize;
                let last_block = ((scroll_top + clip.height()) / block_h).ceil() as usize;
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
    block_h: f32,
    left_margin: f32,
) -> Option<usize> {
    let rel_x = pos.x - rect.min.x - left_margin;
    let rel_y = pos.y - rect.min.y;
    if rel_x < 0.0 || rel_y < 0.0 {
        return None;
    }
    let block_idx = (rel_y / block_h) as usize;
    let col = (rel_x / char_width) as usize;
    if col >= line_width {
        return None;
    }
    let p = block_idx * line_width + col;
    if p > seq_len { None } else { Some(p) }
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
    painter.text(bar.center(), Align2::CENTER_CENTER, &text, font.clone(), color);
}
