//! Compact sequence overview panel rendered below the file browser.
//!
//! Topology-aware: circular sequences render as a plasmid ring with
//! feature arcs; linear sequences render as a proportional bar with
//! feature rectangles. Both modes support click-to-navigate via the
//! existing `AppCommand::Viewer(GoTo{...})` path.
//!
//! The panel is non-focusable and never mutates state directly. All
//! geometry is cached behind `(BufferId, buffer.version)` — the same
//! key used by `SequenceView::feature_cache` — so it recomputes for
//! free once the editor lands and starts bumping `buffer.version`.

use std::f32::consts::{PI, TAU};

use egui::{Color32, Pos2, Rect, Sense, Shape, Stroke, Vec2};
use seqforge_core::{Annotations, BufferId, FeatureId, FeatureKind, Strand, ViewerRequest};

use crate::cache::Cache;
use crate::command::{AppCommand, PendingCommand};
use crate::config::{Config, MinimapSettings};
use crate::workspace::Workspace;

// ── Cached geometry ───────────────────────────────────────────────────────────

/// Pre-computed paint geometry for one minimap frame. Rebuilt only
/// when `(buffer_id, buffer.version)` changes. Panel size is baked
/// into the geometry so a resize also triggers a rebuild (the version
/// key is extended with a quantised panel size in `show()`).
#[derive(Clone)]
struct MinimapGeom {
    is_circular: bool,
    seq_len: usize,
    /// Circular topology: one arc per feature that passed the LOD filter.
    arcs: Vec<PaintArc>,
    /// Linear topology: one bar per feature that passed the LOD filter.
    bars: Vec<PaintBar>,
}

/// A pre-tessellated arc approximated as a polyline.
#[derive(Clone)]
struct PaintArc {
    /// Arc centre coordinates relative to the panel's top-left (not
    /// absolute screen coords). Translated to screen in `show()`.
    points: Vec<Pos2>,
    color: Color32,
    feat_id: FeatureId,
    strand: Strand,
}

/// A scaled feature rectangle, coordinates relative to panel top-left.
#[derive(Clone)]
struct PaintBar {
    rect: Rect,
    color: Color32,
    feat_id: FeatureId,
    strand: Strand,
}

// ── Geometry builders ─────────────────────────────────────────────────────────

/// Build arc geometry for a circular sequence. `panel_size` is the
/// side length of the square panel in logical pixels.
fn build_circular_geom(
    ann: &Annotations,
    seq_len: usize,
    panel_size: f32,
    settings: &MinimapSettings,
    theme: &crate::config::Theme,
) -> MinimapGeom {
    let center = Pos2::new(panel_size / 2.0, panel_size / 2.0);
    let radius = panel_size * 0.38;

    let mut arcs = Vec::with_capacity(ann.len());
    for feat in ann.iter() {
        let start_a = angle_for_pos(feat.range.start, seq_len);
        let end_a = angle_for_pos(feat.range.end, seq_len);

        // Angular span — handle wrap-around (feature crossing origin)
        let mut span = end_a - start_a;
        if span < 0.0 {
            span += TAU;
        }
        let span_deg = span.to_degrees();

        if span_deg < settings.min_arc_degrees {
            continue; // LOD: too small to see
        }

        // Number of polyline segments: ~1 per 3°, minimum 2.
        let n_segs = ((span_deg / 3.0).ceil() as usize).max(2);
        let mut points = Vec::with_capacity(n_segs + 1);
        for i in 0..=n_segs {
            let t = i as f32 / n_segs as f32;
            let a = start_a + t * span;
            points.push(Pos2::new(
                center.x + a.cos() * radius,
                center.y + a.sin() * radius,
            ));
        }

        arcs.push(PaintArc {
            points,
            color: theme.feature_color(FeatureKind::classify(&feat.raw_kind)),
            feat_id: feat.id,
            strand: feat.strand,
        });
    }

    MinimapGeom {
        is_circular: true,
        seq_len,
        arcs,
        bars: vec![],
    }
}

/// Build bar geometry for a linear sequence.
fn build_linear_geom(
    ann: &Annotations,
    seq_len: usize,
    panel_width: f32,
    settings: &MinimapSettings,
    theme: &crate::config::Theme,
) -> MinimapGeom {
    // Feature rows packed identically to the text viewer's stacking.
    let ranges: Vec<(usize, usize)> = ann.iter().map(|f| (f.range.start, f.range.end)).collect();
    let (row_assign, _n_rows) = crate::viewer::greedy_stack(&ranges);

    let mut bars = Vec::with_capacity(ann.len());
    // `feat_idx` is a within-frame render detail (indexes `row_assign`); the
    // stored handle is the stable `feat.id`.
    for (feat_idx, feat) in ann.iter().enumerate() {
        let x = (feat.range.start as f32 / seq_len as f32) * panel_width;
        let w = ((feat.range.end - feat.range.start) as f32 / seq_len as f32) * panel_width;

        if w < settings.min_bar_width {
            continue; // LOD: sub-pixel
        }

        let row = row_assign[feat_idx];
        let y = settings.linear_spine_height
            + settings.spine_feature_gap
            + row as f32 * (settings.linear_feature_row_height + 1.0);

        bars.push(PaintBar {
            rect: Rect::from_min_size(
                Pos2::new(x, y),
                Vec2::new(
                    w.max(settings.min_bar_width),
                    settings.linear_feature_row_height,
                ),
            ),
            color: theme.feature_color(FeatureKind::classify(&feat.raw_kind)),
            feat_id: feat.id,
            strand: feat.strand,
        });
    }

    MinimapGeom {
        is_circular: false,
        seq_len,
        arcs: vec![],
        bars,
    }
}

/// Convert a sequence position to an angle on the ring.
/// `0` maps to the top (`-PI/2`), increasing clockwise.
#[inline]
fn angle_for_pos(pos: usize, seq_len: usize) -> f32 {
    (pos as f32 / seq_len as f32) * TAU - PI / 2.0
}

// ── MiniMap widget ────────────────────────────────────────────────────────────

/// Retained state for the minimap panel.
#[derive(Default)]
pub struct MiniMap {
    /// `(buffer_id, buffer.version, panel_size_q, config_epoch)` → cached geometry.
    /// `panel_size_q` is `(panel_size * 2.0).round() as u32` so sub-0.5px
    /// resize noise doesn't thrash the cache. `config_epoch` bumps on
    /// `ReloadConfig` so a theme/sizing change re-tessellates.
    geom_cache: Cache<(BufferId, u64, u32, u64), MinimapGeom>,
}

impl MiniMap {
    /// Render the minimap panel. `cmds` receives `GoTo` commands on click.
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        workspace: &mut Workspace,
        cmds: &mut Vec<PendingCommand>,
        cfg: &Config,
    ) {
        // ── No document open ─────────────────────────────────────────────────
        if workspace.active_view().is_none() {
            ui.vertical_centered(|ui| {
                ui.add_space(8.0);
                ui.label(
                    egui::RichText::new("No file open")
                        .small()
                        .color(ui.visuals().weak_text_color()),
                );
            });
            return;
        }

        // ── Resolve active buffer (read-only) ────────────────────────────────
        // Snapshot the data we need for painting and click-mapping. The
        // closure releases the read lock before we do any painting so
        // the borrow checker stays happy while we mutate `self` and `cmds`.
        struct Snap {
            buffer_id: BufferId,
            version: u64,
            seq_len: usize,
            is_circular: bool,
            cursor_pos: usize,
            selection: Option<seqforge_core::Selection>,
            selected_feature: Option<FeatureId>,
            /// Display label — file basename when the buffer is backed
            /// by a file, otherwise the sequence name from the record.
            display_name: String,
            visible_range: Option<(usize, usize)>,
        }

        let snap = workspace
            .with_active_buffer(|view, buf, _ann| Snap {
                buffer_id: view.buffer_id,
                version: buf.version,
                seq_len: buf.len(),
                is_circular: buf.is_circular(),
                cursor_pos: view.selection.text_range().map(|s| s.anchor).unwrap_or(0),
                selection: view.selection.text_range(),
                selected_feature: view.selection.selected_feature(),
                display_name: crate::workspace::display_name(buf),
                visible_range: view.visible_range,
            })
            .ok();

        let Some(snap) = snap else { return };
        if snap.seq_len == 0 {
            return;
        }

        // ── Header label: name + bp count ────────────────────────────────────
        ui.add_space(4.0);
        ui.vertical_centered(|ui| {
            let topology = if snap.is_circular {
                "circular"
            } else {
                "linear"
            };
            ui.add(egui::Label::new(egui::RichText::new(&snap.display_name).strong()).truncate());
            // Use the regular text colour at slight de-emphasis instead
            // of `weak_text_color`, which is too faded against the panel
            // background to read at a glance.
            ui.label(
                egui::RichText::new(format!("{} bp  ·  {topology}", snap.seq_len))
                    .color(ui.visuals().text_color().gamma_multiply(0.85)),
            );
        });
        ui.add_space(4.0);

        // ── Allocate panel ───────────────────────────────────────────────────
        // Fill the available pane rather than capping — the pane is already
        // user-resizable so dynamic sizing is the correct behaviour.
        let available_w = ui.available_width().max(60.0);
        let available_h = ui.available_height();
        let m = &cfg.settings.minimap;
        let (geom_dim, panel_w, panel_h) = if snap.is_circular {
            // Keep the ring square; fit inside whichever dimension is smaller.
            let size = available_w.min(available_h).max(60.0);
            (size, size, size)
        } else {
            // Linear: full width, fixed height (capped so it doesn't overflow).
            let h = (m.linear_spine_height
                + m.spine_feature_gap
                + 4.0 * (m.linear_feature_row_height + 1.0)
                + 8.0)
                .min(available_h);
            (available_w, available_w, h)
        };

        // Center the circular ring in the available space. `vertical_centered`
        // sets a center-aligned horizontal layout so the square sits mid-pane
        // when the pane is wider than it is tall.
        let (response, painter) = if snap.is_circular {
            // Vertical offset so the ring is mid-pane when height >> width.
            let v_pad = ((available_h - panel_h) / 2.0).max(0.0);
            ui.add_space(v_pad);
            let mut slot = None;
            ui.vertical_centered(|ui| {
                slot = Some(ui.allocate_painter(Vec2::new(panel_w, panel_h), Sense::click()));
            });
            slot.unwrap()
        } else {
            ui.allocate_painter(Vec2::new(panel_w, panel_h), Sense::click())
        };
        let rect = response.rect;

        // ── Build / reuse geometry ───────────────────────────────────────────
        // Quantise the relevant dimension to 0.5px steps so minor resize jitter
        // doesn't thrash the cache while still triggering rebuilds on meaningful
        // size changes — same strategy as SequenceView::cut_label_cache.
        let panel_size_q = (geom_dim * 2.0).round() as u32;
        let cache_key = (snap.buffer_id, snap.version, panel_size_q, cfg.epoch);

        let geom: MinimapGeom = workspace
            .with_active_buffer(|_view, buf, ann| {
                self.geom_cache
                    .get_or_compute(cache_key, || {
                        if buf.is_circular() {
                            build_circular_geom(
                                ann,
                                buf.len(),
                                geom_dim,
                                &cfg.settings.minimap,
                                &cfg.theme,
                            )
                        } else {
                            build_linear_geom(
                                ann,
                                buf.len(),
                                geom_dim,
                                &cfg.settings.minimap,
                                &cfg.theme,
                            )
                        }
                    })
                    .clone()
            })
            .unwrap_or_else(|_| MinimapGeom {
                is_circular: snap.is_circular,
                seq_len: snap.seq_len,
                arcs: vec![],
                bars: vec![],
            });

        // ── Paint ────────────────────────────────────────────────────────────
        let spine_color = ui.visuals().text_color().gamma_multiply(0.4);

        if geom.is_circular {
            paint_circular(
                &painter,
                rect,
                &geom,
                &snap.selection,
                snap.selected_feature,
                snap.cursor_pos,
                snap.visible_range,
                spine_color,
                &cfg.settings.minimap,
                &cfg.theme,
            );
        } else {
            paint_linear(
                &painter,
                rect,
                &geom,
                &snap.selection,
                snap.selected_feature,
                snap.cursor_pos,
                snap.visible_range,
                panel_w,
                spine_color,
                &cfg.settings.minimap,
                &cfg.theme,
            );
        }

        // ── Click-to-navigate ────────────────────────────────────────────────
        if response.clicked() {
            if let Some(click) = response.interact_pointer_pos() {
                let seq_pos = if geom.is_circular {
                    let center = rect.center();
                    let delta = click - center;
                    let angle = delta.y.atan2(delta.x) + PI / 2.0;
                    let frac = ((angle / TAU) + 1.0) % 1.0;
                    ((frac * snap.seq_len as f32) as usize + 1).clamp(1, snap.seq_len)
                } else {
                    let frac = ((click.x - rect.min.x) / panel_w).clamp(0.0, 1.0);
                    ((frac * snap.seq_len as f32) as usize + 1).clamp(1, snap.seq_len)
                };
                cmds.push((
                    AppCommand::Viewer(ViewerRequest::GoTo {
                        position: seq_pos,
                        view: None,
                    }),
                    None,
                ));
            }
        }
    }
}

// ── Circular painter ──────────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)] // render helper: rects, colors, fractions are all natural params
fn paint_circular(
    painter: &egui::Painter,
    rect: Rect,
    geom: &MinimapGeom,
    selection: &Option<seqforge_core::Selection>,
    selected_feature: Option<FeatureId>,
    cursor_pos: usize,
    visible_range: Option<(usize, usize)>,
    spine_color: Color32,
    settings: &MinimapSettings,
    theme: &crate::config::Theme,
) {
    let center = rect.center();
    let panel_size = rect.width().min(rect.height());
    let radius = panel_size * 0.38;
    let seq_len = geom.seq_len;
    let offset = rect.min.to_vec2();
    let spine_w = settings.spine_stroke;
    let feat_w = settings.feature_arc_width;
    let sel_feat_w = settings.selected_border;
    let cursor_tick = settings.cursor_tick_length;

    // Backbone ring
    painter.circle_stroke(center, radius, Stroke::new(spine_w, spine_color));

    // Viewport highlight arc (behind features)
    if let Some((vs, ve)) = visible_range {
        paint_arc_range(
            painter,
            center,
            radius,
            vs,
            ve,
            seq_len,
            Stroke::new(spine_w + 8.0, theme.minimap.viewport.0),
        );
    }

    // Feature arcs (normal, non-selected first)
    for arc in &geom.arcs {
        if Some(arc.feat_id) == selected_feature {
            continue; // drawn on top below
        }
        let pts: Vec<Pos2> = arc.points.iter().map(|p| rect.min + p.to_vec2()).collect();
        painter.add(Shape::line(pts, Stroke::new(feat_w, arc.color)));
        if let Some(shape) = arc_arrowhead(arc, arc.strand, offset, arc.color) {
            painter.add(shape);
        }
    }

    // Selection range highlight
    if let Some(sel) = selection {
        if !sel.is_cursor() {
            let (s, e) = sel.ordered();
            let sel_color = theme.minimap.selection.0;
            paint_arc_range(
                painter,
                center,
                radius + feat_w * 0.5,
                s,
                e,
                seq_len,
                Stroke::new(feat_w + 4.0, sel_color),
            );
        }
    }

    // Selected feature on top (white border + arrowhead)
    if let Some(sel_idx) = selected_feature {
        if let Some(arc) = geom.arcs.iter().find(|a| a.feat_id == sel_idx) {
            let pts: Vec<Pos2> = arc.points.iter().map(|p| rect.min + p.to_vec2()).collect();
            painter.add(Shape::line(pts.clone(), Stroke::new(feat_w, arc.color)));
            painter.add(Shape::line(pts, Stroke::new(sel_feat_w, Color32::WHITE)));
            if let Some(shape) = arc_arrowhead(arc, arc.strand, offset, arc.color) {
                painter.add(shape);
            }
        }
    }

    // Cursor tick
    let cursor_a = angle_for_pos(cursor_pos, seq_len);
    let dir = Vec2::new(cursor_a.cos(), cursor_a.sin());
    painter.line_segment(
        [
            center + dir * (radius - cursor_tick),
            center + dir * (radius + cursor_tick),
        ],
        Stroke::new(2.0, theme.minimap.cursor.0),
    );
}

/// Arrowhead triangle at the terminal end of a feature arc.
/// Arc points are panel-relative; `offset` translates to screen space.
fn arc_arrowhead(arc: &PaintArc, strand: Strand, offset: Vec2, color: Color32) -> Option<Shape> {
    let pts = &arc.points;
    if pts.len() < 2 {
        return None;
    }
    let (tip, dir) = match strand {
        Strand::Forward => {
            let a = pts[pts.len() - 2] + offset;
            let b = pts[pts.len() - 1] + offset;
            (b, (b - a).normalized())
        }
        Strand::Reverse => {
            let a = pts[1] + offset;
            let b = pts[0] + offset;
            (b, (b - a).normalized())
        }
        _ => return None,
    };
    let perp = Vec2::new(-dir.y, dir.x);
    Some(Shape::convex_polygon(
        vec![tip + dir * 5.0, tip - perp * 3.0, tip + perp * 3.0],
        color,
        Stroke::NONE,
    ))
}

/// Paint an arc covering sequence positions `[start, end)` at the given radius.
fn paint_arc_range(
    painter: &egui::Painter,
    center: Pos2,
    radius: f32,
    start: usize,
    end: usize,
    seq_len: usize,
    stroke: Stroke,
) {
    let start_a = angle_for_pos(start, seq_len);
    let end_a = angle_for_pos(end, seq_len);
    let mut span = end_a - start_a;
    if span < 0.0 {
        span += TAU;
    }
    let n_segs = ((span.to_degrees() / 3.0).ceil() as usize).max(2);
    let pts: Vec<Pos2> = (0..=n_segs)
        .map(|i| {
            let a = start_a + (i as f32 / n_segs as f32) * span;
            Pos2::new(center.x + a.cos() * radius, center.y + a.sin() * radius)
        })
        .collect();
    painter.add(Shape::line(pts, stroke));
}

// ── Linear painter ────────────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)] // render helper: rects, colors, fractions are all natural params
fn paint_linear(
    painter: &egui::Painter,
    rect: Rect,
    geom: &MinimapGeom,
    selection: &Option<seqforge_core::Selection>,
    selected_feature: Option<FeatureId>,
    cursor_pos: usize,
    visible_range: Option<(usize, usize)>,
    panel_width: f32,
    spine_color: Color32,
    settings: &MinimapSettings,
    theme: &crate::config::Theme,
) {
    let origin = rect.min;
    let seq_len = geom.seq_len;
    let panel_h = rect.height();
    let spine_h = settings.linear_spine_height;
    let sel_feat_w = settings.selected_border;

    // Viewport highlight (behind everything)
    if let Some((vs, ve)) = visible_range {
        let vx = origin.x + (vs as f32 / seq_len as f32) * panel_width;
        let vw = ((ve - vs) as f32 / seq_len as f32) * panel_width;
        let vp_rect = Rect::from_min_size(Pos2::new(vx, origin.y), Vec2::new(vw.max(2.0), panel_h));
        painter.rect_filled(vp_rect, 0.0, theme.minimap.viewport.0);
        painter.rect_stroke(
            vp_rect,
            0.0,
            Stroke::new(1.0, theme.minimap.cursor.0.gamma_multiply(0.4)),
            egui::StrokeKind::Inside,
        );
    }

    // Backbone bar
    let spine_rect = Rect::from_min_size(origin, Vec2::new(panel_width, spine_h));
    painter.rect_filled(spine_rect, 2.0, spine_color);

    // Feature bars + strand arrowheads
    for bar in &geom.bars {
        let r = Rect::from_min_size(origin + bar.rect.min.to_vec2(), bar.rect.size());
        painter.rect_filled(r, 1.0, bar.color);
        if Some(bar.feat_id) == selected_feature {
            painter.rect_stroke(
                r,
                1.0,
                Stroke::new(sel_feat_w, Color32::WHITE),
                egui::StrokeKind::Inside,
            );
        }
        if r.width() >= 6.0 {
            if let Some(shape) = bar_arrowhead(r, bar.strand, bar.color) {
                painter.add(shape);
            }
        }
    }

    // Selection range highlight over spine
    if let Some(sel) = selection {
        if !sel.is_cursor() {
            let (s, e) = sel.ordered();
            let sx = origin.x + (s as f32 / seq_len as f32) * panel_width;
            let ex = origin.x + (e as f32 / seq_len as f32) * panel_width;
            painter.rect_filled(
                Rect::from_x_y_ranges(sx..=ex, origin.y..=(origin.y + spine_h)),
                0.0,
                theme.minimap.selection.0,
            );
        }
    }

    // Cursor line
    let cx = origin.x + (cursor_pos as f32 / seq_len as f32) * panel_width;
    painter.vline(
        cx,
        origin.y..=(origin.y + spine_h),
        Stroke::new(1.5, theme.minimap.cursor.0),
    );
}

/// Arrowhead triangle appended to a linear feature bar.
fn bar_arrowhead(bar_rect: Rect, strand: Strand, color: Color32) -> Option<Shape> {
    let h = bar_rect.height();
    let aw = (h * 1.2).min(6.0);
    let mid_y = bar_rect.center().y;
    match strand {
        Strand::Forward => Some(Shape::convex_polygon(
            vec![
                Pos2::new(bar_rect.max.x + aw, mid_y),
                Pos2::new(bar_rect.max.x, mid_y - h * 0.5),
                Pos2::new(bar_rect.max.x, mid_y + h * 0.5),
            ],
            color,
            Stroke::NONE,
        )),
        Strand::Reverse => Some(Shape::convex_polygon(
            vec![
                Pos2::new(bar_rect.min.x - aw, mid_y),
                Pos2::new(bar_rect.min.x, mid_y - h * 0.5),
                Pos2::new(bar_rect.min.x, mid_y + h * 0.5),
            ],
            color,
            Stroke::NONE,
        )),
        _ => None,
    }
}
