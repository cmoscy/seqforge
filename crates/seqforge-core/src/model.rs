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

use crate::{CutSite, Feature, FeatureId, Primer, PrimerId, SearchHit, Selection, Topology};

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
    /// Hash of the on-disk file bytes as last seen by SeqForge (at load, and
    /// re-set after each successful save). Powers the external-change guard:
    /// on save, the file is re-read and re-hashed; a mismatch means the file
    /// changed on disk underneath us. In-memory only (no format involvement),
    /// so it is never serialized and only ever compared within one session.
    #[serde(skip)]
    pub loaded_hash: Option<u64>,
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
            loaded_hash: None,
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
///
/// **Features are addressed only by [`FeatureId`]** (ROADMAP decision 12). The
/// backing `Vec` is `pub(crate)` — invisible outside `seqforge-core`, so no
/// downstream crate can store or misuse a positional index; the public API is
/// id-only (`add` / `get` / `get_mut` / `remove` / `rename` / ordered `iter`).
/// Within `core`, the mutation primitive (`apply_splice`) and history sizing do
/// bulk positional work over the `Vec`. Resolution is a linear scan — N is tiny;
/// an `IndexMap<FeatureId, Feature>` can slot in behind this same API later on
/// profiling evidence, with zero outside churn.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Annotations {
    pub(crate) features: Vec<Feature>,
    /// Monotonic id source, session-scoped. Not persisted — ids are re-minted
    /// on load (`new` reassigns every feature), so GenBank/FASTA stay positional.
    #[serde(skip)]
    next_id: u64,
    /// Authored primers on this buffer (ROADMAP decision 14). Same id-only
    /// discipline as `features` — the backing `Vec` is `pub(crate)` and the
    /// public surface is id-addressed (`add_primer` / `primer` / `primer_mut` /
    /// `remove_primer` / `rename_primer` / ordered `primers`). Empty by default;
    /// GenBank `primer_bind` round-trip populates it (Phase 0.3). Within `core`,
    /// the primer-specific shift handler (`mutations::shift_primers`) does bulk
    /// positional work over the `Vec`.
    #[serde(default)]
    pub(crate) primers: Vec<Primer>,
    /// Monotonic id source for primers, session-scoped and **separate** from
    /// `next_id` — `PrimerId` is a distinct newtype from `FeatureId`.
    #[serde(skip)]
    next_primer_id: u64,
}

impl Annotations {
    /// Build annotations from freshly-loaded features, **minting a new id for
    /// each** (incoming ids, e.g. the `#[serde(skip)]` placeholder, are ignored).
    /// This is the mint-on-load path; ids live only for this process.
    pub fn new(features: Vec<Feature>) -> Self {
        Self::from_parts(features, Vec::new())
    }

    /// Build annotations from freshly-loaded features **and primers**, minting a
    /// fresh id for each (incoming ids ignored — the mint-on-load path). This is
    /// the GenBank load entry point once `primer_bind` round-trip lands
    /// (Phase 0.3); `new` is the features-only convenience.
    pub fn from_parts(features: Vec<Feature>, primers: Vec<Primer>) -> Self {
        let mut ann = Self {
            features: Vec::with_capacity(features.len()),
            primers: Vec::with_capacity(primers.len()),
            ..Default::default()
        };
        for mut f in features {
            f.id = ann.mint();
            ann.features.push(f);
        }
        for mut p in primers {
            p.id = ann.mint_primer();
            ann.primers.push(p);
        }
        ann
    }

    /// Mint the next session-scoped id. Ids start at 1, so `FeatureId(0)` (the
    /// `#[serde(skip)]` default) is always an unminted placeholder.
    fn mint(&mut self) -> FeatureId {
        self.next_id += 1;
        FeatureId(self.next_id)
    }

    pub fn len(&self) -> usize {
        self.features.len()
    }

    pub fn is_empty(&self) -> bool {
        self.features.is_empty()
    }

    /// Ordered iteration over features (definition order). The consumer may
    /// `enumerate()` for within-frame positional work, but must never store the
    /// index across a frame, an edit, or the wire — use the [`FeatureId`].
    pub fn iter(&self) -> impl Iterator<Item = &Feature> {
        self.features.iter()
    }

    /// Look up a feature by id (linear scan). `None` if it was removed.
    pub fn get(&self, id: FeatureId) -> Option<&Feature> {
        self.features.iter().find(|f| f.id == id)
    }

    /// Mutable lookup by id (linear scan).
    pub fn get_mut(&mut self, id: FeatureId) -> Option<&mut Feature> {
        self.features.iter_mut().find(|f| f.id == id)
    }

    /// Read-only positional access for the immediate-mode renderer/minimap
    /// **only**. The position is a private within-frame detail (it changes on
    /// any add/remove); resolve it fresh each frame and never persist it.
    pub fn by_position(&self, pos: usize) -> Option<&Feature> {
        self.features.get(pos)
    }

    /// Add a feature, minting and assigning its id (any incoming `feature.id`
    /// is overwritten). Returns the new id.
    pub fn add(&mut self, mut feature: Feature) -> FeatureId {
        let id = self.mint();
        feature.id = id;
        self.features.push(feature);
        id
    }

    /// Remove the feature with `id`. Returns `true` if one was removed.
    pub fn remove(&mut self, id: FeatureId) -> bool {
        match self.features.iter().position(|f| f.id == id) {
            Some(pos) => {
                self.features.remove(pos);
                true
            }
            None => false,
        }
    }

    /// Rename the feature with `id`. Returns `true` if one was found.
    pub fn rename(&mut self, id: FeatureId, label: String) -> bool {
        match self.get_mut(id) {
            Some(f) => {
                f.label = label;
                true
            }
            None => false,
        }
    }

    // ── Primers (id-only API, mirroring features; ROADMAP decision 14) ──────────

    /// Mint the next session-scoped [`PrimerId`]. Separate counter from features;
    /// ids start at 1, so `PrimerId(0)` (the `#[serde(skip)]` default) is always
    /// an unminted placeholder.
    fn mint_primer(&mut self) -> PrimerId {
        self.next_primer_id += 1;
        PrimerId(self.next_primer_id)
    }

    pub fn primers_len(&self) -> usize {
        self.primers.len()
    }

    pub fn primers_is_empty(&self) -> bool {
        self.primers.is_empty()
    }

    /// Ordered iteration over primers (definition order). As with [`Self::iter`],
    /// callers may `enumerate()` for within-frame work but must never store the
    /// index — use the [`PrimerId`].
    pub fn primers(&self) -> impl Iterator<Item = &Primer> {
        self.primers.iter()
    }

    /// Look up a primer by id (linear scan). `None` if it was removed.
    pub fn primer(&self, id: PrimerId) -> Option<&Primer> {
        self.primers.iter().find(|p| p.id == id)
    }

    /// Mutable lookup by id (linear scan).
    pub fn primer_mut(&mut self, id: PrimerId) -> Option<&mut Primer> {
        self.primers.iter_mut().find(|p| p.id == id)
    }

    /// Add a primer, minting and assigning its id (any incoming `primer.id` is
    /// overwritten). Returns the new id.
    pub fn add_primer(&mut self, mut primer: Primer) -> PrimerId {
        let id = self.mint_primer();
        primer.id = id;
        self.primers.push(primer);
        id
    }

    /// Remove the primer with `id`. Returns `true` if one was removed.
    pub fn remove_primer(&mut self, id: PrimerId) -> bool {
        match self.primers.iter().position(|p| p.id == id) {
            Some(pos) => {
                self.primers.remove(pos);
                true
            }
            None => false,
        }
    }

    /// Rename the primer with `id`. Returns `true` if one was found.
    pub fn rename_primer(&mut self, id: PrimerId, name: String) -> bool {
        match self.primer_mut(id) {
            Some(p) => {
                p.name = name;
                true
            }
            None => false,
        }
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
    /// The currently selected feature, by stable id (ROADMAP decision 12).
    /// Was a positional `usize` — which dangled after any edit that shifted or
    /// removed features. An id is resolved to a position fresh each frame.
    pub selected_feature: Option<FeatureId>,
    /// The currently selected primer, by stable id (ROADMAP decision 12/14).
    /// Mirrors `selected_feature`; resolved to a position fresh each frame.
    pub selected_primer: Option<PrimerId>,
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
            selected_primer: None,
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
        self.selected_primer = None;
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
        v.selected_feature = Some(FeatureId(1));
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

    fn feat(label: &str) -> Feature {
        Feature {
            id: Default::default(),
            range: 0..3,
            raw_kind: "CDS".into(),
            label: label.into(),
            strand: crate::Strand::Forward,
            qualifiers: Default::default(),
            provenance: None,
        }
    }

    #[test]
    fn annotations_mint_unique_ids_on_load() {
        let ann = Annotations::new(vec![feat("a"), feat("b"), feat("c")]);
        let ids: Vec<_> = ann.iter().map(|f| f.id).collect();
        assert_eq!(ids, vec![FeatureId(1), FeatureId(2), FeatureId(3)]);
        // None is the placeholder default.
        assert!(ids.iter().all(|id| *id != FeatureId(0)));
    }

    #[test]
    fn annotations_id_api_add_get_remove_rename() {
        let mut ann = Annotations::new(vec![]);
        let id = ann.add(feat("first"));
        assert_eq!(ann.get(id).unwrap().label, "first");

        // Ids are never reused: removing then adding mints a fresh id.
        assert!(ann.remove(id));
        assert!(ann.get(id).is_none());
        let id2 = ann.add(feat("second"));
        assert_ne!(id, id2);

        assert!(ann.rename(id2, "renamed".into()));
        assert_eq!(ann.get(id2).unwrap().label, "renamed");
        // Operating on a stale id is a no-op, not a panic.
        assert!(!ann.rename(id, "ghost".into()));
        assert!(!ann.remove(id));
    }

    // ── Primer id-API (mirrors features; decision 14) ───────────────────────────

    fn primer(name: &str) -> Primer {
        Primer {
            id: Default::default(),
            name: name.into(),
            sequence: "ACGTACGT".into(),
            binding: Some(0..8),
            strand: crate::Strand::Forward,
            qualifiers: Default::default(),
        }
    }

    #[test]
    fn annotations_default_has_no_primers() {
        let ann = Annotations::default();
        assert!(ann.primers_is_empty());
        assert_eq!(ann.primers_len(), 0);
    }

    #[test]
    fn primer_id_api_add_get_remove_rename() {
        let mut ann = Annotations::new(vec![]);
        let id = ann.add_primer(primer("fwd"));
        assert_eq!(ann.primer(id).unwrap().name, "fwd");
        assert_eq!(ann.primers_len(), 1);

        // Mutable access through the id.
        ann.primer_mut(id).unwrap().binding = None;
        assert!(ann.primer(id).unwrap().binding.is_none());

        // Ids are never reused: removing then adding mints a fresh id.
        assert!(ann.remove_primer(id));
        assert!(ann.primer(id).is_none());
        let id2 = ann.add_primer(primer("rev"));
        assert_ne!(id, id2);

        assert!(ann.rename_primer(id2, "renamed".into()));
        assert_eq!(ann.primer(id2).unwrap().name, "renamed");
        // Operating on a stale id is a no-op, not a panic.
        assert!(!ann.rename_primer(id, "ghost".into()));
        assert!(!ann.remove_primer(id));
    }

    #[test]
    fn primer_ids_are_separate_from_feature_ids() {
        // Distinct counters: a fresh Annotations mints PrimerId(1) even after
        // features exist, so the two id spaces never collide by construction.
        let mut ann = Annotations::new(vec![feat("f1"), feat("f2")]);
        let pid = ann.add_primer(primer("p"));
        assert_eq!(pid, PrimerId(1));
        assert_eq!(ann.iter().next().unwrap().id, FeatureId(1));
    }

    #[test]
    fn primers_iterate_in_insertion_order() {
        let mut ann = Annotations::new(vec![]);
        ann.add_primer(primer("a"));
        ann.add_primer(primer("b"));
        ann.add_primer(primer("c"));
        let names: Vec<_> = ann.primers().map(|p| p.name.clone()).collect();
        assert_eq!(names, vec!["a", "b", "c"]);
    }

    #[test]
    fn view_clear_selection_drops_primer_too() {
        let mut v = View::new(ViewId(1), BufferId(1), ViewKind::TextView);
        v.selected_primer = Some(PrimerId(1));
        v.clear_selection();
        assert!(v.selected_primer.is_none());
    }
}
