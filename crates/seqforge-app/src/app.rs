use std::path::PathBuf;
use std::sync::mpsc;

use egui_dock::{DockArea, DockState, NodeIndex, Style};
use egui_file_dialog::FileDialog;
use serde::{Deserialize, Serialize};
use seqforge_core::{BioOps, CutSite, Document, SearchHit, ViewerRequest, ViewerResponse, ViewerState};

use crate::bar::ActiveBar;
use crate::browser::BrowserState;
use crate::command::{self, AppCommand, PendingCommand};
use crate::event::{AppEvent, EventLog, EventSink};
use crate::focus::FocusState;
use crate::keymap;
use crate::socket::{self, SocketRequest};
use crate::tabs::{Tab, TabViewer};
use crate::terminal::TerminalPane;
use crate::viewer::SequenceView;

pub(crate) const MAX_RECENT: usize = 10;

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
    /// Queue of commands waiting to be applied this frame. Every menu,
    /// hotkey, bar, socket, and pane-click handler pushes into this and
    /// `update()` drains it through `command::apply` exactly once per
    /// frame. See `docs/focus-refactor.md` §2 for the lifecycle.
    #[serde(skip)]
    pub pending_commands: Vec<PendingCommand>,
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
    pub(crate) toasts: egui_notify::Toasts,
    /// Inline Find / GoTo bar shown at the top of the Viewer tab.
    #[serde(skip)]
    pub active_bar: Option<ActiveBar>,
    /// Active pane + key-context stack. Stage 1 of the focus refactor;
    /// see `docs/focus-refactor.md`. Not persisted — startup always
    /// begins on `FocusScope::Terminal`.
    #[serde(skip)]
    pub focus: FocusState,
    /// Producer side of the event bus. `apply()` emits through here.
    #[serde(skip)]
    pub events: EventSink,
    /// Consumer side of the event bus. Drained into [`event_log`]
    /// once per frame.
    #[serde(skip)]
    pub event_rx: Option<mpsc::Receiver<AppEvent>>,
    /// Bounded ring of recent events. Read by the status bar today;
    /// future panels/plugins will subscribe via their own receivers.
    #[serde(skip)]
    pub event_log: EventLog,
}

impl Default for AppState {
    fn default() -> Self {
        let mut dock_state = DockState::new(vec![Tab::Viewer]);
        let surface = dock_state.main_surface_mut();
        let [_right, _left] = surface.split_left(NodeIndex::root(), 0.20, vec![Tab::FileBrowser]);
        let [_viewer, _terminal] =
            surface.split_below(NodeIndex::root(), 0.70, vec![Tab::Terminal]);

        let (events, event_rx) = EventSink::channel();
        Self {
            dock_state,
            browser: BrowserState::default(),
            viewer: ViewerState::default(),
            recent_files: Vec::new(),
            seq_view: SequenceView::default(),
            pending_commands: Vec::new(),
            open_dialog: None,
            terminal: None,
            socket_rx: None,
            cli_status: None,
            toasts: egui_notify::Toasts::default(),
            active_bar: None,
            focus: FocusState::new(),
            events,
            event_rx: Some(event_rx),
            event_log: EventLog::default(),
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

        // Event bus: if state came from storage, the `#[serde(skip)]`
        // defaults gave us a sink with a dropped receiver. Always
        // install a fresh channel so emits have somewhere to land.
        let (events, event_rx) = EventSink::channel();
        state.events = events;
        state.event_rx = Some(event_rx);

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

/// Convenience: push a command with no response channel.
fn enqueue(state: &mut AppState, cmd: AppCommand) {
    state.pending_commands.push((cmd, None));
}

impl eframe::App for SeqForgeApp {
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, eframe::APP_KEY, &self.state);
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // ── Drain events ──────────────────────────────────────────────────────
        // Pull anything emitted by the *previous* frame's `apply()` into the
        // event log so this frame's status bar / panels see fresh data.
        if let Some(rx) = &self.state.event_rx {
            self.state.event_log.drain_from(rx);
        }

        // ── Keymap dispatch ───────────────────────────────────────────────────
        // Single source of truth for keyboard shortcuts. Bindings live in
        // `keymap::KEYMAP`; this call is the *only* place app-level
        // `consume_key` runs. See `docs/focus-refactor.md` §2.4.
        let key_cmds = keymap::dispatch(&self.state.focus, &self.state, ctx);
        for c in key_cmds {
            enqueue(&mut self.state, c);
        }

        // ── File-open dialog (menu-triggered) ─────────────────────────────────
        // Dialog lifecycle stays direct: it's egui widget state, not a user
        // command. Completion enqueues AppCommand::OpenFile.
        if let Some(dialog) = &mut self.state.open_dialog {
            dialog.update(ctx);
            if let Some(picked) = dialog.picked() {
                let path = picked.to_owned();
                self.state.open_dialog = None;
                self.state.pending_commands.push((AppCommand::OpenFile(path), None));
            } else if matches!(
                dialog.state(),
                egui_file_dialog::DialogState::Closed | egui_file_dialog::DialogState::Cancelled
            ) {
                self.state.open_dialog = None;
            }
        }

        // ── Menu bar ─────────────────────────────────────────────────────────
        // Each menu item enqueues an AppCommand; `is_enabled` gates the
        // greyed state so every UI surface uses the same predicate as the
        // keymap (Stage 4) and the future agent reject path.
        let mut menu_cmds: Vec<AppCommand> = Vec::new();
        let recent_snapshot = self.state.recent_files.clone();
        egui::TopBottomPanel::top("menu_bar").show(ctx, |ui| {
            egui::menu::bar(ui, |ui| {
                ui.menu_button("File", |ui| {
                    if ui.button("Open…  ⌘O").clicked() {
                        menu_cmds.push(AppCommand::PromptOpenFile);
                        ui.close_menu();
                    }
                    let can_close = command::is_enabled(&AppCommand::CloseDoc, &self.state);
                    if ui.add_enabled(can_close, egui::Button::new("Close  ⌘W")).clicked() {
                        menu_cmds.push(AppCommand::CloseDoc);
                        ui.close_menu();
                    }
                    if !recent_snapshot.is_empty() {
                        ui.separator();
                        ui.menu_button("Recent Files", |ui| {
                            for path in &recent_snapshot {
                                let label = path
                                    .file_name()
                                    .and_then(|n| n.to_str())
                                    .unwrap_or("(unknown)");
                                if ui.button(label).clicked() {
                                    menu_cmds.push(AppCommand::OpenFile(path.clone()));
                                    ui.close_menu();
                                }
                            }
                            ui.separator();
                            if ui.button("Clear Recent").clicked() {
                                menu_cmds.push(AppCommand::ClearRecent);
                                ui.close_menu();
                            }
                        });
                    }
                });
                ui.menu_button("Edit", |ui| {
                    let can_find = command::is_enabled(&AppCommand::OpenFind, &self.state);
                    if ui.add_enabled(can_find, egui::Button::new("Find…  ⌘F")).clicked() {
                        menu_cmds.push(AppCommand::OpenFind);
                        ui.close_menu();
                    }
                });
                ui.menu_button("View", |ui| {
                    if ui.button("Reset Layout").clicked() {
                        menu_cmds.push(AppCommand::ResetLayout);
                        ui.close_menu();
                    }
                });
                ui.menu_button("Tools", |ui| {
                    ui.add_enabled(false, egui::Button::new("Restriction Sites…"));
                    ui.separator();
                    let label = if crate::cli_install::is_installed() {
                        "Reinstall 'seqforge' CLI to PATH"
                    } else {
                        "Install 'seqforge' CLI to PATH"
                    };
                    if ui.button(label).clicked() {
                        menu_cmds.push(AppCommand::InstallCli);
                        ui.close_menu();
                    }
                });
                ui.menu_button("Navigate", |ui| {
                    let can_nav = command::is_enabled(&AppCommand::OpenGoTo, &self.state);
                    if ui.add_enabled(can_nav, egui::Button::new("Go to Position…  ⌘G")).clicked() {
                        menu_cmds.push(AppCommand::OpenGoTo);
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
        for c in menu_cmds {
            enqueue(&mut self.state, c);
        }

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
                    ui.label(format!("focus: {:?}", self.state.focus.scope));
                    // Stage 3 debug indicator: shows the most recent AppEvent.
                    // Confirms the event bus is actually firing. Both labels are
                    // diagnostic — replace with proper status surfaces once the
                    // refactor lands.
                    if let Some(ev) = self.state.event_log.latest() {
                        ui.separator();
                        ui.label(ev.short_label());
                    }
                });
            });
        });

        // ── CLI install status window ─────────────────────────────────────────
        if let Some(msg) = self.state.cli_status.clone() {
            let mut open = true;
            let mut dismiss = false;
            egui::Window::new("CLI Install")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .open(&mut open)
                .show(ctx, |ui| {
                    ui.label(&msg);
                    ui.add_space(4.0);
                    if ui.button("OK").clicked() {
                        dismiss = true;
                    }
                });
            if !open || dismiss {
                enqueue(&mut self.state, AppCommand::DismissCliStatus);
            }
        }

        // ── Dock area ─────────────────────────────────────────────────────────
        let AppState {
            dock_state,
            browser,
            viewer,
            seq_view,
            pending_commands,
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
                            pending_commands,
                            terminal,
                            active_bar,
                            focus,
                        },
                    );
            });

        // ── Drain socket requests ─────────────────────────────────────────────
        // Socket-originated `Open` is converted to `AppCommand::OpenFile` so
        // recents and `seq_view` stay in sync — `Viewer(req)` is the
        // generic pass-through for everything else.
        if let Some(rx) = &self.state.socket_rx {
            while let Ok((req, resp_tx)) = rx.try_recv() {
                let cmd = match req {
                    ViewerRequest::Open { path } => AppCommand::OpenFile(path),
                    other => AppCommand::Viewer(other),
                };
                self.state.pending_commands.push((cmd, Some(resp_tx)));
            }
        }

        // ── Apply commands ────────────────────────────────────────────────────
        // The single mutation site. Drains the queue exactly once per frame;
        // any commands enqueued *during* application (e.g. one variant
        // chaining into another) wait for the next frame, which keeps the
        // lifecycle ordering predictable.
        let cmds: Vec<PendingCommand> = self.state.pending_commands.drain(..).collect();
        for (cmd, resp_tx) in cmds {
            let result = command::apply(cmd, &mut self.state, &AppBio);
            if let Err(e) = &result {
                eprintln!("[apply error] {e}");
                self.state.toasts.error(e.to_string());
            }
            if let Some(tx) = resp_tx {
                // Socket-facing oneshot expects a concrete ViewerResponse.
                // GUI-only commands return Ok(None); map that to Ok so the
                // CLI client sees a successful no-op rather than an error.
                let wire = result.map(|opt| opt.unwrap_or(ViewerResponse::Ok));
                let _ = tx.send(wire);
            }
        }

        // ── Toast notifications ───────────────────────────────────────────────
        self.state.toasts.show(ctx);
    }
}
