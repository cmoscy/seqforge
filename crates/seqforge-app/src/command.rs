//! Typed application commands and the single mutation site (`apply`).
//!
//! See [`docs/focus-refactor.md`](../../../docs/focus-refactor.md) §2.2.
//!
//! Stage 2 of the focus refactor: every user-, menu-, hotkey-, bar-, and
//! socket-initiated action becomes an [`AppCommand`]. The `update()` loop
//! drains a queue of these and routes them through [`apply`] — the *only*
//! function that mutates [`AppState`] in response to a command.
//!
//! Why a closed enum: the binding set is fixed during this refactor.
//! Plugin extensibility (an `AppCommand::Custom` variant + handler
//! registry) is deferred to the future plugin work (§7 of the refactor
//! doc). Keeping the enum closed gives us exhaustive-match safety in
//! `apply()` and `is_enabled()`.

use std::path::PathBuf;
use std::sync::mpsc;

use egui_file_dialog::FileDialog;
use seqforge_core::{
    dispatch, BioOps, DispatchError, ViewerRequest, ViewerResponse,
};

use crate::app::AppState;
use crate::bar::ActiveBar;
use crate::cli_install;
use crate::focus::FocusScope;

/// A queued command plus the optional one-shot channel that returns
/// the dispatch result. `None` for menu/hotkey/bar-originated commands;
/// `Some(tx)` for socket-originated commands (the CLI client awaits
/// the response over JSON-RPC).
pub type PendingCommand = (
    AppCommand,
    Option<mpsc::SyncSender<Result<ViewerResponse, DispatchError>>>,
);

/// Every user-, agent-, or code-initiated action. Closed enum.
///
/// `Viewer(ViewerRequest)` wraps the existing `seqforge-core` request
/// type so the JSON-RPC wire format and `dispatch()` path are
/// unchanged. GUI-only commands live alongside as explicit variants.
#[derive(Debug, Clone)]
pub enum AppCommand {
    // ── File / document ──────────────────────────────────────────────
    /// Open the native file-picker dialog.
    PromptOpenFile,
    /// Open a specific file by path (recent files, drag-and-drop,
    /// dialog completion all funnel through this).
    OpenFile(PathBuf),
    /// Clear the recent-files list.
    ClearRecent,
    /// Close the currently open document.
    CloseDoc,

    // ── Overlays ─────────────────────────────────────────────────────
    /// Open the inline Find bar.
    OpenFind,
    /// Open the inline GoTo bar.
    OpenGoTo,
    /// Close the topmost overlay (Stage 5 will generalise this to a
    /// proper overlay stack; Stage 2 only knows about `active_bar`).
    DismissOverlay,
    /// Bar submission: run a search.
    SubmitFind { pattern: String, mismatches: u8 },
    /// Bar submission: jump to a 1-based position.
    SubmitGoTo { position: usize },
    /// Acknowledge the CLI-install result window.
    DismissCliStatus,

    // ── Focus / layout ───────────────────────────────────────────────
    /// Explicit focus move (Stage 4 keymap and programmatic focus
    /// transfers route through this).
    FocusPane(FocusScope),
    /// Reset the dock layout to defaults.
    ResetLayout,

    // ── Tools ────────────────────────────────────────────────────────
    /// Symlink the bundled CLI into PATH.
    InstallCli,

    // ── Pass-through ─────────────────────────────────────────────────
    /// Wrap a raw `ViewerRequest` — used by the socket consumer and by
    /// any future caller that wants to drive `dispatch()` directly.
    Viewer(ViewerRequest),
}

/// Predicate: is this command currently runnable?
///
/// Used by:
/// - menu rendering to grey unavailable items,
/// - the keymap dispatcher (Stage 4) to gate `consume_key`,
/// - future agent reject paths to return a clear error.
pub fn is_enabled(cmd: &AppCommand, state: &AppState) -> bool {
    use AppCommand::*;
    match cmd {
        OpenFind | OpenGoTo | SubmitFind { .. } | SubmitGoTo { .. } | CloseDoc => {
            state.viewer.open_doc.is_some()
        }
        // Pass-through: the underlying dispatcher enforces preconditions.
        Viewer(_) => true,
        // Universally available.
        PromptOpenFile | OpenFile(_) | ClearRecent | DismissOverlay | DismissCliStatus
        | FocusPane(_) | ResetLayout | InstallCli => true,
    }
}

/// The single mutation site. Every command's effect on `AppState` is
/// here; nowhere else in the app may construct a `ViewerRequest` or
/// directly mutate the same fields.
///
/// Returns the `ViewerResponse` for commands that drive
/// `seqforge_core::dispatch` (so the socket caller can be notified);
/// `Ok(None)` for purely GUI-side commands.
pub fn apply<B: BioOps>(
    cmd: AppCommand,
    state: &mut AppState,
    bio: &B,
) -> Result<Option<ViewerResponse>, DispatchError> {
    use AppCommand::*;
    match cmd {
        PromptOpenFile => {
            let mut dialog = FileDialog::new();
            dialog.pick_file();
            state.open_dialog = Some(dialog);
            Ok(None)
        }

        OpenFile(path) => {
            state.seq_view.reset();
            state.recent_files.retain(|p| p != &path);
            state.recent_files.insert(0, path.clone());
            state.recent_files.truncate(crate::app::MAX_RECENT);
            let resp = dispatch(&mut state.viewer, bio, ViewerRequest::Open { path })?;
            // Stage 3 will emit AppEvent::DocOpened here.
            Ok(Some(resp))
        }

        ClearRecent => {
            state.recent_files.clear();
            Ok(None)
        }

        CloseDoc => {
            let resp = dispatch(&mut state.viewer, bio, ViewerRequest::Close)?;
            // Stage 3 will emit AppEvent::DocClosed here.
            Ok(Some(resp))
        }

        OpenFind => {
            state
                .active_bar
                .get_or_insert_with(|| ActiveBar::Find(Default::default()));
            Ok(None)
        }

        OpenGoTo => {
            state
                .active_bar
                .get_or_insert_with(|| ActiveBar::GoTo(Default::default()));
            Ok(None)
        }

        DismissOverlay => {
            // Stage 5 will pop a real OverlayStack; for now the only
            // bar-type overlay is `active_bar`.
            state.active_bar = None;
            Ok(None)
        }

        SubmitFind { pattern, mismatches } => {
            state.active_bar = None;
            let resp = dispatch(
                &mut state.viewer,
                bio,
                ViewerRequest::Find { pattern, mismatches },
            )?;
            Ok(Some(resp))
        }

        SubmitGoTo { position } => {
            state.active_bar = None;
            let resp = dispatch(
                &mut state.viewer,
                bio,
                ViewerRequest::GoTo { position },
            )?;
            Ok(Some(resp))
        }

        DismissCliStatus => {
            state.cli_status = None;
            Ok(None)
        }

        FocusPane(scope) => {
            state.focus.set_scope(scope);
            Ok(None)
        }

        ResetLayout => {
            state.dock_state = AppState::default().dock_state;
            Ok(None)
        }

        InstallCli => {
            state.cli_status = Some(match cli_install::install_cli_to_path() {
                Ok(r) => format!(
                    "✓ seqforge installed to {}{}",
                    r.target.display(),
                    if r.was_updated { " (updated)" } else { "" }
                ),
                Err(e) => format!("✗ Install failed: {e}"),
            });
            Ok(None)
        }

        Viewer(req) => {
            let resp = dispatch(&mut state.viewer, bio, req)?;
            Ok(Some(resp))
        }
    }
}
