use serde::{Deserialize, Serialize};
use seqforge_core::{DispatchError, ViewerRequest, ViewerResponse, ViewerState};
use std::sync::mpsc;

use crate::bar::{show_bar, ActiveBar};
use crate::browser::BrowserState;
use crate::focus::{FocusScope, FocusState};
use crate::terminal::TerminalPane;
use crate::viewer::SequenceView;

type PendingReq = (ViewerRequest, Option<mpsc::SyncSender<Result<ViewerResponse, DispatchError>>>);

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
    pub pending_requests: &'a mut Vec<PendingReq>,
    pub terminal: &'a mut Option<TerminalPane>,
    pub active_bar: &'a mut Option<ActiveBar>,
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
                    self.pending_requests.push((ViewerRequest::Open { path }, None));
                }
            }
            Tab::Viewer => {
                // Inline Find / GoTo bar at the top of the viewer pane.
                if let Some(req) = show_bar(self.active_bar, ui) {
                    self.pending_requests.push((req, None));
                }
                self.seq_view.show(ui, self.viewer);
            }
            Tab::Terminal => match self.terminal.as_mut() {
                Some(term) => {
                    let terminal_has_focus = self.focus.scope == FocusScope::Terminal;
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
        // grid) still handles it normally.
        if ui.rect_contains_pointer(pane_rect)
            && ui.ctx().input(|i| i.pointer.any_pressed())
        {
            self.focus.set_scope(pane_scope);
        }
    }
}
