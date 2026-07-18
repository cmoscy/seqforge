//! Workspace + BufferStore — the GUI-side orchestration of the
//! editor-ready model types (`Buffer`, `Annotations`, `View`) from
//! `seqforge-core::model`.
//!
//! ## Model
//!
//! After the Stage 2.5c follow-up flatten, the workspace tracks views
//! as a flat map keyed by `ViewId`. The dock tree (`DockState<Tab>`)
//! owns layout: which views live in which leaf, the tab order within
//! a leaf, and the active tab per leaf. A "pane" is no longer a
//! first-class workspace concept — it's whatever leaf the dock
//! currently places a view in. egui_dock therefore handles
//! split-view, drag-rearrange, and per-leaf tab cycling natively
//! with one tab strip (no second strip needed).
//!
//! Workspace still owns the things the dock can't: buffer storage,
//! view identity, the active-view focus marker, and the per-view
//! render cache (`SequenceView`). Buffer sharing across multiple
//! views (split-view of the same plasmid) still works via
//! `BufferStore`'s Arc-clone behaviour.

#![allow(dead_code)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use std::ops::Range;

use seqforge_core::{
    Annotations, BioOps, Buffer, BufferId, DispatchError, EditKind, History, Orient, Selection,
    SeqSlice, View, ViewId, ViewKind, ViewSelection, mutations, transport,
};
use serde::{Deserialize, Serialize};

/// Hash the raw bytes of a file on disk, for the external-change guard.
/// Returns `None` if the file can't be read (treated as "no baseline" —
/// the guard then can't fire). `DefaultHasher` is fine: the value only ever
/// lives in memory and is compared within a single session, so cross-version
/// hash stability is irrelevant.
pub(crate) fn hash_file_bytes(path: &Path) -> Option<u64> {
    use std::hash::{Hash, Hasher};
    let bytes = std::fs::read(path).ok()?;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hasher);
    Some(hasher.finish())
}

/// User-facing label for a buffer: the source file's basename when the
/// buffer is backed by a file, otherwise the sequence name from the
/// record (e.g. for socket-injected or in-memory buffers). Single
/// source of truth used by the tab title, minimap header, and
/// DocOpened event so they never drift.
pub fn display_name(buf: &Buffer) -> String {
    buf.source_path
        .as_ref()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .map(str::to_owned)
        .unwrap_or_else(|| buf.name.clone())
}

use crate::viewer::SequenceView;

// ── BufferStore ──────────────────────────────────────────────────────────────

/// Buffer + Annotations storage keyed by `BufferId`.
///
/// Same path opens once and is shared across views via
/// `Arc<RwLock<Buffer>>` — split-view of the same file shares one rope
/// and one undo history. `by_path` deduplicates on `open_path`.
#[derive(Default, Debug)]
pub struct BufferStore {
    buffers: HashMap<BufferId, Arc<RwLock<Buffer>>>,
    annotations: HashMap<BufferId, Annotations>,
    /// Per-buffer undo/redo history, shared across all views into the buffer
    /// and dropped with it. Lazily created on first edit.
    histories: HashMap<BufferId, History>,
    by_path: HashMap<PathBuf, BufferId>,
    next_id: u64,
}

impl BufferStore {
    pub fn new() -> Self {
        Self::default()
    }

    fn alloc_id(&mut self) -> BufferId {
        self.next_id += 1;
        BufferId(self.next_id)
    }

    pub fn get(&self, id: BufferId) -> Option<Arc<RwLock<Buffer>>> {
        self.buffers.get(&id).cloned()
    }

    pub fn annotations(&self, id: BufferId) -> Option<&Annotations> {
        self.annotations.get(&id)
    }

    pub fn annotations_mut(&mut self, id: BufferId) -> Option<&mut Annotations> {
        self.annotations.get_mut(&id)
    }

    /// The buffer's undo/redo history, created on first access.
    pub fn history_mut(&mut self, id: BufferId) -> &mut History {
        self.histories.entry(id).or_default()
    }

    pub fn history(&self, id: BufferId) -> Option<&History> {
        self.histories.get(&id)
    }

    /// Mutable access to a buffer's annotations and history together. Both
    /// live in `BufferStore` as separate maps, so this hands out disjoint
    /// borrows the edit/undo path needs in one call. Returns `None` if the
    /// buffer's annotations are missing (history is created on demand).
    fn ann_and_history_mut(&mut self, id: BufferId) -> Option<(&mut Annotations, &mut History)> {
        let ann = self.annotations.get_mut(&id)?;
        let history = self.histories.entry(id).or_default();
        Some((ann, history))
    }

    /// Drop a buffer + its annotations and forget any path alias.
    /// Returns the strong-count of the Arc just before drop.
    pub fn remove(&mut self, id: BufferId) -> Option<usize> {
        let buf = self.buffers.remove(&id)?;
        self.annotations.remove(&id);
        self.histories.remove(&id);
        self.by_path.retain(|_, v| *v != id);
        let count = Arc::strong_count(&buf);
        drop(buf);
        Some(count)
    }

    /// Find or load a buffer for `path`. If a buffer is already loaded
    /// for this path, returns its id without re-reading the file.
    pub fn open_path(&mut self, path: &Path, bio: &dyn BioOps) -> Result<BufferId, String> {
        if let Some(existing) = self.by_path.get(path).copied() {
            return Ok(existing);
        }
        let doc = bio.load(path)?;
        let mut buffer = Buffer::new(
            doc.name.clone(),
            doc.source_path.clone(),
            doc.sequence,
            doc.topology,
        );
        // Snapshot the on-disk bytes for the external-change guard (§Phase 15).
        buffer.loaded_hash = hash_file_bytes(path);
        let annotations = Annotations::from_parts(doc.features, doc.primers);

        let id = self.alloc_id();
        self.buffers.insert(id, Arc::new(RwLock::new(buffer)));
        self.annotations.insert(id, annotations);
        self.by_path.insert(path.to_path_buf(), id);
        Ok(id)
    }

    /// Reload a buffer's contents from disk in place (File → Revert). Keeps the
    /// same `BufferId` + `Arc` so open views stay valid; replaces text +
    /// annotations, clears undo history, and re-baselines `dirty` + `loaded_hash`.
    pub fn reload(&mut self, id: BufferId, path: &Path, bio: &dyn BioOps) -> Result<(), String> {
        let doc = bio.load(path)?;
        let arc = self.buffers.get(&id).ok_or("buffer not found")?;
        {
            let mut buf = arc
                .write()
                .map_err(|_| "buffer lock poisoned".to_string())?;
            buf.name = doc.name.clone();
            buf.text = doc.sequence;
            buf.topology = doc.topology;
            buf.source_path = doc.source_path.clone();
            buf.dirty = false;
            buf.version += 1;
            buf.loaded_hash = hash_file_bytes(path);
        }
        self.annotations
            .insert(id, Annotations::from_parts(doc.features, doc.primers));
        self.histories.remove(&id);
        Ok(())
    }

    /// Create a new in-memory **scratch** buffer (empty or seeded), not backed by
    /// a file — powers `New` / paste-into-new (and, later, PCR/assembly product
    /// buffers). No `by_path` entry (no source path), so it never dedupes.
    pub fn new_scratch(
        &mut self,
        name: String,
        text: Vec<u8>,
        topology: seqforge_core::Topology,
    ) -> BufferId {
        let buffer = Buffer::new(name, None, text, topology);
        let id = self.alloc_id();
        self.buffers.insert(id, Arc::new(RwLock::new(buffer)));
        self.annotations.insert(id, Annotations::default());
        id
    }

    pub fn len(&self) -> usize {
        self.buffers.len()
    }

    pub fn is_empty(&self) -> bool {
        self.buffers.is_empty()
    }
}

// ── Workspace ────────────────────────────────────────────────────────────────

/// Flat workspace state: view identity + buffer storage + render
/// caches. The dock tree owns spatial layout (which view is in which
/// leaf, tab order, per-leaf active tab).
#[derive(Debug, Serialize, Deserialize)]
pub struct Workspace {
    /// All open views, keyed by id. Layout (which leaf, which order)
    /// lives in the dock tree; this map is the source of truth for
    /// view identity + persistent per-view state.
    pub views: HashMap<ViewId, View>,
    /// The view that currently has keyboard focus. Mirrors the dock's
    /// focused tab; updated by `AppCommand::FocusPane(View(vid))` when
    /// the user clicks a leaf or programmatic focus moves.
    pub active_view: Option<ViewId>,

    /// Buffer + annotations storage. Skipped in serde because
    /// `Arc<RwLock>` doesn't round-trip through eframe persistence.
    /// `recent_files` restores the working set across restarts.
    #[serde(skip)]
    pub buffers: BufferStore,

    /// Per-view render caches for the sequence viewer. Transient;
    /// rebuilt on next paint via the `(buffer_id, version)` cache
    /// key. Keyed by `ViewId` so opening the same buffer in two views
    /// gives each one its own independent cache (e.g. different
    /// feature-row stacking when zoom levels diverge later).
    #[serde(skip)]
    pub seq_views: HashMap<ViewId, SequenceView>,

    next_view: u64,
}

impl Default for Workspace {
    fn default() -> Self {
        Self {
            views: HashMap::new(),
            active_view: None,
            buffers: BufferStore::new(),
            seq_views: HashMap::new(),
            next_view: 0,
        }
    }
}

impl Workspace {
    pub fn alloc_view_id(&mut self) -> ViewId {
        self.next_view += 1;
        ViewId(self.next_view)
    }

    pub fn active_view(&self) -> Option<&View> {
        self.active_view.and_then(|id| self.views.get(&id))
    }

    pub fn active_view_mut(&mut self) -> Option<&mut View> {
        let id = self.active_view?;
        self.views.get_mut(&id)
    }

    /// Read-only view-by-id accessor.
    pub fn view(&self, view_id: ViewId) -> Option<&View> {
        self.views.get(&view_id)
    }

    /// Mutable view-by-id accessor.
    pub fn view_mut(&mut self, view_id: ViewId) -> Option<&mut View> {
        self.views.get_mut(&view_id)
    }

    /// Reload `view_id`'s buffer from disk (File → Revert), discarding
    /// in-memory edits, annotations, and undo history.
    pub fn revert_from_disk(
        &mut self,
        view_id: ViewId,
        path: &Path,
        bio: &dyn BioOps,
    ) -> Result<(), DispatchError> {
        let bid = self
            .view(view_id)
            .ok_or(DispatchError::ViewNotFound(view_id))?
            .buffer_id;
        self.buffers
            .reload(bid, path, bio)
            .map_err(DispatchError::BioError)?;
        let len = self
            .buffers
            .get(bid)
            .and_then(|b| b.read().ok().map(|b| b.len()))
            .unwrap_or(0);
        self.rehome_views_for_buffer(bid, len);
        Ok(())
    }

    /// Resolve the active view's buffer handle.
    pub fn active_buffer(&self) -> Option<Arc<RwLock<Buffer>>> {
        let view = self.active_view()?;
        self.buffers.get(view.buffer_id)
    }

    /// Set the active view by id. No-op if `view_id` is unknown.
    pub fn focus_view(&mut self, view_id: ViewId) {
        if self.views.contains_key(&view_id) {
            self.active_view = Some(view_id);
        }
    }

    /// Allocate a new `View` onto `buffer_id`. The caller is responsible
    /// for placing the corresponding `Tab::View(id)` into the dock tree.
    /// Returns the new view's id.
    pub fn add_view(&mut self, buffer_id: BufferId, kind: ViewKind) -> ViewId {
        let id = self.alloc_view_id();
        self.views.insert(id, View::new(id, buffer_id, kind));
        self.seq_views.insert(id, SequenceView::default());
        self.active_view = Some(id);
        id
    }

    /// Open `path` and attach a new View. Buffer storage dedupes by
    /// path. Returns the new view's id; caller adds the dock tab.
    pub fn open_path(&mut self, path: &Path, bio: &dyn BioOps) -> Result<ViewId, String> {
        let buffer_id = self.buffers.open_path(path, bio)?;
        Ok(self.add_view(buffer_id, ViewKind::TextView))
    }

    /// Create a new scratch buffer (empty or seeded) + View, not backed by a
    /// file. Returns the new view's id; the caller adds the dock tab (mirrors
    /// [`Workspace::open_path`]).
    pub fn new_buffer(
        &mut self,
        name: String,
        text: Vec<u8>,
        topology: seqforge_core::Topology,
    ) -> ViewId {
        let buffer_id = self.buffers.new_scratch(name, text, topology);
        self.add_view(buffer_id, ViewKind::TextView)
    }

    /// Close a view by id. If the closed view held the last reference
    /// to its buffer, the buffer is dropped from the store too. The
    /// caller is responsible for removing the corresponding dock tab.
    /// Returns Ok(view_id) on success, ViewNotFound otherwise.
    pub fn close_view(&mut self, view_id: ViewId) -> Result<ViewId, DispatchError> {
        let view = self
            .views
            .remove(&view_id)
            .ok_or(DispatchError::ViewNotFound(view_id))?;
        self.seq_views.remove(&view_id);

        // Drop the buffer if no surviving view references it.
        let still = self.views.values().any(|v| v.buffer_id == view.buffer_id);
        if !still {
            self.buffers.remove(view.buffer_id);
        }

        if self.active_view == Some(view_id) {
            self.active_view = self.views.keys().copied().next();
        }
        Ok(view_id)
    }

    /// Find an existing view whose buffer's source path matches `path`.
    /// Used to dedupe open-of-already-open.
    pub fn find_view_for_path(&self, path: &Path) -> Option<ViewId> {
        for view in self.views.values() {
            let arc = self.buffers.get(view.buffer_id)?;
            if let Ok(buf) = arc.read() {
                if buf.source_path.as_deref() == Some(path) {
                    return Some(view.id);
                }
            }
        }
        None
    }

    // ── Buffer-locking helpers ────────────────────────────────────────────────

    /// Run `f` with `(seq_view, view, buffer, annotations)` for a
    /// specific view. Lock acquisition is bounded by the closure scope.
    pub fn with_view_buffer<R>(
        &mut self,
        view_id: ViewId,
        f: impl FnOnce(&mut SequenceView, &mut View, &Buffer, &mut Annotations) -> R,
    ) -> Result<R, DispatchError> {
        let bid = self
            .views
            .get(&view_id)
            .ok_or(DispatchError::ViewNotFound(view_id))?
            .buffer_id;
        let buf_arc = self
            .buffers
            .get(bid)
            .ok_or(DispatchError::ViewNotFound(view_id))?;
        let buf = buf_arc.read().map_err(|_| DispatchError::PoisonedLock)?;
        let view = self.views.get_mut(&view_id).expect("located above");
        let seq_view = self.seq_views.entry(view_id).or_default();
        let ann = self.buffers.annotations_mut(bid).expect("located above");
        Ok(f(seq_view, view, &buf, ann))
    }

    /// `with_view_buffer` variant that read-locks the buffer and only
    /// provides view + buffer + annotations (no seq_view) — for the
    /// command-dispatch path where SequenceView isn't relevant.
    pub fn with_buffer<R>(
        &mut self,
        view_id: ViewId,
        f: impl FnOnce(&mut View, &Buffer, &mut Annotations) -> R,
    ) -> Result<R, DispatchError> {
        let bid = self
            .views
            .get(&view_id)
            .ok_or(DispatchError::ViewNotFound(view_id))?
            .buffer_id;
        let buf_arc = self
            .buffers
            .get(bid)
            .ok_or(DispatchError::ViewNotFound(view_id))?;
        let buf = buf_arc.read().map_err(|_| DispatchError::PoisonedLock)?;
        let view = self.views.get_mut(&view_id).expect("located above");
        let ann = self.buffers.annotations_mut(bid).expect("located above");
        Ok(f(view, &buf, ann))
    }

    /// Write-lock variant of `with_buffer`. Used by edit operations
    /// (Tier 3d) and other commands that need to mutate the buffer.
    pub fn with_buffer_mut<R>(
        &mut self,
        view_id: ViewId,
        f: impl FnOnce(&mut View, &mut Buffer, &mut Annotations) -> R,
    ) -> Result<R, DispatchError> {
        let bid = self
            .views
            .get(&view_id)
            .ok_or(DispatchError::ViewNotFound(view_id))?
            .buffer_id;
        let buf_arc = self
            .buffers
            .get(bid)
            .ok_or(DispatchError::ViewNotFound(view_id))?;
        let mut buf = buf_arc.write().map_err(|_| DispatchError::PoisonedLock)?;
        let view = self.views.get_mut(&view_id).expect("located above");
        let ann = self.buffers.annotations_mut(bid).expect("located above");
        Ok(f(view, &mut buf, ann))
    }

    pub fn with_active_buffer<R>(
        &mut self,
        f: impl FnOnce(&mut View, &Buffer, &mut Annotations) -> R,
    ) -> Result<R, DispatchError> {
        let id = self.active_view.ok_or(DispatchError::NoActiveView)?;
        self.with_buffer(id, f)
    }

    pub fn with_active_buffer_mut<R>(
        &mut self,
        f: impl FnOnce(&mut View, &mut Buffer, &mut Annotations) -> R,
    ) -> Result<R, DispatchError> {
        let id = self.active_view.ok_or(DispatchError::NoActiveView)?;
        self.with_buffer_mut(id, f)
    }

    /// After a buffer-length change, clamp every text-bearing selection on
    /// views of `bid` into `0..=len` (empty → `cursor(0)`). Feature / CutSite
    /// object selections reduce to clamped `Text`, matching the caret edit
    /// leaves behind. Primer / None are left alone.
    fn rehome_views_for_buffer(&mut self, bid: BufferId, len: usize) {
        for view in self.views.values_mut() {
            if view.buffer_id != bid {
                continue;
            }
            match view.selection {
                ViewSelection::Text(s)
                | ViewSelection::Feature { range: s, .. }
                | ViewSelection::CutSite { range: s, .. } => {
                    view.selection = ViewSelection::Text(s.clamp_to_len(len));
                }
                ViewSelection::Primer(_) | ViewSelection::None => {}
            }
        }
    }

    /// The single edit entry point: record a reverse delta into the buffer's
    /// history, then apply the splice. Every editor mutation (Phase 12's
    /// `command/edit.rs`, from GUI / terminal / agent) routes through here so
    /// undo, dirty, and version stay consistent regardless of source.
    ///
    /// Bounds are validated here (command-layer policy) so `apply_splice`'s
    /// precondition holds; the editing view's cursor is moved past the edit.
    pub fn edit(
        &mut self,
        view_id: ViewId,
        kind: EditKind,
        range: Range<usize>,
        new_bytes: &[u8],
    ) -> Result<(), DispatchError> {
        let bid = self
            .views
            .get(&view_id)
            .ok_or(DispatchError::ViewNotFound(view_id))?
            .buffer_id;
        let buf_arc = self
            .buffers
            .get(bid)
            .ok_or(DispatchError::ViewNotFound(view_id))?;
        let mut buf = buf_arc.write().map_err(|_| DispatchError::PoisonedLock)?;

        if range.start > range.end || range.end > buf.text.len() {
            return Err(DispatchError::OutOfRange {
                position: range.end,
                seq_len: buf.text.len(),
            });
        }

        let (ann, history) = self
            .buffers
            .ann_and_history_mut(bid)
            .ok_or(DispatchError::ViewNotFound(view_id))?;
        let old_bytes = buf.text[range.clone()].to_vec();
        history.record(range.start, old_bytes, new_bytes.to_vec(), ann, kind);
        mutations::apply_splice(&mut buf, ann, range.clone(), new_bytes);
        let cursor = range.start + new_bytes.len();
        let len = buf.text.len();
        drop(buf);

        if let Some(view) = self.views.get_mut(&view_id) {
            view.selection = ViewSelection::Text(Selection::cursor(cursor));
        }
        self.rehome_views_for_buffer(bid, len);
        Ok(())
    }

    /// Paste an annotated [`SeqSlice`] at `pos` in **one undo transaction**: the
    /// slice's bytes are spliced in (shifting existing annotations to make room),
    /// then its carried features/primers are re-homed via [`transport::place`]
    /// (fresh ids, decision 12; provenance-gated `merge`). The history entry
    /// snapshots annotations *before* the splice, so undo restores both the bytes
    /// and the placed annotations for free.
    ///
    /// This is the copy/paste consumer of the transport primitive; PCR and
    /// ligation reuse `transport::place` with different `orient`/`merge`.
    pub fn paste_slice(
        &mut self,
        view_id: ViewId,
        pos: usize,
        slice: &SeqSlice,
        orient: Orient,
        merge: bool,
    ) -> Result<(), DispatchError> {
        let bid = self
            .views
            .get(&view_id)
            .ok_or(DispatchError::ViewNotFound(view_id))?
            .buffer_id;
        let buf_arc = self
            .buffers
            .get(bid)
            .ok_or(DispatchError::ViewNotFound(view_id))?;
        let mut buf = buf_arc.write().map_err(|_| DispatchError::PoisonedLock)?;

        if pos > buf.text.len() {
            return Err(DispatchError::OutOfRange {
                position: pos,
                seq_len: buf.text.len(),
            });
        }

        let (ann, history) = self
            .buffers
            .ann_and_history_mut(bid)
            .ok_or(DispatchError::ViewNotFound(view_id))?;
        // Snapshot annotations before the splice + place; the reverse delta is a
        // plain insertion at `pos`.
        history.record(pos, Vec::new(), slice.bytes.clone(), ann, EditKind::Other);
        mutations::apply_splice(&mut buf, ann, pos..pos, &slice.bytes);
        let len_total = buf.text.len();
        transport::place(ann, slice, pos, orient, merge, len_total);
        let cursor = pos + slice.bytes.len();
        drop(buf);

        if let Some(view) = self.views.get_mut(&view_id) {
            view.selection = ViewSelection::Text(Selection::cursor(cursor));
        }
        self.rehome_views_for_buffer(bid, len_total);
        Ok(())
    }

    // ── Topology / buffer-lifecycle ops ───────────────────────────────────────

    /// Replace a buffer's **whole** text + annotations (and optionally topology)
    /// as ONE undo unit. `f` reads the current buffer + annotations and returns
    /// the new `(text, annotations, topology)`. Records a full-buffer history
    /// entry (start 0, old→new bytes, pre-edit annotation snapshot), stamping the
    /// old topology when it changes so undo/redo restore it. The shared engine
    /// behind Set-Origin / Linearize / Circularize.
    fn replace_whole<F>(&mut self, view_id: ViewId, f: F) -> Result<(), DispatchError>
    where
        F: FnOnce(&Buffer, &Annotations) -> (Vec<u8>, Annotations, seqforge_core::Topology),
    {
        let bid = self
            .views
            .get(&view_id)
            .ok_or(DispatchError::ViewNotFound(view_id))?
            .buffer_id;
        let buf_arc = self
            .buffers
            .get(bid)
            .ok_or(DispatchError::ViewNotFound(view_id))?;
        let mut buf = buf_arc.write().map_err(|_| DispatchError::PoisonedLock)?;
        let (ann, history) = self
            .buffers
            .ann_and_history_mut(bid)
            .ok_or(DispatchError::ViewNotFound(view_id))?;

        let old_text = buf.text.clone();
        let old_topo = buf.topology;
        let (new_text, new_ann, new_topo) = f(&buf, ann);

        // Record with the pre-edit annotations (still live in `ann`) + full-text
        // delta, then swap in the new state.
        history.record(0, old_text, new_text.clone(), ann, EditKind::Other);
        if new_topo != old_topo {
            history.stamp_topology(old_topo);
        }
        buf.text = new_text;
        buf.topology = new_topo;
        buf.version += 1;
        buf.dirty = true;
        *ann = new_ann;
        let len = buf.text.len();
        drop(buf);
        self.rehome_views_for_buffer(bid, len);
        Ok(())
    }

    /// Set Origin: rotate a **circular** buffer so `index` becomes position 0.
    pub fn set_origin(&mut self, view_id: ViewId, index: usize) -> Result<(), DispatchError> {
        self.require_circular(view_id)?;
        self.replace_whole(view_id, |buf, ann| {
            let mut text = buf.text.clone();
            let mut ann = ann.clone();
            seqforge_core::rotate_origin(&mut text, &mut ann, index);
            (text, ann, buf.topology)
        })
    }

    /// Linearize a **circular** buffer, cutting at `at` (`None` = current origin).
    /// Reuses `transport::extract` of the whole circle rooted at `at`, so a
    /// feature straddling the seam is truncated + fuzzy-marked (`TruncatePartials`).
    pub fn linearize(&mut self, view_id: ViewId, at: Option<usize>) -> Result<(), DispatchError> {
        self.require_circular(view_id)?;
        self.replace_whole(view_id, |buf, ann| {
            let len = buf.text.len();
            let start = at.unwrap_or(0) % len.max(1);
            let slice = seqforge_core::extract(
                &buf.text,
                ann,
                seqforge_core::Span::new(start, len),
                seqforge_core::PartialPolicy::TruncatePartials,
                &buf.name,
            );
            let new_ann = Annotations::from_parts(slice.features, slice.primers);
            (slice.bytes, new_ann, seqforge_core::Topology::Linear)
        })
    }

    /// Circularize a **linear** buffer (join the ends); `origin` optionally rotates
    /// the new circle so that base becomes position 0.
    pub fn circularize(
        &mut self,
        view_id: ViewId,
        origin: Option<usize>,
    ) -> Result<(), DispatchError> {
        self.require_linear(view_id)?;
        self.replace_whole(view_id, |buf, ann| {
            let mut text = buf.text.clone();
            let mut ann = ann.clone();
            if let Some(o) = origin {
                // Rotate after conceptually closing the circle.
                seqforge_core::rotate_origin(&mut text, &mut ann, o);
            }
            (text, ann, seqforge_core::Topology::Circular)
        })
    }

    /// Mirror the **whole-buffer** annotation layer to match a reverse-complement
    /// of the bytes (called by the RC applier after it installs the RC'd bytes via
    /// [`Workspace::edit`], so it rides that edit's single undo unit). Bumps
    /// version; records no separate history entry.
    pub fn reverse_complement_annotations_whole(
        &mut self,
        view_id: ViewId,
    ) -> Result<(), DispatchError> {
        let bid = self
            .views
            .get(&view_id)
            .ok_or(DispatchError::ViewNotFound(view_id))?
            .buffer_id;
        let buf_arc = self
            .buffers
            .get(bid)
            .ok_or(DispatchError::ViewNotFound(view_id))?;
        let mut buf = buf_arc.write().map_err(|_| DispatchError::PoisonedLock)?;
        let len = buf.text.len();
        let ann = self
            .buffers
            .annotations_mut(bid)
            .ok_or(DispatchError::ViewNotFound(view_id))?;
        seqforge_core::reverse_complement_circular(ann, len);
        buf.version += 1;
        Ok(())
    }

    fn require_circular(&self, view_id: ViewId) -> Result<(), DispatchError> {
        self.topology_of(view_id)
            .filter(|t| matches!(t, seqforge_core::Topology::Circular))
            .map(|_| ())
            .ok_or_else(|| DispatchError::InvalidInput("buffer is not circular".into()))
    }

    fn require_linear(&self, view_id: ViewId) -> Result<(), DispatchError> {
        self.topology_of(view_id)
            .filter(|t| matches!(t, seqforge_core::Topology::Linear))
            .map(|_| ())
            .ok_or_else(|| DispatchError::InvalidInput("buffer is not linear".into()))
    }

    fn topology_of(&self, view_id: ViewId) -> Option<seqforge_core::Topology> {
        let bid = self.views.get(&view_id)?.buffer_id;
        let buf = self.buffers.get(bid)?;
        let t = buf.read().ok()?.topology;
        Some(t)
    }

    /// The edit entry point for **annotation-only** mutations (feature
    /// add/remove/rename): the sequence bytes are untouched, but the change is
    /// still undoable via the annotation-snapshot half of the history entry.
    ///
    /// The closure mutates the annotations (using the id-only API); it may read
    /// the buffer for validation. On success we record a history entry with an
    /// **empty splice delta** (`start = 0`, no bytes) plus the pre-edit
    /// annotation snapshot — so `undo` restores the features (ids ride the
    /// snapshot for free) and touches no text — then bump `version` + `dirty`.
    /// `EditKind::Other` guarantees the entry never coalesces with typing. If
    /// the closure returns `Err`, nothing is recorded and no flags change.
    pub fn edit_annotations<R>(
        &mut self,
        view_id: ViewId,
        f: impl FnOnce(&mut Annotations, &Buffer) -> Result<R, DispatchError>,
    ) -> Result<R, DispatchError> {
        let bid = self
            .views
            .get(&view_id)
            .ok_or(DispatchError::ViewNotFound(view_id))?
            .buffer_id;
        let buf_arc = self
            .buffers
            .get(bid)
            .ok_or(DispatchError::ViewNotFound(view_id))?;
        let mut buf = buf_arc.write().map_err(|_| DispatchError::PoisonedLock)?;
        let (ann, history) = self
            .buffers
            .ann_and_history_mut(bid)
            .ok_or(DispatchError::ViewNotFound(view_id))?;
        let ann_before = ann.clone();
        let result = f(ann, &buf)?;
        history.record(0, Vec::new(), Vec::new(), &ann_before, EditKind::Other);
        buf.version += 1;
        buf.dirty = true;
        Ok(result)
    }

    /// Undo the last edit on the view's buffer. Returns whether anything was
    /// undone. Pure history op — takes no new snapshot.
    pub fn undo(&mut self, view_id: ViewId) -> Result<bool, DispatchError> {
        self.history_step(view_id, true)
    }

    /// Redo the last undone edit on the view's buffer.
    pub fn redo(&mut self, view_id: ViewId) -> Result<bool, DispatchError> {
        self.history_step(view_id, false)
    }

    fn history_step(&mut self, view_id: ViewId, undo: bool) -> Result<bool, DispatchError> {
        let bid = self
            .views
            .get(&view_id)
            .ok_or(DispatchError::ViewNotFound(view_id))?
            .buffer_id;
        let buf_arc = self
            .buffers
            .get(bid)
            .ok_or(DispatchError::ViewNotFound(view_id))?;
        let mut buf = buf_arc.write().map_err(|_| DispatchError::PoisonedLock)?;
        let (ann, history) = self
            .buffers
            .ann_and_history_mut(bid)
            .ok_or(DispatchError::ViewNotFound(view_id))?;
        let changed = if undo {
            history.undo(&mut buf, ann)
        } else {
            history.redo(&mut buf, ann)
        };
        let len = buf.text.len();
        drop(buf);
        if changed {
            // Forward edit/paste rehome the caret; undo/redo must too so a
            // stale post-paste cursor cannot survive an empty restore.
            self.rehome_views_for_buffer(bid, len);
        }
        Ok(changed)
    }

    /// Reset every per-view render cache. Called by command arms
    /// (Open, Close, GoTo, etc.) that previously reset the single
    /// `seq_view` on `AppState`.
    pub fn reset_all_seq_views(&mut self) {
        for sv in self.seq_views.values_mut() {
            sv.reset();
        }
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use seqforge_core::Topology;

    #[test]
    fn workspace_starts_empty() {
        let ws = Workspace::default();
        assert!(ws.views.is_empty());
        assert!(ws.active_view.is_none());
        assert!(ws.buffers.is_empty());
    }

    #[test]
    fn buffer_store_dedupes_by_path() {
        let mut ws = Workspace::default();
        let path = PathBuf::from("/tmp/fake.gb");
        let id = ws
            .buffers
            .new_scratch("fake".into(), b"ATGC".to_vec(), Topology::Linear);
        ws.buffers.by_path.insert(path.clone(), id);

        struct ExplodingBio;
        impl BioOps for ExplodingBio {
            fn load(&self, _: &Path) -> Result<seqforge_core::Document, String> {
                panic!("must not load when dedup hits")
            }
            fn find_matches(
                &self,
                _: &[u8],
                _: &[u8],
                _: u8,
                _: bool,
            ) -> Vec<seqforge_core::SearchHit> {
                vec![]
            }
            fn find_cut_sites(&self, _: &[u8], _: &[&str], _: bool) -> Vec<seqforge_core::CutSite> {
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
                sites: &[seqforge_core::CutSite],
                _: &[u8],
                _: &seqforge_core::MethylContext,
            ) -> Vec<seqforge_core::MethylState> {
                vec![seqforge_core::MethylState::Cuttable; sites.len()]
            }
        }

        let got = ws.buffers.open_path(&path, &ExplodingBio).unwrap();
        assert_eq!(got, id);
    }

    #[test]
    fn add_view_makes_it_active() {
        let mut ws = Workspace::default();
        let bid = ws
            .buffers
            .new_scratch("x".into(), b"ATGC".to_vec(), Topology::Linear);
        let vid = ws.add_view(bid, ViewKind::TextView);
        assert_eq!(ws.active_view, Some(vid));
        assert!(ws.views.contains_key(&vid));
        assert!(ws.seq_views.contains_key(&vid));
    }

    #[test]
    fn with_active_buffer_passes_correct_handles() {
        let mut ws = Workspace::default();
        let bid = ws
            .buffers
            .new_scratch("x".into(), b"ATGC".to_vec(), Topology::Linear);
        let vid = ws.add_view(bid, ViewKind::TextView);

        let outcome = ws
            .with_active_buffer(|view, buf, ann| {
                assert_eq!(view.id, vid);
                assert_eq!(buf.text, b"ATGC");
                assert!(ann.is_empty());
                42
            })
            .unwrap();
        assert_eq!(outcome, 42);
    }

    #[test]
    fn with_buffer_mut_allows_mutation() {
        let mut ws = Workspace::default();
        let bid = ws
            .buffers
            .new_scratch("x".into(), b"AT".to_vec(), Topology::Linear);
        let vid = ws.add_view(bid, ViewKind::TextView);

        ws.with_buffer_mut(vid, |_view, buf, _ann| {
            buf.version += 1;
            buf.text.push(b'G');
        })
        .unwrap();

        ws.with_active_buffer(|_view, buf, _ann| {
            assert_eq!(buf.text, b"ATG");
            assert_eq!(buf.version, 1);
        })
        .unwrap();
    }

    #[test]
    fn with_active_buffer_without_view_errors() {
        let mut ws = Workspace::default();
        let err = ws.with_active_buffer(|_, _, _| ()).unwrap_err();
        assert!(matches!(err, DispatchError::NoActiveView));
    }

    #[test]
    fn edit_records_history_and_undo_redo_round_trip() {
        let mut ws = Workspace::default();
        let bid = ws
            .buffers
            .new_scratch("x".into(), b"ATGC".to_vec(), Topology::Linear);
        let vid = ws.add_view(bid, ViewKind::TextView);

        // insert "TT" at pos 2
        ws.edit(vid, EditKind::Insert, 2..2, b"TT").unwrap();
        ws.with_active_buffer(|view, buf, _| {
            assert_eq!(buf.text, b"ATTTGC");
            assert!(buf.dirty);
            assert_eq!(buf.version, 1);
            // cursor moved past the insert
            assert_eq!(view.selection.text_range().unwrap().focus, 4);
        })
        .unwrap();

        assert!(ws.undo(vid).unwrap());
        ws.with_active_buffer(|view, buf, _| {
            assert_eq!(buf.text, b"ATGC");
            // Undo rehomes the caret into 0..=len (was at 4 after insert).
            assert_eq!(view.selection.text_range(), Some(Selection::cursor(4)));
        })
        .unwrap();

        assert!(ws.redo(vid).unwrap());
        ws.with_active_buffer(|_, buf, _| assert_eq!(buf.text, b"ATTTGC"))
            .unwrap();

        // nothing left to redo
        assert!(!ws.redo(vid).unwrap());
    }

    #[test]
    fn undo_paste_into_empty_rehomes_caret_to_zero() {
        // The crash path: paste N bp into empty → cursor(N) → undo → len 0 must
        // leave cursor(0), not a stale caret past EOF.
        let mut ws = Workspace::default();
        let bid = ws
            .buffers
            .new_scratch("scratch".into(), Vec::new(), Topology::Linear);
        let vid = ws.add_view(bid, ViewKind::TextView);
        let slice = SeqSlice {
            bytes: b"ATGCATGC".to_vec(),
            features: Vec::new(),
            primers: Vec::new(),
        };
        ws.paste_slice(vid, 0, &slice, Orient::Identity, true)
            .unwrap();
        ws.with_active_buffer(|view, buf, _| {
            assert_eq!(buf.text, b"ATGCATGC");
            assert_eq!(view.selection.text_range(), Some(Selection::cursor(8)));
        })
        .unwrap();

        assert!(ws.undo(vid).unwrap());
        ws.with_active_buffer(|view, buf, _| {
            assert_eq!(buf.text, b"");
            assert_eq!(
                view.selection.text_range(),
                Some(Selection::cursor(0)),
                "undo into empty must rehome the caret"
            );
        })
        .unwrap();

        // A second paste at the rehomed caret must succeed (no OutOfRange).
        ws.paste_slice(vid, 0, &slice, Orient::Identity, true)
            .unwrap();
        ws.with_active_buffer(|_, buf, _| assert_eq!(buf.text, b"ATGCATGC"))
            .unwrap();
    }

    #[test]
    fn edit_out_of_range_errors_and_does_not_mutate() {
        let mut ws = Workspace::default();
        let bid = ws
            .buffers
            .new_scratch("x".into(), b"ATGC".to_vec(), Topology::Linear);
        let vid = ws.add_view(bid, ViewKind::TextView);

        let err = ws.edit(vid, EditKind::Insert, 0..99, b"X").unwrap_err();
        assert!(matches!(err, DispatchError::OutOfRange { .. }));
        ws.with_active_buffer(|_, buf, _| {
            assert_eq!(buf.text, b"ATGC");
            assert!(!buf.dirty);
        })
        .unwrap();
    }

    #[test]
    fn history_is_shared_across_views_of_one_buffer() {
        let mut ws = Workspace::default();
        let bid = ws
            .buffers
            .new_scratch("x".into(), b"ATGC".to_vec(), Topology::Linear);
        let v1 = ws.add_view(bid, ViewKind::TextView);
        let v2 = ws.add_view(bid, ViewKind::TextView);

        // edit through v1, undo through v2 — same buffer, same history
        ws.edit(v1, EditKind::Insert, 4..4, b"GG").unwrap();
        ws.with_buffer_mut(v2, |_, buf, _| assert_eq!(buf.text, b"ATGCGG"))
            .unwrap();
        assert!(ws.undo(v2).unwrap());
        ws.with_buffer_mut(v1, |_, buf, _| assert_eq!(buf.text, b"ATGC"))
            .unwrap();
    }

    #[test]
    fn with_buffer_unknown_view_errors() {
        let mut ws = Workspace::default();
        let err = ws.with_buffer(ViewId(999), |_, _, _| ()).unwrap_err();
        assert!(matches!(err, DispatchError::ViewNotFound(ViewId(999))));
    }

    #[test]
    fn close_view_drops_buffer_when_last_reference() {
        let mut ws = Workspace::default();
        let bid = ws
            .buffers
            .new_scratch("x".into(), b"AT".to_vec(), Topology::Linear);
        let vid = ws.add_view(bid, ViewKind::TextView);

        assert!(ws.buffers.get(bid).is_some());
        ws.close_view(vid).unwrap();
        assert!(ws.buffers.get(bid).is_none(), "buffer should be dropped");
        assert!(ws.active_view.is_none());
    }

    #[test]
    fn close_view_keeps_buffer_when_another_view_holds_it() {
        let mut ws = Workspace::default();
        let bid = ws
            .buffers
            .new_scratch("x".into(), b"AT".to_vec(), Topology::Linear);
        let v1 = ws.add_view(bid, ViewKind::TextView);
        let v2 = ws.add_view(bid, ViewKind::TextView);

        ws.close_view(v2).unwrap();
        assert!(ws.buffers.get(bid).is_some(), "buffer should remain");
        assert_eq!(ws.active_view, Some(v1));
    }

    #[test]
    fn two_views_can_share_a_buffer() {
        let mut ws = Workspace::default();
        let bid = ws
            .buffers
            .new_scratch("shared".into(), b"ATGC".to_vec(), Topology::Linear);
        let _v1 = ws.add_view(bid, ViewKind::TextView);
        let _v2 = ws.add_view(bid, ViewKind::TextView);
        let arc = ws.buffers.get(bid).unwrap();
        assert!(Arc::strong_count(&arc) >= 2);
    }

    #[test]
    fn focus_view_sets_active() {
        let mut ws = Workspace::default();
        let bid = ws
            .buffers
            .new_scratch("x".into(), b"AT".to_vec(), Topology::Linear);
        let v1 = ws.add_view(bid, ViewKind::TextView);
        let v2 = ws.add_view(bid, ViewKind::TextView);
        ws.focus_view(v1);
        assert_eq!(ws.active_view, Some(v1));
        ws.focus_view(v2);
        assert_eq!(ws.active_view, Some(v2));
        ws.focus_view(ViewId(99999));
        assert_eq!(ws.active_view, Some(v2));
    }

    #[test]
    fn find_view_for_path_resolves_existing() {
        let mut ws = Workspace::default();
        let path = PathBuf::from("/tmp/q.gb");
        let bid = ws
            .buffers
            .new_scratch("q".into(), b"AT".to_vec(), Topology::Linear);
        // Wire the buffer onto a path and onto a view.
        let arc = ws.buffers.get(bid).unwrap();
        arc.write().unwrap().source_path = Some(path.clone());
        let vid = ws.add_view(bid, ViewKind::TextView);
        assert_eq!(ws.find_view_for_path(&path), Some(vid));
        assert_eq!(ws.find_view_for_path(Path::new("/no.gb")), None);
    }

    #[test]
    fn buffer_store_remove_drops_arc() {
        let mut store = BufferStore::new();
        let id = store.new_scratch("x".into(), b"AT".to_vec(), Topology::Linear);
        let _handle = store.get(id).unwrap();
        let strong_before_drop = store.remove(id).unwrap();
        assert!(strong_before_drop >= 2);
        assert!(store.get(id).is_none());
        assert!(store.annotations(id).is_none());
    }
}
