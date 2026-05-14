use std::sync::mpsc;

use egui_dock::{DockArea, DockState, NodeIndex, Style};
use egui_file_dialog::FileDialog;
use serde::{Deserialize, Serialize};
use seqforge_core::{
    dispatch, BioOps, CutSite, Document, SearchHit, ViewerRequest, ViewerResponse, ViewerState,
};

use crate::browser::BrowserState;
use crate::cli_install;
use crate::socket;
use crate::tabs::{Tab, TabViewer};
use crate::terminal::TerminalPane;
use crate::viewer::SequenceView;

// ── AppBio ────────────────────────────────────────────────────────────────────

struct AppBio;

impl BioOps for AppBio {
    fn load(&self, path: &std::path::Path) -> Result<Document, String> {
        seqforge_bio::load(path).map_err(|e| e.to_string())
    }

    fn find_matches(
        &self,
        seq: &[u8],
        pattern: &[u8],
        mismatches: u8,
        circular: bool,
    ) -> Vec<SearchHit> {
        seqforge_bio::find_iupac_matches(seq, pattern, mismatches, circular)
    }

    fn find_cut_sites(&self, seq: &[u8], enzymes: &[&str], circular: bool) -> Vec<CutSite> {
        seqforge_bio::find_cut_sites(seq, enzymes, circular)
    }
}

// ── AppState ──────────────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize)]
pub struct AppState {
    pub dock_state: DockState<Tab>,
    pub browser: BrowserState,
    /// Document + selection state — no GUI deps; persisted across restarts.
    pub viewer: ViewerState,
    // ── Transient: not persisted ──────────────────────────────────────────
    #[serde(skip)]
    pub seq_view: SequenceView,
    #[serde(skip)]
    pub pending_requests: Vec<ViewerRequest>,
    #[serde(skip)]
    pub open_dialog: Option<FileDialog>,
    /// Live terminal pane (egui_term + PTY). Initialised in SeqForgeApp::new.
    #[serde(skip)]
    pub terminal: Option<TerminalPane>,
    /// Receiver for ViewerRequests arriving via the Unix domain socket.
    #[serde(skip)]
    pub socket_rx: Option<mpsc::Receiver<ViewerRequest>>,
    /// Ephemeral status message shown after a CLI install attempt.
    #[serde(skip)]
    pub cli_status: Option<String>,
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
            pending_requests: Vec::new(),
            open_dialog: None,
            terminal: None,
            socket_rx: None,
            cli_status: None,
        }
    }
}

// ── SeqForgeApp ───────────────────────────────────────────────────────────────

pub struct SeqForgeApp {
    state: AppState,
}

impl SeqForgeApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let mut state: AppState = cc
            .storage
            .and_then(|s| eframe::get_value(s, eframe::APP_KEY))
            .unwrap_or_default();

        // Start the Unix domain socket listener and wire up the terminal.
        let socket_path = match socket::start_socket_listener(cc.egui_ctx.clone()) {
            Ok((path, rx)) => {
                state.socket_rx = Some(rx);
                Some(path)
            }
            Err(e) => {
                eprintln!("[seqforge] socket init failed: {e}");
                None
            }
        };

        state.terminal =
            TerminalPane::new(cc.egui_ctx.clone(), socket_path.as_deref())
                .map_err(|e| eprintln!("[seqforge] terminal init failed: {e}"))
                .ok();

        Self { state }
    }
}

impl eframe::App for SeqForgeApp {
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, eframe::APP_KEY, &self.state);
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // ── File-open dialog (menu-triggered) ─────────────────────────────────
        if let Some(dialog) = &mut self.state.open_dialog {
            dialog.update(ctx);
            if let Some(picked) = dialog.picked() {
                self.state
                    .pending_requests
                    .push(ViewerRequest::Open { path: picked.to_owned() });
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
                        self.state.pending_requests.push(ViewerRequest::Close);
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
                    ui.separator();
                    let label = if cli_install::is_installed() {
                        "Reinstall 'seqforge' CLI to PATH"
                    } else {
                        "Install 'seqforge' CLI to PATH"
                    };
                    if ui.button(label).clicked() {
                        self.state.cli_status = Some(match cli_install::install_cli_to_path() {
                            Ok(r) => format!(
                                "✓ seqforge installed to {}{}",
                                r.target.display(),
                                if r.was_updated { " (updated)" } else { "" }
                            ),
                            Err(e) => format!("✗ Install failed: {e}"),
                        });
                        ui.close_menu();
                    }
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

        // ── CLI install status window ─────────────────────────────────────────
        if let Some(msg) = &self.state.cli_status.clone() {
            let mut open = true;
            egui::Window::new("CLI Install")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .open(&mut open)
                .show(ctx, |ui| {
                    ui.label(msg);
                    ui.add_space(4.0);
                    if ui.button("OK").clicked() {
                        self.state.cli_status = None;
                    }
                });
            if !open {
                self.state.cli_status = None;
            }
        }

        // ── Dock area ─────────────────────────────────────────────────────────
        // Destructure all AppState fields at once to satisfy the borrow checker
        // when passing split mutable refs into TabViewer.
        let AppState {
            dock_state,
            browser,
            viewer,
            seq_view,
            pending_requests,
            terminal,
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
                            pending_requests,
                            terminal,
                        },
                    );
            });

        // ── Drain socket requests ─────────────────────────────────────────────
        if let Some(rx) = &self.state.socket_rx {
            while let Ok(req) = rx.try_recv() {
                self.state.pending_requests.push(req);
            }
        }

        // ── Process pending requests ──────────────────────────────────────────
        let reqs: Vec<ViewerRequest> = self.state.pending_requests.drain(..).collect();
        for req in reqs {
            // Reset view state before loading a new document.
            if matches!(req, ViewerRequest::Open { .. }) {
                self.state.seq_view.reset();
            }
            match dispatch(&mut self.state.viewer, &AppBio, req) {
                Ok(ViewerResponse::Message { text }) => eprintln!("[dispatch] {text}"),
                Ok(ViewerResponse::Ok) => {}
                Err(e) => eprintln!("[dispatch error] {e}"),
            }
        }
    }
}
