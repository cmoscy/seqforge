use std::sync::mpsc;

use egui_term::{BackendSettings, PtyEvent, TerminalBackend, TerminalView};

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Returns the directory containing a `seqforge` binary that is a sibling of
/// the running app binary, or `None` if no such binary exists.
///
/// When both crates are built via `cargo build`, they land in the same
/// `target/{profile}/` directory, so the embedded terminal can find the CLI
/// without a separate `cargo install`.
fn sibling_seqforge_dir() -> Option<std::path::PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    let candidate = dir.join("seqforge");
    candidate.exists().then(|| dir.to_owned())
}

// ── Process-wide env setup ────────────────────────────────────────────────────

/// Install the process-wide environment variables that the embedded shell (and
/// any `seqforge` CLI invocations within it) inherit.
///
/// **MUST be called from the main thread before any other thread is spawned**,
/// in particular before [`crate::socket::start_socket_listener`]. Rust 2024
/// treats `std::env::set_var` as `unsafe` because concurrent env access from
/// another thread is UB; sequencing this strictly first is how we keep it safe.
///
/// Sets:
/// - `SEQFORGE_SOCKET` — the Unix socket path for viewer-command dispatch.
/// - `PATH` — prepends the directory of a sibling `seqforge` binary so the
///   embedded terminal can use it without `cargo install` (mirrors VS Code's
///   `code` CLI pattern).
/// - `HISTFILE` — isolates embedded-terminal history from the user's main
///   shell history.
pub fn install_pty_env(socket_path: Option<&std::path::Path>) {
    // Safety: caller contract is "main thread, before any thread spawns".
    unsafe {
        if let Some(path) = socket_path {
            std::env::set_var("SEQFORGE_SOCKET", path);
        }

        if let Some(bin_dir) = sibling_seqforge_dir() {
            let current_path = std::env::var("PATH").unwrap_or_default();
            let new_path = format!("{}:{}", bin_dir.display(), current_path);
            std::env::set_var("PATH", new_path);
        }

        if let Ok(home) = std::env::var("HOME") {
            let dir = std::path::PathBuf::from(&home).join(".local/share/seqforge");
            let _ = std::fs::create_dir_all(&dir);
            std::env::set_var("HISTFILE", dir.join("terminal_history"));
        }
    }
}

// ── TerminalPane ──────────────────────────────────────────────────────────────

pub struct TerminalPane {
    backend: TerminalBackend,
    /// Must be held open; dropping it would break the PTY event subscription thread.
    _pty_rx: mpsc::Receiver<(u64, PtyEvent)>,
}

impl TerminalPane {
    /// Construct the embedded terminal. Assumes [`install_pty_env`] has
    /// already been called on the main thread before any thread was spawned.
    pub fn new(ctx: egui::Context, configured_shell: &str) -> anyhow::Result<Self> {
        let shell = if !configured_shell.is_empty() {
            configured_shell.to_string()
        } else {
            std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string())
        };
        let settings = BackendSettings {
            shell,
            // stub: sandbox_wrapper — post-MVP: prepend args to shell command
            //   (e.g. ["sandbox-exec", "-f", "profile.sb"])
            ..BackendSettings::default()
        };

        let (tx, rx) = mpsc::channel();
        let backend = TerminalBackend::new(1, ctx, tx, settings)?;

        Ok(Self {
            backend,
            _pty_rx: rx,
        })
    }

    /// Render the terminal pane. Viewer commands reach the GUI via the session
    /// socket (`seqforge goto 100`, `seqforge find ATGC`, etc.) — the CLI
    /// detects `SEQFORGE_SOCKET` and routes them without any keystroke intercept.
    ///
    /// `terminal_has_focus` is the single keyboard-ownership signal,
    /// computed in `tabs.rs` as `focus.scope == Terminal && overlays.is_empty()`.
    /// The terminal does not probe egui memory or any other widget state —
    /// state flows outward (see `docs/focus-refactor.md` §2.1).
    pub fn show(&mut self, ui: &mut egui::Ui, terminal_has_focus: bool) {
        let term_size = ui.available_size();
        // Create the view before calling ui.add to avoid a double-borrow of `ui`.
        let view = TerminalView::new(ui, &mut self.backend)
            .set_focus(terminal_has_focus)
            .set_size(term_size);
        ui.add(view);
    }
}
