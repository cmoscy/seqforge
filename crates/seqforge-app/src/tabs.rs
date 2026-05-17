use serde::{Deserialize, Serialize};
use seqforge_core::ViewId;

use crate::browser::BrowserState;
use crate::command::{AppCommand, PendingCommand};
use crate::focus::{FocusScope, FocusState};
use crate::overlay::{self, OverlayStack};
use crate::terminal::TerminalPane;
use crate::workspace::Workspace;

/// A leaf in the egui_dock tree.
///
/// After the Stage 2.5c follow-up flatten, viewer tabs are addressed
/// by `ViewId` directly — egui_dock's native tab bar handles
/// switching, drag-rearrange, and split-across-leaves. `Welcome` is a
/// placeholder shown when no view is open; it gets replaced by the
/// first `View(_)` tab and re-introduced when the last view closes,
/// so the central dock area never becomes an empty void.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Tab {
    FileBrowser,
    Terminal,
    Welcome,
    View(ViewId),
}

pub struct TabViewer<'a> {
    pub browser: &'a mut BrowserState,
    pub workspace: &'a mut Workspace,
    pub pending_commands: &'a mut Vec<PendingCommand>,
    pub terminal: &'a mut Option<TerminalPane>,
    pub overlays: &'a mut OverlayStack,
    pub focus: &'a mut FocusState,
}

impl egui_dock::TabViewer for TabViewer<'_> {
    type Tab = Tab;

    fn title(&mut self, tab: &mut Tab) -> egui::WidgetText {
        match tab {
            Tab::FileBrowser => "Files".into(),
            Tab::Terminal => "Terminal".into(),
            Tab::Welcome => "Welcome".into(),
            Tab::View(vid) => {
                let name = self.workspace.view(*vid).and_then(|v| {
                    let arc = self.workspace.buffers.get(v.buffer_id)?;
                    let buf = arc.read().ok()?;
                    Some(buf.name.clone())
                });
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
            Tab::FileBrowser => FocusScope::Browser,
            Tab::Terminal => FocusScope::Terminal,
            Tab::Welcome => FocusScope::View(ViewId(0)),
            Tab::View(vid) => FocusScope::View(*vid),
        };

        match tab {
            Tab::FileBrowser => {
                if let Some(path) = self.browser.show(ui) {
                    self.pending_commands.push((AppCommand::OpenFile(path), None));
                }
            }
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
                let TabViewer {
                    workspace,
                    pending_commands,
                    ..
                } = self;
                // ViewKind dispatch (Stage 2.5d). Today only `TextView`
                // exists; adding `LinearView` / `CircularView` later is
                // a new arm + a new renderer module — no changes
                // elsewhere. The pattern matches Helix's view-kind
                // dispatch and keeps the renderer module per-kind
                // closed.
                let rendered =
                    workspace.with_view_buffer(view_id, |seq_view, view, buf, ann| {
                        match view.kind {
                            seqforge_core::ViewKind::TextView => {
                                seq_view.show(ui, view, buf, ann, pending_commands);
                            }
                        }
                    });
                if rendered.is_err() {
                    ui.centered_and_justified(|ui| {
                        ui.label("(view closed)");
                    });
                }

                // Focused-pane indicator. Paint a thin accent stroke
                // around this pane when it owns keyboard focus, so the
                // user can tell at a glance which split is active in
                // multi-pane layouts. Drawn last so it sits above the
                // viewer content.
                if self.focus.scope == FocusScope::View(view_id) {
                    let accent = ui.visuals().selection.stroke.color;
                    ui.painter().rect_stroke(
                        pane_rect.shrink(1.0),
                        egui::CornerRadius::ZERO,
                        egui::Stroke::new(2.0, accent),
                        egui::StrokeKind::Inside,
                    );
                }
            }
            Tab::Terminal => match self.terminal.as_mut() {
                Some(term) => {
                    let terminal_has_focus = self.focus.scope == FocusScope::Terminal
                        && self.overlays.is_empty();
                    term.show(ui, terminal_has_focus);
                }
                None => {
                    ui.centered_and_justified(|ui| {
                        ui.label("Terminal failed to initialise.\nCheck stderr for details.");
                    });
                }
            },
        }

        // Leaf click → FocusScope. Welcome doesn't enqueue a focus
        // change (FocusScope::View(0) is a sentinel that wouldn't
        // resolve in workspace.views); leaving focus alone is fine.
        if !matches!(tab, Tab::Welcome)
            && ui.rect_contains_pointer(pane_rect)
            && ui.ctx().input(|i| i.pointer.any_pressed())
            && self.focus.scope != pane_scope
        {
            self.pending_commands.push((AppCommand::FocusPane(pane_scope), None));
        }
    }
}
