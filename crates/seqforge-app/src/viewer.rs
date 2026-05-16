use egui::{text::LayoutJob, Align2, Color32, FontId, Pos2, Rect, Sense, Stroke, Vec2};
use seqforge_core::{
    Annotations, Buffer, BufferId, Feature, FeatureKind, Selection, Strand, View,
};

use crate::command::{AppCommand, PendingCommand};

const FONT_SIZE: f32 = 13.0;
const ANNOT_ROW_HEIGHT: f32 = 16.0;
const CUT_LABEL_ROW_H: f32 = 14.0;
const RULER_HEIGHT: f32 = 14.0;
const STRAND_HEIGHT: f32 = 17.0;
const BLOCK_GAP: f32 = 14.0;
const LEFT_MARGIN: f32 = 30.0;
const RIGHT_MARGIN: f32 = 20.0;
const SELECTION_COLOR: Color32 = Color32::from_rgb(173, 214, 255);
const CURSOR_COLOR: Color32 = Color32::from_rgb(50, 120, 255);
const CUT_SITE_COLOR: Color32 = Color32::from_rgb(220, 80, 200);
const LABEL_CHAR_W: f32 = (FONT_SIZE - 3.0) * 0.55;

// ── Stacking ─────────────────────────────────────────────────────────────────

/// Core greedy interval stacking (port of seqviz `stackElements`).
/// Sorts ranges by start, then packs each into the first row whose last
/// element ends at or before the current range's start.
/// Returns `(item_idx → row, n_rows)`.
fn greedy_stack(ranges: &[(usize, usize)]) -> (Vec<usize>, usize) {
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

fn stack_cut_labels(sites: &[seqforge_core::CutSite], char_width: f32) -> (Vec<usize>, usize) {
    let ranges: Vec<(usize, usize)> = sites
        .iter()
        .map(|s| {
            let half_px = s.enzyme.len() as f32 * LABEL_CHAR_W * 0.5 + 4.0;
            let half_bases = (half_px / char_width).ceil() as usize + 1;
            (s.cut_pos.saturating_sub(half_bases), s.cut_pos + half_bases)
        })
        .collect();
    greedy_stack(&ranges)
}

// ── Colors ────────────────────────────────────────────────────────────────────

fn feature_color(kind: FeatureKind) -> Color32 {
    match kind {
        FeatureKind::Gene => Color32::from_rgb(100, 149, 237),
        FeatureKind::Cds => Color32::from_rgb(72, 201, 176),
        FeatureKind::Promoter => Color32::from_rgb(241, 196, 15),
        FeatureKind::Terminator => Color32::from_rgb(231, 76, 60),
        FeatureKind::Rep => Color32::from_rgb(155, 89, 182),
        FeatureKind::Source => Color32::from_rgb(149, 165, 166),
        FeatureKind::Misc | FeatureKind::Other => Color32::from_rgb(189, 195, 199),
    }
}

fn base_color(base: u8) -> Color32 {
    match base.to_ascii_uppercase() {
        b'A' => Color32::from_rgb(0, 150, 64),
        b'T' => Color32::from_rgb(200, 30, 60),
        b'G' => Color32::from_rgb(220, 120, 0),
        b'C' => Color32::from_rgb(50, 100, 220),
        _ => Color32::DARK_GRAY,
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
) -> Option<Rect> {
    if feat.range.end <= block_start || feat.range.start >= block_end {
        return None;
    }
    let col_s = feat.range.start.max(block_start) - block_start;
    let col_e = feat.range.end.min(block_end) - block_start;
    Some(Rect::from_min_size(
        Pos2::new(seq_x0 + col_s as f32 * char_width, bar_row_y + 1.0),
        Vec2::new(
            (col_e - col_s) as f32 * char_width,
            ANNOT_ROW_HEIGHT - 2.0,
        ),
    ))
}

// ── Widget state ──────────────────────────────────────────────────────────────

/// Rendering caches and interaction state for the sequence viewer widget.
/// Document data lives on [`Buffer`]; selection / scroll / search live
/// on [`View`]. Caches invalidate on `(buffer_id, buffer.version)` —
/// version-keyed invalidation is the Tier 3a contract, available for
/// free now even though edits don't bump the version yet.
#[derive(Debug, Default)]
pub struct SequenceView {
    drag_start: Option<usize>,
    /// Buffer this widget was last rendered against; differs from
    /// current → tear down all caches.
    cached_buffer_id: Option<BufferId>,
    /// Buffer version at last cache fill. Bumped on every future edit.
    cached_version: u64,
    cached_feat_row: Vec<usize>, // feat_idx → stacked row index
    cached_n_annot_rows: usize,
    // Cut-label cache invalidates on cut-site positions + char_width.
    cached_cut_site_key: Vec<usize>, // sorted cut positions
    cached_char_width: f32,
    cached_cut_label_row: Vec<usize>, // site_idx → stacked row index
    cached_n_cut_label_rows: usize,
}

impl SequenceView {
    /// Clear caches so the next `show()` recomputes from scratch.
    /// Called by `command::apply` on Open / Close so per-doc derived
    /// data doesn't leak across document boundaries.
    pub fn reset(&mut self) {
        self.drag_start = None;
        self.cached_buffer_id = None;
        self.cached_version = 0;
        self.cached_feat_row.clear();
        self.cached_n_annot_rows = 0;
        self.cached_cut_site_key = Vec::new();
        self.cached_char_width = 0.0;
        self.cached_cut_label_row.clear();
        self.cached_n_cut_label_rows = 0;
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
    ) {
        let seq = &buffer.text;
        let seq_len = seq.len();

        if seq_len == 0 {
            ui.centered_and_justified(|ui| {
                ui.label("Empty sequence.");
            });
            return;
        }

        // Version-keyed cache invalidation. Mismatch on `buffer_id` ⇒
        // we're rendering a different buffer (e.g. after tab switch
        // in Stage 2.5b). Mismatch on `version` ⇒ buffer was edited
        // (Tier 3d). Both rebuild from scratch.
        let cache_stale = self.cached_buffer_id != Some(view.buffer_id)
            || self.cached_version != buffer.version;
        if cache_stale {
            let (feat_rows, n_rows) = stack_features(&annotations.features);
            self.cached_feat_row = feat_rows;
            self.cached_n_annot_rows = n_rows;
            self.cached_buffer_id = Some(view.buffer_id);
            self.cached_version = buffer.version;
        }

        // Complement lives on Buffer (computed at load time, recomputed
        // on edit by Buffer's mutator). View-side caching no longer needed.
        let comp = &buffer.complement;
        let feat_row = &self.cached_feat_row;
        let n_annot_rows = self.cached_n_annot_rows;

        let font_id = FontId::monospace(FONT_SIZE);
        let small_font = FontId::proportional(FONT_SIZE - 2.0);

        // Measure char_width from an actual galley so feature bar positions
        // use the same per-character advance that LayoutJob renders, not the
        // single-glyph metric which can differ due to subpixel rounding.
        let (char_width, char_height) = ui.fonts(|f| {
            let probe = f.layout_no_wrap("A".repeat(64), font_id.clone(), Color32::BLACK);
            (probe.rect.width() / 64.0, f.row_height(&font_id))
        });

        // Fit the line width to the available pane width.
        let avail = (ui.available_width() - LEFT_MARGIN - RIGHT_MARGIN).max(char_width);
        let line_width = ((avail / char_width) as usize).max(10);

        let annot_section_h = n_annot_rows as f32 * ANNOT_ROW_HEIGHT;

        // Recompute cut-label stacking when the set of cut positions or char_width changes.
        // Using sorted positions as the key catches same-count enzyme swaps (e.g. EcoRI→BamHI).
        // char_width is pane-width-dependent, so a resize also invalidates the cache.
        let cut_site_key: Vec<usize> = {
            let mut positions: Vec<usize> =
                view.cut_sites.iter().map(|s| s.cut_pos).collect();
            positions.sort_unstable();
            positions
        };
        if cut_site_key != self.cached_cut_site_key
            || (char_width - self.cached_char_width).abs() > 0.5
        {
            let (rows, n_rows) = stack_cut_labels(&view.cut_sites, char_width);
            self.cached_cut_label_row = rows;
            self.cached_n_cut_label_rows = n_rows;
            self.cached_cut_site_key = cut_site_key;
            self.cached_char_width = char_width;
        }
        let cut_label_row = &self.cached_cut_label_row;
        let n_cut_label_rows = self.cached_n_cut_label_rows;
        let cut_label_h = n_cut_label_rows as f32 * CUT_LABEL_ROW_H;

        let block_h = cut_label_h + RULER_HEIGHT + STRAND_HEIGHT * 2.0 + annot_section_h + BLOCK_GAP;
        let n_blocks = seq_len.div_ceil(line_width);
        let total_height = n_blocks as f32 * block_h;
        let content_width = LEFT_MARGIN + line_width as f32 * char_width + RIGHT_MARGIN;
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
        scroll_area.show(ui, |ui| {
                let (response, painter) = ui.allocate_painter(
                    Vec2::new(alloc_width, total_height),
                    Sense::click_and_drag(),
                );
                let rect = response.rect;
                let clip = painter.clip_rect();
                let text_color = ui.visuals().text_color();
                let seq_x0 = rect.min.x + LEFT_MARGIN;

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
                    let top_y = block_y + cut_label_h + RULER_HEIGHT;
                    let bot_y = top_y + STRAND_HEIGHT;
                    let annot_base_y = bot_y + STRAND_HEIGHT;

                    for (feat_idx, feat) in annotations.features.iter().enumerate() {
                        let bar_row_y = annot_base_y + feat_row[feat_idx] as f32 * ANNOT_ROW_HEIGHT;
                        if let Some(r) =
                            annot_bar_rect(feat, block_start, block_end, bar_row_y, seq_x0, char_width)
                        {
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
                                    Vec2::new(sw, STRAND_HEIGHT * 2.0),
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
                            let label_w = site.enzyme.len() as f32 * LABEL_CHAR_W + 8.0;
                            let row = cut_label_row[site_idx];
                            cut_site_rects.push((
                                Rect::from_center_size(
                                    Pos2::new(cx, block_y + (row as f32 + 0.5) * CUT_LABEL_ROW_H),
                                    Vec2::new(label_w, CUT_LABEL_ROW_H),
                                ),
                                site_idx,
                            ));
                        }
                    }
                }

                // ── Interactions ──────────────────────────────────────────────

                let ptr = response.interact_pointer_pos();
                let ptr_seq = ptr.and_then(|p| {
                    screen_to_seq(p, rect, char_width, line_width, seq_len, block_h)
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
                    for col in 0..block_len {
                        let abs = block_start + col;
                        if abs == 0 || (abs + 1) % 10 == 0 {
                            painter.text(
                                Pos2::new(seq_x0 + col as f32 * char_width, ruler_y),
                                Align2::LEFT_TOP,
                                format!("{}", abs + 1),
                                small_font.clone(),
                                text_color.gamma_multiply(0.55),
                            );
                        }
                    }

                    let top_y = ruler_y + RULER_HEIGHT;
                    let bot_y = top_y + STRAND_HEIGHT;
                    let annot_base_y = bot_y + STRAND_HEIGHT;

                    // ── Search hit highlights (behind selection and text) ──────
                    for hit in &view.search_hits {
                        let vis_s = hit.start.max(block_start).min(block_end);
                        let vis_e = hit.end.min(block_end); // clamp wrap-arounds
                        if vis_s < vis_e && vis_e > block_start {
                            let sx = seq_x0 + (vis_s - block_start) as f32 * char_width;
                            let sw = (vis_e - vis_s) as f32 * char_width;
                            let color = search_hit_color(hit.strand);
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
                                        Vec2::new(1.5, STRAND_HEIGHT * 2.0),
                                    ),
                                    0.0,
                                    CURSOR_COLOR,
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
                                    SELECTION_COLOR,
                                );
                                painter.rect_filled(
                                    Rect::from_min_size(
                                        Pos2::new(sx, bot_y),
                                        Vec2::new(sw, char_height),
                                    ),
                                    0.0,
                                    SELECTION_COLOR.gamma_multiply(0.7),
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
                    let top_galley =
                        build_strand_galley(ui, &seq[block_start..block_end], &font_id, 1.0);
                    painter.galley(Pos2::new(seq_x0, top_y), top_galley, text_color);

                    let bot_galley =
                        build_strand_galley(ui, &comp[block_start..block_end], &font_id, 0.65);
                    painter.galley(Pos2::new(seq_x0, bot_y), bot_galley, text_color);

                    // ── Annotation bars (below strands) ───────────────────────
                    for (feat_idx, feat) in annotations.features.iter().enumerate() {
                        let bar_row_y = annot_base_y + feat_row[feat_idx] as f32 * ANNOT_ROW_HEIGHT;
                        if let Some(bar) =
                            annot_bar_rect(feat, block_start, block_end, bar_row_y, seq_x0, char_width)
                        {
                            let is_selected = view.selected_feature == Some(feat_idx);
                            painter.rect_filled(bar, 2.0, feature_color(feat.kind));
                            if is_selected {
                                painter.rect_stroke(
                                    bar,
                                    2.0,
                                    Stroke::new(1.5, Color32::WHITE),
                                    egui::StrokeKind::Inside,
                                );
                            }
                            let label_min_width = feat.label.chars().count() as f32 * char_width;
                            if bar.width() >= label_min_width && !feat.label.is_empty() {
                                painter.text(
                                    bar.center(),
                                    Align2::CENTER_CENTER,
                                    &feat.label,
                                    small_font.clone(),
                                    Color32::WHITE,
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
                        let stroke = Stroke::new(1.5, CUT_SITE_COLOR);

                        // Label in its assigned stacking row.
                        let row = cut_label_row[site_idx];
                        let label_y = block_y + row as f32 * CUT_LABEL_ROW_H;
                        painter.text(
                            Pos2::new(tcx, label_y),
                            Align2::CENTER_TOP,
                            &site.enzyme,
                            FontId::proportional(FONT_SIZE - 3.0),
                            CUT_SITE_COLOR,
                        );

                        // Vertical from bottom of label row down through the top strand.
                        let line_top = block_y + (row + 1) as f32 * CUT_LABEL_ROW_H;
                        painter.line_segment(
                            [Pos2::new(tcx, line_top), Pos2::new(tcx, bot_y)],
                            stroke,
                        );

                        if top_cut == bot_cut {
                            // Blunt: straight line through both strands.
                            painter.line_segment(
                                [Pos2::new(tcx, bot_y), Pos2::new(tcx, bot_y + STRAND_HEIGHT)],
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
                                [Pos2::new(bcx, bot_y), Pos2::new(bcx, bot_y + STRAND_HEIGHT)],
                                stroke,
                            );
                        } else {
                            // Bottom cut in a different block — stub the top half.
                            painter.line_segment(
                                [Pos2::new(tcx, bot_y),
                                 Pos2::new(tcx, bot_y + STRAND_HEIGHT * 0.5)],
                                Stroke::new(1.5, CUT_SITE_COLOR.gamma_multiply(0.4)),
                            );
                        }
                    }

                    // ── Block separator ───────────────────────────────────────
                    if block_idx + 1 < n_blocks {
                        painter.hline(
                            rect.min.x..=rect.min.x + content_width,
                            block_y + block_h - BLOCK_GAP * 0.5,
                            Stroke::new(0.5, text_color.gamma_multiply(0.08)),
                        );
                    }
                }
            });
    }
}

// ── Free helpers ──────────────────────────────────────────────────────────────

fn screen_to_seq(
    pos: Pos2,
    rect: Rect,
    char_width: f32,
    line_width: usize,
    seq_len: usize,
    block_h: f32,
) -> Option<usize> {
    let rel_x = pos.x - rect.min.x - LEFT_MARGIN;
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
    if p >= seq_len { None } else { Some(p) }
}

fn search_hit_color(strand: Strand) -> Color32 {
    match strand {
        // amber for forward, cyan for reverse — semi-transparent via gamma
        Strand::Forward => Color32::from_rgba_unmultiplied(255, 190, 0, 110),
        Strand::Reverse => Color32::from_rgba_unmultiplied(0, 190, 255, 110),
        _ => Color32::from_rgba_unmultiplied(200, 200, 200, 90),
    }
}

fn build_strand_galley(
    ui: &egui::Ui,
    bases: &[u8],
    font_id: &FontId,
    alpha: f32,
) -> std::sync::Arc<egui::Galley> {
    let mut job = LayoutJob::default();
    for &b in bases {
        job.append(
            &(b.to_ascii_uppercase() as char).to_string(),
            0.0,
            egui::text::TextFormat {
                font_id: font_id.clone(),
                color: base_color(b).gamma_multiply(alpha),
                ..Default::default()
            },
        );
    }
    ui.fonts(|f| f.layout_job(job))
}
