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

use serde::{Deserialize, Serialize};
use seqforge_core::{
    Annotations, BioOps, Buffer, BufferId, DispatchError, View, ViewId, ViewKind,
};

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

    /// Drop a buffer + its annotations and forget any path alias.
    /// Returns the strong-count of the Arc just before drop.
    pub fn remove(&mut self, id: BufferId) -> Option<usize> {
        let buf = self.buffers.remove(&id)?;
        self.annotations.remove(&id);
        self.by_path.retain(|_, v| *v != id);
        let count = Arc::strong_count(&buf);
        drop(buf);
        Some(count)
    }

    /// Find or load a buffer for `path`. If a buffer is already loaded
    /// for this path, returns its id without re-reading the file.
    pub fn open_path(
        &mut self,
        path: &Path,
        bio: &dyn BioOps,
    ) -> Result<BufferId, String> {
        if let Some(existing) = self.by_path.get(path).copied() {
            return Ok(existing);
        }
        let doc = bio.load(path)?;
        let complement = pure_complement(&doc.sequence);
        let buffer = Buffer::new(
            doc.name.clone(),
            doc.source_path.clone(),
            doc.sequence,
            complement,
            doc.topology,
        );
        let annotations = Annotations::new(doc.features);

        let id = self.alloc_id();
        self.buffers.insert(id, Arc::new(RwLock::new(buffer)));
        self.annotations.insert(id, annotations);
        self.by_path.insert(path.to_path_buf(), id);
        Ok(id)
    }

    /// Create a buffer from raw inputs — used by tests.
    #[cfg(test)]
    pub fn insert_raw(
        &mut self,
        name: String,
        text: Vec<u8>,
        topology: seqforge_core::Topology,
    ) -> BufferId {
        let complement = pure_complement(&text);
        let buffer = Buffer::new(name, None, text, complement, topology);
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

/// Local IUPAC complement.
fn pure_complement(seq: &[u8]) -> Vec<u8> {
    seqforge_bio::complement(seq)
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
    pub fn open_path(
        &mut self,
        path: &Path,
        bio: &dyn BioOps,
    ) -> Result<ViewId, String> {
        let buffer_id = self.buffers.open_path(path, bio)?;
        Ok(self.add_view(buffer_id, ViewKind::TextView))
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
        let ann = self
            .buffers
            .annotations_mut(bid)
            .expect("located above");
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
        let ann = self
            .buffers
            .annotations_mut(bid)
            .expect("located above");
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
        let ann = self
            .buffers
            .annotations_mut(bid)
            .expect("located above");
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
        let id = ws.buffers.insert_raw(
            "fake".into(),
            b"ATGC".to_vec(),
            Topology::Linear,
        );
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
            fn find_cut_sites(
                &self,
                _: &[u8],
                _: &[&str],
                _: bool,
            ) -> Vec<seqforge_core::CutSite> {
                vec![]
            }
        }

        let got = ws.buffers.open_path(&path, &ExplodingBio).unwrap();
        assert_eq!(got, id);
    }

    #[test]
    fn add_view_makes_it_active() {
        let mut ws = Workspace::default();
        let bid = ws.buffers.insert_raw("x".into(), b"ATGC".to_vec(), Topology::Linear);
        let vid = ws.add_view(bid, ViewKind::TextView);
        assert_eq!(ws.active_view, Some(vid));
        assert!(ws.views.contains_key(&vid));
        assert!(ws.seq_views.contains_key(&vid));
    }

    #[test]
    fn with_active_buffer_passes_correct_handles() {
        let mut ws = Workspace::default();
        let bid = ws.buffers.insert_raw("x".into(), b"ATGC".to_vec(), Topology::Linear);
        let vid = ws.add_view(bid, ViewKind::TextView);

        let outcome = ws
            .with_active_buffer(|view, buf, ann| {
                assert_eq!(view.id, vid);
                assert_eq!(buf.text, b"ATGC");
                assert!(ann.features.is_empty());
                42
            })
            .unwrap();
        assert_eq!(outcome, 42);
    }

    #[test]
    fn with_buffer_mut_allows_mutation() {
        let mut ws = Workspace::default();
        let bid = ws.buffers.insert_raw("x".into(), b"AT".to_vec(), Topology::Linear);
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
    fn with_buffer_unknown_view_errors() {
        let mut ws = Workspace::default();
        let err = ws.with_buffer(ViewId(999), |_, _, _| ()).unwrap_err();
        assert!(matches!(err, DispatchError::ViewNotFound(ViewId(999))));
    }

    #[test]
    fn close_view_drops_buffer_when_last_reference() {
        let mut ws = Workspace::default();
        let bid = ws.buffers.insert_raw("x".into(), b"AT".to_vec(), Topology::Linear);
        let vid = ws.add_view(bid, ViewKind::TextView);

        assert!(ws.buffers.get(bid).is_some());
        ws.close_view(vid).unwrap();
        assert!(ws.buffers.get(bid).is_none(), "buffer should be dropped");
        assert!(ws.active_view.is_none());
    }

    #[test]
    fn close_view_keeps_buffer_when_another_view_holds_it() {
        let mut ws = Workspace::default();
        let bid = ws.buffers.insert_raw("x".into(), b"AT".to_vec(), Topology::Linear);
        let v1 = ws.add_view(bid, ViewKind::TextView);
        let v2 = ws.add_view(bid, ViewKind::TextView);

        ws.close_view(v2).unwrap();
        assert!(ws.buffers.get(bid).is_some(), "buffer should remain");
        assert_eq!(ws.active_view, Some(v1));
    }

    #[test]
    fn two_views_can_share_a_buffer() {
        let mut ws = Workspace::default();
        let bid = ws.buffers.insert_raw("shared".into(), b"ATGC".to_vec(), Topology::Linear);
        let _v1 = ws.add_view(bid, ViewKind::TextView);
        let _v2 = ws.add_view(bid, ViewKind::TextView);
        let arc = ws.buffers.get(bid).unwrap();
        assert!(Arc::strong_count(&arc) >= 2);
    }

    #[test]
    fn focus_view_sets_active() {
        let mut ws = Workspace::default();
        let bid = ws.buffers.insert_raw("x".into(), b"AT".to_vec(), Topology::Linear);
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
        let bid = ws.buffers.insert_raw("q".into(), b"AT".to_vec(), Topology::Linear);
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
        let id = store.insert_raw("x".into(), b"AT".to_vec(), Topology::Linear);
        let _handle = store.get(id).unwrap();
        let strong_before_drop = store.remove(id).unwrap();
        assert!(strong_before_drop >= 2);
        assert!(store.get(id).is_none());
        assert!(store.annotations(id).is_none());
    }
}
