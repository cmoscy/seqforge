use serde::{Deserialize, Serialize};
use seqforge_core::{DispatchError, ViewerRequest, ViewerResponse, ViewerState};
use std::sync::mpsc;

use crate::browser::BrowserState;
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
        match tab {
            Tab::FileBrowser => {
                if let Some(path) = self.browser.show(ui) {
                    self.pending_requests.push((ViewerRequest::Open { path }, None));
                }
            }
            Tab::Viewer => {
                self.seq_view.show(ui, self.viewer);
            }
            Tab::Terminal => match self.terminal.as_mut() {
                Some(term) => {
                    if let Some(req) = term.show(ui) {
                        self.pending_requests.push((req, None));
                    }
                }
                None => {
                    ui.centered_and_justified(|ui| {
                        ui.label("Terminal failed to initialise.\nCheck stderr for details.");
                    });
                }
            },
        }
    }
}
