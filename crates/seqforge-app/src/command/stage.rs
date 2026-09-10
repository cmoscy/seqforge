//! Canvas edit staging — the one write-path handler that is genuinely GUI-only.
//!
//! Staging arms a `PendingEdit` on the view's render cache instead of mutating
//! the buffer, so a menu Cut/Delete/Paste previews before it commits. That makes
//! it a presentation concern end to end: it touches `SeqViewCache`, the focus
//! stack, and the OS clipboard, none of which exist headlessly. It therefore
//! stays in `seqforge-app` while the rest of the write path lives in
//! `seqforge-session` (ROADMAP decision 27).
//!
//! There is no parity gap here: `StagedEdit` never crosses the socket/CLI wire —
//! commit rides the identical `ViewerRequest` path an in-canvas keystroke does.

use seqforge_core::{DispatchError, ViewerResponse};

use crate::app::AppState;
use crate::command::StagedEdit;
use crate::focus::FocusScope;

/// Arm a staged, destructive edit on the active view's canvas (the menu path
/// for Cut/Delete/Paste). This does **not** mutate the buffer — it sets the
/// same `PendingEdit` an in-canvas keystroke would, so the menu previews before
/// commit. Commit (`Enter`) then rides the identical keyboard path
/// (`PendingEdit::to_request` → one `ViewerRequest` → `apply_splice`).
///
/// Focusing the view is essential: staging is gated on pane focus and losing
/// focus *clears* `pending`, so without this a menu-armed stage would vanish
/// the next frame (the menu may have been opened from another pane).
pub(super) fn apply_stage_edit(
    state: &mut AppState,
    edit: StagedEdit,
) -> Result<Option<ViewerResponse>, DispatchError> {
    let vid = state
        .workspace
        .active_view()
        .map(|v| v.id)
        .ok_or(DispatchError::NoActiveView)?;
    // Focus the target pane so the stage survives + Enter reaches it.
    state.workspace.focus_view(vid);
    state.focus.set_scope(FocusScope::View(vid));
    // Reconcile paste payload before borrowing the sequence view.
    if matches!(edit, StagedEdit::Paste { .. }) {
        crate::clipboard::sync_from_os(state);
        if state.clipboard.is_empty() {
            return Ok(None);
        }
    }
    let sv = state.seq_views.get_or_default(vid);
    match edit {
        StagedEdit::Cut { start, end } => sv.stage_cut(start, end),
        StagedEdit::Delete { start, end } => sv.stage_delete(start, end),
        StagedEdit::Paste { pos } => sv.stage_paste(pos),
    }
    Ok(None)
}
