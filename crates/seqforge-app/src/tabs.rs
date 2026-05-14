use serde::{Deserialize, Serialize};
use seqforge_core::{ViewerCommand, ViewerState};

use crate::browser::BrowserState;
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
    pub pending_commands: &'a mut Vec<ViewerCommand>,
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
                    self.pending_commands.push(ViewerCommand::Open { path });
                }
            }
            Tab::Viewer => {
                self.seq_view.show(ui, self.viewer);
            }
            Tab::Terminal => {
                ui.centered_and_justified(|ui| {
                    ui.label("Terminal — coming in Phase 6");
                });
            }
        }
    }
}
