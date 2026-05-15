use std::path::PathBuf;
use std::sync::mpsc;

use egui_dock::{DockArea, DockState, NodeIndex, Style};
use egui_file_dialog::FileDialog;
use serde::{Deserialize, Serialize};
use seqforge_core::{
    dispatch, BioOps, CutSite, DispatchError, Document, SearchHit, ViewerRequest, ViewerResponse,
    ViewerState,
};

use crate::socket::SocketRequest;

/// Internal pending request: the command plus an optional one-shot channel to
/// return the dispatch result. `None` for fire-and-forget (menu, terminal).
type PendingReq = (ViewerRequest, Option<mpsc::SyncSender<Result<ViewerResponse, DispatchError>>>);

use crate::bar::ActiveBar;
use crate::browser::BrowserState;
use crate::cli_install;
use crate::focus::FocusState;
use crate::socket;
use crate::tabs::{Tab, TabViewer};
use crate::terminal::TerminalPane;
use crate::viewer::SequenceView;

const MAX_RECENT: usize = 10;

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
    /// Recently opened files (most-recent first, max 10).
    pub recent_files: Vec<PathBuf>,
    // ── Transient: not persisted ──────────────────────────────────────────
    #[serde(skip)]
    pub seq_view: SequenceView,
    #[serde(skip)]
    pub pending_requests: Vec<PendingReq>,
    #[serde(skip)]
    pub open_dialog: Option<FileDialog>,
    /// Live terminal pane (egui_term + PTY). Initialised in SeqForgeApp::new.
    #[serde(skip)]
    pub terminal: Option<TerminalPane>,
    /// Receiver for requests arriving via the Unix domain socket.
    #[serde(skip)]
    pub socket_rx: Option<mpsc::Receiver<SocketRequest>>,
    /// Ephemeral status message shown after a CLI install attempt.
    #[serde(skip)]
    pub cli_status: Option<String>,
    #[serde(skip)]
    toasts: egui_notify::Toasts,
    /// Inline Find / GoTo bar shown at the top of the Viewer tab.
    #[serde(skip)]
    pub active_bar: Option<ActiveBar>,
    /// Active pane + key-context stack. Stage 1 of the focus refactor;
    /// see `docs/focus-refactor.md`. Not persisted — startup always
    /// begins on `FocusScope::Terminal`.
    #[serde(skip)]
    pub focus: FocusState,
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
            recent_files: Vec::new(),
            seq_view: SequenceView::default(),
            pending_requests: Vec::new(),
            open_dialog: None,
            terminal: None,
            socket_rx: None,
            cli_status: None,
            toasts: egui_notify::Toasts::default(),
            active_bar: None,
            focus: FocusState::new(),
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

    fn push_open(&mut self, path: PathBuf) {
        self.state.seq_view.reset();
        self.state.pending_requests.push((ViewerRequest::Open { path: path.clone() }, None));
        // Prepend to recent list, dedup, cap at MAX_RECENT.
        self.state.recent_files.retain(|p| p != &path);
        self.state.recent_files.insert(0, path);
        self.state.recent_files.truncate(MAX_RECENT);
    }
}

impl eframe::App for SeqForgeApp {
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, eframe::APP_KEY, &self.state);
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // ── Keyboard shortcuts ────────────────────────────────────────────────
        let cmd = egui::Modifiers::COMMAND;
        ctx.input_mut(|i| {
            if i.consume_key(cmd, egui::Key::O) {
                let mut dialog = FileDialog::new();
                dialog.pick_file();
                self.state.open_dialog = Some(dialog);
            }
            if i.consume_key(cmd, egui::Key::W) && self.state.viewer.open_doc.is_some() {
                self.state.pending_requests.push((ViewerRequest::Close, None));
            }
            if i.consume_key(cmd, egui::Key::F) && self.state.viewer.open_doc.is_some() {
                self.state.active_bar.get_or_insert_with(|| ActiveBar::Find(Default::default()));
            }
            if i.consume_key(cmd, egui::Key::G) && self.state.viewer.open_doc.is_some() {
                self.state.active_bar.get_or_insert_with(|| ActiveBar::GoTo(Default::default()));
            }
        });

        // ── File-open dialog (menu-triggered) ─────────────────────────────────
        if let Some(dialog) = &mut self.state.open_dialog {
            dialog.update(ctx);
            if let Some(picked) = dialog.picked() {
                let path = picked.to_owned();
                self.state.open_dialog = None;
                self.push_open(path);
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
                    if ui.button("Open…  ⌘O").clicked() {
                        let mut dialog = FileDialog::new();
                        dialog.pick_file();
                        self.state.open_dialog = Some(dialog);
                        ui.close_menu();
                    }
                    let can_close = self.state.viewer.open_doc.is_some();
                    if ui.add_enabled(can_close, egui::Button::new("Close  ⌘W")).clicked() {
                        self.state.pending_requests.push((ViewerRequest::Close, None));
                        ui.close_menu();
                    }
                    if !self.state.recent_files.is_empty() {
                        ui.separator();
                        ui.menu_button("Recent Files", |ui| {
                            let recent = self.state.recent_files.clone();
                            for path in &recent {
                                let label = path
                                    .file_name()
                                    .and_then(|n| n.to_str())
                                    .unwrap_or("(unknown)");
                                if ui.button(label).clicked() {
                                    self.push_open(path.clone());
                                    ui.close_menu();
                                }
                            }
                            ui.separator();
                            if ui.button("Clear Recent").clicked() {
                                self.state.recent_files.clear();
                                ui.close_menu();
                            }
                        });
                    }
                });
                ui.menu_button("Edit", |ui| {
                    let can_find = self.state.viewer.open_doc.is_some();
                    if ui.add_enabled(can_find, egui::Button::new("Find…  ⌘F")).clicked() {
                        self.state.active_bar =
                            Some(ActiveBar::Find(Default::default()));
                        ui.close_menu();
                    }
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
                    let can_nav = self.state.viewer.open_doc.is_some();
                    if ui.add_enabled(can_nav, egui::Button::new("Go to Position…  ⌘G")).clicked() {
                        self.state.active_bar =
                            Some(ActiveBar::GoTo(Default::default()));
                        ui.close_menu();
                    }
                });
                ui.menu_button("Help", |ui| {
                    if ui.button("About").clicked() {
                        ui.close_menu();
                    }
                });
            });
        });

        // ── Status bar ────────────────────────────────────────────────────────
        egui::TopBottomPanel::bottom("status_bar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 12.0;
                if let Some(doc) = &self.state.viewer.open_doc {
                    let seq_len = doc.sequence.len();
                    let topology = format!("{:?}", doc.topology);
                    ui.label(format!("{seq_len} bp  ·  {topology}"));
                    if let Some(sel) = self.state.viewer.selection {
                        if sel.is_cursor() {
                            ui.label(format!("pos {}", sel.anchor + 1));
                        } else {
                            let (s, e) = sel.ordered();
                            ui.label(format!("sel {s}–{e}  ({} bp)", e - s));
                        }
                    }
                } else {
                    ui.label("No file open");
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    // Stage 1 debug indicator: confirms pane-click → FocusScope wiring.
                    // Remove once the keymap dispatcher (Stage 4) is the visible proof.
                    ui.label(format!("focus: {:?}", self.state.focus.scope));
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
            active_bar,
            focus,
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
                            active_bar,
                            focus,
                        },
                    );
            });

        // ── Drain socket requests ─────────────────────────────────────────────
        if let Some(rx) = &self.state.socket_rx {
            while let Ok((req, resp_tx)) = rx.try_recv() {
                self.state.pending_requests.push((req, Some(resp_tx)));
            }
        }

        // ── Process pending requests ──────────────────────────────────────────
        let reqs: Vec<PendingReq> = self.state.pending_requests.drain(..).collect();
        for (req, resp_tx) in reqs {
            if let ViewerRequest::Open { ref path } = req {
                // Track opens that come from socket/terminal (not already tracked via push_open).
                let path = path.clone();
                self.state.recent_files.retain(|p| p != &path);
                self.state.recent_files.insert(0, path);
                self.state.recent_files.truncate(MAX_RECENT);
                self.state.seq_view.reset();
            }
            let result = dispatch(&mut self.state.viewer, &AppBio, req);
            if let Err(e) = &result {
                eprintln!("[dispatch error] {e}");
                self.state.toasts.error(e.to_string());
            }
            if let Some(tx) = resp_tx {
                let _ = tx.send(result);
            }
        }

        // ── Toast notifications ───────────────────────────────────────────────
        self.state.toasts.show(ctx);
    }
}
