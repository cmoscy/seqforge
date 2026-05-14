use std::path::PathBuf;

use egui_dock::{DockArea, DockState, NodeIndex, Style};
use serde::{Deserialize, Serialize};
use seqforge_core::Document;

use crate::browser::BrowserState;
use crate::tabs::{Tab, TabViewer};
use crate::viewer::SequenceView;

#[derive(Serialize, Deserialize)]
pub struct AppState {
    pub dock_state: DockState<Tab>,
    pub browser: BrowserState,
    pub open_doc: Option<Document>,
    pub viewer: SequenceView,
    // Transient: not persisted; set by TabViewer, consumed in update()
    #[serde(skip)]
    pub pending_open: Option<PathBuf>,
}

impl Default for AppState {
    fn default() -> Self {
        let mut dock_state = DockState::new(vec![Tab::Viewer]);
        let surface = dock_state.main_surface_mut();
        let [_right, _left] = surface.split_left(NodeIndex::root(), 0.20, vec![Tab::FileBrowser]);
        let [_viewer, _terminal] =
            surface.split_below(NodeIndex::root(), 0.70, vec![Tab::Terminal]);

        Self {
            dock_state,
            browser: BrowserState::default(),
            open_doc: None,
            viewer: SequenceView::default(),
            pending_open: None,
        }
    }
}

pub struct SeqForgeApp {
    state: AppState,
}

impl SeqForgeApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let state = cc
            .storage
            .and_then(|s| eframe::get_value(s, eframe::APP_KEY))
            .unwrap_or_default();
        Self { state }
    }
}

impl eframe::App for SeqForgeApp {
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, eframe::APP_KEY, &self.state);
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::TopBottomPanel::top("menu_bar").show(ctx, |ui| {
            egui::menu::bar(ui, |ui| {
                ui.menu_button("File", |ui| {
                    ui.add_enabled(false, egui::Button::new("Open…"));
                    ui.add_enabled(false, egui::Button::new("Close"));
                });
                ui.menu_button("Edit", |ui| {
                    ui.add_enabled(false, egui::Button::new("Find…"));
                });
                ui.menu_button("View", |ui| {
                    if ui.button("Reset Layout").clicked() {
                        self.state.dock_state = AppState::default().dock_state;
                        ui.close_menu();
                    }
                });
                ui.menu_button("Tools", |ui| {
                    ui.add_enabled(false, egui::Button::new("Restriction Sites…"));
                });
                ui.menu_button("Navigate", |ui| {
                    ui.add_enabled(false, egui::Button::new("Go to Position…"));
                });
                ui.menu_button("Help", |ui| {
                    if ui.button("About").clicked() {
                        ui.close_menu();
                    }
                });
            });
        });

        // Destructure to allow separate mutable borrows alongside dock_state
        let AppState {
            dock_state,
            browser,
            open_doc,
            viewer,
            pending_open,
        } = &mut self.state;

        egui::CentralPanel::default()
            .frame(egui::Frame::central_panel(&ctx.style()).inner_margin(0.0))
            .show(ctx, |ui| {
                DockArea::new(dock_state)
                    .style(Style::from_egui(ui.style()))
                    .show_inside(
                        ui,
                        &mut TabViewer {
                            browser,
                            open_doc: open_doc.as_ref(),
                            viewer,
                            pending_open,
                        },
                    );
            });

        // Handle any file-open requests emitted by the browser tab
        if let Some(path) = self.state.pending_open.take() {
            match seqforge_bio::load(&path) {
                Ok(doc) => {
                    self.state.viewer.reset();
                    self.state.open_doc = Some(doc);
                }
                Err(e) => eprintln!("Failed to load {}: {e}", path.display()),
            }
        }
    }
}
