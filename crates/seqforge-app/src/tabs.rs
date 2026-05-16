use serde::{Deserialize, Serialize};
use seqforge_core::ViewerState;

use crate::browser::BrowserState;
use crate::command::{AppCommand, PendingCommand};
use crate::focus::{FocusScope, FocusState};
use crate::overlay::{self, OverlayStack};
use crate::terminal::TerminalPane;
use crate::viewer::SequenceView;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Tab {
    FileBrowser,
    Viewer,
    Terminal,
}

pub struct TabViewer<'a> {
    pub browser: &'a mut BrowserState,
    pub viewer: &'a mut ViewerState,
    pub seq_view: &'a mut SequenceView,
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
            Tab::Viewer => self
                .viewer
                .open_doc
                .as_ref()
                .map(|d| format!("Viewer — {}", d.name))
                .unwrap_or_else(|| "Sequence Viewer".to_owned())
                .into(),
            Tab::Terminal => "Terminal".into(),
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, tab: &mut Tab) {
        // Snapshot the pane rect before rendering so click-detection covers
        // the full pane area, not just whatever sub-rect content occupies.
        let pane_rect = ui.max_rect();
        let pane_scope = match tab {
            Tab::FileBrowser => FocusScope::Browser,
            Tab::Viewer => FocusScope::Viewer,
            Tab::Terminal => FocusScope::Terminal,
        };

        match tab {
            Tab::FileBrowser => {
                if let Some(path) = self.browser.show(ui) {
                    self.pending_commands.push((AppCommand::OpenFile(path), None));
                }
            }
            Tab::Viewer => {
                // Inline Find / GoTo bar at the top of the viewer pane.
                if let Some(cmd) = overlay::show_inline_bar(self.overlays, ui) {
                    self.pending_commands.push((cmd, None));
                }
                self.seq_view.show(ui, self.viewer, self.pending_commands);
            }
            Tab::Terminal => match self.terminal.as_mut() {
                Some(term) => {
                    // Terminal yields keyboard whenever *any* overlay is
                    // active — even a non-focus-capturing one — so Escape
                    // and other dismiss bindings work uniformly.
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

        // Pane click → FocusScope. Geometry-only check; does not consume the
        // click, so the actual clicked widget (button, text field, terminal
        // grid) still handles it normally. Routed through AppCommand so the
        // focus mutation goes through the same single applier as everything
        // else.
        if ui.rect_contains_pointer(pane_rect)
            && ui.ctx().input(|i| i.pointer.any_pressed())
            && self.focus.scope != pane_scope
        {
            self.pending_commands.push((AppCommand::FocusPane(pane_scope), None));
        }
    }
}
