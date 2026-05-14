use std::sync::mpsc;

use clap::Parser;
use egui_term::{BackendSettings, PtyEvent, TerminalBackend, TerminalView};
use seqforge_core::{ViewerCli, ViewerCommand};

// ── TerminalPane ──────────────────────────────────────────────────────────────

pub struct TerminalPane {
    backend: TerminalBackend,
    /// Must be held open; dropping it would break the PTY event subscription thread.
    _pty_rx: mpsc::Receiver<(u64, PtyEvent)>,
    /// Present when the user has typed `:` and we are buffering a viewer command.
    intercept_buf: Option<String>,
}

impl TerminalPane {
    pub fn new(ctx: egui::Context, socket_path: Option<&std::path::Path>) -> anyhow::Result<Self> {
        // Expose the session socket to subprocesses before the PTY inherits our env.
        // Safety: called from the main thread at app startup; no concurrent env reads.
        if let Some(path) = socket_path {
            unsafe { std::env::set_var("SEQFORGE_SOCKET", path) };
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

        Ok(Self {
            backend,
            _pty_rx: rx,
            intercept_buf: None,
        })
    }

    /// Render the terminal pane. Returns a `ViewerCommand` if the user submitted a `:` line.
    pub fn show(&mut self, ui: &mut egui::Ui) -> Option<ViewerCommand> {
        let available = ui.available_size();

        // ── : command intercept ───────────────────────────────────────────────
        //
        // TerminalView reads events via `ctx.input(|i| i.events.clone())`.
        // By draining matching events from ctx.input_mut *before* TerminalView
        // renders, we prevent them from reaching the PTY.

        let mut result = None;

        if self.intercept_buf.is_some() {
            // Take ownership so we can mutate freely without borrow conflicts.
            let mut buf = self.intercept_buf.take().unwrap();
            let events: Vec<egui::Event> = ui.ctx().input_mut(|i| i.events.drain(..).collect());
            let mut passthrough: Vec<egui::Event> = Vec::new();
            let mut submitted = false;
            let mut cancelled = false;

            for ev in events {
                if submitted || cancelled {
                    passthrough.push(ev);
                    continue;
                }
                match &ev {
                    egui::Event::Text(t) => buf.push_str(t),
                    egui::Event::Key { key: egui::Key::Backspace, pressed: true, .. } => {
                        if buf.len() > 1 {
                            buf.pop();
                        } else {
                            cancelled = true;
                        }
                    }
                    egui::Event::Key { key: egui::Key::Enter, pressed: true, .. } => {
                        submitted = true;
                    }
                    egui::Event::Key { key: egui::Key::Escape, pressed: true, .. } => {
                        cancelled = true;
                    }
                    _ => passthrough.push(ev),
                }
            }

            // Return non-text events (mouse, scroll) so the terminal can still use them.
            ui.ctx().input_mut(|i| {
                let mut combined = passthrough;
                combined.extend(i.events.drain(..));
                i.events = combined;
            });

            if submitted {
                result = parse_colon_command(buf.trim_start_matches(':').trim());
                // intercept_buf stays None (buf consumed)
            } else if cancelled {
                // intercept_buf stays None
            } else {
                self.intercept_buf = Some(buf);
            }
        } else {
            // Watch for a lone ':' keystroke to enter intercept mode.
            let triggered = ui.ctx().input_mut(|i| {
                if let Some(pos) = i.events.iter().position(|e| {
                    matches!(e, egui::Event::Text(t) if t == ":")
                }) {
                    i.events.remove(pos);
                    true
                } else {
                    false
                }
            });
            if triggered {
                self.intercept_buf = Some(":".to_string());
            }
        }

        // ── Terminal widget ───────────────────────────────────────────────────
        let cmd_bar_h: f32 = if self.intercept_buf.is_some() { 28.0 } else { 0.0 };
        let term_size = egui::Vec2::new(available.x, (available.y - cmd_bar_h).max(20.0));

        // Create the view before calling ui.add to avoid a double-borrow of `ui`.
        let view = TerminalView::new(ui, &mut self.backend)
            .set_focus(true)
            .set_size(term_size);
        ui.add(view);

        // ── Command bar overlay ───────────────────────────────────────────────
        if let Some(buf) = &self.intercept_buf {
            ui.separator();
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(buf.as_str())
                        .monospace()
                        .color(egui::Color32::YELLOW),
                );
                ui.label(egui::RichText::new("▌").monospace().color(egui::Color32::YELLOW));
            });
        }

        result
    }
}

// ── Parser ────────────────────────────────────────────────────────────────────

/// Parse a colon command line (without the leading `:`) into a `ViewerCommand`.
/// Returns `None` for empty input or unrecognised commands.
pub fn parse_colon_command(args: &str) -> Option<ViewerCommand> {
    if args.is_empty() {
        return None;
    }
    let tokens: Vec<&str> = std::iter::once("seqforge")
        .chain(args.split_whitespace())
        .collect();
    ViewerCli::try_parse_from(tokens).ok().map(|c| c.command)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colon_goto_parses() {
        let cmd = parse_colon_command("goto 100").unwrap();
        assert!(matches!(cmd, ViewerCommand::GoTo { position: 100 }));
    }

    #[test]
    fn colon_find_parses() {
        let cmd = parse_colon_command("find ATGC").unwrap();
        assert!(matches!(cmd, ViewerCommand::Find { ref pattern, mismatches: 0 } if pattern == "ATGC"));
    }

    #[test]
    fn colon_enzymes_parses() {
        let cmd = parse_colon_command("enzymes EcoRI BamHI").unwrap();
        assert!(
            matches!(cmd, ViewerCommand::Enzymes { ref enzymes } if enzymes == &["EcoRI", "BamHI"])
        );
    }

    #[test]
    fn empty_colon_returns_none() {
        assert!(parse_colon_command("").is_none());
    }

    #[test]
    fn unrecognised_command_returns_none() {
        assert!(parse_colon_command("notacommand arg").is_none());
    }
}
