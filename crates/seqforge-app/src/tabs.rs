use std::sync::Arc;

use seqforge_core::ViewId;
use serde::{Deserialize, Serialize};

use crate::command::{AppCommand, PendingCommand};
use crate::config::Config;
use crate::focus::{FocusScope, FocusState};
use crate::overlay::{self, OverlayStack};
use crate::workspace::Workspace;

/// A leaf in the **center** egui_dock tree.
///
/// The shell regions (Files / Terminal / Inspector) are native panels now
/// (decision 19); the dock manages only the center editor tab strip. Viewer
/// tabs are addressed by `ViewId` directly (egui_dock handles switching,
/// drag-rearrange, split-across-leaves). `Welcome` is the placeholder shown when
/// no view is open, so the center never becomes an empty void.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Tab {
    Welcome,
    View(ViewId),
}

pub struct TabViewer<'a> {
    pub workspace: &'a mut Workspace,
    pub pending_commands: &'a mut Vec<PendingCommand>,
    pub overlays: &'a mut OverlayStack,
    pub focus: &'a mut FocusState,
    /// In-memory clipboard bytes (`AppState.clipboard`), passed to the sequence
    /// viewer so a staged Paste can preview the clipboard contents.
    pub clipboard: Option<&'a [u8]>,
    /// Per-frame snapshot of the active config; cheap to clone.
    pub config: Arc<Config>,
}

impl egui_dock::TabViewer for TabViewer<'_> {
    type Tab = Tab;

    fn title(&mut self, tab: &mut Tab) -> egui::WidgetText {
        match tab {
            Tab::Welcome => "Welcome".into(),
            Tab::View(vid) => {
                let name = self.workspace.view(*vid).and_then(|v| {
                    let arc = self.workspace.buffers.get(v.buffer_id)?;
                    let buf = arc.read().ok()?;
                    Some(crate::workspace::display_name(&buf))
                });
                // Focus cue is egui_dock's native tab styling: `Style::from_egui`
                // colors the focused leaf's tab with `strong_text_color()` (white)
                // and the rest with `text_color()` (grey). No hand-painting here.
                name.unwrap_or_else(|| "Untitled".to_string()).into()
            }
        }
    }

    /// View tabs are user-closeable; everything else is structural.
    fn closeable(&mut self, tab: &mut Tab) -> bool {
        matches!(tab, Tab::View(_))
    }

    /// Route the dock's × button through our command system so close
    /// behaviour stays identical to ⌘W (event emission, buffer
    /// cleanup, focus refocus). Returning false tells egui_dock not
    /// to remove the tab from the tree — `CloseTab` does it via
    /// `dock_state.remove_tab` inside `apply()`.
    fn on_close(&mut self, tab: &mut Tab) -> bool {
        if let Tab::View(vid) = tab {
            self.pending_commands
                .push((AppCommand::CloseTab { view: *vid }, None));
            return false;
        }
        true
    }

    fn ui(&mut self, ui: &mut egui::Ui, tab: &mut Tab) {
        let pane_rect = ui.max_rect();
        let pane_scope = match tab {
            Tab::Welcome => FocusScope::View(ViewId(0)),
            Tab::View(vid) => FocusScope::View(*vid),
        };

        match tab {
            Tab::Welcome => {
                if let Some(cmd) = overlay::show_inline_bar(self.overlays, ui) {
                    self.pending_commands.push((cmd, None));
                }
                ui.centered_and_justified(|ui| {
                    ui.vertical_centered(|ui| {
                        ui.add_space(20.0);
                        ui.heading("SeqForge");
                        ui.add_space(8.0);
                        ui.label("No file open.");
                        ui.label("Double-click a .gb or .fasta file in the browser,");
                        ui.label("or press ⌘O to open one.");
                    });
                });
            }
            Tab::View(vid) => {
                let view_id = *vid;
                // The Find / GoTo bar anchors to the workspace's active
                // view — render it only in that pane so it's
                // unambiguous which view the bar will operate on. When
                // focus is on Browser / Terminal the bar still shows
                // in the active viewer pane (matches what the command
                // will actually act on).
                let bar_target = self.workspace.active_view == Some(view_id);
                if bar_target {
                    if let Some(cmd) = overlay::show_inline_bar(self.overlays, ui) {
                        self.pending_commands.push((cmd, None));
                    }
                }
                // App-level focus for this pane — gates in-canvas editing, the
                // same way the terminal gates on `FocusScope::Terminal`. No
                // staging while an overlay (Find/GoTo bar) owns the keyboard.
                // Computed before the `self` destructure below borrows it.
                let view_focused =
                    self.focus.scope == FocusScope::View(view_id) && self.overlays.is_empty();
                // `Option<&[u8]>` is `Copy`; read it out before the destructure.
                let clipboard = self.clipboard;
                let TabViewer {
                    workspace,
                    pending_commands,
                    config,
                    ..
                } = self;
                // ViewKind dispatch (Stage 2.5d). Today only `TextView`
                // exists; adding `LinearView` / `CircularView` later is
                // a new arm + a new renderer module — no changes
                // elsewhere. The pattern matches Helix's view-kind
                // dispatch and keeps the renderer module per-kind
                // closed.
                let cfg = config.clone();
                let rendered = workspace.with_view_buffer(view_id, |seq_view, view, buf, ann| {
                    match view.kind {
                        seqforge_core::ViewKind::TextView => {
                            seq_view.show(
                                ui,
                                view,
                                buf,
                                ann,
                                pending_commands,
                                &cfg,
                                view_focused,
                                clipboard,
                            );
                        }
                    }
                });
                if rendered.is_err() {
                    ui.centered_and_justified(|ui| {
                        ui.label("(view closed)");
                    });
                }

                // No focused-pane border: focus is cued by the blinking caret
                // (solid when unfocused, blinking when focused). The accent
                // rectangle read as visual noise, especially in a single pane.
            }
        }

        // Leaf click → FocusScope. Welcome doesn't enqueue a focus
        // change (FocusScope::View(0) is a sentinel that wouldn't
        // resolve in workspace.views); leaving focus alone is fine.
        if !matches!(tab, Tab::Welcome)
            && ui.rect_contains_pointer(pane_rect)
            && ui.ctx().input(|i| i.pointer.any_pressed())
            && self.focus.scope != pane_scope
        {
            self.pending_commands
                .push((AppCommand::FocusPane(pane_scope), None));
        }
    }
}
