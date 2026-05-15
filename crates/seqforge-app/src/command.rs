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
use crate::event::AppEvent;
use crate::focus::FocusScope;

// Overlay tag constants used by `AppEvent::Overlay{Pushed,Popped}`.
// Stage 5 formalises these as `OverlayStack` named identifiers; for
// now they live alongside the variants that emit them.
const TAG_FIND_BAR: &str = "FindBar";
const TAG_GOTO_BAR: &str = "GoToBar";
const TAG_CLI_STATUS: &str = "CliStatus";

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
            let sel_before = state.viewer.selection;
            let resp = dispatch(&mut state.viewer, bio, ViewerRequest::Open { path })?;
            if let Some(doc) = &state.viewer.open_doc {
                state.events.emit(AppEvent::DocOpened {
                    name: doc.name.clone(),
                    len: doc.sequence.len(),
                });
            }
            emit_selection_diff(state, sel_before);
            Ok(Some(resp))
        }

        ClearRecent => {
            state.recent_files.clear();
            Ok(None)
        }

        CloseDoc => {
            let sel_before = state.viewer.selection;
            let resp = dispatch(&mut state.viewer, bio, ViewerRequest::Close)?;
            state.events.emit(AppEvent::DocClosed);
            emit_selection_diff(state, sel_before);
            Ok(Some(resp))
        }

        OpenFind => {
            if state.active_bar.is_none() {
                state.active_bar = Some(ActiveBar::Find(Default::default()));
                state.events.emit(AppEvent::OverlayPushed(TAG_FIND_BAR));
            }
            Ok(None)
        }

        OpenGoTo => {
            if state.active_bar.is_none() {
                state.active_bar = Some(ActiveBar::GoTo(Default::default()));
                state.events.emit(AppEvent::OverlayPushed(TAG_GOTO_BAR));
            }
            Ok(None)
        }

        DismissOverlay => {
            // Stage 5 will pop a real OverlayStack; for now the only
            // bar-type overlay is `active_bar`.
            if let Some(tag) = active_bar_tag(&state.active_bar) {
                state.active_bar = None;
                state.events.emit(AppEvent::OverlayPopped(tag));
            }
            Ok(None)
        }

        SubmitFind { pattern, mismatches } => {
            if let Some(tag) = active_bar_tag(&state.active_bar) {
                state.active_bar = None;
                state.events.emit(AppEvent::OverlayPopped(tag));
            }
            let sel_before = state.viewer.selection;
            let resp = dispatch(
                &mut state.viewer,
                bio,
                ViewerRequest::Find { pattern, mismatches },
            )?;
            if let ViewerResponse::SearchResults { count, .. } = &resp {
                state.events.emit(AppEvent::SearchCompleted { hits: *count });
            }
            emit_selection_diff(state, sel_before);
            Ok(Some(resp))
        }

        SubmitGoTo { position } => {
            if let Some(tag) = active_bar_tag(&state.active_bar) {
                state.active_bar = None;
                state.events.emit(AppEvent::OverlayPopped(tag));
            }
            let sel_before = state.viewer.selection;
            let resp = dispatch(
                &mut state.viewer,
                bio,
                ViewerRequest::GoTo { position },
            )?;
            emit_selection_diff(state, sel_before);
            Ok(Some(resp))
        }

        DismissCliStatus => {
            if state.cli_status.is_some() {
                state.cli_status = None;
                state.events.emit(AppEvent::OverlayPopped(TAG_CLI_STATUS));
            }
            Ok(None)
        }

        FocusPane(scope) => {
            if state.focus.scope != scope {
                state.focus.set_scope(scope);
                state.events.emit(AppEvent::FocusChanged(scope));
            }
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
            state.events.emit(AppEvent::OverlayPushed(TAG_CLI_STATUS));
            Ok(None)
        }

        Viewer(req) => {
            // Pass-through path: classify response shape into the right
            // events so socket-originated commands generate the same
            // signals as their GUI-originated equivalents.
            let was_open = state.viewer.open_doc.is_some();
            let sel_before = state.viewer.selection;
            let resp = dispatch(&mut state.viewer, bio, req)?;
            match &resp {
                ViewerResponse::SearchResults { count, .. } => {
                    state.events.emit(AppEvent::SearchCompleted { hits: *count });
                }
                ViewerResponse::Ok => {
                    // Close manifests as Ok with the doc cleared.
                    if was_open && state.viewer.open_doc.is_none() {
                        state.events.emit(AppEvent::DocClosed);
                    }
                }
                _ => {}
            }
            emit_selection_diff(state, sel_before);
            Ok(Some(resp))
        }
    }
}

/// Snapshot helper: emits `SelectionChanged` iff `state.viewer.selection`
/// differs from `before`. Pulled out so every viewer-dispatching variant
/// has the same diffing contract.
fn emit_selection_diff(state: &AppState, before: Option<seqforge_core::Selection>) {
    if state.viewer.selection != before {
        state.events.emit(AppEvent::SelectionChanged {
            selection: state.viewer.selection,
        });
    }
}

/// Returns the overlay tag for whichever bar is currently active, or
/// `None` if no bar is open. Stage 5 replaces this with a real
/// `OverlayStack::top_tag()`.
fn active_bar_tag(bar: &Option<ActiveBar>) -> Option<&'static str> {
    match bar {
        Some(ActiveBar::Find(_)) => Some(TAG_FIND_BAR),
        Some(ActiveBar::GoTo(_)) => Some(TAG_GOTO_BAR),
        None => None,
    }
}
