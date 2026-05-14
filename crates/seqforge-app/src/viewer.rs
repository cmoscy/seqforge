use egui::{text::LayoutJob, Align2, Color32, FontId, Pos2, Rect, Sense, Stroke, Vec2};
use seqforge_bio::complement;
use seqforge_core::{Feature, FeatureKind, Selection, ViewerState};

const FONT_SIZE: f32 = 13.0;
const ANNOT_ROW_HEIGHT: f32 = 16.0; // height per stacked annotation row
const RULER_HEIGHT: f32 = 14.0;
const STRAND_HEIGHT: f32 = 17.0;
const BLOCK_GAP: f32 = 14.0;
const LEFT_MARGIN: f32 = 30.0;
const RIGHT_MARGIN: f32 = 20.0;
const SELECTION_COLOR: Color32 = Color32::from_rgb(173, 214, 255);
const CURSOR_COLOR: Color32 = Color32::from_rgb(50, 120, 255);

// ── Stacking ─────────────────────────────────────────────────────────────────

/// Assign features to non-overlapping rows (port of seqviz `stackElements`).
/// Sorts by start (then end), then greedily packs each feature into the first
/// row whose last element ends at or before the current feature's start.
/// Returns `rows[row_idx] = [feat_idx, …]`.
fn stack_features(features: &[Feature]) -> Vec<Vec<usize>> {
    if features.is_empty() {
        return vec![];
    }
    let mut order: Vec<usize> = (0..features.len()).collect();
    order.sort_by(|&a, &b| {
        features[a]
            .range
            .start
            .cmp(&features[b].range.start)
            .then(features[a].range.end.cmp(&features[b].range.end))
    });

    let mut rows: Vec<Vec<usize>> = Vec::new();
    let mut row_ends: Vec<usize> = Vec::new();

    for feat_idx in order {
        let start = features[feat_idx].range.start;
        let end = features[feat_idx].range.end;
        match row_ends.iter().position(|&e| e <= start) {
            Some(r) => {
                rows[r].push(feat_idx);
                row_ends[r] = end;
            }
            None => {
                rows.push(vec![feat_idx]);
                row_ends.push(end);
            }
        }
    }
    rows
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
/// Selection and document data live in `ViewerState` (seqforge-core).
#[derive(Debug, Default)]
pub struct SequenceView {
    drag_start: Option<usize>,
    // Cached per-document values — recomputed only when seq_len changes.
    cached_seq_len: usize,
    cached_complement: Vec<u8>,
    cached_feat_row: Vec<usize>, // feat_idx → stacked row index
    cached_n_annot_rows: usize,
}

impl SequenceView {
    /// Call when a new document is loaded so stale caches are cleared.
    pub fn reset(&mut self) {
        self.drag_start = None;
        self.cached_seq_len = 0;
        self.cached_complement.clear();
        self.cached_feat_row.clear();
        self.cached_n_annot_rows = 0;
    }

    /// Render the sequence viewer. Selection and document data are read/written
    /// through `vstate` so dispatch_viewer can mutate them independently.
    pub fn show(&mut self, ui: &mut egui::Ui, vstate: &mut ViewerState) {
        let doc = match vstate.open_doc.as_ref() {
            Some(d) => d,
            None => {
                ui.centered_and_justified(|ui| {
                    ui.label("No file open.\nDouble-click a .gb or .fasta file in the browser.");
                });
                return;
            }
        };
        let seq = &doc.sequence;
        let seq_len = seq.len();

        if seq_len == 0 {
            ui.centered_and_justified(|ui| {
                ui.label("Empty sequence.");
            });
            return;
        }

        // Recompute doc-derived values only when the document changes.
        if self.cached_seq_len != seq_len {
            self.cached_complement = complement(seq);
            let stacked_rows = stack_features(&doc.features);
            self.cached_n_annot_rows = stacked_rows.len();
            self.cached_feat_row = vec![0usize; doc.features.len()];
            for (row_idx, row) in stacked_rows.iter().enumerate() {
                for &feat_idx in row {
                    self.cached_feat_row[feat_idx] = row_idx;
                }
            }
            self.cached_seq_len = seq_len;
        }

        let comp = &self.cached_complement;
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

        let block_h = annot_section_h + RULER_HEIGHT + STRAND_HEIGHT * 2.0 + BLOCK_GAP;
        let n_blocks = seq_len.div_ceil(line_width);
        let total_height = n_blocks as f32 * block_h;
        let content_width = LEFT_MARGIN + line_width as f32 * char_width + RIGHT_MARGIN;
        let alloc_width = content_width.max(ui.available_width());

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let (response, painter) = ui.allocate_painter(
                    Vec2::new(alloc_width, total_height),
                    Sense::click_and_drag(),
                );
                let rect = response.rect;
                let clip = painter.clip_rect();
                let text_color = ui.visuals().text_color();
                let seq_x0 = rect.min.x + LEFT_MARGIN;

                // ── Pass 1: collect annotation hit-rects for visible blocks ──

                let mut annot_hits: Vec<(Rect, usize)> = Vec::new();
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
                    let annot_base_y =
                        block_y + RULER_HEIGHT + STRAND_HEIGHT * 2.0;
                    for (feat_idx, feat) in doc.features.iter().enumerate() {
                        let bar_row_y = annot_base_y + feat_row[feat_idx] as f32 * ANNOT_ROW_HEIGHT;
                        if let Some(r) =
                            annot_bar_rect(feat, block_start, block_end, bar_row_y, seq_x0, char_width)
                        {
                            annot_hits.push((r, feat_idx));
                        }
                    }
                }

                // ── Interactions ──────────────────────────────────────────────

                let ptr = response.interact_pointer_pos();
                let ptr_seq = ptr.and_then(|p| {
                    screen_to_seq(p, rect, char_width, line_width, seq_len, block_h)
                });

                if response.clicked() {
                    if let Some(pos) = ptr {
                        if let Some(&(_, feat_idx)) =
                            annot_hits.iter().find(|(r, _)| r.contains(pos))
                        {
                            // Click on annotation bar → feature range selection.
                            let feat = &doc.features[feat_idx];
                            vstate.selection =
                                Some(Selection::range(feat.range.start, feat.range.end));
                            vstate.selected_feature = Some(feat_idx);
                        } else if let Some(seq_pos) = ptr_seq {
                            // Click on strand → place cursor.
                            vstate.selection = Some(Selection::cursor(seq_pos));
                            vstate.selected_feature = None;
                        } else {
                            vstate.selection = None;
                            vstate.selected_feature = None;
                        }
                    }
                }

                if response.drag_started() {
                    let on_annot = ptr.is_some_and(|p| annot_hits.iter().any(|(r, _)| r.contains(p)));
                    if !on_annot {
                        self.drag_start = ptr_seq;
                        vstate.selected_feature = None;
                        vstate.selection = ptr_seq.map(Selection::cursor);
                    }
                }
                if response.dragged() {
                    if let (Some(anchor), Some(focus)) = (self.drag_start, ptr_seq) {
                        vstate.selection = Some(Selection { anchor, focus });
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
                    let ruler_y = block_y;
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

                    // ── Selection highlight / cursor (behind text) ────────────
                    if let Some(sel) = vstate.selection {
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
                    for (feat_idx, feat) in doc.features.iter().enumerate() {
                        let bar_row_y = annot_base_y + feat_row[feat_idx] as f32 * ANNOT_ROW_HEIGHT;
                        if let Some(bar) =
                            annot_bar_rect(feat, block_start, block_end, bar_row_y, seq_x0, char_width)
                        {
                            let is_selected = vstate.selected_feature == Some(feat_idx);
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
