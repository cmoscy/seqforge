//! Typed application commands and the single mutation site (`apply`).
//!
//! See [`docs/focus-refactor.md`](../../../docs/focus-refactor.md) §2.2.
//!
//! Every user-, menu-, hotkey-, bar-, and socket-initiated action is an
//! [`AppCommand`]. The frame loop in [`crate::app`] drains
//! `pending_commands` and routes each through [`apply`] — the *only*
//! function that mutates [`AppState`] in response to a command. Nothing
//! else in the crate may construct a `ViewerRequest` or directly touch
//! the fields that `apply` writes.
//!
//! Why a closed enum: plugin extensibility (an `AppCommand::Custom`
//! variant + handler registry) is deferred to future plugin work (§7
//! of the refactor doc). Keeping the enum closed buys exhaustive-match
//! safety in `apply()` and `is_enabled()`.

use std::path::PathBuf;
use std::sync::mpsc;

use egui_file_dialog::FileDialog;
use seqforge_core::{
    dispatch, BioOps, DispatchError, Selection, ViewerRequest, ViewerResponse,
};

use crate::app::AppState;
use crate::cli_install;
use crate::event::AppEvent;
use crate::focus::FocusScope;
use crate::overlay::{FindBar, GoToBar, Overlay};

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
    /// Pop the topmost overlay from [`AppState::overlays`].
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

    // ── Selection (user-driven, click/drag) ──────────────────────────
    /// Set the cursor / range selection. `None` clears it. Issued by the
    /// viewer widget for every click / drag / shift-extend so the
    /// resulting mutation goes through the single `apply` site and
    /// `AppEvent::SelectionChanged` fires from one place.
    SetSelection(Option<Selection>),
    /// Set (or clear with `None`) the feature-bar highlight. Independent
    /// of `SetSelection`; clicks that select an annotation push both.
    SelectFeature(Option<usize>),

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
            state.workspace.active_view().is_some()
        }
        // Pass-through: the underlying dispatcher enforces preconditions.
        Viewer(_) => true,
        // Selection commands only meaningful with a doc open, but harmless
        // as no-ops otherwise — keep them enabled so the viewer doesn't
        // need to ask before enqueuing.
        SetSelection(_) | SelectFeature(_) => true,
        // Universally available.
        PromptOpenFile | OpenFile(_) | ClearRecent | DismissOverlay | DismissCliStatus
        | FocusPane(_) | ResetLayout | InstallCli => true,
    }
}

/// Read the active view's selection. Used by `emit_selection_diff` and
/// by command arms that need a before-snapshot.
fn active_selection(state: &AppState) -> Option<Selection> {
    state.workspace.active_view().and_then(|v| v.selection)
}

/// Snapshot helper: emits `SelectionChanged` iff the active view's
/// selection differs from `before`. Pulled out so every dispatching
/// variant has the same diffing contract.
fn emit_selection_diff(state: &AppState, before: Option<Selection>) {
    let after = active_selection(state);
    if after != before {
        state.events.emit(AppEvent::SelectionChanged { selection: after });
    }
}

/// Shared apply path for both menu-driven `OpenFile` and socket-driven
/// `Viewer(ViewerRequest::Open { path })` — they should be observably
/// indistinguishable from event subscribers' perspective.
fn apply_open_file<B: BioOps>(
    state: &mut AppState,
    bio: &B,
    path: PathBuf,
) -> Result<Option<ViewerResponse>, DispatchError> {
    state.seq_view.reset();
    state.recent_files.retain(|p| p != &path);
    state.recent_files.insert(0, path.clone());
    state.recent_files.truncate(crate::app::MAX_RECENT);
    let sel_before = active_selection(state);

    // Workspace::open_path loads the buffer (via BioOps) and attaches a
    // new View in the active pane. Errors bubble up as BioError.
    let view_id = state
        .workspace
        .open_path(&path, bio)
        .map_err(DispatchError::BioError)?;

    // Emit DocOpened with the new view's buffer summary.
    if let Some((name, len)) = state.workspace.view(view_id).and_then(|v| {
        state
            .workspace
            .buffers
            .get(v.buffer_id)
            .and_then(|arc| arc.read().ok().map(|b| (b.name.clone(), b.len())))
    }) {
        state.events.emit(AppEvent::DocOpened { name, len });
    }

    emit_selection_diff(state, sel_before);
    Ok(Some(ViewerResponse::Ok))
}

/// Shared apply path for `CloseDoc` (menu / hotkey) and
/// `Viewer(ViewerRequest::Close)` (socket).
fn apply_close_doc(
    state: &mut AppState,
) -> Result<Option<ViewerResponse>, DispatchError> {
    let sel_before = active_selection(state);
    state.workspace.close_active_view()?;
    state.events.emit(AppEvent::DocClosed);
    emit_selection_diff(state, sel_before);
    state.seq_view.reset();
    Ok(Some(ViewerResponse::Ok))
}

/// Dispatch a view-scoped `ViewerRequest` against the active view +
/// its buffer. Flattens the `with_active_buffer` -> dispatch nesting so
/// callers get a single `Result<ViewerResponse, DispatchError>`.
fn dispatch_active<B: BioOps>(
    state: &mut AppState,
    bio: &B,
    req: ViewerRequest,
) -> Result<ViewerResponse, DispatchError> {
    state
        .workspace
        .with_active_buffer(|view, buf, ann| dispatch(view, buf, ann, bio, req))
        .and_then(|inner| inner)
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
            if let Some(tag) = state
                .overlays
                .push_unique(Overlay::FileDialog(Box::new(dialog)))
            {
                state.events.emit(AppEvent::OverlayPushed(tag));
            }
            Ok(None)
        }

        OpenFile(path) => apply_open_file(state, bio, path),

        ClearRecent => {
            state.recent_files.clear();
            Ok(None)
        }

        CloseDoc => apply_close_doc(state),

        OpenFind => {
            if let Some(tag) = state
                .overlays
                .push_unique(Overlay::FindBar(FindBar::default()))
            {
                state.events.emit(AppEvent::OverlayPushed(tag));
            }
            Ok(None)
        }

        OpenGoTo => {
            if let Some(tag) = state
                .overlays
                .push_unique(Overlay::GoToBar(GoToBar::default()))
            {
                state.events.emit(AppEvent::OverlayPushed(tag));
            }
            Ok(None)
        }

        DismissOverlay => {
            if let Some(tag) = state.overlays.pop() {
                state.events.emit(AppEvent::OverlayPopped(tag));
            }
            Ok(None)
        }

        SubmitFind { pattern, mismatches } => {
            if let Some(tag) = state.overlays.pop_kind(Overlay::TAG_FIND_BAR) {
                state.events.emit(AppEvent::OverlayPopped(tag));
            }
            let sel_before = active_selection(state);
            let resp = dispatch_active(state, bio, ViewerRequest::Find { pattern, mismatches })?;
            if let ViewerResponse::SearchResults { count, .. } = &resp {
                state.events.emit(AppEvent::SearchCompleted { hits: *count });
            }
            emit_selection_diff(state, sel_before);
            Ok(Some(resp))
        }

        SubmitGoTo { position } => {
            if let Some(tag) = state.overlays.pop_kind(Overlay::TAG_GOTO_BAR) {
                state.events.emit(AppEvent::OverlayPopped(tag));
            }
            let sel_before = active_selection(state);
            let resp = dispatch_active(state, bio, ViewerRequest::GoTo { position })?;
            emit_selection_diff(state, sel_before);
            Ok(Some(resp))
        }

        DismissCliStatus => {
            if let Some(tag) = state.overlays.pop_kind(Overlay::TAG_CLI_STATUS) {
                state.events.emit(AppEvent::OverlayPopped(tag));
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

        SetSelection(new_sel) => {
            let before = active_selection(state);
            if let Some(view) = state.workspace.active_view_mut() {
                view.selection = new_sel;
            }
            emit_selection_diff(state, before);
            Ok(None)
        }

        SelectFeature(new_feat) => {
            if let Some(view) = state.workspace.active_view_mut() {
                view.selected_feature = new_feat;
            }
            Ok(None)
        }

        InstallCli => {
            let msg = match cli_install::install_cli_to_path() {
                Ok(r) => format!(
                    "✓ seqforge installed to {}{}",
                    r.target.display(),
                    if r.was_updated { " (updated)" } else { "" }
                ),
                Err(e) => format!("✗ Install failed: {e}"),
            };
            // Replace any prior CliStatus (a previous install attempt
            // may still be showing) so the user sees the latest result.
            state.overlays.pop_kind(Overlay::TAG_CLI_STATUS);
            if let Some(tag) = state.overlays.push_unique(Overlay::CliStatus(msg)) {
                state.events.emit(AppEvent::OverlayPushed(tag));
            }
            Ok(None)
        }

        Viewer(req) => {
            // Pass-through path: socket-originated commands. Open/Close
            // route through the same shared helpers as menu/hotkey so
            // event emission is identical regardless of origin.
            match req {
                ViewerRequest::Open { path } => apply_open_file(state, bio, path),
                ViewerRequest::Close => apply_close_doc(state),
                other => {
                    let sel_before = active_selection(state);
                    let resp = dispatch_active(state, bio, other)?;
                    if let ViewerResponse::SearchResults { count, .. } = &resp {
                        state.events.emit(AppEvent::SearchCompleted { hits: *count });
                    }
                    emit_selection_diff(state, sel_before);
                    Ok(Some(resp))
                }
            }
        }
    }
}

