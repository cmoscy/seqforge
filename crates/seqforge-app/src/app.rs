use std::path::PathBuf;
use std::sync::mpsc;

use egui_dock::{DockArea, DockState, Style};
use seqforge_core::{
    BioOps, CutSite, DispatchError, Document, FeatureKind, SearchHit, ViewId, ViewSelection,
    ViewerRequest, ViewerResponse,
};

use std::sync::Arc;

use crate::browser::BrowserState;
use crate::command::{self, AppCommand, PendingCommand};
use crate::config::Config;
use crate::event::{AppEvent, EventLog, EventSink};
use crate::focus::{FocusScope, FocusState};
use crate::keymap;
use crate::minimap::MiniMap;
use crate::overlay::{FEATURE_KINDS, OverlayStack};
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

/// Upper bound (bp) for showing a melting temperature on a selection. The
/// nearest-neighbour Tm is a two-state *oligo* model (SantaLucia NN); beyond
/// typical oligo lengths — long primers with 5' tails top out around here — the
/// number stops being physically meaningful, so we show %GC alone. It also keeps
/// the per-frame status-bar compute bounded on large selections.
const MAX_SELECTION_TM_BP: usize = 120;

/// QC for a selected nucleotide region, for the status-bar readout (Phase 0.5):
/// `(%GC, Some(Tm °C))`. %GC is meaningful at any length; the nearest-neighbour
/// Tm is shown only for oligo-length selections (`2..=MAX_SELECTION_TM_BP`),
/// otherwise `None`. Returns `None` for an empty or non-UTF-8 region.
///
/// Reaches the vendored seqfold engine through `seqforge-bio`'s thin `tm`/`gc`
/// surface (`bio → thermo`); the same computation backs the future primer
/// dialog's QC panel. Pure — unit-tested below.
fn selection_qc(region: &[u8]) -> Option<(f64, Option<f64>)> {
    if region.is_empty() {
        return None;
    }
    let region = std::str::from_utf8(region).ok()?;
    let gc = seqforge_bio::gc(region);
    let tm = if (2..=MAX_SELECTION_TM_BP).contains(&region.len()) {
        seqforge_bio::tm(region).ok()
    } else {
        None
    };
    Some((gc, tm))
}

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

    fn resolve_enzyme_names(&self, seq: &[u8], query: &str, circular: bool) -> Vec<String> {
        let parsed = seqforge_bio::parse_enzyme_query(query);
        seqforge_bio::resolve_query_names(&parsed, seq, circular)
    }

    fn primer_infos(
        &self,
        seq: &[u8],
        primers: &[&seqforge_core::Primer],
        circular: bool,
    ) -> Vec<seqforge_core::PrimerInfo> {
        seqforge_bio::primer_infos(seq, primers, circular)
    }

    fn methyl_states_for_sites(
        &self,
        sites: &[CutSite],
        seq: &[u8],
        methylation: &seqforge_core::MethylContext,
    ) -> Vec<seqforge_core::MethylState> {
        seqforge_bio::methyl_states_for_sites(sites, seq, methylation)
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
    pub pending_file_state: std::collections::HashMap<PathBuf, crate::persistence::FileState>,
    /// User configuration (settings + theme + key overrides). Loaded
    /// from disk in `SeqForgeApp::new`; can be re-read at runtime via
    /// the `ReloadConfig` command. Wrapped in `Arc` so widgets cheaply
    /// clone a per-frame reference.
    pub config: Arc<Config>,
    /// In-memory clipboard for Cut/Copy/Paste (editor v0.2). Holds an annotated
    /// [`SeqSlice`] — bytes **plus** carried features/primers (feature-model
    /// track F1) — so paste re-homes annotations, not just bytes. The `.bytes()`
    /// projection feeds byte-only consumers (staged-paste preview, GUI clipboard
    /// interop via arboard, which is layered on separately).
    pub(crate) clipboard: Option<seqforge_core::SeqSlice>,
    /// Set while a Save-As file dialog is open, naming the view to write on
    /// pick. Discriminates the save-mode dialog from an open-mode one in the
    /// shared file-dialog pick handler. Cleared on pick or cancel.
    pub(crate) pending_save_as: Option<ViewId>,
    /// Set by `AppCommand::Quit`; the update loop reads it and routes an app
    /// quit through the same dirty-buffer intercept as an OS window close.
    pub(crate) quit_requested: bool,
    /// Last window title sent via `ViewportCommand::Title`, so the dirty-`*`
    /// title is refreshed only when it actually changes (not every frame).
    pub(crate) last_title: Option<String>,
    /// Set once a dirty-quit has been confirmed/cleared, so the OS close is
    /// allowed to proceed on the next `close_requested` without re-prompting.
    pub(crate) allow_close: bool,
    /// Inspector pane state (memoized primer projection for the active view).
    /// Refreshed once per frame before the dock renders. Singleton — holds no
    /// `ViewId`; reads whatever view is active.
    pub(crate) inspector: crate::inspector::InspectorState,
    /// Workbench shell region visibility (native panels — decision 19). The
    /// editor `CentralPanel` is always present; these gate the surrounding
    /// regions. Sizes are owned by egui's per-panel memory.
    pub(crate) show_inspector: bool,
    pub(crate) show_terminal: bool,
    pub(crate) show_files: bool,
    /// Minimap overview visibility. Independent of `show_inspector`: the two
    /// share the right column as sibling panels (Inspector fills, Minimap is the
    /// sized bottom strip) and either can be shown alone.
    pub(crate) show_minimap: bool,
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
            clipboard: None,
            pending_save_as: None,
            quit_requested: false,
            last_title: None,
            allow_close: false,
            inspector: crate::inspector::InspectorState::default(),
            show_inspector: true,
            show_terminal: true,
            show_files: true,
            show_minimap: true,
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

    // Flat workbench layout (decision 19): restore panel visibility, then reopen
    // the persisted documents as tabs in a fresh center dock.
    let wb = session.workbench;
    state.show_files = wb.show_files;
    state.show_terminal = wb.show_terminal;
    state.show_inspector = wb.show_inspector;
    state.show_minimap = wb.show_minimap;

    let mut view_ids: Vec<seqforge_core::ViewId> = Vec::new();
    for path in &wb.open_paths {
        match state.workspace.open_path(path, bio) {
            Ok(vid) => {
                if let Some(fs) = state.pending_file_state.remove(path) {
                    if let Some(view) = state.workspace.view_mut(vid) {
                        view.selection = fs
                            .selection
                            .map_or(ViewSelection::None, ViewSelection::Text);
                        view.scroll_pos = fs.scroll_pos;
                    }
                }
                view_ids.push(vid);
            }
            Err(e) => eprintln!("[seqforge] restore: failed to reopen {path:?}: {e}"),
        }
    }

    if view_ids.is_empty() {
        state.dock_state = DockState::new(vec![Tab::Welcome]);
        return;
    }

    state.dock_state = DockState::new(view_ids.iter().copied().map(Tab::View).collect());
    let active = wb.active.min(view_ids.len() - 1);
    let vid = view_ids[active];
    if let Some(loc) = state.dock_state.find_tab(&Tab::View(vid)) {
        state.dock_state.set_active_tab(loc);
    }
    state.workspace.focus_view(vid);
    state.focus.set_scope(FocusScope::View(vid));
}

/// Reset the dock to a fresh Welcome placeholder (the shell regions are native
/// panels — decision 19). Called on first launch (no saved session) and by
/// `AppCommand::ResetLayout`.
pub(crate) fn rebuild_default_dock(dock: &mut DockState<Tab>, _cfg: &Config) {
    // Files / Terminal / Inspector are native panels now (decision 19). The dock
    // holds only the center View tabs — a lone Welcome placeholder until a file
    // opens (Phase d moves the empty-state onto the CentralPanel directly).
    *dock = DockState::new(vec![Tab::Welcome]);
}

/// A click anywhere in a Files-sidebar sub-region focuses the Browser pane —
/// the native-panel equivalent of the old dock leaf-click → `FocusPane`.
fn focus_files_on_click(ui: &egui::Ui, focus: &FocusState, pending: &mut Vec<PendingCommand>) {
    if ui.rect_contains_pointer(ui.max_rect())
        && ui.ctx().input(|i| i.pointer.any_pressed())
        && focus.scope != FocusScope::Browser
    {
        pending.push((AppCommand::FocusPane(FocusScope::Browser), None));
    }
}

// ── SeqForgeApp ───────────────────────────────────────────────────────────────

pub struct SeqForgeApp {
    state: AppState,
}

impl SeqForgeApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        // Merge the Phosphor icon font into egui's defaults so UI glyphs (remove
        // ✕, delete trash, …) render — the bundled font lacks them (they tofu'd).
        let mut fonts = egui::FontDefinitions::default();
        egui_phosphor::add_to_fonts(&mut fonts, egui_phosphor::Variant::Regular);
        cc.egui_ctx.set_fonts(fonts);

        let mut state = AppState::default();
        let (config, cfg_warnings) = Config::load();
        state.config = config;
        for w in cfg_warnings {
            state.toasts.warning(w);
        }
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

            match socket::start_socket_listener(socket_path.clone(), cc.egui_ctx.clone()) {
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

        state.terminal =
            TerminalPane::new(cc.egui_ctx.clone(), &state.config.settings.terminal.shell)
                .map_err(|e| eprintln!("[seqforge] terminal init failed: {e}"))
                .ok();

        Self { state }
    }

    /// Render the Phase 14 feature/translation modal windows (centered egui
    /// Windows, mirroring the CLI-install status window). Each collects at most
    /// one command per frame and enqueues it through the single applier.
    fn show_feature_modals(&mut self, ctx: &egui::Context) {
        // ── Add / Edit Feature (one form; create vs edit = `form.id`) ──
        {
            let mut submit: Option<AppCommand> = None;
            let mut cancel = false;
            if let Some(form) = self.state.overlays.feature_form_mut() {
                let editing = form.is_edit();
                let (title, action) = if editing {
                    ("Edit Feature", "Save")
                } else {
                    ("New Feature", "Create")
                };
                let mut open = true;
                egui::Window::new(title)
                    .collapsible(false)
                    .resizable(false)
                    .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                    .open(&mut open)
                    .show(ctx, |ui| {
                        // Enter from the label field submits (Find-bar idiom): a
                        // singleline TextEdit gives up focus on Enter, so
                        // `lost_focus()` + the Enter key is the submit moment.
                        let mut submit_on_enter = false;
                        egui::Grid::new("feature_form")
                            .num_columns(2)
                            .spacing([12.0, 6.0])
                            .show(ui, |ui| {
                                ui.label("Label");
                                let r = ui.text_edit_singleline(&mut form.label);
                                if form.needs_focus {
                                    r.request_focus();
                                    form.needs_focus = false;
                                }
                                if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                                    submit_on_enter = true;
                                }
                                ui.end_row();

                                ui.label("Kind");
                                egui::ComboBox::from_id_salt("feature_form_kind")
                                    .selected_text(&form.kind)
                                    .show_ui(ui, |ui| {
                                        for k in FEATURE_KINDS {
                                            ui.selectable_value(
                                                &mut form.kind,
                                                (*k).to_string(),
                                                *k,
                                            );
                                        }
                                    });
                                ui.end_row();

                                ui.label("Strand");
                                ui.horizontal(|ui| {
                                    ui.selectable_value(&mut form.strand, "+".into(), "+ fwd");
                                    ui.selectable_value(&mut form.strand, "-".into(), "− rev");
                                    ui.selectable_value(&mut form.strand, ".".into(), ". none");
                                });
                                ui.end_row();

                                ui.label("Start");
                                ui.add(egui::DragValue::new(&mut form.start).range(0..=usize::MAX));
                                ui.end_row();

                                ui.label("End");
                                ui.add(
                                    egui::DragValue::new(&mut form.end)
                                        .range(form.start + 1..=usize::MAX),
                                );
                                ui.end_row();
                            });
                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            if ui.button(action).clicked() || submit_on_enter {
                                submit = Some(AppCommand::SubmitFeatureForm {
                                    id: form.id,
                                    label: form.label.clone(),
                                    kind: form.kind.clone(),
                                    strand: form.strand.clone(),
                                    start: form.start,
                                    end: form.end,
                                });
                            }
                            if ui.button("Cancel").clicked() {
                                cancel = true;
                            }
                        });
                    });
                if !open {
                    cancel = true;
                }
            }
            if let Some(cmd) = submit {
                enqueue(&mut self.state, cmd);
            } else if cancel {
                enqueue(&mut self.state, AppCommand::DismissOverlay);
            }
        }

        // ── Rename Feature ──
        {
            let mut submit: Option<AppCommand> = None;
            let mut cancel = false;
            if let Some(form) = self.state.overlays.rename_feature_mut() {
                let mut open = true;
                egui::Window::new("Rename Feature")
                    .collapsible(false)
                    .resizable(false)
                    .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                    .open(&mut open)
                    .show(ctx, |ui| {
                        ui.label("Label");
                        let r = ui.text_edit_singleline(&mut form.input);
                        if form.needs_focus {
                            r.request_focus();
                            form.needs_focus = false;
                        }
                        let submit_now =
                            r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            if ui.button("Rename").clicked() || submit_now {
                                submit = Some(AppCommand::SubmitRenameFeature {
                                    id: form.id,
                                    label: form.input.clone(),
                                });
                            }
                            if ui.button("Cancel").clicked() {
                                cancel = true;
                            }
                        });
                    });
                if !open {
                    cancel = true;
                }
            }
            if let Some(cmd) = submit {
                enqueue(&mut self.state, cmd);
            } else if cancel {
                enqueue(&mut self.state, AppCommand::DismissOverlay);
            }
        }

        // ── Translation (read-only) ──
        {
            let mut cancel = false;
            // Snapshot the active buffer's bytes once (owned, releases the read
            // lock) so we can compute the protein inside the window closure.
            let seq: Option<Vec<u8>> = self.state.workspace.active_view().and_then(|v| {
                let arc = self.state.workspace.buffers.get(v.buffer_id)?;
                let buf = arc.read().ok()?;
                Some(buf.text.clone())
            });
            if let Some(t) = self.state.overlays.translation_mut() {
                let mut open = true;
                egui::Window::new("Translation")
                    .collapsible(false)
                    .resizable(true)
                    .default_width(360.0)
                    .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                    .open(&mut open)
                    .show(ctx, |ui| {
                        ui.label(
                            egui::RichText::new(format!(
                                "{}  ·  {}..{} ({} bp)",
                                t.title,
                                t.start,
                                t.end,
                                t.end.saturating_sub(t.start)
                            ))
                            .strong(),
                        );
                        ui.horizontal(|ui| {
                            ui.checkbox(&mut t.all_frames, "All 6 frames");
                            if !t.all_frames {
                                ui.separator();
                                ui.label("Strand");
                                ui.selectable_value(
                                    &mut t.strand,
                                    seqforge_core::Strand::Forward,
                                    "+",
                                );
                                ui.selectable_value(
                                    &mut t.strand,
                                    seqforge_core::Strand::Reverse,
                                    "−",
                                );
                                ui.separator();
                                ui.label("Frame");
                                for f in 1..=3usize {
                                    ui.selectable_value(&mut t.frame, f, f.to_string());
                                }
                            }
                        });
                        ui.separator();
                        // Compute after the controls so edits show the same frame.
                        let range_seq = seq.as_ref().and_then(|s| {
                            let end = t.end.min(s.len());
                            (t.start < end).then(|| &s[t.start..end])
                        });
                        if t.all_frames {
                            // +1/+2/+3 then −1/−2/−3, each a labeled monospace row.
                            egui::Grid::new("translation_all_frames")
                                .num_columns(2)
                                .spacing([10.0, 4.0])
                                .show(ui, |ui| {
                                    for (strand, sign) in [
                                        (seqforge_core::Strand::Forward, '+'),
                                        (seqforge_core::Strand::Reverse, '-'),
                                    ] {
                                        for frame in 1..=3usize {
                                            let protein = range_seq
                                                .map(|s| seqforge_bio::translate(s, strand, frame))
                                                .unwrap_or_default();
                                            ui.label(
                                                egui::RichText::new(format!("{sign}{frame}"))
                                                    .strong(),
                                            );
                                            ui.add(
                                                egui::Label::new(
                                                    egui::RichText::new(protein).monospace(),
                                                )
                                                .wrap(),
                                            );
                                            ui.end_row();
                                        }
                                    }
                                });
                        } else {
                            let protein = range_seq
                                .map(|s| seqforge_bio::translate(s, t.strand, t.frame))
                                .unwrap_or_default();
                            ui.label(
                                egui::RichText::new(format!("{} aa", protein.chars().count()))
                                    .weak(),
                            );
                            ui.add(
                                egui::Label::new(egui::RichText::new(&protein).monospace()).wrap(),
                            );
                        }
                        ui.add_space(4.0);
                        // Read-only window: Enter closes (Escape is handled by
                        // the global overlay keymap, like every other dialog).
                        if ui.button("Close").clicked()
                            || ui.input(|i| i.key_pressed(egui::Key::Enter))
                        {
                            cancel = true;
                        }
                    });
                if !open {
                    cancel = true;
                }
            }
            if cancel {
                enqueue(&mut self.state, AppCommand::DismissOverlay);
            }
        }

        // ── Dirty-close / dirty-quit confirm ──
        if let Some((view_id, quitting)) = self.state.overlays.dirty_close_confirm() {
            let name = self.buffer_display_name(view_id);
            let mut choice: Option<DirtyChoice> = None;
            let mut open = true;
            let title = if quitting { "Quit" } else { "Close" };
            egui::Window::new(format!("Unsaved changes — {title}"))
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .open(&mut open)
                .show(ctx, |ui| {
                    ui.label(format!("\"{name}\" has unsaved changes."));
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("Save").clicked() {
                            choice = Some(DirtyChoice::Save);
                        }
                        if ui.button("Discard").clicked() {
                            choice = Some(DirtyChoice::Discard);
                        }
                        if ui.button("Cancel").clicked() {
                            choice = Some(DirtyChoice::Cancel);
                        }
                    });
                });
            if !open {
                choice = choice.or(Some(DirtyChoice::Cancel));
            }
            if let Some(choice) = choice {
                self.resolve_dirty_close(view_id, quitting, choice);
            }
        }

        // ── Save conflict (external change) ──
        if let Some((view_id, path)) = self.state.overlays.save_conflict() {
            let mut choice: Option<ConflictChoice> = None;
            let mut open = true;
            egui::Window::new("File changed on disk")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .open(&mut open)
                .show(ctx, |ui| {
                    ui.label(format!(
                        "\"{}\" changed on disk since it was opened.",
                        path.display()
                    ));
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("Overwrite").clicked() {
                            choice = Some(ConflictChoice::Overwrite);
                        }
                        if ui.button("Reload").clicked() {
                            choice = Some(ConflictChoice::Reload);
                        }
                        if ui.button("Cancel").clicked() {
                            choice = Some(ConflictChoice::Cancel);
                        }
                    });
                });
            if !open {
                choice = choice.or(Some(ConflictChoice::Cancel));
            }
            if let Some(choice) = choice {
                enqueue(&mut self.state, AppCommand::DismissOverlay);
                match choice {
                    ConflictChoice::Overwrite => enqueue(
                        &mut self.state,
                        AppCommand::Viewer(ViewerRequest::Save {
                            force: true,
                            view: Some(view_id),
                        }),
                    ),
                    ConflictChoice::Reload => enqueue(
                        &mut self.state,
                        AppCommand::RevertBuffer {
                            view: Some(view_id),
                        },
                    ),
                    ConflictChoice::Cancel => {}
                }
            }
        }

        // ── Confirm Revert ──
        if let Some(view_id) = self.state.overlays.confirm_revert() {
            let name = self.buffer_display_name(view_id);
            let mut do_revert = false;
            let mut cancel = false;
            let mut open = true;
            egui::Window::new("Revert to Saved")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .open(&mut open)
                .show(ctx, |ui| {
                    ui.label(format!(
                        "Discard all in-memory changes to \"{name}\" and reload from disk?"
                    ));
                    ui.label(
                        egui::RichText::new("This clears the undo history and cannot be undone.")
                            .weak(),
                    );
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("Revert").clicked() {
                            do_revert = true;
                        }
                        if ui.button("Cancel").clicked() {
                            cancel = true;
                        }
                    });
                });
            if !open {
                cancel = true;
            }
            if do_revert {
                enqueue(&mut self.state, AppCommand::DismissOverlay);
                enqueue(
                    &mut self.state,
                    AppCommand::RevertBuffer {
                        view: Some(view_id),
                    },
                );
            } else if cancel {
                enqueue(&mut self.state, AppCommand::DismissOverlay);
            }
        }
    }

    /// Resolve the dirty-close/quit modal. `Save` writes then re-issues the
    /// close/quit *only* for a path-backed buffer (a pathless Save opens the
    /// async Save-As dialog, so we don't auto-close underneath it). `Discard`
    /// clears the dirty flag so the re-issued close doesn't re-prompt.
    fn resolve_dirty_close(&mut self, view_id: ViewId, quitting: bool, choice: DirtyChoice) {
        enqueue(&mut self.state, AppCommand::DismissOverlay);
        match choice {
            DirtyChoice::Cancel => {}
            DirtyChoice::Save => {
                let has_path = self
                    .state
                    .workspace
                    .view(view_id)
                    .and_then(|v| self.state.workspace.buffers.get(v.buffer_id))
                    .and_then(|arc| arc.read().ok().map(|b| b.source_path.is_some()))
                    .unwrap_or(false);
                enqueue(
                    &mut self.state,
                    AppCommand::Viewer(ViewerRequest::Save {
                        force: false,
                        view: Some(view_id),
                    }),
                );
                if has_path {
                    self.enqueue_close_or_quit(view_id, quitting);
                }
            }
            DirtyChoice::Discard => {
                if let Some(v) = self.state.workspace.view(view_id) {
                    if let Some(arc) = self.state.workspace.buffers.get(v.buffer_id) {
                        if let Ok(mut b) = arc.write() {
                            b.dirty = false;
                        }
                    }
                }
                self.enqueue_close_or_quit(view_id, quitting);
            }
        }
    }

    fn enqueue_close_or_quit(&mut self, view_id: ViewId, quitting: bool) {
        if quitting {
            self.state.quit_requested = true;
        } else {
            enqueue(&mut self.state, AppCommand::CloseTab { view: view_id });
        }
    }

    /// Display name for a buffer behind a view (for modal copy).
    fn buffer_display_name(&self, view_id: ViewId) -> String {
        self.state
            .workspace
            .view(view_id)
            .and_then(|v| self.state.workspace.buffers.get(v.buffer_id))
            .and_then(|arc| arc.read().ok().map(|b| crate::workspace::display_name(&b)))
            .unwrap_or_else(|| "Untitled".to_string())
    }
}

/// User choice from the dirty-close/quit confirm modal.
#[derive(Clone, Copy)]
enum DirtyChoice {
    Save,
    Discard,
    Cancel,
}

/// User choice from the external-change conflict modal.
#[derive(Clone, Copy)]
enum ConflictChoice {
    Overwrite,
    Reload,
    Cancel,
}

/// Convenience: push a command with no response channel.
fn enqueue(state: &mut AppState, cmd: AppCommand) {
    state.pending_commands.push((cmd, None));
}

impl SeqForgeApp {
    /// Refresh the window title, prefixing `*` when the active buffer is dirty.
    /// Sends `ViewportCommand::Title` only when the title actually changes.
    fn sync_window_title(&mut self, ctx: &egui::Context) {
        let title = match self.state.workspace.active_view() {
            Some(v) => self
                .state
                .workspace
                .buffers
                .get(v.buffer_id)
                .and_then(|arc| {
                    let b = arc.read().ok()?;
                    let name = crate::workspace::display_name(&b);
                    let star = if b.dirty { "*" } else { "" };
                    Some(format!("{star}{name} — SeqForge"))
                })
                .unwrap_or_else(|| "SeqForge".to_string()),
            None => "SeqForge".to_string(),
        };
        if self.state.last_title.as_deref() != Some(title.as_str()) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.state.last_title = Some(title);
        }
    }

    /// Route an OS window-close or `AppCommand::Quit` through the dirty guard:
    /// with unsaved buffers, cancel the close and raise the confirm modal;
    /// otherwise let it proceed.
    fn handle_quit_intercept(&mut self, ctx: &egui::Context) {
        let os_close = ctx.input(|i| i.viewport().close_requested());
        let want_quit = os_close || std::mem::take(&mut self.state.quit_requested);
        if !want_quit {
            return;
        }
        // A prior confirm already cleared us to exit — let the close proceed.
        if self.state.allow_close {
            if !os_close {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            return;
        }
        if let Some(view_id) = self.first_dirty_view() {
            if os_close {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            }
            command::file::push_dirty_close_confirm(&mut self.state, view_id, true);
        } else if os_close {
            // Nothing dirty — allow the OS close to proceed.
        } else {
            self.state.allow_close = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    /// The first view whose buffer is dirty (any pane), if any.
    fn first_dirty_view(&self) -> Option<ViewId> {
        self.state.workspace.views.iter().find_map(|(id, v)| {
            let dirty = self
                .state
                .workspace
                .buffers
                .get(v.buffer_id)
                .and_then(|arc| arc.read().ok().map(|b| b.dirty))
                .unwrap_or(false);
            dirty.then_some(*id)
        })
    }
}

impl eframe::App for SeqForgeApp {
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        // Capture the current session as a path-keyed snapshot. ViewIds
        // and BufferIds are not persisted — they're session-scoped.
        let session = PersistedSession {
            recent_files: self.state.recent_files.clone(),
            workbench: persistence::capture_workbench(
                &self.state.dock_state,
                &self.state.workspace,
                self.state.show_files,
                self.state.show_terminal,
                self.state.show_inspector,
                self.state.show_minimap,
            ),
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

        // ── Dirty title bar + quit/close intercept ────────────────────────────
        self.sync_window_title(ctx);
        self.handle_quit_intercept(ctx);

        // ── Rebuild key context ───────────────────────────────────────────────
        // Workspace base + generic pane tag + ViewKind-specific tag
        // (Stage 2.5d) + overlay tags. Drift-proof: the overlay stack
        // and the active view's kind are the sources of truth.
        let active_view_kind = self.state.workspace.active_view().map(|v| v.kind);
        let mut overlay_tags: Vec<&'static str> = self.state.overlays.context_tags().collect();
        // Inline Inspector field-edit contributes a capture tag (Phase 1.5a), so
        // the keymap suppresses single-key user bindings while typing in a field.
        if self.state.inspector.is_editing() {
            overlay_tags.push(crate::focus::KeyContext::PANE_INSPECTOR_EDITING);
        }
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
                // A Save-As dialog (pending_save_as set) routes to SaveDocument;
                // an ordinary open dialog routes to OpenFile.
                dialog_followup = Some(match self.state.pending_save_as.take() {
                    Some(view) => AppCommand::SaveDocument {
                        view: Some(view),
                        path,
                    },
                    None => AppCommand::OpenFile(path),
                });
            } else if matches!(
                dialog.state(),
                egui_file_dialog::DialogState::Closed | egui_file_dialog::DialogState::Cancelled
            ) {
                self.state.pending_save_as = None;
                dialog_followup = Some(AppCommand::DismissOverlay);
            }
        }
        if let Some(cmd) = dialog_followup {
            // OpenFile/SaveDocument were accepted from the dialog — pop the
            // FileDialog now and discard the saved focus snapshot (OpenFile
            // moves focus to the new view; for a save we just return to the
            // active pane). DismissOverlay pops the dialog itself via apply.
            if matches!(
                cmd,
                AppCommand::OpenFile(_) | AppCommand::SaveDocument { .. }
            ) {
                self.state
                    .overlays
                    .pop_kind(crate::overlay::Overlay::TAG_FILE_DIALOG);
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
        // Selection-derived operands for the Edit menu's editor ops. A range
        // selection (not a bare cursor) feeds Cut/Copy/Delete/RC; the cursor
        // start is the paste position. `is_enabled` greys items whose operand
        // is missing, so these are only read when the action is enabled.
        let active_sel = self
            .state
            .workspace
            .active_view()
            .and_then(|v| v.selection.text_range());
        let sel_range = active_sel.filter(|s| !s.is_cursor()).map(|s| s.ordered());
        let paste_pos = active_sel.map(|s| s.ordered().0).unwrap_or(0);
        egui::TopBottomPanel::top("menu_bar").show(ctx, |ui| {
            egui::menu::bar(ui, |ui| {
                ui.menu_button("File", |ui| {
                    if ui.button("Open…  ⌘O").clicked() {
                        menu_cmds.push(AppCommand::PromptOpenFile);
                        ui.close_menu();
                    }
                    let can_close = command::is_enabled(&AppCommand::CloseDoc, &self.state);
                    if ui
                        .add_enabled(can_close, egui::Button::new("Close  ⌘W"))
                        .clicked()
                    {
                        menu_cmds.push(AppCommand::CloseDoc);
                        ui.close_menu();
                    }
                    ui.separator();
                    let save_req = ViewerRequest::Save {
                        force: false,
                        view: None,
                    };
                    let can_save =
                        command::is_enabled(&AppCommand::Viewer(save_req.clone()), &self.state);
                    if ui
                        .add_enabled(can_save, egui::Button::new("Save  ⌘S"))
                        .clicked()
                    {
                        menu_cmds.push(AppCommand::Viewer(save_req));
                        ui.close_menu();
                    }
                    let can_save_as =
                        command::is_enabled(&AppCommand::OpenSaveAs { view: None }, &self.state);
                    if ui
                        .add_enabled(can_save_as, egui::Button::new("Save As…  ⇧⌘S"))
                        .clicked()
                    {
                        menu_cmds.push(AppCommand::OpenSaveAs { view: None });
                        ui.close_menu();
                    }
                    // ── Revert (reload from disk) ──
                    let revert = AppCommand::OpenRevertConfirm { view: None };
                    let can_revert = command::is_enabled(&revert, &self.state);
                    if ui
                        .add_enabled(can_revert, egui::Button::new("Revert to Saved"))
                        .clicked()
                    {
                        menu_cmds.push(revert);
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
                    ui.separator();
                    if ui.button("Quit  ⌘Q").clicked() {
                        menu_cmds.push(AppCommand::Quit);
                        ui.close_menu();
                    }
                });
                ui.menu_button("Edit", |ui| {
                    // ── Undo / Redo ──
                    let undo_req = ViewerRequest::Undo { view: None };
                    let can_undo =
                        command::is_enabled(&AppCommand::Viewer(undo_req.clone()), &self.state);
                    if ui
                        .add_enabled(can_undo, egui::Button::new("Undo  ⌘Z"))
                        .clicked()
                    {
                        menu_cmds.push(AppCommand::Viewer(undo_req));
                        ui.close_menu();
                    }
                    let redo_req = ViewerRequest::Redo { view: None };
                    let can_redo =
                        command::is_enabled(&AppCommand::Viewer(redo_req.clone()), &self.state);
                    if ui
                        .add_enabled(can_redo, egui::Button::new("Redo  ⇧⌘Z"))
                        .clicked()
                    {
                        menu_cmds.push(AppCommand::Viewer(redo_req));
                        ui.close_menu();
                    }
                    ui.separator();

                    // ── Cut / Copy / Paste / Delete ── (operands from selection)
                    let range_probe = ViewerRequest::Cut {
                        start: 0,
                        end: 0,
                        view: None,
                    };
                    let has_range =
                        command::is_enabled(&AppCommand::Viewer(range_probe), &self.state);
                    if ui
                        .add_enabled(has_range, egui::Button::new("Cut  ⌘X"))
                        .clicked()
                    {
                        if let Some((start, end)) = sel_range {
                            // Stage (preview-before-commit), matching the ⌘X
                            // keyboard path — not an immediate mutation.
                            menu_cmds.push(AppCommand::StageEdit(command::StagedEdit::Cut {
                                start,
                                end,
                            }));
                        }
                        ui.close_menu();
                    }
                    if ui
                        .add_enabled(has_range, egui::Button::new("Copy  ⌘C"))
                        .clicked()
                    {
                        if let Some((start, end)) = sel_range {
                            menu_cmds.push(AppCommand::Viewer(ViewerRequest::Copy {
                                start,
                                end,
                                view: None,
                            }));
                        }
                        ui.close_menu();
                    }
                    let paste_req = ViewerRequest::Paste {
                        pos: paste_pos,
                        view: None,
                    };
                    let can_paste =
                        command::is_enabled(&AppCommand::Viewer(paste_req.clone()), &self.state);
                    if ui
                        .add_enabled(can_paste, egui::Button::new("Paste  ⌘V"))
                        .clicked()
                    {
                        // Stage the paste (preview), matching the ⌘V keyboard path.
                        menu_cmds.push(AppCommand::StageEdit(command::StagedEdit::Paste {
                            pos: paste_pos,
                        }));
                        ui.close_menu();
                    }
                    if ui
                        .add_enabled(has_range, egui::Button::new("Delete"))
                        .clicked()
                    {
                        if let Some((start, end)) = sel_range {
                            // Stage the delete (red-struck preview before commit).
                            menu_cmds.push(AppCommand::StageEdit(command::StagedEdit::Delete {
                                start,
                                end,
                            }));
                        }
                        ui.close_menu();
                    }
                    ui.separator();

                    // ── Reverse Complement ──
                    if ui
                        .add_enabled(has_range, egui::Button::new("Reverse Complement Selection"))
                        .clicked()
                    {
                        if let Some((start, end)) = sel_range {
                            menu_cmds.push(AppCommand::Viewer(ViewerRequest::ReverseComplement {
                                start,
                                end,
                                view: None,
                            }));
                        }
                        ui.close_menu();
                    }
                    ui.separator();

                    let can_find = command::is_enabled(&AppCommand::OpenFind, &self.state);
                    if ui
                        .add_enabled(can_find, egui::Button::new("Find…  ⌘F"))
                        .clicked()
                    {
                        menu_cmds.push(AppCommand::OpenFind);
                        ui.close_menu();
                    }
                });
                ui.menu_button("View", |ui| {
                    // Side-by-side is native: drag a document tab to an edge.
                    // ── Translation lanes (View → Translation) ──
                    let cur_trans = self
                        .state
                        .workspace
                        .active_view()
                        .and_then(|v| self.state.workspace.seq_views.get(&v.id))
                        .map(|sv| sv.translation.clone())
                        .unwrap_or_default();
                    let has_view = self.state.workspace.active_view().is_some();
                    ui.add_enabled_ui(has_view, |ui| {
                        ui.menu_button("Translation", |ui| {
                            let mut d = cur_trans;
                            let mut changed = false;
                            changed |= ui.checkbox(&mut d.show_cds, "CDS translations").changed();
                            ui.separator();
                            let labels = [
                                "Frame +1",
                                "Frame +2",
                                "Frame +3",
                                "Frame −1",
                                "Frame −2",
                                "Frame −3",
                            ];
                            for (i, lbl) in labels.iter().enumerate() {
                                changed |= ui.checkbox(&mut d.frames[i], *lbl).changed();
                            }
                            ui.separator();
                            changed |= ui
                                .checkbox(&mut d.show_orfs, "Show ORFs (mark stops / starts)")
                                .changed();
                            if changed {
                                menu_cmds.push(AppCommand::SetTranslationDisplay(d));
                            }
                        });
                    });
                    ui.separator();
                    let files_shown = self.state.show_files;
                    if ui.selectable_label(files_shown, "Files").clicked() {
                        menu_cmds.push(AppCommand::ToggleFiles);
                        ui.close_menu();
                    }
                    let inspector_shown = self.state.show_inspector;
                    if ui.selectable_label(inspector_shown, "Inspector").clicked() {
                        menu_cmds.push(AppCommand::ToggleInspector);
                        ui.close_menu();
                    }
                    let minimap_shown = self.state.show_minimap;
                    if ui.selectable_label(minimap_shown, "Minimap").clicked() {
                        menu_cmds.push(AppCommand::ToggleMinimap);
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

                    // ── Feature editing / translation (Phase 14) ──
                    let has_range = sel_range.is_some();
                    if ui
                        .add_enabled(has_range, egui::Button::new("New Feature from Selection…"))
                        .clicked()
                    {
                        if let Some((start, end)) = sel_range {
                            menu_cmds.push(AppCommand::OpenFeatureForm {
                                id: None,
                                label: String::new(),
                                kind: "misc_feature".to_string(),
                                strand: "+".to_string(),
                                start,
                                end,
                            });
                        }
                        ui.close_menu();
                    }
                    if ui
                        .add_enabled(has_range, egui::Button::new("Translate Selection…"))
                        .clicked()
                    {
                        if let Some((start, end)) = sel_range {
                            menu_cmds.push(AppCommand::OpenTranslation {
                                title: "Selection".to_string(),
                                start,
                                end,
                                strand: seqforge_core::Strand::Forward,
                                frame: 1,
                            });
                        }
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
                    if ui
                        .add_enabled(can_nav, egui::Button::new("Go to Position…  ⌘G"))
                        .clicked()
                    {
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
                    // Auto-translate the selected feature if it's a CDS (derived,
                    // read-only — decision 8). Frame from `/codon_start`, strand
                    // from the feature. Other kinds translate on demand via the
                    // Translate window, not here.
                    let cds = v.selection.selected_feature().and_then(|fid| {
                        let f = self
                            .state
                            .workspace
                            .buffers
                            .annotations(v.buffer_id)?
                            .get(fid)?;
                        if !matches!(FeatureKind::classify(&f.raw_kind), FeatureKind::Cds) {
                            return None;
                        }
                        // Hull translation (behavior-preserving): a joined CDS
                        // still translates through its hull. Segment-aware CDS
                        // translation is a follow-up (feature-model track F0 note).
                        let span = f.hull(buf.text.len());
                        let end = span.end.min(buf.text.len());
                        if span.start >= end {
                            return None;
                        }
                        let codon_start = f
                            .qualifiers
                            .get("codon_start")
                            .and_then(|x| x.as_deref())
                            .and_then(|s| s.trim().parse::<usize>().ok())
                            .filter(|n| (1..=3).contains(n))
                            .unwrap_or(1);
                        let protein = seqforge_bio::translate(
                            &buf.text[span.start..end],
                            f.strand,
                            codon_start,
                        );
                        Some((f.label.clone(), protein))
                    });
                    // Tm/%GC of the selected region (Phase 0.5), derived — decision
                    // 8. Feeds the top strand 5'→3' to the NN engine.
                    let sel_qc = v
                        .selection
                        .text_range()
                        .filter(|s| !s.is_cursor())
                        .and_then(|s| {
                            let (a, b) = s.ordered();
                            let end = b.min(buf.text.len());
                            if a >= end {
                                return None;
                            }
                            selection_qc(&buf.text[a..end])
                        });
                    Some((
                        v.id,
                        buf.len(),
                        format!("{:?}", buf.topology),
                        v.selection.text_range(),
                        cds,
                        sel_qc,
                    ))
                });
                if let Some((view_id, seq_len, topology, selection, cds, sel_qc)) = info {
                    ui.label(format!("{seq_len} bp  ·  {topology}"));
                    if let Some(sel) = selection {
                        if sel.is_cursor() {
                            ui.label(format!("pos {}", sel.anchor + 1));
                        } else {
                            let (s, e) = sel.ordered();
                            ui.label(format!("sel {s}–{e}  ({} bp)", e - s));
                        }
                    }
                    // Live Tm/%GC of the selected region (Phase 0.5). Weak, like
                    // the CDS protein below — derived, at-a-glance read-out.
                    if let Some((gc, tm)) = sel_qc {
                        let text = match tm {
                            Some(tm) => format!("Tm {tm:.1} °C  ·  {gc:.1}% GC"),
                            None => format!("{gc:.1}% GC"),
                        };
                        ui.label(egui::RichText::new(text).weak());
                    }
                    // Read-only CDS protein for the selected feature.
                    if let Some((label, protein)) = &cds {
                        let aa = protein.chars().count();
                        let shown: String = protein.chars().take(24).collect();
                        let ellipsis = if aa > 24 { "…" } else { "" };
                        let name = if label.is_empty() {
                            "CDS"
                        } else {
                            label.as_str()
                        };
                        ui.label(
                            egui::RichText::new(format!("{name}: {shown}{ellipsis} ({aa} aa)"))
                                .monospace()
                                .weak(),
                        );
                    }
                    // Staged-edit indicator (Phase 13.6). Lives here rather than
                    // floating in the canvas; the accent colour marks the active
                    // staging mode. The track-changes diff wash stays in-canvas.
                    let clipboard = self.state.clipboard.as_ref().map(|s| s.bytes());
                    if let Some(summary) = self
                        .state
                        .workspace
                        .seq_views
                        .get(&view_id)
                        .and_then(|sv| sv.staged_summary(clipboard))
                    {
                        let accent = ui.visuals().selection.stroke.color;
                        ui.label(
                            egui::RichText::new(format!("{summary}  ·  ⏎ commit  ·  esc cancel"))
                                .color(accent),
                        );
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

        // ── Feature modals (Phase 14) ─────────────────────────────────────────
        self.show_feature_modals(ctx);

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
            clipboard,
            inspector,
            show_inspector,
            show_terminal,
            show_files,
            show_minimap,
            ..
        } = &mut self.state;

        // Refresh the Inspector's memoized primer projection before the dock
        // reads it (version-keyed; a no-op when nothing changed).
        inspector.refresh(workspace, config.settings.inspector.follow_selection);

        // ── Terminal: native bottom region (decision 19) ──────────────────────
        // Drawn before the side panels so it spans full width (matches the prior
        // `split_below(root)` look); above the status bar (which is outermost).
        if *show_terminal {
            egui::TopBottomPanel::bottom("terminal_panel")
                .resizable(true)
                .default_height(180.0)
                .show(ctx, |ui| {
                    match terminal.as_mut() {
                        Some(term) => {
                            let has_focus =
                                focus.scope == FocusScope::Terminal && overlays.is_empty();
                            term.show(ui, has_focus);
                        }
                        None => {
                            ui.centered_and_justified(|ui| {
                                ui.label(
                                    "Terminal failed to initialise.\nCheck stderr for details.",
                                );
                            });
                        }
                    }
                    if ui.rect_contains_pointer(ui.max_rect())
                        && ui.ctx().input(|i| i.pointer.any_pressed())
                        && focus.scope != FocusScope::Terminal
                    {
                        pending_commands.push((AppCommand::FocusPane(FocusScope::Terminal), None));
                    }
                });
        }

        // ── Files: native left region — workspace navigation (decision 19) ────
        // Browser only; the minimap is document-scoped and lives on the right.
        if *show_files {
            egui::SidePanel::left("files_panel")
                .resizable(true)
                .default_width(220.0)
                .show(ctx, |ui| {
                    if let Some(path) = browser.show(ui) {
                        pending_commands.push((AppCommand::OpenFile(path), None));
                    }
                    focus_files_on_click(ui, focus, pending_commands);
                });
        }

        // ── Right region — active-document surface (decision 19) ──────────────
        // egui's canonical layout (see its `panels.rs` demo): one resizable
        // *sized* panel + a `CentralPanel` that fills the remainder — never two
        // stacked resizable panels, never a spacer. Role assignment matters: the
        // sized panel wants a stable, bounded size, while the CentralPanel is the
        // *greedy* one and is the natural home for unbounded, list-like content.
        // So the **Minimap** (a fixed sequence overview — bounded) is the sized
        // bottom panel, and the **Inspector** (primer/enzyme lists — unbounded)
        // is the greedy CentralPanel that fills the rest and gets the majority by
        // default. A single divider splits the whole column: drag it up to grow
        // the Minimap (Inspector → 0), down to shrink the Minimap to its small min
        // (Inspector maximized). egui clamps a dragged panel only to its
        // `height_range`, never to content-min (see `TopBottomPanel::show_inside_dyn`),
        // so the divider ranges nearly the full column and each side simply shows
        // whitespace when its content is short. A small Minimap min keeps its
        // handle grabbable; full-hide of either is on the View toggles. The
        // column owns its width; egui owns every height. Minimap clicks are
        // focus-neutral (they only scroll the active view).
        if *show_inspector || *show_minimap {
            egui::SidePanel::right("inspector_panel")
                .resizable(true)
                .default_width(280.0)
                .width_range(220.0..=560.0)
                .show(ctx, |ui| {
                    match (*show_inspector, *show_minimap) {
                        // Both visible: Minimap = sized bottom panel (single
                        // divider, ~200px default), Inspector = greedy CentralPanel
                        // remainder above it (gets the majority by default).
                        (true, true) => {
                            egui::TopBottomPanel::bottom("minimap_body")
                                .resizable(true)
                                .default_height(200.0)
                                .height_range(48.0..=f32::INFINITY)
                                .show_inside(ui, |ui| {
                                    minimap.show(ui, workspace, pending_commands, config);
                                });
                            egui::CentralPanel::default().show_inside(ui, |ui| {
                                inspector.show(ui, pending_commands);
                            });
                        }
                        // Only the Inspector: it fills the whole column.
                        (true, false) => {
                            egui::CentralPanel::default().show_inside(ui, |ui| {
                                inspector.show(ui, pending_commands);
                            });
                        }
                        // Only the minimap: it fills the whole column.
                        (false, true) => {
                            egui::CentralPanel::default().show_inside(ui, |ui| {
                                minimap.show(ui, workspace, pending_commands, config);
                            });
                        }
                        // Unreachable: the column is gated off when both are hidden.
                        (false, false) => {}
                    }
                    // Click anywhere in the pane (outside the minimap's GoTo
                    // targets) → focus the Inspector.
                    if ui.rect_contains_pointer(ui.max_rect())
                        && ui.ctx().input(|i| i.pointer.any_pressed())
                        && focus.scope != FocusScope::Inspector
                    {
                        pending_commands.push((AppCommand::FocusPane(FocusScope::Inspector), None));
                    }
                });
        }

        egui::CentralPanel::default()
            .frame(egui::Frame::central_panel(&ctx.style()).inner_margin(0.0))
            .show(ctx, |ui| {
                // Draggable tabs (default) give native side-by-side — drag a
                // document tab to an edge to split. `show_inside` keeps everything
                // docked (no floating windows), consistent with the native shell.
                DockArea::new(dock_state)
                    .style(Style::from_egui(ui.style()))
                    .show_inside(
                        ui,
                        &mut TabViewer {
                            workspace,
                            pending_commands,
                            overlays,
                            focus,
                            clipboard: clipboard.as_ref().map(|s| s.bytes()),
                            config: config.clone(),
                        },
                    );
            });

        // ── Derive workspace.active_view from egui_dock's focus (one-way) ─────
        // egui_dock owns the center's tab focus (clicks, drag, tab close). We
        // *mirror* its focused view into the workspace **inline** — not by
        // enqueuing a `SwitchTab`, which could outlive a view being closed in the
        // same frame (→ `ViewNotFound`). Programmatic focus (cycle / CLI `focus`)
        // drives egui's focus via `set_focused_node_and_surface`, so this read
        // agrees with it. Guarded on `contains_key` for resilience to any drift.
        if let Some((_rect, Tab::View(vid))) = self.state.dock_state.find_active_focused() {
            let vid = *vid;
            if self.state.workspace.active_view != Some(vid)
                && self.state.workspace.views.contains_key(&vid)
            {
                self.state.workspace.focus_view(vid);
                self.state.focus.set_scope(FocusScope::View(vid));
                self.state.events.emit(AppEvent::TabSwitched { view: vid });
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
                // A SaveConflict already raised the Overwrite/Reload/Cancel modal
                // (and the socket client sees the structured error); don't also
                // toast it in the GUI.
                if !matches!(e, DispatchError::SaveConflict(_)) {
                    self.state.toasts.error(e.to_string());
                }
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

#[cfg(test)]
mod tests {
    use super::{MAX_SELECTION_TM_BP, rebuild_default_dock, selection_qc};
    use crate::tabs::Tab;

    #[test]
    fn default_dock_is_only_the_center_welcome() {
        // Shell regions are native panels now (decision 19); the dock holds only
        // the center — a lone Welcome placeholder until a file opens.
        let cfg = crate::config::Config::default();
        let mut dock = egui_dock::DockState::new(vec![Tab::Welcome]);
        rebuild_default_dock(&mut dock, &cfg);
        let tree = dock.main_surface();
        let tabs: Vec<&Tab> = (0..tree.len())
            .filter_map(|i| match &tree[egui_dock::NodeIndex(i)] {
                egui_dock::Node::Leaf { tabs, .. } => Some(tabs.iter()),
                _ => None,
            })
            .flatten()
            .collect();
        assert_eq!(tabs, vec![&Tab::Welcome], "dock is just the center Welcome");
    }

    #[test]
    fn selection_qc_reports_gc_and_tm_for_an_oligo() {
        // GGGACCGCCT: seqfold's Owczarzy reference oligo (Tm ≈ 51.9 ± 7).
        let (gc, tm) = selection_qc(b"GGGACCGCCT").unwrap();
        assert_eq!(gc, 80.0);
        let tm = tm.expect("oligo-length selection should carry a Tm");
        assert!((tm - 51.9).abs() <= 7.0, "tm {tm} off reference");
    }

    #[test]
    fn selection_qc_gc_only_below_two_bp() {
        // A single base: %GC is defined, Tm is not (NN needs a pair).
        let (gc, tm) = selection_qc(b"G").unwrap();
        assert_eq!(gc, 100.0);
        assert!(tm.is_none());
    }

    #[test]
    fn selection_qc_drops_tm_past_the_oligo_cap() {
        // A selection longer than the oligo cap keeps %GC but hides the
        // (meaningless) Tm.
        let long: Vec<u8> = b"AT"
            .iter()
            .copied()
            .cycle()
            .take(MAX_SELECTION_TM_BP + 2)
            .collect();
        let (gc, tm) = selection_qc(&long).unwrap();
        assert_eq!(gc, 0.0);
        assert!(
            tm.is_none(),
            "Tm should be suppressed past {MAX_SELECTION_TM_BP} bp"
        );
    }

    #[test]
    fn selection_qc_none_for_empty() {
        assert!(selection_qc(b"").is_none());
    }
}
