//! File / document commands: Open, Close, recents, CLI install.

use std::path::{Path, PathBuf};

use egui_file_dialog::FileDialog;
use seqforge_core::{BioOps, DispatchError, Topology, ViewId, ViewerResponse};

use super::{active_selection, edit, emit_selection_diff, layout, snapshot_focus_for_overlay};
use crate::app::AppState;
use crate::cli_install;
use crate::event::AppEvent;
use crate::focus::FocusScope;
use crate::overlay::Overlay;
use crate::tabs::Tab;

pub(super) fn apply_prompt_open(
    state: &mut AppState,
) -> Result<Option<ViewerResponse>, DispatchError> {
    let mut dialog = FileDialog::new();
    dialog.pick_file();
    snapshot_focus_for_overlay(state);
    if let Some(tag) = state
        .overlays
        .push_unique(Overlay::FileDialog(Box::new(dialog)))
    {
        state.events.emit(AppEvent::OverlayPushed(tag));
    }
    Ok(None)
}

pub(super) fn apply_clear_recent(
    state: &mut AppState,
) -> Result<Option<ViewerResponse>, DispatchError> {
    state.recent_files.clear();
    Ok(None)
}

/// Open `path`, dedup against already-open views, target the right
/// dock leaf, restore per-file state if a `pending_file_state` entry
/// exists, focus the new view.
pub(super) fn apply_open_file<B: BioOps>(
    state: &mut AppState,
    bio: &B,
    path: PathBuf,
) -> Result<Option<ViewerResponse>, DispatchError> {
    state.recent_files.retain(|p| p != &path);
    state.recent_files.insert(0, path.clone());
    state.recent_files.truncate(crate::app::MAX_RECENT);

    // Already open? Switch to its tab.
    if let Some(view_id) = state.workspace.find_view_for_path(&path) {
        state.workspace.focus_view(view_id);
        layout::dock_activate_view(state, view_id);
        state.focus.set_scope(FocusScope::View(view_id));
        state.events.emit(AppEvent::TabSwitched { view: view_id });
        return Ok(Some(ViewerResponse::Ok));
    }

    let sel_before = active_selection(state);
    let view_id = state
        .workspace
        .open_path(&path, bio)
        .map_err(DispatchError::BioError)?;

    // If we have persisted state for this path (from session restore
    // OR from a prior close+reopen), apply it before the view paints.
    if let Some(fs) = state.pending_file_state.remove(&path) {
        if let Some(view) = state.workspace.view_mut(view_id) {
            view.selection = fs.selection.map_or(
                seqforge_core::ViewSelection::Text(seqforge_core::Selection::cursor(0)),
                seqforge_core::ViewSelection::Text,
            );
            view.scroll_pos = fs.scroll_pos;
        }
    }

    layout::place_view_tab(state, view_id);
    layout::ensure_welcome_invariant(state);
    layout::dock_activate_view(state, view_id);
    state.focus.set_scope(FocusScope::View(view_id));

    if let Some((name, len)) = state.workspace.view(view_id).and_then(|v| {
        state.workspace.buffers.get(v.buffer_id).and_then(|arc| {
            arc.read()
                .ok()
                .map(|b| (crate::workspace::display_name(&b), b.len()))
        })
    }) {
        state.events.emit(AppEvent::DocOpened { name, len });
    }

    emit_selection_diff(state, sel_before);
    Ok(Some(ViewerResponse::Ok))
}

/// Create a new empty in-memory buffer (not backed by a file) and open it in a
/// new dock tab — mirrors the tail of [`apply_open_file`]. Enables copy → New →
/// paste, the bare-metal transport loop.
pub(super) fn apply_new(
    state: &mut AppState,
    circular: bool,
    name: Option<String>,
) -> Result<Option<ViewerResponse>, DispatchError> {
    let topology = if circular {
        Topology::Circular
    } else {
        Topology::Linear
    };
    let name = name.unwrap_or_else(|| "untitled".to_string());
    let view_id = state.workspace.new_buffer(name, Vec::new(), topology);

    layout::place_view_tab(state, view_id);
    layout::ensure_welcome_invariant(state);
    layout::dock_activate_view(state, view_id);
    state.focus.set_scope(FocusScope::View(view_id));

    if let Some((name, len)) = state.workspace.view(view_id).and_then(|v| {
        state.workspace.buffers.get(v.buffer_id).and_then(|arc| {
            arc.read()
                .ok()
                .map(|b| (crate::workspace::display_name(&b), b.len()))
        })
    }) {
        state.events.emit(AppEvent::DocOpened { name, len });
    }
    Ok(Some(ViewerResponse::Ok))
}

/// Amplify between two attached primers → a new **linear** product buffer that
/// inherits the template's annotations (Primers Phase 3.1a). The biology lives
/// in `seqforge_bio::pcr`; here we re-home the amplicon's annotations onto the
/// fresh product via `transport::{extract,place}` and open it as a tab (the
/// `apply_new` buffer/tab flow).
pub(super) fn apply_pcr(
    state: &mut AppState,
    view: Option<ViewId>,
    fwd: seqforge_core::PrimerId,
    rev: seqforge_core::PrimerId,
    name: Option<String>,
) -> Result<Option<ViewerResponse>, DispatchError> {
    use seqforge_core::{Annotations, Orient, PartialPolicy, transport};

    let vid = edit::resolve_target(state, view)?;

    struct Built {
        bytes: Vec<u8>,
        ann: Annotations,
        name: String,
        warnings: Vec<String>,
    }

    // ── Read-only over the template: build product bytes + inherit annotations ──
    let built = state.workspace.with_buffer(vid, |_, buf, ann| {
        let fwd_p = ann
            .primer(fwd)
            .ok_or_else(|| DispatchError::InvalidInput(format!("no primer with id {fwd}")))?;
        let rev_p = ann
            .primer(rev)
            .ok_or_else(|| DispatchError::InvalidInput(format!("no primer with id {rev}")))?;

        let prod = seqforge_bio::pcr(&buf.text, fwd_p, rev_p, buf.is_circular())
            .map_err(|e| DispatchError::InvalidInput(e.to_string()))?;

        // Inherit template annotations across the amplicon. Straddling features
        // are clamped + fuzzy-marked (TruncatePartials); straddling primers are
        // detached by `extract` (binding = None) — we drop those below.
        let mut slice = transport::extract(
            &buf.text,
            ann,
            prod.amplicon,
            PartialPolicy::TruncatePartials,
            &buf.name,
        );
        slice.primers.retain(|p| p.binding.is_some());

        // Place at the forward tail offset (the tail prepends bases ahead of the
        // first template column). Fresh product → nothing to reunite (merge=false).
        let mut prod_ann = Annotations::default();
        transport::place(
            &mut prod_ann,
            &slice,
            prod.tail_f_len,
            Orient::Identity,
            false,
            prod.bytes.len(),
        );

        // No whole-product marker feature: the inherited amplicon features
        // already carry their own extract-stamped lineage, and product-level
        // provenance is the recipe's job (the composed Lineage map), not a
        // hand-rolled whole-span feature. See docs/architecture.md "Lineage".

        let name = name
            .clone()
            .unwrap_or_else(|| format!("{} amplicon", buf.name));
        Ok::<Built, DispatchError>(Built {
            bytes: prod.bytes,
            ann: prod_ann,
            name,
            warnings: prod.warnings,
        })
    })??;

    // ── Materialize the product buffer + open it (mirrors `apply_new`) ──
    let len = built.bytes.len();
    let view_id =
        state
            .workspace
            .new_buffer_annotated(built.name, built.bytes, Topology::Linear, built.ann);

    layout::place_view_tab(state, view_id);
    layout::ensure_welcome_invariant(state);
    layout::dock_activate_view(state, view_id);
    state.focus.set_scope(FocusScope::View(view_id));

    if let Some((name, len)) = state.workspace.view(view_id).and_then(|v| {
        state.workspace.buffers.get(v.buffer_id).and_then(|arc| {
            arc.read()
                .ok()
                .map(|b| (crate::workspace::display_name(&b), b.len()))
        })
    }) {
        state.events.emit(AppEvent::DocOpened { name, len });
    }

    for w in &built.warnings {
        state.toasts.warning(format!("PCR: {w}"));
    }

    Ok(Some(ViewerResponse::Edited { len, changed: true }))
}

/// Digest the source view's buffer and open a read-only Fragments view over it.
/// Fragments are **virtual** — no buffer is materialized here (decision 25); the
/// view stores the enzyme query and recomputes the list on demand.
pub(super) fn apply_digest(
    state: &mut AppState,
    view: Option<ViewId>,
    query: String,
) -> Result<Option<ViewerResponse>, DispatchError> {
    use seqforge_core::ViewKind;

    let vid = edit::resolve_target(state, view)?;

    // Read-only over the source: resolve enzymes + compute the projection.
    let (source_buffer, canonical, infos, warnings) =
        state.workspace.with_buffer(vid, |v, buf, ann| {
            let methyl = v.methylation;
            let (infos, warnings, canonical) = crate::fragments::compute(buf, ann, &query, &methyl);
            (v.buffer_id, canonical, infos, warnings)
        })?;

    // Open a Fragments view onto the SOURCE buffer (not a new buffer).
    let view_id = state.workspace.add_view(source_buffer, ViewKind::Fragments);
    if let Some(v) = state.workspace.view_mut(view_id) {
        v.fragments_query = Some(canonical);
    }
    layout::place_view_tab(state, view_id);
    layout::ensure_welcome_invariant(state);
    layout::dock_activate_view(state, view_id);
    state.focus.set_scope(FocusScope::View(view_id));

    for w in &warnings {
        state.toasts.warning(format!("Digest: {w}"));
    }

    Ok(Some(ViewerResponse::Fragments {
        fragments: infos,
        warnings,
    }))
}

/// Materialize one digest fragment as its own buffer — the opt-in single-fragment
/// export (decision 25; the only per-fragment `new_buffer_annotated`). Recomputes
/// the digest on the source, picks `index`, and promotes its slice.
pub(super) fn apply_export_fragment(
    state: &mut AppState,
    source_view: ViewId,
    index: usize,
) -> Result<Option<ViewerResponse>, DispatchError> {
    use seqforge_core::Annotations;

    struct Built {
        name: String,
        bytes: Vec<u8>,
        ann: Annotations,
        topology: Topology,
    }

    let built = state.workspace.with_buffer(source_view, |v, buf, ann| {
        let query = v.fragments_query.clone().unwrap_or_default();
        let methyl = v.methylation;
        let circular = buf.is_circular();
        let parsed = seqforge_bio::parse_enzyme_query(&query);
        let names = seqforge_bio::resolve_query_names(&parsed, &buf.text, circular);
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let (frags, _) =
            seqforge_bio::digest_fragments(&buf.text, ann, &refs, circular, &buf.name, &methyl);
        let frag = frags
            .into_iter()
            .nth(index)
            .ok_or_else(|| DispatchError::InvalidInput(format!("no fragment #{index}")))?;
        Ok::<Built, DispatchError>(Built {
            name: format!("{} fragment {}", buf.name, index + 1),
            topology: frag.topology,
            bytes: frag.slice.bytes,
            ann: Annotations::from_parts(frag.slice.features, frag.slice.primers),
        })
    })??;

    let view_id =
        state
            .workspace
            .new_buffer_annotated(built.name, built.bytes, built.topology, built.ann);
    layout::place_view_tab(state, view_id);
    layout::ensure_welcome_invariant(state);
    layout::dock_activate_view(state, view_id);
    state.focus.set_scope(FocusScope::View(view_id));

    Ok(Some(ViewerResponse::Ok))
}

pub(super) fn apply_close_doc(
    state: &mut AppState,
) -> Result<Option<ViewerResponse>, DispatchError> {
    let view_id = state
        .workspace
        .active_view()
        .ok_or(DispatchError::NoActiveView)?
        .id;
    apply_close_view(state, view_id)
}

/// Close one view: stash its UI state by path (so a subsequent
/// reopen picks up selection/scroll), remove from dock, drop from
/// workspace, drop buffer if last reference, fire events.
pub(super) fn apply_close_view(
    state: &mut AppState,
    view_id: ViewId,
) -> Result<Option<ViewerResponse>, DispatchError> {
    // Data-loss guard: closing the *last* view onto a dirty buffer drops the
    // buffer (and its edits). Prompt first. The modal's Discard path clears
    // `dirty` before re-issuing CloseTab, so this doesn't loop.
    if view_dirty_and_last_ref(state, view_id) {
        push_dirty_close_confirm(state, view_id, false);
        return Ok(None);
    }

    let sel_before = active_selection(state);

    // Capture per-file state under the buffer's path so a later
    // reopen restores selection/scroll. This makes close+reopen
    // feel editor-grade without explicit user action.
    let buffer_id = state
        .workspace
        .view(view_id)
        .ok_or(DispatchError::ViewNotFound(view_id))?
        .buffer_id;
    if let Some(view) = state.workspace.view(view_id) {
        if let Some(arc) = state.workspace.buffers.get(view.buffer_id) {
            if let Ok(buf) = arc.read() {
                if let Some(path) = buf.source_path.clone() {
                    state.pending_file_state.insert(
                        path,
                        crate::persistence::FileState {
                            selection: view.selection.text_range(),
                            scroll_pos: view.scroll_pos,
                        },
                    );
                }
            }
        }
    }

    let last_ref = state
        .workspace
        .views
        .values()
        .filter(|v| v.buffer_id == buffer_id)
        .count()
        == 1;

    if let Some((si, ni, ti)) = state.dock_state.find_tab(&Tab::View(view_id)) {
        let _ = state.dock_state.remove_tab((si, ni, ti));
    }
    state.workspace.close_view(view_id)?;
    layout::ensure_welcome_invariant(state);

    state.events.emit(AppEvent::TabClosed { view: view_id });
    if last_ref {
        state.events.emit(AppEvent::DocClosed);
    }
    emit_selection_diff(state, sel_before);
    Ok(Some(ViewerResponse::Ok))
}

// ── Editor save (Phase 12e) ────────────────────────────────────────────────--

/// Write `vid`'s buffer + annotations to `path` via `seqforge-bio::save`,
/// clear `dirty` on success, and toast either way. The synchronous core shared
/// by `edit::apply_save` (path known) and the Save-As dialog follow-up.
pub(super) fn save_buffer(
    state: &mut AppState,
    vid: ViewId,
    path: &Path,
    force: bool,
) -> Result<(), DispatchError> {
    // External-change guard: if the file changed on disk since we loaded (or
    // last saved) it, block the write unless `force`. The GUI gets a modal to
    // resolve it; a returned SaveConflict feeds CLI/agent callers (its toast is
    // suppressed in app.rs since the modal already explains it).
    if !force {
        let loaded = state
            .workspace
            .with_buffer(vid, |_, buf, _| buf.loaded_hash)?;
        if let Some(loaded) = loaded {
            if let Some(disk) = crate::workspace::hash_file_bytes(path) {
                if disk != loaded {
                    push_save_conflict(state, vid, path);
                    return Err(DispatchError::SaveConflict(path.display().to_string()));
                }
            }
        }
    }

    let result = state.workspace.with_buffer_mut(vid, |_, buf, ann| {
        let r = seqforge_bio::save(buf, ann, path);
        if r.is_ok() {
            buf.dirty = false;
            // Re-baseline the on-disk hash to what we just wrote, so a later
            // save doesn't spuriously flag our own write as an external change.
            buf.loaded_hash = crate::workspace::hash_file_bytes(path);
        }
        r
    })?;
    match result {
        Ok(()) => {
            state.toasts.success(format!("Saved {}", path.display()));
            Ok(())
        }
        Err(e) => {
            state.toasts.error(format!("Save failed: {e}"));
            Err(DispatchError::BioError(e.to_string()))
        }
    }
}

/// Save a buffer by id (used when recipe export must flush dirty sources that
/// may not be the active view). Finds any view onto the buffer for conflict UI;
/// if none exist, writes without the conflict modal path.
pub(super) fn save_buffer_id(
    state: &mut AppState,
    bid: seqforge_core::BufferId,
    force: bool,
) -> Result<(), DispatchError> {
    let path = state
        .workspace
        .buffers
        .get(bid)
        .and_then(|arc| arc.read().ok().and_then(|b| b.source_path.clone()))
        .ok_or_else(|| DispatchError::InvalidInput(format!("buffer {bid} has no path to save")))?;

    if let Some(vid) = state
        .workspace
        .views
        .values()
        .find(|v| v.buffer_id == bid)
        .map(|v| v.id)
    {
        return save_buffer(state, vid, &path, force);
    }

    // No open view — still persist bytes (recipe source can outlive a closed tab
    // only if the buffer remains; normally there is a view).
    if !force {
        let loaded = state
            .workspace
            .buffers
            .get(bid)
            .and_then(|arc| arc.read().ok().map(|b| b.loaded_hash))
            .flatten();
        if let Some(loaded) = loaded {
            if let Some(disk) = crate::workspace::hash_file_bytes(&path) {
                if disk != loaded {
                    state.toasts.warning(format!(
                        "\"{}\" changed on disk — open the sequence and Save to resolve",
                        path.display()
                    ));
                    return Err(DispatchError::SaveConflict(path.display().to_string()));
                }
            }
        }
    }

    let arc = state
        .workspace
        .buffers
        .get(bid)
        .ok_or_else(|| DispatchError::InvalidInput(format!("buffer {bid} gone")))?;
    let mut buf = arc.write().map_err(|_| DispatchError::PoisonedLock)?;
    let ann = state
        .workspace
        .buffers
        .annotations(bid)
        .cloned()
        .unwrap_or_default();
    match seqforge_bio::save(&buf, &ann, &path) {
        Ok(()) => {
            buf.dirty = false;
            buf.loaded_hash = crate::workspace::hash_file_bytes(&path);
            state.toasts.success(format!("Saved {}", path.display()));
            Ok(())
        }
        Err(e) => {
            state.toasts.error(format!("Save failed: {e}"));
            Err(DispatchError::BioError(e.to_string()))
        }
    }
}

/// Push the Overwrite/Reload/Cancel conflict modal for a save blocked by the
/// external-change guard.
fn push_save_conflict(state: &mut AppState, view_id: ViewId, path: &Path) {
    if let Some(tag) = state.overlays.push_unique(Overlay::SaveConflict {
        view_id,
        path: path.to_path_buf(),
    }) {
        state.events.emit(AppEvent::OverlayPushed(tag));
    }
}

/// Is `view_id`'s buffer dirty *and* is this the only view referencing it?
/// (Closing a non-last view loses nothing — the buffer stays alive.)
pub(super) fn view_dirty_and_last_ref(state: &AppState, view_id: ViewId) -> bool {
    let Some(view) = state.workspace.view(view_id) else {
        return false;
    };
    let bid = view.buffer_id;
    let dirty = state
        .workspace
        .buffers
        .get(bid)
        .and_then(|arc| arc.read().ok().map(|b| b.dirty))
        .unwrap_or(false);
    if !dirty {
        return false;
    }
    state
        .workspace
        .views
        .values()
        .filter(|v| v.buffer_id == bid)
        .count()
        == 1
}

/// Push the Save/Discard/Cancel modal for closing/quitting with unsaved work.
pub(crate) fn push_dirty_close_confirm(state: &mut AppState, view_id: ViewId, quitting: bool) {
    snapshot_focus_for_overlay(state);
    if let Some(tag) = state
        .overlays
        .push_unique(Overlay::DirtyCloseConfirm { view_id, quitting })
    {
        state.events.emit(AppEvent::OverlayPushed(tag));
    }
}

/// Handle `AppCommand::Quit`: flag the request; the update loop routes it
/// through the same dirty-buffer intercept as an OS window close.
pub(super) fn apply_quit(state: &mut AppState) -> Result<Option<ViewerResponse>, DispatchError> {
    state.quit_requested = true;
    Ok(None)
}

/// Handle `AppCommand::OpenRevertConfirm`: raise the revert confirm modal.
pub(super) fn apply_open_revert_confirm(
    state: &mut AppState,
    view: Option<ViewId>,
) -> Result<Option<ViewerResponse>, DispatchError> {
    let vid = edit::resolve_target(state, view)?;
    snapshot_focus_for_overlay(state);
    if let Some(tag) = state
        .overlays
        .push_unique(Overlay::ConfirmRevert { view_id: vid })
    {
        state.events.emit(AppEvent::OverlayPushed(tag));
    }
    Ok(None)
}

/// Handle `AppCommand::RevertBuffer`: reload the target buffer from disk,
/// discarding in-memory text, annotations, and undo history.
pub(super) fn apply_revert(
    state: &mut AppState,
    bio: &dyn BioOps,
    view: Option<ViewId>,
) -> Result<Option<ViewerResponse>, DispatchError> {
    let vid = edit::resolve_target(state, view)?;
    let path = state
        .workspace
        .with_buffer(vid, |_, buf, _| buf.source_path.clone())?
        .ok_or_else(|| {
            DispatchError::InvalidInput("buffer has no source file to revert to".into())
        })?;
    state.workspace.revert_from_disk(vid, &path, bio)?;
    state
        .toasts
        .success(format!("Reverted to {}", path.display()));
    Ok(Some(ViewerResponse::Ok))
}

/// Handle `AppCommand::SaveDocument`: resolve the view and save to `path`.
pub(super) fn apply_save_document(
    state: &mut AppState,
    view: Option<ViewId>,
    path: PathBuf,
) -> Result<Option<ViewerResponse>, DispatchError> {
    let vid = edit::resolve_target(state, view)?;
    // Save-As targets a user-chosen path — the guard is about the *original*
    // source; writing to a new/confirmed path is always intended, so force.
    save_buffer(state, vid, &path, true)?;
    Ok(Some(ViewerResponse::Ok))
}

/// Handle `AppCommand::OpenSaveAs`: open the file dialog in save mode, tagging
/// `pending_save_as` so the pick handler routes to `SaveDocument` (not Open).
pub(super) fn apply_open_save_as(
    state: &mut AppState,
    view: Option<ViewId>,
) -> Result<Option<ViewerResponse>, DispatchError> {
    let vid = edit::resolve_target(state, view)?;
    let mut dialog = FileDialog::new();
    dialog.save_file();
    state.pending_save_as = Some(vid);
    snapshot_focus_for_overlay(state);
    if let Some(tag) = state
        .overlays
        .push_unique(Overlay::FileDialog(Box::new(dialog)))
    {
        state.events.emit(AppEvent::OverlayPushed(tag));
    }
    Ok(None)
}

pub(super) fn apply_dismiss_cli_status(
    state: &mut AppState,
) -> Result<Option<ViewerResponse>, DispatchError> {
    if let Some(tag) = state.overlays.pop_kind(Overlay::TAG_CLI_STATUS) {
        state.events.emit(AppEvent::OverlayPopped(tag));
    }
    super::restore_focus_after_overlay(state);
    Ok(None)
}

pub(super) fn apply_install_cli(
    state: &mut AppState,
) -> Result<Option<ViewerResponse>, DispatchError> {
    let msg = match cli_install::install_cli_to_path() {
        Ok(r) => format!(
            "✓ seqforge installed to {}{}",
            r.target.display(),
            if r.was_updated { " (updated)" } else { "" }
        ),
        Err(e) => format!("✗ Install failed: {e}"),
    };
    state.overlays.pop_kind(Overlay::TAG_CLI_STATUS);
    snapshot_focus_for_overlay(state);
    if let Some(tag) = state.overlays.push_unique(Overlay::CliStatus(msg)) {
        state.events.emit(AppEvent::OverlayPushed(tag));
    }
    Ok(None)
}

#[cfg(test)]
mod phase15_tests {
    use super::*;
    use seqforge_core::{CutSite, Document, SearchHit};

    /// Minimal `BioOps` that loads real files via `seqforge_bio` and no-ops the
    /// scan methods (unused by the save/close/revert paths under test).
    struct TestBio;
    impl BioOps for TestBio {
        fn load(&self, path: &Path) -> Result<Document, String> {
            seqforge_bio::load(path).map_err(|e| e.to_string())
        }
        fn find_matches(&self, _: &[u8], _: &[u8], _: u8, _: bool) -> Vec<SearchHit> {
            vec![]
        }
        fn find_cut_sites(&self, _: &[u8], _: &[&str], _: bool) -> Vec<CutSite> {
            vec![]
        }
        fn resolve_enzyme_names(&self, _: &[u8], _: &str, _: bool) -> Vec<String> {
            vec![]
        }
        fn primer_infos(
            &self,
            _: &[u8],
            _: &[&seqforge_core::Primer],
            _: bool,
        ) -> Vec<seqforge_core::PrimerInfo> {
            vec![]
        }
        fn methyl_states_for_sites(
            &self,
            sites: &[CutSite],
            _: &[u8],
            _: &seqforge_core::MethylContext,
        ) -> Vec<seqforge_core::MethylState> {
            vec![seqforge_core::MethylState::Cuttable; sites.len()]
        }
    }

    fn temp_fasta(seq: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let p = std::env::temp_dir().join(format!("seqforge_ph15_{nanos}.fasta"));
        std::fs::write(&p, format!(">test\n{seq}\n")).unwrap();
        p
    }

    /// Open `path` into a fresh headless state, returning the active view id.
    fn open(path: &Path) -> (AppState, ViewId) {
        let mut state = AppState::default();
        let vid = state.workspace.open_path(path, &TestBio).unwrap();
        state.workspace.focus_view(vid);
        (state, vid)
    }

    #[test]
    fn dirty_close_pushes_confirm_instead_of_closing() {
        let path = temp_fasta("ACGTACGT");
        let (mut state, vid) = open(&path);
        state
            .workspace
            .with_buffer_mut(vid, |_, buf, _| buf.dirty = true)
            .unwrap();

        apply_close_view(&mut state, vid).unwrap();

        assert_eq!(state.overlays.dirty_close_confirm(), Some((vid, false)));
        assert!(
            state.workspace.view(vid).is_some(),
            "the view must not close while the confirm modal is up"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn external_change_guard_blocks_then_forces() {
        let path = temp_fasta("ACGTACGT");
        let (mut state, vid) = open(&path);

        // Someone edits the file on disk behind our back.
        std::fs::write(&path, ">test\nTTTTTTTT\n").unwrap();

        let blocked = save_buffer(&mut state, vid, &path, false);
        assert!(
            matches!(blocked, Err(DispatchError::SaveConflict(_))),
            "an external change must block a non-forced save, got {blocked:?}"
        );
        assert!(state.overlays.save_conflict().is_some());

        let forced = save_buffer(&mut state, vid, &path, true);
        assert!(forced.is_ok(), "--force must overwrite, got {forced:?}");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn revert_resets_text_dirty_and_history() {
        let path = temp_fasta("ACGTACGT");
        let (mut state, vid) = open(&path);
        let bid = state.workspace.view(vid).unwrap().buffer_id;
        let original = state
            .workspace
            .with_buffer(vid, |_, buf, _| buf.text.clone())
            .unwrap();

        // Dirty the buffer + fabricate undo history.
        state
            .workspace
            .with_buffer_mut(vid, |_, buf, _| {
                buf.text = b"XXXX".to_vec();
                buf.dirty = true;
            })
            .unwrap();
        state.workspace.buffers.history_mut(bid).record(
            0,
            Vec::new(),
            b"XXXX".to_vec(),
            &Default::default(),
            seqforge_core::EditKind::Other,
        );

        apply_revert(&mut state, &TestBio, Some(vid)).unwrap();

        let (text, dirty) = state
            .workspace
            .with_buffer(vid, |_, buf, _| (buf.text.clone(), buf.dirty))
            .unwrap();
        assert_eq!(text, original, "revert restores the on-disk sequence");
        assert!(!dirty, "revert clears the dirty flag");
        assert!(
            state.workspace.buffers.history(bid).is_none(),
            "revert clears undo history"
        );
        let _ = std::fs::remove_file(&path);
    }
}
