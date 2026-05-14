use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use seqforge_core::Document;

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
    pub open_doc: Option<&'a Document>,
    pub viewer: &'a mut SequenceView,
    pub pending_open: &'a mut Option<PathBuf>,
}

impl egui_dock::TabViewer for TabViewer<'_> {
    type Tab = Tab;

    fn title(&mut self, tab: &mut Tab) -> egui::WidgetText {
        match tab {
            Tab::FileBrowser => "Files".into(),
            Tab::Viewer => self
                .open_doc
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
                    *self.pending_open = Some(path);
                }
            }
            Tab::Viewer => match self.open_doc {
                Some(doc) => {
                    self.viewer.show(ui, doc);
                }
                None => {
                    ui.centered_and_justified(|ui| {
                        ui.label(
                            "No file open.\nDouble-click a .gb or .fasta file in the browser.",
                        );
                    });
                }
            },
            Tab::Terminal => {
                ui.centered_and_justified(|ui| {
                    ui.label("Terminal — coming in Phase 6");
                });
            }
        }
    }
}
