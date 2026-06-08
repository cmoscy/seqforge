//! Editor-ready data model — introduced in Stage 2.5a of the pre-editor
//! refactor. These types are not yet wired into `AppState` or `dispatch`;
//! the migration happens in subsequent sub-commits.
//!
//! Hierarchy (see `plans/refactor.md` Tier 2.5 for the full design):
//!
//! - [`Buffer`] — the editable sequence + identity. Shareable across views
//!   via `Arc<RwLock<Buffer>>` (in `seqforge-app`). One per loaded file.
//! - [`Annotations`] — features and view-independent derived data, layered
//!   on a [`Buffer`]. One per buffer.
//! - [`View`] — per-render state: selection, scroll, search results, active
//!   enzymes. Multiple [`View`]s can reference the same [`Buffer`] (e.g.
//!   split-view of the same plasmid).
//! - [`ViewKind`] — discriminates text / linear / circular renderings. Only
//!   `TextView` exists today; the enum is here so future kinds slot in
//!   without a dispatch refactor.
//!
//! [`Buffer::version`] is the cache-invalidation key for all per-view
//! caches (complement, feature stacking, etc.) once Tier 3a wires it
//! through. Tier 3b will replace `text: Vec<u8>` with a rope; Tier 3c
//! turns `Feature.range` and `Selection` into anchors; Tier 3d adds the
//! transaction log + undo stack on `Buffer`.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::{CutSite, Feature, SearchHit, Selection, Topology};

// ── Id newtypes ──────────────────────────────────────────────────────────────
//
// Newtypes (not bare `u64`) so a `BufferId` can't be accidentally passed to a
// function expecting a `ViewId`, etc. The id space is per-process; ids are not
// stable across restarts.

macro_rules! id_newtype {
    ($name:ident) => {
        #[derive(
            Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
        )]
        pub struct $name(pub u64);

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "{}({})", stringify!($name), self.0)
            }
        }

        /// Parse a bare numeric id (`"42"`) into the newtype. Needed by
        /// clap's auto-derived value parsers for CLI flags like
        /// `--view 5` and by socket-protocol clients that pass the id
        /// as a JSON number.
        impl std::str::FromStr for $name {
            type Err = std::num::ParseIntError;
            fn from_str(s: &str) -> Result<Self, Self::Err> {
                s.parse::<u64>().map($name)
            }
        }
    };
}

id_newtype!(BufferId);
id_newtype!(ViewId);

// ── Buffer ───────────────────────────────────────────────────────────────────

/// The editable sequence and its identity.
///
/// In Tier 2.5a this is structurally a renamed [`crate::Document`] minus
/// `features` (which moved to [`Annotations`]). Future tiers evolve `text`
/// from `Vec<u8>` to a rope (3b) and add anchor + history fields (3c, 3d).
///
/// The complement strand is **not** stored here: it is a pure function of
/// `text` and is derived on demand (by `seqforge-bio` for operations, and
/// inline at render for the viewport). See `docs/architecture.md` —
/// "derived sequence data is computed, never stored on core."
///
/// [`version`]: Buffer::version
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Buffer {
    pub name: String,
    pub source_path: Option<PathBuf>,
    /// Raw ASCII bytes. Becomes a `Rope` in Tier 3b.
    pub text: Vec<u8>,
    pub topology: Topology,
    /// Monotonically increasing version; bumped by every mutation. The
    /// invalidation key for all per-view caches.
    pub version: u64,
    /// Unsaved-changes flag. Set by `mutations::apply_splice`; cleared by
    /// the save handler. Transient — a freshly loaded buffer is clean, so
    /// it is not persisted.
    #[serde(skip)]
    pub dirty: bool,
}

impl Buffer {
    /// Build a Buffer from raw sequence bytes and identity. The complement
    /// strand is derived on demand, not stored, so `seqforge-core` needs no
    /// dependency on `seqforge-bio`.
    ///
    /// Future Tier 3b: text → Rope.
    pub fn new(
        name: String,
        source_path: Option<PathBuf>,
        text: Vec<u8>,
        topology: Topology,
    ) -> Self {
        Self {
            name,
            source_path,
            text,
            topology,
            version: 0,
            dirty: false,
        }
    }

    pub fn len(&self) -> usize {
        self.text.len()
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    pub fn is_circular(&self) -> bool {
        matches!(self.topology, Topology::Circular)
    }
}

// ── Annotations ──────────────────────────────────────────────────────────────

/// Features and any view-independent derived data layered on a [`Buffer`].
///
/// One Annotations value per Buffer; lifetimes match. In Tier 3c the
/// `Feature.range: Range<usize>` field becomes an anchor range so feature
/// positions track edits without manual remapping.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Annotations {
    pub features: Vec<Feature>,
}

impl Annotations {
    pub fn new(features: Vec<Feature>) -> Self {
        Self { features }
    }

    pub fn is_empty(&self) -> bool {
        self.features.is_empty()
    }
}

// ── ViewKind ─────────────────────────────────────────────────────────────────

/// Which render strategy a [`View`] uses. Only `TextView` exists in MVP;
/// `LinearView` / `CircularView` are post-MVP additions and land as new
/// variants here (plus a render impl) rather than a dispatch refactor.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ViewKind {
    /// Dual-strand monospace text rendering — the current viewer.
    #[default]
    TextView,
}

impl ViewKind {
    /// Static tag for `KeyContext` predicates. Keymaps target view kinds
    /// (`Pane:TextView`, `Pane:LinearView`) without naming specific panes.
    pub fn context_tag(self) -> &'static str {
        match self {
            ViewKind::TextView => "Pane:TextView",
        }
    }
}

// ── View ─────────────────────────────────────────────────────────────────────

/// Per-render state. Each open view in the UI gets one of these.
///
/// Selection, scroll, search results, find query, and the active enzyme
/// list are all per-view: switching tabs (Stage 2.5b) restores them; split
/// view (Stage 2.5c) gives each pane its own independent set even when the
/// same buffer is shown twice.
///
/// `search_hits` / `cut_sites` / `active_enzymes` and the one-shot
/// `scroll_to` are `#[serde(skip)]`: they're transient and don't survive
/// process restart.
#[derive(Debug, Serialize, Deserialize)]
pub struct View {
    pub id: ViewId,
    pub buffer_id: BufferId,
    pub kind: ViewKind,
    pub selection: Option<Selection>,
    pub selected_feature: Option<usize>,
    /// Last persisted scroll offset, restored on tab switch / app restart.
    pub scroll_pos: Option<f32>,
    /// One-shot scroll request, consumed by the viewer each frame.
    #[serde(skip)]
    pub scroll_to: Option<usize>,
    #[serde(skip)]
    pub search_hits: Vec<SearchHit>,
    #[serde(skip)]
    pub cut_sites: Vec<CutSite>,
    #[serde(skip)]
    pub active_enzymes: Vec<String>,
    /// Visible sequence range written each frame by the text viewer.
    /// Used by the minimap to paint the viewport indicator.
    #[serde(skip)]
    pub visible_range: Option<(usize, usize)>,
}

impl View {
    pub fn new(id: ViewId, buffer_id: BufferId, kind: ViewKind) -> Self {
        Self {
            id,
            buffer_id,
            kind,
            selection: None,
            selected_feature: None,
            scroll_pos: None,
            scroll_to: None,
            search_hits: Vec::new(),
            cut_sites: Vec::new(),
            active_enzymes: Vec::new(),
            visible_range: None,
        }
    }

    /// Reset selection state. Used when reloading the same view onto a
    /// different buffer, or when explicit user action (e.g. Close) demands.
    pub fn clear_selection(&mut self) {
        self.selection = None;
        self.selected_feature = None;
    }

    /// Drop all derived results. Called when the underlying buffer changes
    /// in a way that invalidates them (close, reload, or future edits
    /// before the per-result anchor-mapping path lands).
    pub fn clear_results(&mut self) {
        self.search_hits.clear();
        self.cut_sites.clear();
        self.active_enzymes.clear();
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buffer_new_stores_fields() {
        let b = Buffer::new("test".into(), None, b"ATGC".to_vec(), Topology::Linear);
        assert_eq!(b.text, b"ATGC");
        assert_eq!(b.version, 0);
        assert_eq!(b.len(), 4);
        assert!(!b.is_empty());
        assert!(!b.is_circular());
    }

    #[test]
    fn buffer_circular_topology() {
        let b = Buffer::new("p".into(), None, b"AAAA".to_vec(), Topology::Circular);
        assert!(b.is_circular());
    }

    #[test]
    fn view_clear_selection_drops_feature_too() {
        let mut v = View::new(ViewId(1), BufferId(1), ViewKind::TextView);
        v.selection = Some(Selection::range(0, 4));
        v.selected_feature = Some(0);
        v.clear_selection();
        assert!(v.selection.is_none());
        assert!(v.selected_feature.is_none());
    }

    #[test]
    fn view_clear_results_empties_caches() {
        let mut v = View::new(ViewId(1), BufferId(1), ViewKind::TextView);
        v.active_enzymes.push("EcoRI".into());
        v.clear_results();
        assert!(v.active_enzymes.is_empty());
        assert!(v.search_hits.is_empty());
        assert!(v.cut_sites.is_empty());
    }

    #[test]
    fn ids_are_distinct_types() {
        // Compile-time check: these would not compile if BufferId and ViewId
        // were the same type. The cfg_attr keeps the assertion in test-only.
        let b = BufferId(1);
        let v = ViewId(1);
        assert_eq!(format!("{b}"), "BufferId(1)");
        assert_eq!(format!("{v}"), "ViewId(1)");
    }

    #[test]
    fn view_kind_context_tag() {
        assert_eq!(ViewKind::TextView.context_tag(), "Pane:TextView");
    }
}
