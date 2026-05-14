use egui_dock::{DockArea, DockState, NodeIndex, Style};
use egui_file_dialog::FileDialog;
use serde::{Deserialize, Serialize};
use seqforge_core::{dispatch_viewer, SideEffect, ViewerCommand, ViewerState};

use crate::browser::BrowserState;
use crate::tabs::{Tab, TabViewer};
use crate::viewer::SequenceView;

#[derive(Serialize, Deserialize)]
pub struct AppState {
    pub dock_state: DockState<Tab>,
    pub browser: BrowserState,
    /// Document + selection state — no GUI deps.
    pub viewer: ViewerState,
    // Transient: not persisted.
    #[serde(skip)]
    pub seq_view: SequenceView,
    #[serde(skip)]
    pub pending_commands: Vec<ViewerCommand>,
    #[serde(skip)]
    open_dialog: Option<FileDialog>,
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
            viewer: ViewerState::default(),
            seq_view: SequenceView::default(),
            pending_commands: Vec::new(),
            open_dialog: None,
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
        // ── File-open dialog (menu-triggered) ────────────────────────────────
        if let Some(dialog) = &mut self.state.open_dialog {
            dialog.update(ctx);
            if let Some(picked) = dialog.picked() {
                self.state
                    .pending_commands
                    .push(ViewerCommand::Open { path: picked.to_owned() });
                self.state.open_dialog = None;
            } else if matches!(
                dialog.state(),
                egui_file_dialog::DialogState::Closed | egui_file_dialog::DialogState::Cancelled
            ) {
                self.state.open_dialog = None;
            }
        }

        // ── Menu bar ─────────────────────────────────────────────────────────
        egui::TopBottomPanel::top("menu_bar").show(ctx, |ui| {
            egui::menu::bar(ui, |ui| {
                ui.menu_button("File", |ui| {
                    if ui.button("Open…").clicked() {
                        let mut dialog = FileDialog::new();
                        dialog.pick_file();
                        self.state.open_dialog = Some(dialog);
                        ui.close_menu();
                    }
                    let can_close = self.state.viewer.open_doc.is_some();
                    if ui.add_enabled(can_close, egui::Button::new("Close")).clicked() {
                        self.state.pending_commands.push(ViewerCommand::Close);
                        ui.close_menu();
                    }
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

        // ── Dock area ─────────────────────────────────────────────────────────
        let AppState {
            dock_state,
            browser,
            viewer,
            seq_view,
            pending_commands,
            ..
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
                            viewer,
                            seq_view,
                            pending_commands,
                        },
                    );
            });

        // ── Process pending commands ──────────────────────────────────────────
        let cmds: Vec<ViewerCommand> = self.state.pending_commands.drain(..).collect();
        for cmd in cmds {
            match dispatch_viewer(&mut self.state.viewer, cmd) {
                Ok(out) => {
                    for effect in out.side_effects {
                        match effect {
                            SideEffect::LoadDocument(path) => {
                                match seqforge_bio::load(&path) {
                                    Ok(doc) => {
                                        self.state.seq_view.reset();
                                        self.state.viewer.clear_selection();
                                        self.state.viewer.scroll_to = None;
                                        self.state.viewer.open_doc = Some(doc);
                                    }
                                    Err(e) => eprintln!("Failed to load {}: {e}", path.display()),
                                }
                            }
                            SideEffect::FocusRange(_, _) | SideEffect::OpenTab(_) => {
                                // handled in later phases
                            }
                        }
                    }
                    for msg in out.messages {
                        eprintln!("[dispatch] {msg}");
                    }
                }
                Err(e) => eprintln!("[dispatch error] {e}"),
            }
        }
    }
}
