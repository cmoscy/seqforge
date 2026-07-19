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

use crate::span::Span;
use crate::{
    CutSite, Feature, FeatureId, MethylContext, MethylState, Primer, PrimerId, SearchHit, Topology,
};

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
            // An import with no name-bearing qualifier arrives here unnamed;
            // give it a unique default from the one shared generator (decision 9)
            // so nameless imports never collide. Named primers are untouched.
            if p.name.trim().is_empty() {
                p.name = ann.suggest_primer_name();
            }
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

    /// Read-only positional access for the immediate-mode renderer **only**
    /// (mirrors [`Self::by_position`]). The position is a private within-frame
    /// detail — resolve it fresh each frame and never persist it; carry the
    /// [`PrimerId`] instead.
    pub fn primer_by_position(&self, pos: usize) -> Option<&Primer> {
        self.primers.get(pos)
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

    /// Suggest a unique default primer name — the lowest `Primer N` (N ≥ 1) that
    /// no existing primer already uses (ROADMAP-track decision 9). One shared
    /// generator for creation (the CLI `--name` fallback and the GUI dialog
    /// pre-fill, Phase 2.1/2.3) and the GenBank import fallback, so every path
    /// names primers the same way. Creation is never blocked on a missing name;
    /// `rename_primer` covers relabeling afterwards.
    pub fn suggest_primer_name(&self) -> String {
        (1..)
            .map(|n| format!("Primer {n}"))
            .find(|name| self.primers.iter().all(|p| &p.name != name))
            .expect("an infinite range always yields an unused name")
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
/// A specific restriction cut site, keyed **structurally** (cut sites are derived
/// and re-scanned each version, so they carry no persistent id — the enzyme
/// analog of decision 16's derived primer sites). `(enzyme, recognition_start)`
/// uniquely identifies a site: one enzyme has at most one recognition per start.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CutSiteKey {
    pub enzyme: String,
    pub recognition_start: usize,
}

// ── Selection ─────────────────────────────────────────────────────────────────

/// A selection or cursor position in the sequence.
///
/// When `anchor == focus` the selection is a **cursor** — rendered as a thin
/// vertical line between bases. When they differ it is a **range**. The anchor
/// is where the user first clicked; the focus tracks the current extent.
///
/// `wrap` makes the selection **circular-native**: with `wrap == true` the
/// selected region is the arc from `anchor` **through the origin** to `focus`
/// (the complement of the `(min, max)` interval), so shift-selecting from near
/// the end through position 0 to the start is representable on a plasmid. On a
/// linear molecule `wrap` stays `false`. The single wrap-aware projection is
/// [`Selection::to_span`]; render/copy consume its [`Span::linear_pieces`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Selection {
    /// Fixed end — where the interaction began.
    pub anchor: usize,
    /// Moving end — equals `anchor` for a cursor.
    pub focus: usize,
    /// Whether the region runs from `anchor` through the origin to `focus`.
    /// `#[serde(default)]` keeps older serialized selections (no `wrap`) loadable
    /// as the non-wrapping default.
    #[serde(default)]
    pub wrap: bool,
}

impl Selection {
    pub fn cursor(pos: usize) -> Self {
        Self {
            anchor: pos,
            focus: pos,
            wrap: false,
        }
    }

    pub fn range(start: usize, end: usize) -> Self {
        Self {
            anchor: start,
            focus: end,
            wrap: false,
        }
    }

    pub fn is_cursor(self) -> bool {
        self.anchor == self.focus && !self.wrap
    }

    /// Clamp anchor/focus into the valid caret range `0..=len`.
    ///
    /// Used after buffer-length changes (undo/redo, topology, revert) so a
    /// stale caret past EOF cannot reach `apply_splice`. Empty buffers
    /// (`len == 0`) always become `cursor(0)`. Wrap is dropped — after a
    /// shrink the safe editable selection is a linear range or caret.
    pub fn clamp_to_len(self, len: usize) -> Selection {
        if len == 0 {
            return Selection::cursor(0);
        }
        let anchor = self.anchor.min(len);
        let focus = self.focus.min(len);
        if anchor == focus {
            Selection::cursor(anchor)
        } else {
            Selection {
                anchor,
                focus,
                wrap: false,
            }
        }
    }

    /// Returns `(start, end)` in ascending order regardless of drag direction.
    /// Bounds-only — for a wrapping selection this is the interval the region is
    /// the **complement** of, not the region itself; use [`Selection::to_span`]
    /// for the actual covered geometry.
    pub fn ordered(self) -> (usize, usize) {
        (self.anchor.min(self.focus), self.anchor.max(self.focus))
    }

    /// A selection covering `span` on a molecule of length `len` — the inverse of
    /// [`Selection::to_span`]. Anchor at the span start, focus at its (mod-`len`)
    /// end, `wrap` set iff the span crosses the origin. A whole-molecule span
    /// (`len == len_total`) maps to a **full-circle** selection (`anchor == focus`
    /// with `wrap`), not the empty cursor that `anchor == focus` alone would mean.
    pub fn from_span(span: Span, len: usize) -> Selection {
        if len > 0 && span.len == len {
            return Selection {
                anchor: span.start,
                focus: span.start,
                wrap: true,
            };
        }
        Selection {
            anchor: span.start,
            focus: span.end(len),
            wrap: span.wraps(len),
        }
    }

    /// The covered region as a wrap-aware [`Span`] on a molecule of length `len`
    /// — the single geometry projection (highlight / copy / minimap all consume
    /// its [`Span::linear_pieces`]). Non-wrapping → the `[min, max)` arc;
    /// wrapping → the origin-crossing arc `[max..len) ∪ [0..min)`, with the
    /// `anchor == focus && wrap` degenerate meaning the whole molecule.
    pub fn to_span(self, len: usize) -> Span {
        let (lo, hi) = self.ordered();
        if self.wrap {
            if lo == hi {
                Span::full(len)
            } else {
                Span::between(hi, lo, len)
            }
        } else {
            Span::from_range(lo..hi)
        }
    }

    /// Move the `focus` by a signed `delta` on a **circular** molecule of length
    /// `len`: the focus steps mod `len` (never clamped), and `wrap` toggles each
    /// time the step crosses the origin — so extending a selection past position
    /// `0`/`len` grows it *through* the origin instead of stalling. Pure; the
    /// anchor is untouched. Linear molecules keep the clamping arrow-nav path.
    pub fn move_focus_circular(self, delta: isize, len: usize) -> Selection {
        let (focus, wrap) = step_focus_circular(self.focus, self.wrap, delta, len);
        Selection {
            anchor: self.anchor,
            focus,
            wrap,
        }
    }
}

/// The pure focus/wrap transition backing [`Selection::move_focus_circular`],
/// factored out for exhaustive truth-table tests. Returns `(new_focus,
/// new_wrap)`. `new_wrap` flips exactly when the step leaves `[0, len)` — i.e.
/// crosses the origin — which is independent of the anchor. Assumes
/// `|delta| < len` (true for arrow/line steps on any real molecule).
fn step_focus_circular(focus: usize, wrap: bool, delta: isize, len: usize) -> (usize, bool) {
    if len == 0 {
        return (0, false);
    }
    let raw = focus as isize + delta;
    let crossed = raw < 0 || raw >= len as isize;
    let new_focus = raw.rem_euclid(len as isize) as usize;
    (new_focus, wrap ^ crossed)
}

/// The one selection on a [`View`] — a tagged union so the mutual exclusion
/// ("at most one object selected; the range mirrors it") is **structural**, not
/// maintained by convention across every click/command site (ROADMAP decision 12,
/// extended from feature ids to selection state). Object variants carry their
/// template range so [`ViewSelection::text_range`] is self-contained: a `View`
/// can't reach the [`Annotations`] that own feature/primer geometry.
///
/// The payload is the reusable "selectable item" vocabulary a future assembly
/// workbench / plugin surface consumes (decision 11); a `Fragment` variant + the
/// multi-select cart are deferred to the cloning track (not built here).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ViewSelection {
    /// Nothing selected.
    #[default]
    None,
    /// A bare text selection — cursor or range. The editable selection.
    Text(Selection),
    /// A feature object; `range` == the feature's span (what copy/edit act on).
    Feature { id: FeatureId, range: Selection },
    /// A primer object. Object-only: a primer has no template range (a reverse
    /// primer sits on the bottom strand; a 5' tail has no template column —
    /// Phase 1.5e). The highlight lands on the oligo via the PrimerTrack.
    Primer(PrimerId),
    /// A restriction cut site; `range` == the recognition span.
    CutSite { key: CutSiteKey, range: Selection },
    /// An ordered PCR primer pair (Primers Phase 3.1b; extends ROADMAP decision
    /// 20). Bounded — **not** a general multi-select (that stays deferred to the
    /// assembly cart). `range` == the amplicon span (fwd 5'-anchor → rev
    /// 5'-anchor), so selecting the pair highlights what `Pcr` would produce.
    PrimerPair {
        fwd: PrimerId,
        rev: PrimerId,
        range: Selection,
    },
}

impl ViewSelection {
    /// The editable / render text range, if any. `Text`/`Feature`/`CutSite` carry
    /// one; `Primer` (object-only) and `None` do not. This is what editing, copy,
    /// and the sequence-row highlight read.
    pub fn text_range(&self) -> Option<Selection> {
        match self {
            ViewSelection::Text(s)
            | ViewSelection::Feature { range: s, .. }
            | ViewSelection::CutSite { range: s, .. }
            | ViewSelection::PrimerPair { range: s, .. } => Some(*s),
            ViewSelection::Primer(_) | ViewSelection::None => None,
        }
    }

    /// The selected feature id, if a feature object is selected.
    pub fn selected_feature(&self) -> Option<FeatureId> {
        match self {
            ViewSelection::Feature { id, .. } => Some(*id),
            _ => None,
        }
    }

    /// The selected primer id, if a primer object is selected.
    pub fn selected_primer(&self) -> Option<PrimerId> {
        match self {
            ViewSelection::Primer(id) => Some(*id),
            _ => None,
        }
    }

    /// The selected `(fwd, rev)` primer ids, if a PCR primer pair is selected.
    pub fn selected_primer_pair(&self) -> Option<(PrimerId, PrimerId)> {
        match self {
            ViewSelection::PrimerPair { fwd, rev, .. } => Some((*fwd, *rev)),
            _ => None,
        }
    }

    /// The selected cut site key, if a cut site is selected.
    pub fn selected_cut_site(&self) -> Option<&CutSiteKey> {
        match self {
            ViewSelection::CutSite { key, .. } => Some(key),
            _ => None,
        }
    }

    /// True when nothing is selected.
    pub fn is_none(&self) -> bool {
        matches!(self, ViewSelection::None)
    }

    /// What a Delete/Backspace gesture means for this selection. Centralizes the
    /// dispatch so callers `match` once (and the compiler forces every variant to
    /// be handled when a new selectable noun is added). A feature/primer object
    /// reinterprets Delete as *object* deletion; everything else (`Text` /
    /// `CutSite` / `None`) falls through to the normal sequence-delete path (which
    /// reads [`ViewSelection::text_range`]).
    pub fn delete_intent(&self) -> DeleteIntent {
        match self {
            ViewSelection::Feature { id, .. } => DeleteIntent::Feature(*id),
            ViewSelection::Primer(id) => DeleteIntent::Primer(*id),
            ViewSelection::Text(_)
            | ViewSelection::CutSite { .. }
            | ViewSelection::PrimerPair { .. }
            | ViewSelection::None => DeleteIntent::Sequence,
        }
    }
}

/// The meaning of a Delete/Backspace gesture over a [`ViewSelection`] — the
/// return of [`ViewSelection::delete_intent`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeleteIntent {
    /// Delete the feature object (route to the Inspector's staged feature delete).
    Feature(FeatureId),
    /// Delete the primer object (route to the Inspector's staged primer delete).
    Primer(PrimerId),
    /// Not an object selection — the normal sequence/staging delete applies.
    Sequence,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct View {
    pub id: ViewId,
    pub buffer_id: BufferId,
    pub kind: ViewKind,
    /// The one selection (range or object), replacing the former three parallel
    /// fields. Accessors ([`ViewSelection::text_range`] /
    /// [`ViewSelection::selected_feature`] / …) derive the pieces consumers need.
    pub selection: ViewSelection,
    /// Last persisted scroll offset, restored on tab switch / app restart.
    pub scroll_pos: Option<f32>,
    /// One-shot scroll request, consumed by the viewer each frame.
    #[serde(skip)]
    pub scroll_to: Option<usize>,
    #[serde(skip)]
    pub search_hits: Vec<SearchHit>,
    #[serde(skip)]
    pub cut_sites: Vec<CutSite>,
    /// Methylation verdict per site, parallel to `cut_sites`. Derived cache
    /// (like `cut_sites`): recomputed only when the enzyme set or `methylation`
    /// changes, never per frame. Empty when `cut_sites` is empty.
    #[serde(skip)]
    pub methyl_states: Vec<MethylState>,
    #[serde(skip)]
    pub active_enzymes: Vec<String>,
    /// Which host methylation systems are active for this view's cut-site
    /// verdicts. Default Dam+Dcm on matches standard *E. coli* plasmid DNA.
    #[serde(default)]
    pub methylation: MethylContext,
    /// Visible sequence range written each frame by the text viewer.
    /// Used by the minimap to paint the viewport indicator.
    #[serde(skip)]
    pub visible_range: Option<(usize, usize)>,
    /// `buffer.version` at which `cut_sites` / `methyl_states` were scanned.
    /// A mismatch with the live version means the overlay is stale (bytes moved
    /// under it); `rescan_if_stale` re-derives. `None` = never scanned.
    #[serde(skip)]
    pub results_version: Option<u64>,
    /// `buffer.version` at which `search_hits` were found. A mismatch means the
    /// highlights are stale; the query is not stored, so the stale response is to
    /// clear (an edit invalidates a search). `None` = no active search.
    #[serde(skip)]
    pub search_version: Option<u64>,
}

impl View {
    pub fn new(id: ViewId, buffer_id: BufferId, kind: ViewKind) -> Self {
        Self {
            id,
            buffer_id,
            kind,
            // Fresh views open with a live caret so typing / paste work
            // immediately (including empty buffers). Restored file state
            // may overwrite this; `clear_selection` still clears to None.
            selection: ViewSelection::Text(Selection::cursor(0)),
            scroll_pos: None,
            scroll_to: None,
            search_hits: Vec::new(),
            cut_sites: Vec::new(),
            methyl_states: Vec::new(),
            active_enzymes: Vec::new(),
            methylation: MethylContext::default(),
            visible_range: None,
            results_version: None,
            search_version: None,
        }
    }

    /// Whether `cut_sites` / `methyl_states` must be re-derived for `version` —
    /// there is an active enzyme set and its scan stamp is behind the live buffer
    /// version. Keyed on `active_enzymes` (the scan *config*), not `cut_sites` (its
    /// *output*): an edit can introduce a site where the last scan found none, so
    /// an empty result is not proof of freshness. Consumers call this before
    /// reading; `rescan_if_stale` acts on it.
    pub fn cut_sites_stale(&self, version: u64) -> bool {
        !self.active_enzymes.is_empty() && self.results_version != Some(version)
    }

    /// Whether `search_hits` are stale for `version`. The query is not retained,
    /// so the stale response is to clear rather than recompute.
    pub fn search_stale(&self, version: u64) -> bool {
        !self.search_hits.is_empty() && self.search_version != Some(version)
    }

    /// Reset selection state. Used when reloading the same view onto a
    /// different buffer, or when explicit user action (e.g. Close) demands.
    pub fn clear_selection(&mut self) {
        self.selection = ViewSelection::None;
    }

    /// Drop all derived results — a **hard reset** for when the view's identity
    /// changes (Close / Revert / reload onto a different buffer). Not the
    /// edit-time invalidator: in-place edits keep `active_enzymes` and re-derive
    /// via the freshness stamp + [`View::cut_sites_stale`] / `rescan_if_stale`.
    pub fn clear_results(&mut self) {
        self.search_hits.clear();
        self.cut_sites.clear();
        self.methyl_states.clear();
        self.active_enzymes.clear();
        self.results_version = None;
        self.search_version = None;
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::span::Pieces;

    // ── Selection: wrap-aware region projection + circular focus stepping ──────

    #[test]
    fn clamp_to_len_empty_and_past_eof() {
        assert_eq!(Selection::cursor(105).clamp_to_len(0), Selection::cursor(0));
        assert_eq!(Selection::range(3, 9).clamp_to_len(0), Selection::cursor(0));
        assert_eq!(Selection::cursor(105).clamp_to_len(8), Selection::cursor(8));
        assert_eq!(
            Selection::range(3, 20).clamp_to_len(8),
            Selection {
                anchor: 3,
                focus: 8,
                wrap: false
            }
        );
        // In-range caret is unchanged.
        assert_eq!(Selection::cursor(4).clamp_to_len(8), Selection::cursor(4));
    }

    #[test]
    fn to_span_non_wrapping_is_the_ordered_arc() {
        // Direction-independent: a forward and a backward drag over 3..9 agree.
        assert_eq!(Selection::range(3, 9).to_span(20), Span::from_range(3..9));
        assert_eq!(Selection::range(9, 3).to_span(20), Span::from_range(3..9));
        // A cursor is the empty span.
        assert_eq!(Selection::cursor(5).to_span(20).len, 0);
    }

    #[test]
    fn to_span_wrapping_is_the_origin_crossing_arc() {
        // anchor 16, focus 4, wrap → covers [16..20) ∪ [0..4), NOT [4..16).
        let s = Selection {
            anchor: 16,
            focus: 4,
            wrap: true,
        };
        let span = s.to_span(20);
        assert_eq!(span.linear_pieces(20), Pieces::Two(16..20, 0..4));
        assert!(span.contains(18, 20));
        assert!(span.contains(2, 20));
        assert!(!span.contains(10, 20));
    }

    #[test]
    fn from_span_round_trips_wrapping_and_full() {
        // Wrapping arc (pUC19 ori shape): from_span → wrap selection → to_span
        // recovers the same span (the two arms), NOT the whole molecule.
        let ori = Span::new(16, 8); // [16..20) ∪ [0..4) on L=20
        let sel = Selection::from_span(ori, 20);
        assert!(sel.wrap);
        assert_eq!(sel.to_span(20), ori);
        assert_eq!(sel.to_span(20).linear_pieces(20), Pieces::Two(16..20, 0..4));
        // A whole-molecule span is a full-circle selection, not an empty cursor.
        let full = Selection::from_span(Span::full(20), 20);
        assert!(full.wrap && !full.is_cursor());
        assert_eq!(full.to_span(20), Span::full(20));
    }

    #[test]
    fn to_span_wrapping_full_circle_when_anchor_meets_focus() {
        let s = Selection {
            anchor: 7,
            focus: 7,
            wrap: true,
        };
        assert_eq!(s.to_span(20), Span::full(20));
        assert!(
            !s.is_cursor(),
            "anchor==focus with wrap is a full ring, not a cursor"
        );
    }

    #[test]
    fn step_focus_no_cross_keeps_wrap() {
        assert_eq!(step_focus_circular(100, false, 1, 2686), (101, false));
        assert_eq!(step_focus_circular(100, false, -1, 2686), (99, false));
        assert_eq!(step_focus_circular(100, true, 5, 2686), (105, true));
    }

    #[test]
    fn step_focus_rightward_across_origin_toggles_wrap() {
        assert_eq!(step_focus_circular(2685, false, 1, 2686), (0, true));
        assert_eq!(step_focus_circular(2685, true, 1, 2686), (0, false));
    }

    #[test]
    fn step_focus_leftward_across_origin_toggles_wrap() {
        assert_eq!(step_focus_circular(0, false, -1, 2686), (2685, true));
        assert_eq!(step_focus_circular(0, true, -1, 2686), (2685, false));
    }

    #[test]
    fn step_focus_line_jump_across_origin() {
        assert_eq!(step_focus_circular(2680, false, 60, 2686), (54, true));
        assert_eq!(step_focus_circular(30, false, -60, 2686), (2656, true));
    }

    #[test]
    fn step_focus_degenerate_length() {
        assert_eq!(step_focus_circular(0, false, 1, 0), (0, false));
    }

    #[test]
    fn move_focus_circular_extends_selection_through_origin() {
        let sel = Selection::cursor(2685);
        let sel = sel.move_focus_circular(1, 2686); // focus → 0, wrap on
        assert!(sel.wrap);
        assert_eq!(
            sel.to_span(2686).linear_pieces(2686),
            Pieces::One(2685..2686)
        );
        let sel = sel.move_focus_circular(3, 2686); // focus → 3
        assert_eq!(
            sel.to_span(2686).linear_pieces(2686),
            Pieces::Two(2685..2686, 0..3)
        );
    }

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
        v.selection = ViewSelection::Feature {
            id: FeatureId(1),
            range: Selection::range(0, 4),
        };
        v.clear_selection();
        assert!(v.selection.is_none());
        assert!(v.selection.selected_feature().is_none());
    }

    #[test]
    fn view_selection_is_structurally_exclusive() {
        // Setting an object selection *replaces* the whole enum — a feature and a
        // primer can't both be selected (the invariant the old parallel fields
        // maintained by convention is now unrepresentable).
        let feat = ViewSelection::Feature {
            id: FeatureId(3),
            range: Selection::range(1, 5),
        };
        assert_eq!(feat.selected_feature(), Some(FeatureId(3)));
        assert_eq!(feat.selected_primer(), None);
        assert_eq!(feat.text_range(), Some(Selection::range(1, 5)));

        // A primer selection is object-only: no template range.
        let prim = ViewSelection::Primer(PrimerId(2));
        assert_eq!(prim.selected_primer(), Some(PrimerId(2)));
        assert_eq!(prim.selected_feature(), None);
        assert_eq!(prim.text_range(), None);

        // A cut site derives its recognition range.
        let cut = ViewSelection::CutSite {
            key: CutSiteKey {
                enzyme: "EcoRI".into(),
                recognition_start: 10,
            },
            range: Selection::range(10, 16),
        };
        assert_eq!(
            cut.selected_cut_site().map(|k| k.recognition_start),
            Some(10)
        );
        assert_eq!(cut.text_range(), Some(Selection::range(10, 16)));
    }

    #[test]
    fn primer_pair_carries_amplicon_range_not_a_single_primer() {
        // A PCR pair (Phase 3.1b) is bounded + ordered; its `range` is the amplicon
        // (what PCR would produce), so `text_range` drives the sequence highlight —
        // but it is *not* a single-primer object selection.
        let pair = ViewSelection::PrimerPair {
            fwd: PrimerId(1),
            rev: PrimerId(2),
            range: Selection::range(4, 26),
        };
        assert_eq!(
            pair.selected_primer_pair(),
            Some((PrimerId(1), PrimerId(2)))
        );
        assert_eq!(
            pair.selected_primer(),
            None,
            "a pair is not a single primer"
        );
        assert_eq!(pair.selected_feature(), None);
        assert_eq!(
            pair.text_range(),
            Some(Selection::range(4, 26)),
            "the amplicon range highlights what PCR produces"
        );
        // Delete over a pair falls through to the sequence path (not object-delete).
        assert_eq!(pair.delete_intent(), DeleteIntent::Sequence);
    }

    #[test]
    fn delete_intent_dispatches_by_kind() {
        let feat = ViewSelection::Feature {
            id: FeatureId(5),
            range: Selection::range(1, 5),
        };
        assert_eq!(feat.delete_intent(), DeleteIntent::Feature(FeatureId(5)));
        assert_eq!(
            ViewSelection::Primer(PrimerId(2)).delete_intent(),
            DeleteIntent::Primer(PrimerId(2))
        );
        // Text / cut-site / none all fall through to the sequence-delete path.
        assert_eq!(
            ViewSelection::Text(Selection::range(0, 3)).delete_intent(),
            DeleteIntent::Sequence
        );
        assert_eq!(ViewSelection::None.delete_intent(), DeleteIntent::Sequence);
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
    fn view_new_defaults_to_caret_at_zero() {
        let v = View::new(ViewId(1), BufferId(1), ViewKind::TextView);
        assert_eq!(
            v.selection.text_range(),
            Some(Selection::cursor(0)),
            "fresh views must have a live caret so typing/paste work immediately"
        );
        assert!(!v.selection.is_none());
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
            location: crate::Location::simple(0..3),
            raw_kind: "CDS".into(),
            label: label.into(),
            strand: crate::Strand::Forward,
            qualifiers: Default::default(),
            lineage: None,
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
            binding: Some(crate::Span::from_range(0..8)),
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
        v.selection = ViewSelection::Primer(PrimerId(1));
        v.clear_selection();
        assert!(v.selection.selected_primer().is_none());
    }

    #[test]
    fn suggest_primer_name_picks_lowest_unused() {
        let mut ann = Annotations::new(vec![]);
        assert_eq!(ann.suggest_primer_name(), "Primer 1");
        ann.add_primer(primer("Primer 1"));
        assert_eq!(ann.suggest_primer_name(), "Primer 2");
        // Gaps are filled: with 1 and 3 taken, the next is 2.
        ann.add_primer(primer("Primer 3"));
        assert_eq!(ann.suggest_primer_name(), "Primer 2");
    }

    #[test]
    fn from_parts_auto_names_unnamed_primers_uniquely() {
        // Two unnamed imports get distinct defaults; a named one is untouched.
        let unnamed = || Primer {
            name: String::new(),
            ..primer("x")
        };
        let ann = Annotations::from_parts(vec![], vec![unnamed(), primer("kept"), unnamed()]);
        let names: Vec<_> = ann.primers().map(|p| p.name.clone()).collect();
        assert_eq!(names, vec!["Primer 1", "kept", "Primer 2"]);
    }
}
