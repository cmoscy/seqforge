//! Workspace, Pane, BufferStore — the GUI-side orchestration of the
//! editor-ready model types (`Buffer`, `Annotations`, `View`) introduced
//! in `seqforge-core::model`.
//!
//! Stage 2.5a (this sub-commit): types only. Nothing in `AppState` or
//! `command::apply` calls these yet. Subsequent sub-commits migrate
//! state, dispatch, and the viewer widget to use them.
//!
//! See `PLAN.md` Tier 2.5 for the locked-down architectural decisions
//! these types embody (Pane = dock-tab unit, `Arc<RwLock<Buffer>>` for
//! shared ownership, same-buffer-in-multiple-panes, etc.).
//!
//! `dead_code` is allowed module-wide because the consumers land in the
//! next sub-commits of Stage 2.5a; lifting the attribute is a checkpoint
//! that the migration is complete.

#![allow(dead_code)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use serde::{Deserialize, Serialize};
use seqforge_core::{
    Annotations, BioOps, Buffer, BufferId, View, ViewId, ViewKind,
};

// ── PaneId ───────────────────────────────────────────────────────────────────

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
)]
pub struct PaneId(pub u64);

impl std::fmt::Display for PaneId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "PaneId({})", self.0)
    }
}

// ── Pane ─────────────────────────────────────────────────────────────────────

/// A pane in the dock area. Holds an ordered list of [`View`]s (the tab
/// strip) and tracks which one is active.
///
/// Multi-tab support (Stage 2.5b) gives users a tab strip widget that
/// renders `views` and routes clicks to `SwitchTab`. Stage 2.5a keeps a
/// single Pane with a single View — same shape, just renamed.
#[derive(Debug, Serialize, Deserialize)]
pub struct Pane {
    pub id: PaneId,
    pub views: Vec<View>,
    /// Index into `views`. Always valid when `views` is non-empty.
    pub active: usize,
}

impl Pane {
    pub fn new(id: PaneId) -> Self {
        Self { id, views: Vec::new(), active: 0 }
    }

    pub fn active_view(&self) -> Option<&View> {
        self.views.get(self.active)
    }

    pub fn active_view_mut(&mut self) -> Option<&mut View> {
        self.views.get_mut(self.active)
    }

    /// Push a new view onto the tab strip and make it active.
    pub fn push_active(&mut self, view: View) {
        self.views.push(view);
        self.active = self.views.len() - 1;
    }

    /// Find a view by id; returns its index in `views`.
    pub fn find(&self, view_id: ViewId) -> Option<usize> {
        self.views.iter().position(|v| v.id == view_id)
    }

    /// Switch focus to the view with the given id. No-op if not present.
    pub fn switch_to(&mut self, view_id: ViewId) -> bool {
        if let Some(idx) = self.find(view_id) {
            self.active = idx;
            true
        } else {
            false
        }
    }

    /// Remove a view by id. If the removed view was active, falls back to
    /// the previous index (or 0 if at the head).
    pub fn close(&mut self, view_id: ViewId) -> Option<View> {
        let idx = self.find(view_id)?;
        let removed = self.views.remove(idx);
        if !self.views.is_empty() && self.active >= self.views.len() {
            self.active = self.views.len() - 1;
        }
        Some(removed)
    }

    pub fn is_empty(&self) -> bool {
        self.views.is_empty()
    }
}

// ── BufferStore ──────────────────────────────────────────────────────────────

/// Buffer + Annotations storage keyed by `BufferId`.
///
/// Same path opens once and is shared across panes via
/// `Arc<RwLock<Buffer>>` — split-view of the same file shares one rope
/// and one undo history. `by_path` deduplicates on `open_path`.
///
/// Tier 3d will add transaction broadcasting (one edit → invalidate all
/// views referencing the buffer); for 2.5a we just store handles.
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

    /// Look up a buffer handle by id.
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
    /// Returns the strong-count of the Arc just before drop (callers use
    /// this to verify nothing else still holds a reference, mostly for
    /// tests / debug).
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
        // Map the legacy `Document` into `Buffer` + `Annotations`. After
        // sub-commit 5, `BioOps::load` can return the split directly;
        // for now we adapt here so the existing bio crate stays untouched.
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

    /// Create a buffer from raw inputs — used by tests and any future
    /// scratch-buffer support.
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

/// Local IUPAC complement — duplicated from `seqforge-bio::complement` so
/// `seqforge-app` can compute it without an extra round-trip while the
/// migration is in flight. Sub-commit 5 collapses this back into the bio
/// crate by widening `BioOps::load` to return `(Buffer, Annotations)`
/// directly.
fn pure_complement(seq: &[u8]) -> Vec<u8> {
    seqforge_bio::complement(seq)
}

// ── Workspace ────────────────────────────────────────────────────────────────

/// The collection of panes + buffer store. One per `AppState`.
///
/// Stage 2.5a (here): one pane, one view, no behavior change.
/// Stage 2.5b: multi-tab within a pane.
/// Stage 2.5c: multi-pane via egui_dock splits.
#[derive(Debug, Serialize, Deserialize)]
pub struct Workspace {
    pub panes: HashMap<PaneId, Pane>,
    /// Pane order in the dock — drives Cmd+Shift+] cycling later.
    pub pane_order: Vec<PaneId>,
    pub active_pane: Option<PaneId>,

    /// Buffer + annotations storage. Skipped in serde because Arc<RwLock>
    /// doesn't round-trip through eframe persistence. On restart the
    /// recent-files / persisted-views list re-opens buffers; for 2.5a
    /// we just start empty.
    #[serde(skip)]
    pub buffers: BufferStore,

    next_pane: u64,
    next_view: u64,
}

impl Default for Workspace {
    fn default() -> Self {
        let mut ws = Self {
            panes: HashMap::new(),
            pane_order: Vec::new(),
            active_pane: None,
            buffers: BufferStore::new(),
            next_pane: 0,
            next_view: 0,
        };
        // Start with one empty pane — invariant maintained: at least one
        // pane exists for the dock to render into. Migration sub-commits
        // rely on this.
        let pane_id = ws.alloc_pane();
        ws.panes.insert(pane_id, Pane::new(pane_id));
        ws.pane_order.push(pane_id);
        ws.active_pane = Some(pane_id);
        ws
    }
}

impl Workspace {
    fn alloc_pane(&mut self) -> PaneId {
        self.next_pane += 1;
        PaneId(self.next_pane)
    }

    pub fn alloc_view_id(&mut self) -> ViewId {
        self.next_view += 1;
        ViewId(self.next_view)
    }

    pub fn active_pane(&self) -> Option<&Pane> {
        self.active_pane.and_then(|id| self.panes.get(&id))
    }

    pub fn active_pane_mut(&mut self) -> Option<&mut Pane> {
        let id = self.active_pane?;
        self.panes.get_mut(&id)
    }

    pub fn active_view(&self) -> Option<&View> {
        self.active_pane()?.active_view()
    }

    pub fn active_view_mut(&mut self) -> Option<&mut View> {
        self.active_pane_mut()?.active_view_mut()
    }

    /// Resolve the active view's buffer handle. Most dispatch paths go
    /// through this: it returns both the View (to mutate selection /
    /// scroll) and the buffer Arc (to read text / version).
    pub fn active_buffer(&self) -> Option<Arc<RwLock<Buffer>>> {
        let view = self.active_view()?;
        self.buffers.get(view.buffer_id)
    }

    /// Open `path` and attach a new View in the active pane. Returns the
    /// new view's id, or an error if no pane is active or load failed.
    pub fn open_path(
        &mut self,
        path: &Path,
        bio: &dyn BioOps,
    ) -> Result<ViewId, String> {
        let buffer_id = self.buffers.open_path(path, bio)?;
        let view_id = self.alloc_view_id();
        let pane = self
            .active_pane_mut()
            .ok_or_else(|| "no active pane".to_string())?;
        pane.push_active(View::new(view_id, buffer_id, ViewKind::TextView));
        Ok(view_id)
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use seqforge_core::Topology;

    #[test]
    fn workspace_starts_with_one_empty_pane() {
        let ws = Workspace::default();
        assert_eq!(ws.panes.len(), 1);
        assert_eq!(ws.pane_order.len(), 1);
        assert!(ws.active_pane.is_some());
        assert!(ws.active_pane().unwrap().is_empty());
        assert!(ws.active_view().is_none());
    }

    #[test]
    fn buffer_store_dedupes_by_path() {
        let mut ws = Workspace::default();
        let path = PathBuf::from("/tmp/fake.gb");
        // Stash a buffer by path so the second open returns the same id
        // without calling load() — we don't want a real BioOps in unit tests.
        let id = ws.buffers.insert_raw(
            "fake".into(),
            b"ATGC".to_vec(),
            Topology::Linear,
        );
        ws.buffers.by_path.insert(path.clone(), id);

        struct ExplodingBio;
        impl BioOps for ExplodingBio {
            fn load(&self, _: &std::path::Path) -> Result<seqforge_core::Document, String> {
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
    fn pane_push_and_close() {
        let mut p = Pane::new(PaneId(1));
        assert!(p.is_empty());
        p.push_active(View::new(ViewId(1), BufferId(1), ViewKind::TextView));
        p.push_active(View::new(ViewId(2), BufferId(1), ViewKind::TextView));
        assert_eq!(p.views.len(), 2);
        assert_eq!(p.active, 1);
        assert!(p.close(ViewId(2)).is_some());
        // Active clamps back to the remaining view.
        assert_eq!(p.active, 0);
        assert_eq!(p.views.len(), 1);
    }

    #[test]
    fn buffer_store_remove_drops_arc() {
        let mut store = BufferStore::new();
        let id = store.insert_raw("x".into(), b"AT".to_vec(), Topology::Linear);
        // Take a second handle to verify strong_count > 1 case.
        let _handle = store.get(id).unwrap();
        let strong_before_drop = store.remove(id).unwrap();
        assert!(strong_before_drop >= 2);
        assert!(store.get(id).is_none());
        assert!(store.annotations(id).is_none());
    }
}
