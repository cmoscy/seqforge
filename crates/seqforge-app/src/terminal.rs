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

// ── TerminalPane ──────────────────────────────────────────────────────────────

pub struct TerminalPane {
    backend: TerminalBackend,
    /// Must be held open; dropping it would break the PTY event subscription thread.
    _pty_rx: mpsc::Receiver<(u64, PtyEvent)>,
}

impl TerminalPane {
    pub fn new(ctx: egui::Context, socket_path: Option<&std::path::Path>) -> anyhow::Result<Self> {
        // Safety: called from the main thread at app startup; no concurrent env reads.
        unsafe {
            // Expose the session socket so subprocesses can dispatch viewer commands.
            if let Some(path) = socket_path {
                std::env::set_var("SEQFORGE_SOCKET", path);
            }

            // If a `seqforge` binary lives next to the running app binary, prepend its
            // directory to PATH so the embedded terminal can use it without a separate
            // `cargo install` step. Mirrors how VS Code exposes its `code` CLI.
            if let Some(bin_dir) = sibling_seqforge_dir() {
                let current_path = std::env::var("PATH").unwrap_or_default();
                let new_path = format!("{}:{}", bin_dir.display(), current_path);
                std::env::set_var("PATH", new_path);
            }

            // Isolate embedded-terminal history from the user's main shell history.
            // Both bash and zsh honour HISTFILE, so this prevents seqforge commands
            // from appearing in the user's global history or autocomplete.
            if let Ok(home) = std::env::var("HOME") {
                let dir = std::path::PathBuf::from(&home).join(".local/share/seqforge");
                let _ = std::fs::create_dir_all(&dir);
                std::env::set_var("HISTFILE", dir.join("terminal_history"));
            }
        }

        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string());
        let settings = BackendSettings {
            shell,
            // stub: sandbox_wrapper — post-MVP: prepend args to shell command
            //   (e.g. ["sandbox-exec", "-f", "profile.sb"])
            ..BackendSettings::default()
        };

        let (tx, rx) = mpsc::channel();
        let backend = TerminalBackend::new(1, ctx, tx, settings)?;

        Ok(Self { backend, _pty_rx: rx })
    }

    /// Render the terminal pane. Viewer commands reach the GUI via the session
    /// socket (`seqforge goto 100`, `seqforge find ATGC`, etc.) — the CLI
    /// detects `SEQFORGE_SOCKET` and routes them without any keystroke intercept.
    pub fn show(&mut self, ui: &mut egui::Ui) {
        let term_size = ui.available_size();
        // Create the view before calling ui.add to avoid a double-borrow of `ui`.
        let view = TerminalView::new(ui, &mut self.backend)
            .set_focus(true)
            .set_size(term_size);
        ui.add(view);
    }
}
