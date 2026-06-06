use std::path::PathBuf;
use std::sync::mpsc;

use egui_dock::{DockArea, DockState, NodeIndex, Style};
use seqforge_core::{BioOps, CutSite, Document, SearchHit, ViewerRequest, ViewerResponse};

use std::sync::Arc;

use crate::browser::BrowserState;
use crate::command::{self, AppCommand, PendingCommand};
use crate::config::Config;
use crate::minimap::MiniMap;
use crate::event::{AppEvent, EventLog, EventSink};
use crate::focus::FocusState;
use crate::keymap;
use crate::overlay::OverlayStack;
use crate::persistence::{self, PersistedSession};
#[cfg(unix)]
use crate::socket::{self, SocketRequest};
use crate::tabs::{Tab, TabViewer};
use crate::terminal::TerminalPane;
use crate::workspace::Workspace;

pub(crate) const MAX_RECENT: usize = 10;
/// eframe storage key for the [`PersistedSession`] blob. Stage 2.5e
/// replaces the full-`AppState` round-trip; everything else is
/// rebuilt fresh each launch.
const SESSION_KEY: &str = "seqforge_session_v1";

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

    fn resolve_enzymes(
        &self,
        seq: &[u8],
        query: &str,
        circular: bool,
    ) -> (Vec<String>, Vec<CutSite>) {
        let parsed = seqforge_bio::parse_enzyme_query(query);
        seqforge_bio::resolve_query(&parsed, seq, circular)
    }
}

// ── AppState ──────────────────────────────────────────────────────────────────

/// Runtime app state. **Not serializable** — Stage 2.5e moved
/// persistence out to [`PersistedSession`] (path-keyed), which is
/// captured at save and replayed at load. See `persistence.rs` for
/// the design rationale.
pub struct AppState {
    pub dock_state: DockState<Tab>,
    pub browser: BrowserState,
    /// Workspace = view storage + buffer store + active-view bookkeeping.
    pub workspace: Workspace,
    /// Recently opened files (most-recent first, max 10). Restored from
    /// [`PersistedSession::recent_files`] on launch; saved back on exit.
    pub recent_files: Vec<PathBuf>,
    /// Queue of commands waiting to be applied this frame. Every menu,
    /// hotkey, bar, socket, and pane-click handler pushes into this and
    /// `update()` drains it through `command::apply` exactly once per
    /// frame. See `docs/focus-refactor.md` §2 for the lifecycle.
    pub pending_commands: Vec<PendingCommand>,
    /// Live terminal pane (egui_term + PTY). Initialised in SeqForgeApp::new.
    pub terminal: Option<TerminalPane>,
    /// Receiver for requests arriving via the Unix domain socket.
    /// Unix-only; agent IPC on Windows is out of scope for v0.1.
    #[cfg(unix)]
    pub socket_rx: Option<mpsc::Receiver<SocketRequest>>,
    /// RAII guard that removes the socket file when `AppState` is
    /// dropped. The listener thread also cleans up on accept error,
    /// but it doesn't run on normal window-close exit — this guard
    /// covers that path. Tier 1 #4.
    #[cfg(unix)]
    pub socket_guard: Option<crate::socket::SocketGuard>,
    pub(crate) toasts: egui_notify::Toasts,
    /// All transient UI (Find/GoTo bars, file dialog, CLI status).
    /// See `docs/focus-refactor.md` §2.5.
    pub overlays: OverlayStack,
    /// Active pane + key-context stack. See `docs/focus-refactor.md` §2.1.
    /// Not persisted — startup always begins on `FocusScope::Terminal`.
    pub focus: FocusState,
    /// Snapshot of `focus.scope` taken when the overlay stack
    /// transitioned from empty → non-empty (e.g. user pressed ⌘F).
    /// Restored when the stack goes back to empty so closing the bar
    /// returns the user to the pane they were on. `None` whenever no
    /// overlay is active.
    pub focus_before_overlay: Option<crate::focus::FocusScope>,
    /// Producer side of the event bus. `apply()` emits through here.
    pub events: EventSink,
    /// Consumer side of the event bus. Drained into [`event_log`]
    /// once per frame.
    pub event_rx: Option<mpsc::Receiver<AppEvent>>,
    /// Bounded ring of recent events. Read by the status bar today;
    /// future panels/plugins will subscribe via their own receivers.
    pub event_log: EventLog,
    /// Minimap sidebar panel rendered below the file browser.
    pub minimap: MiniMap,
    /// Per-file UI state (selection, scroll) keyed by source path.
    /// Loaded from [`PersistedSession::file_state`] at launch and
    /// consumed when a file's `View` is first created (then dropped
    /// from the map). New saves capture the *current* view state, not
    /// this restore buffer.
    pub pending_file_state: std::collections::HashMap<
        PathBuf,
        crate::persistence::FileState,
    >,
    /// User configuration (settings + theme + key overrides). Loaded
    /// from disk in `SeqForgeApp::new`; can be re-read at runtime via
    /// the `ReloadConfig` command. Wrapped in `Arc` so widgets cheaply
    /// clone a per-frame reference.
    pub config: Arc<Config>,
}

impl Default for AppState {
    fn default() -> Self {
        // Stub layout: SeqForgeApp::new populates the real splits via
        // rebuild_default_dock once the user config is loaded.
        let dock_state = DockState::new(vec![Tab::Welcome]);

        let (events, event_rx) = EventSink::channel();
        Self {
            dock_state,
            browser: BrowserState::default(),
            workspace: Workspace::default(),
            recent_files: Vec::new(),
            pending_commands: Vec::new(),
            terminal: None,
            #[cfg(unix)]
            socket_rx: None,
            #[cfg(unix)]
            socket_guard: None,
            toasts: egui_notify::Toasts::default(),
            overlays: OverlayStack::default(),
            focus: FocusState::new(),
            focus_before_overlay: None,
            events,
            event_rx: Some(event_rx),
            event_log: EventLog::default(),
            minimap: MiniMap::default(),
            pending_file_state: std::collections::HashMap::new(),
            config: Arc::new(Config::default()),
        }
    }
}

/// Restore a previously-saved session into `state`: rebuild dock
/// layout from the snapshot, eagerly open each persisted file path
/// into the corresponding leaf, and stash per-file UI state so each
/// new `View` picks it up.
///
/// Errors at any step degrade gracefully: a malformed snapshot falls
/// back to the default layout; an OpenFile failure (file moved /
/// deleted) drops just that file. No panics; no orphan tabs possible
/// because `Tab::View(_)` ids are minted fresh during this replay,
/// never persisted.
fn restore_session(state: &mut AppState, session: PersistedSession, bio: &dyn BioOps) {
    state.recent_files = session.recent_files;
    state.pending_file_state = session.file_state;

    let Some(snapshot) = session.layout else { return };

    // Build the dock skeleton (splits + Browser/Terminal/Welcome
    // placeholders for viewer leaves) and collect the per-leaf opens
    // that need replaying.
    let (dock, pending) = persistence::rebuild_dock(&snapshot);
    state.dock_state = dock;

    // Replay opens, targeting each persisted leaf directly. Bypasses
    // the command pipeline because we're inside startup — no events,
    // no recent_files churn, no focus moves.
    for (surface, node, paths, active) in pending.leaves {
        let mut view_tabs: Vec<seqforge_core::ViewId> = Vec::new();
        for path in paths {
            match state.workspace.open_path(&path, bio) {
                Ok(vid) => {
                    // Restore selection / scroll if we have any.
                    if let Some(fs) = state.pending_file_state.remove(&path) {
                        if let Some(view) = state.workspace.view_mut(vid) {
                            view.selection = fs.selection;
                            view.scroll_pos = fs.scroll_pos;
                        }
                    }
                    view_tabs.push(vid);
                }
                Err(e) => {
                    eprintln!("[seqforge] restore: failed to reopen {path:?}: {e}");
                }
            }
        }
        if view_tabs.is_empty() {
            continue;
        }
        // Replace the Welcome placeholder in this leaf with the
        // freshly minted View tabs.
        if let egui_dock::Node::Leaf { tabs, active: tab_active, .. } =
            &mut state.dock_state[surface][node]
        {
            *tabs = view_tabs.iter().copied().map(Tab::View).collect();
            *tab_active = egui_dock::TabIndex(active.min(tabs.len().saturating_sub(1)));
        }
    }

    // Set workspace.active_view to whichever view the dock currently
    // shows as focused (egui_dock keeps a focused_node hint).
    if let Some((_, Tab::View(vid))) = state.dock_state.find_active_focused() {
        let vid = *vid;
        state.workspace.focus_view(vid);
        state.focus.set_scope(crate::focus::FocusScope::View(vid));
    }
}

/// Reset the dock to a fresh Welcome+Browser+Terminal layout using the
/// active configuration's split fractions. Called on first launch (no
/// saved session) and by `AppCommand::ResetLayout`.
pub(crate) fn rebuild_default_dock(dock: &mut DockState<Tab>, cfg: &Config) {
    *dock = DockState::new(vec![Tab::Welcome]);
    let surface = dock.main_surface_mut();
    let [_right, _left] = surface.split_left(
        NodeIndex::root(),
        cfg.settings.layout.file_browser_fraction,
        vec![Tab::FileBrowser],
    );
    let [_viewer, _terminal] = surface.split_below(
        NodeIndex::root(),
        cfg.settings.layout.terminal_fraction,
        vec![Tab::Terminal],
    );
}

// ── SeqForgeApp ───────────────────────────────────────────────────────────────

pub struct SeqForgeApp {
    state: AppState,
}

impl SeqForgeApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let mut state = AppState::default();
        let (config, cfg_warnings) = Config::load();
        state.config = config;
        for w in cfg_warnings {
            state.toasts.warning(w);
        }
        state.minimap.browser_fraction =
            state.config.settings.layout.minimap_browser_fraction.clamp(0.15, 0.85);
        // If no saved session restores the layout below, rebuild the
        // default dock using the *user-configured* split fractions so a
        // first launch honours `[layout]` overrides.
        rebuild_default_dock(&mut state.dock_state, &state.config);

        // Stage 2.5e: dock_state is no longer persisted as raw egui_dock
        // state; we save/load a path-keyed `PersistedSession` blob and
        // rebuild the dock fresh each launch. This eliminates the
        // ViewId-orphan class of bugs by construction.
        if let Some(session) = cc
            .storage
            .and_then(|s| eframe::get_value::<PersistedSession>(s, SESSION_KEY))
        {
            restore_session(&mut state, session, &AppBio);
        }

        // ── PTY environment + socket listener (Unix only) ─────────────────────
        // Sequencing is load-bearing: in Rust 2024 `std::env::set_var` is
        // unsafe because env mutation while another thread exists is UB. So
        // we (1) decide the socket path, (2) install all env vars on the
        // main thread, (3) THEN spawn the listener thread. See
        // `terminal::install_pty_env`.
        //
        // Windows: agent IPC is out of scope for v0.1. The terminal pane
        // still installs PATH for the bundled CLI; the CLI's viewer-IPC
        // half is also `#[cfg(unix)]` and surfaces an error if invoked.
        #[cfg(unix)]
        {
            let socket_path = socket::socket_path();
            crate::terminal::install_pty_env(Some(&socket_path));

            match socket::start_socket_listener(socket_path.clone(), cc.egui_ctx.clone())
            {
                Ok(rx) => {
                    state.socket_rx = Some(rx);
                    state.socket_guard = Some(socket::SocketGuard::new(socket_path));
                }
                Err(e) => {
                    eprintln!("[seqforge] socket init failed: {e}");
                }
            }
        }
        #[cfg(not(unix))]
        crate::terminal::install_pty_env(None);

        state.terminal = TerminalPane::new(
            cc.egui_ctx.clone(),
            &state.config.settings.terminal.shell,
        )
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
        // Capture the current session as a path-keyed snapshot. ViewIds
        // and BufferIds are not persisted — they're session-scoped.
        let session = PersistedSession {
            recent_files: self.state.recent_files.clone(),
            layout: persistence::capture_layout(&self.state.dock_state, &self.state.workspace),
            file_state: persistence::capture_file_state(&self.state.workspace),
        };
        eframe::set_value(storage, SESSION_KEY, &session);
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // ── Drain events ──────────────────────────────────────────────────────
        // Pull anything emitted by the *previous* frame's `apply()` into the
        // event log so this frame's status bar / panels see fresh data.
        if let Some(rx) = &self.state.event_rx {
            self.state.event_log.drain_from(rx);
        }

        // ── Rebuild key context ───────────────────────────────────────────────
        // Workspace base + generic pane tag + ViewKind-specific tag
        // (Stage 2.5d) + overlay tags. Drift-proof: the overlay stack
        // and the active view's kind are the sources of truth.
        let active_view_kind =
            self.state.workspace.active_view().map(|v| v.kind);
        let overlay_tags: Vec<&'static str> =
            self.state.overlays.context_tags().collect();
        self.state
            .focus
            .rebuild_context(active_view_kind, overlay_tags.into_iter());

        // ── Keymap dispatch ───────────────────────────────────────────────────
        // Single source of truth for keyboard shortcuts. Bindings live in
        // `keymap::KEYMAP`; this call is the *only* place app-level
        // `consume_key` runs. See `docs/focus-refactor.md` §2.4.
        let key_cmds = keymap::dispatch(&self.state.focus, &self.state, ctx);
        for c in key_cmds {
            enqueue(&mut self.state, c);
        }

        // ── File-open dialog lifecycle ────────────────────────────────────────
        // The dialog overlay drives itself via egui events; we tick its
        // state machine each frame. On pick or cancel we enqueue the
        // appropriate AppCommand and let `apply()` pop the overlay.
        let mut dialog_followup: Option<AppCommand> = None;
        if let Some(dialog) = self.state.overlays.file_dialog_mut() {
            dialog.update(ctx);
            if let Some(picked) = dialog.picked() {
                let path = picked.to_owned();
                dialog_followup = Some(AppCommand::OpenFile(path));
            } else if matches!(
                dialog.state(),
                egui_file_dialog::DialogState::Closed | egui_file_dialog::DialogState::Cancelled
            ) {
                dialog_followup = Some(AppCommand::DismissOverlay);
            }
        }
        if let Some(cmd) = dialog_followup {
            // OpenFile carries its own seq_view reset; DismissOverlay
            // pops the FileDialog from the stack. Both go through the
            // single applier.
            if matches!(cmd, AppCommand::OpenFile(_)) {
                self.state.overlays.pop_kind(crate::overlay::Overlay::TAG_FILE_DIALOG);
                // Dialog was accepted — discard the saved focus
                // snapshot. The `OpenFile` apply will move focus to
                // the newly-opened view; restoring the pre-dialog
                // pane afterward would undo that.
                self.state.focus_before_overlay = None;
            }
            enqueue(&mut self.state, cmd);
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
                    if ui.button("Split Right  ⌘\\").clicked() {
                        menu_cmds.push(AppCommand::SplitPane {
                            direction: crate::command::SplitDirection::Horizontal,
                        });
                        ui.close_menu();
                    }
                    if ui.button("Split Below").clicked() {
                        menu_cmds.push(AppCommand::SplitPane {
                            direction: crate::command::SplitDirection::Vertical,
                        });
                        ui.close_menu();
                    }
                    ui.separator();
                    if ui.button("Reset Layout").clicked() {
                        menu_cmds.push(AppCommand::ResetLayout);
                        ui.close_menu();
                    }
                });
                ui.menu_button("Tools", |ui| {
                    if ui.button("Restriction Sites…  ⌘E").clicked() {
                        menu_cmds.push(AppCommand::OpenEnzymes);
                        ui.close_menu();
                    }
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
                ui.menu_button("Settings", |ui| {
                    if ui.button("Open Settings…").clicked() {
                        menu_cmds.push(AppCommand::OpenSettingsFile);
                        ui.close_menu();
                    }
                    if ui.button("Open Keybindings…").clicked() {
                        menu_cmds.push(AppCommand::OpenKeybindingsFile);
                        ui.close_menu();
                    }
                    if ui.button("Open Theme File…").clicked() {
                        menu_cmds.push(AppCommand::OpenThemeFile);
                        ui.close_menu();
                    }
                    if ui.button("Open Config Folder").clicked() {
                        menu_cmds.push(AppCommand::OpenConfigDir);
                        ui.close_menu();
                    }
                    ui.separator();
                    if ui.button("Reload Config").clicked() {
                        menu_cmds.push(AppCommand::ReloadConfig);
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
        // Reads the active view + its buffer. A briefly-held read lock is
        // the only access the status bar needs; no events bus required.
        egui::TopBottomPanel::bottom("status_bar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 12.0;
                let info = self.state.workspace.active_view().and_then(|v| {
                    let buf_arc = self.state.workspace.buffers.get(v.buffer_id)?;
                    let buf = buf_arc.read().ok()?;
                    Some((buf.len(), format!("{:?}", buf.topology), v.selection))
                });
                if let Some((seq_len, topology, selection)) = info {
                    ui.label(format!("{seq_len} bp  ·  {topology}"));
                    if let Some(sel) = selection {
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
            });
        });

        // ── CLI install status window ─────────────────────────────────────────
        if let Some(msg) = self.state.overlays.cli_status().map(str::to_owned) {
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
            workspace,
            pending_commands,
            terminal,
            overlays,
            focus,
            minimap,
            config,
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
                            workspace,
                            pending_commands,
                            terminal,
                            overlays,
                            focus,
                            minimap,
                            config: config.clone(),
                        },
                    );
            });

        // ── Reconcile dock-internal focus with workspace.active_view ──────────
        // egui_dock activates tabs on its own (tab-strip clicks, drag).
        // Detect divergence and enqueue a SwitchTab so the workspace +
        // FocusScope catch up through the single-applier path.
        //
        // Only enqueue when the workspace actually knows the view —
        // otherwise we'd issue SwitchTab for a ghost tab every frame
        // and the applier would toast `ViewNotFound`. The startup
        // sanitizer should make ghost tabs impossible, but this guard
        // keeps the runtime resilient to drift from any future code
        // path that mutates the dock without updating the workspace.
        if let Some((_rect, Tab::View(vid))) = self.state.dock_state.find_active_focused()
        {
            let vid = *vid;
            if self.state.workspace.active_view != Some(vid)
                && self.state.workspace.views.contains_key(&vid)
            {
                self.state
                    .pending_commands
                    .push((AppCommand::SwitchTab { view: vid }, None));
            }
        }

        // ── Drain socket requests (Unix only) ─────────────────────────────────
        // Socket-originated `Open` is converted to `AppCommand::OpenFile` so
        // recents and `seq_view` stay in sync — `Viewer(req)` is the
        // generic pass-through for everything else.
        #[cfg(unix)]
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
