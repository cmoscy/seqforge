use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::ops::Range;
use std::path::PathBuf;

use crate::span::Span;

// ── Result types (computed from a Document) ───────────────────────────────────

/// A pattern match hit — 0-based half-open range, indicates which strand was matched.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchHit {
    pub start: usize,
    pub end: usize,
    pub strand: Strand,
}

/// Which host methylation systems are active on the molecule being viewed.
/// Authored/persisted per-view (default Dam+Dcm on = standard *E. coli*
/// plasmid prep); passed to the evaluator when deriving cut-site verdicts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MethylContext {
    pub dam: bool,
    pub dcm: bool,
    pub cpg: bool,
}

impl Default for MethylContext {
    fn default() -> Self {
        MethylContext {
            dam: true,
            dcm: true,
            cpg: false,
        }
    }
}

/// Two-factor verdict for one cut site under a methylation context. Variants are
/// ordered by severity (`Cuttable < Impaired < Blocked`) so the worst state
/// across systems/sites is `.max()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Serialize, Deserialize)]
pub enum MethylState {
    #[default]
    Cuttable,
    Impaired,
    Blocked,
}

/// A restriction enzyme recognition site found in the sequence.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CutSite {
    pub enzyme: String,
    /// IUPAC recognition pattern of the enzyme (e.g. `"GGTCTC"`). Display-only;
    /// the canonical pattern, not the concrete bases at this site.
    pub recognition: String,
    /// 0-based start of the recognition sequence.
    pub recognition_start: usize,
    /// 0-based exclusive end of the recognition sequence.
    pub recognition_end: usize,
    /// Inter-base position of the top-strand cut (between bases `cut_pos-1` and `cut_pos`).
    pub cut_pos: usize,
    /// Inter-base position of the bottom-strand cut — derived from palindrome symmetry.
    /// Equal to `cut_pos` for blunt-end enzymes. Greater than `cut_pos` for 5' overhangs,
    /// less than `cut_pos` for 3' overhangs.
    pub bottom_cut_pos: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Topology {
    Linear,
    Circular,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Strand {
    Forward,
    Reverse,
    Both,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FeatureKind {
    Gene,
    Cds,
    Promoter,
    Terminator,
    Rep,
    Misc,
    Source,
    Other,
}

impl FeatureKind {
    /// Classify a verbatim GenBank feature-type string into a display kind.
    ///
    /// The authoritative type is `Feature.raw_kind` (the exact string from
    /// the file, e.g. `"CDS"`, `"rep_origin"`); this derives the coloring/
    /// display variant on the fly so no information is lost on round-trip.
    pub fn classify(raw_kind: &str) -> FeatureKind {
        match raw_kind {
            "gene" => FeatureKind::Gene,
            "CDS" => FeatureKind::Cds,
            "promoter" => FeatureKind::Promoter,
            "terminator" => FeatureKind::Terminator,
            "rep_origin" => FeatureKind::Rep,
            "source" => FeatureKind::Source,
            "misc_feature" | "misc_binding" => FeatureKind::Misc,
            _ => FeatureKind::Other,
        }
    }
}

/// Session-scoped stable handle for a [`Feature`].
///
/// Ids are minted by [`crate::Annotations`] (a per-instance monotonic
/// counter) and are **never persisted**: `Feature.id` is `#[serde(skip)]`,
/// so on load every feature deserializes with the [`Default`] placeholder
/// (`FeatureId(0)`) and `Annotations` re-mints a fresh id for each. GenBank /
/// FASTA therefore stay positional; ids live only for the life of the
/// process. This is what makes the stale-index bug class unrepresentable —
/// features are addressed by id, never by their position in the `Vec`. See
/// ROADMAP decision 12.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default, Serialize, Deserialize,
)]
pub struct FeatureId(pub u64);

impl std::fmt::Display for FeatureId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Parse a bare numeric id (`"42"`), for clap's `--id` flag and JSON-number
/// socket clients.
impl std::str::FromStr for FeatureId {
    type Err = std::num::ParseIntError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        s.parse::<u64>().map(FeatureId)
    }
}

/// Session-scoped stable handle for a [`Primer`].
///
/// A distinct newtype from [`FeatureId`] — primers and features are separate
/// collections with separate id counters — but the id-at-rest rule is identical
/// (ROADMAP decision 12/14): minted by [`crate::Annotations`], **never
/// persisted** (`Primer.id` is `#[serde(skip)]`), re-minted on load. GenBank
/// (`primer_bind`) and FASTA therefore stay positional; ids live only for the
/// life of the process.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default, Serialize, Deserialize,
)]
pub struct PrimerId(pub u64);

impl std::fmt::Display for PrimerId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Parse a bare numeric id (`"42"`), for clap's `--id` flag and JSON-number
/// socket clients.
impl std::str::FromStr for PrimerId {
    type Err = std::num::ParseIntError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        s.parse::<u64>().map(PrimerId)
    }
}

/// Lineage of a feature across edits / cloning operations. Round-trips
/// through GenBank as a single JSON-valued `/seqforge_provenance` qualifier
/// so it survives save/reload without committing to any cloning shape now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Provenance {
    pub source_doc: String,
    pub source_range: Range<usize>,
    pub operation: String,
}

/// GenBank-native feature geometry — a core-owned mirror of
/// `gb_io::seq::Location` (core cannot depend on the parser crate). Recursive so
/// it captures the full location grammar losslessly instead of flattening to a
/// bounding range on import (ROADMAP decision 23; `plans/feature-model.md`).
///
/// - [`Location::Simple`] carries `before`/`after` = GenBank `<`/`>` fuzzy
///   (partial/truncated) markers.
/// - [`Location::Join`] is a compound location (SnapGene "segments" — a spliced
///   CDS, an origin-spanning feature on a circular molecule).
/// - [`Location::Complement`] is retained for round-trip fidelity and future
///   trans-splicing; in practice a feature's overall strand lives in
///   [`Feature::strand`] (the authoritative field), and the GenBank mapping
///   normalizes the outer complement into it. A **mixed-strand** `Join` (inner
///   complements) is stubbed to single-strand for now.
///
/// The bounding hull is the **derived** [`Location::span`] — never stored
/// (decision 8): the span is a pure function of the location, so storing it
/// would denormalize with a sync invariant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Location {
    /// A single contiguous region ([`Span`], which may wrap the origin on a
    /// circular molecule), optionally fuzzy at either end (`before` = `<start`,
    /// `after` = `>end`).
    Simple {
        span: Span,
        before: bool,
        after: bool,
    },
    /// Compound location: ordered **spliced** segments (introns between them).
    /// Origin-wrap is a wrapping [`Span`] on a `Simple`, **not** a `Join`.
    Join(Vec<Location>),
    /// Strand wrapper — retained for fidelity/trans-splicing.
    Complement(Box<Location>),
}

impl Location {
    /// A crisp (non-fuzzy) single-region location — the common construction.
    pub fn simple(range: Range<usize>) -> Self {
        Location::Simple {
            span: Span::from_range(range),
            before: false,
            after: false,
        }
    }

    /// A crisp single-region location from a [`Span`] (may wrap the origin).
    pub fn from_span(span: Span) -> Self {
        Location::Simple {
            span,
            before: false,
            after: false,
        }
    }

    /// Every linear half-open run this location occupies on a molecule of length
    /// `len`, origin-split, in leaf order — **the** render / highlight / copy
    /// geometry primitive that both the main viewer and the minimap derive from
    /// (so they cannot drift). A plain `Simple` → one run; a wrapping `Simple` or
    /// a `Join` → several.
    pub fn pieces(&self, len: usize) -> Vec<Range<usize>> {
        let mut out = Vec::new();
        self.collect_pieces(len, &mut out);
        out
    }

    fn collect_pieces(&self, len: usize, out: &mut Vec<Range<usize>>) {
        match self {
            Location::Simple { span, .. } => out.extend(span.linear_pieces(len).iter()),
            Location::Complement(inner) => inner.collect_pieces(len, out),
            Location::Join(parts) => {
                for p in parts {
                    p.collect_pieces(len, out);
                }
            }
        }
    }

    /// Whether base index `pos` falls inside this location (any segment).
    pub fn contains(&self, pos: usize, len: usize) -> bool {
        match self {
            Location::Simple { span, .. } => span.contains(pos, len),
            Location::Complement(inner) => inner.contains(pos, len),
            Location::Join(parts) => parts.iter().any(|p| p.contains(pos, len)),
        }
    }

    /// Linear bounding hull `[min, max)` — the **explicit, opt-in** accessor for
    /// genuine bounds-only consumers (interval stacking / LOD). Lossy for a
    /// wrapping location (returns `0..len`). Prefer [`Location::pieces`] /
    /// [`Location::contains`] everywhere else.
    pub fn hull(&self, len: usize) -> Range<usize> {
        let pieces = self.pieces(len);
        match (
            pieces.iter().map(|r| r.start).min(),
            pieces.iter().map(|r| r.end).max(),
        ) {
            (Some(s), Some(e)) => s..e,
            _ => 0..0,
        }
    }

    /// DEPRECATED len-free hull shim — kept only during the Span migration (P1)
    /// so `.span()` call sites keep compiling. Correct **only while no `Simple`
    /// wraps** (true until GenBank origin-join normalization lands in P2); a
    /// wrapping `Simple` would report `start..start+len` past the molecule end.
    /// Migrate callers to [`Location::hull`] / [`Location::contains`] /
    /// [`Location::pieces`], then delete this.
    pub fn span(&self) -> Range<usize> {
        match self {
            Location::Simple { span, .. } => span.start..span.start + span.len,
            Location::Complement(inner) => inner.span(),
            Location::Join(parts) => {
                let mut iter = parts.iter().map(Location::span);
                match iter.next() {
                    Some(first) => {
                        iter.fold(first, |acc, r| acc.start.min(r.start)..acc.end.max(r.end))
                    }
                    None => 0..0,
                }
            }
        }
    }

    /// DEPRECATED len-free ordered-leaf-ranges shim (see [`Location::span`]).
    /// Migrate segment-aware callers to [`Location::pieces`], then delete.
    pub fn segments(&self) -> impl Iterator<Item = Range<usize>> {
        let mut out = Vec::new();
        self.collect_segments(&mut out);
        out.into_iter()
    }

    fn collect_segments(&self, out: &mut Vec<Range<usize>>) {
        match self {
            Location::Simple { span, .. } => out.push(span.start..span.start + span.len),
            Location::Complement(inner) => inner.collect_segments(out),
            Location::Join(parts) => {
                for p in parts {
                    p.collect_segments(out);
                }
            }
        }
    }

    /// Fuzzy markers at the spatial ends: `(left, right)` = the `before` (`<`) of
    /// the lowest-start leaf and the `after` (`>`) of the highest-end leaf.
    /// Drives the torn-edge render. `(false, false)` for a crisp location.
    pub fn fuzzy_ends(&self) -> (bool, bool) {
        let mut leaves: Vec<(Span, bool, bool)> = Vec::new();
        self.collect_leaves(&mut leaves);
        let left = leaves
            .iter()
            .min_by_key(|(s, _, _)| s.start)
            .map(|(_, before, _)| *before)
            .unwrap_or(false);
        let right = leaves
            .iter()
            .max_by_key(|(s, _, _)| s.start + s.len)
            .map(|(_, _, after)| *after)
            .unwrap_or(false);
        (left, right)
    }

    fn collect_leaves(&self, out: &mut Vec<(Span, bool, bool)>) {
        match self {
            Location::Simple {
                span,
                before,
                after,
            } => out.push((*span, *before, *after)),
            Location::Complement(inner) => inner.collect_leaves(out),
            Location::Join(parts) => {
                for p in parts {
                    p.collect_leaves(out);
                }
            }
        }
    }

    /// True if any leaf is fuzzy (`<`/`>`) — a torn edge to render.
    pub fn is_fuzzy(&self) -> bool {
        match self {
            Location::Simple { before, after, .. } => *before || *after,
            Location::Complement(inner) => inner.is_fuzzy(),
            Location::Join(parts) => parts.iter().any(Location::is_fuzzy),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Feature {
    /// Session-scoped handle. Never persisted (`#[serde(skip)]`); re-minted
    /// by [`crate::Annotations`] on load. See [`FeatureId`].
    #[serde(skip)]
    pub id: FeatureId,
    /// GenBank-native geometry. The bounding hull is the derived
    /// [`Feature::span`]; segment-aware code matches on [`Location`] variants.
    pub location: Location,
    /// Verbatim GenBank feature-type string (the authoritative type).
    /// Display kind is derived via [`FeatureKind::classify`].
    pub raw_kind: String,
    pub label: String,
    pub strand: Strand,
    /// `None` value encodes a flag-style qualifier (`/pseudo`, `/partial`).
    pub qualifiers: BTreeMap<String, Option<String>>,
    pub provenance: Option<Provenance>,
}

impl Feature {
    /// Linear runs this feature occupies on a molecule of `len` — the geometry
    /// primitive for render/highlight/copy. Delegates to [`Location::pieces`].
    pub fn pieces(&self, len: usize) -> Vec<Range<usize>> {
        self.location.pieces(len)
    }

    /// Whether base `pos` falls in this feature. Delegates to [`Location::contains`].
    pub fn contains(&self, pos: usize, len: usize) -> bool {
        self.location.contains(pos, len)
    }

    /// Explicit bounds-only hull (lossy on wrap). Delegates to [`Location::hull`].
    pub fn hull(&self, len: usize) -> Range<usize> {
        self.location.hull(len)
    }

    /// DEPRECATED len-free hull shim (see [`Location::span`]) — migrate to
    /// [`Feature::hull`] / [`Feature::contains`] / [`Feature::pieces`].
    pub fn span(&self) -> Range<usize> {
        self.location.span()
    }
}

/// An **authored oligo attached relationally** to the template (ROADMAP
/// decision 14; full contract in `plans/primers.md`). Unlike a [`Feature`] — a
/// labelled sub-range — a primer is an independent reagent: its `sequence` may
/// include a 5' tail with **no** template counterpart, so it cannot be modelled
/// as a positional annotation. Authored (persisted) here; its annealed/tail/
/// mismatch decomposition, Tm/GC/QC, and attachment state are **derived**
/// (decision 8 governs template *projections*, not authored annotations).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Primer {
    /// Session-scoped handle. Never persisted (`#[serde(skip)]`); re-minted
    /// by [`crate::Annotations`] on load. See [`PrimerId`].
    #[serde(skip)]
    pub id: PrimerId,
    pub name: String,
    /// Full oligo 5'→3', tail included. AUTHORED — the intrinsic identity of
    /// the reagent; may contain bases that appear nowhere in the template (a 5'
    /// tail). A reverse primer's bases are the revcomp of the top strand at
    /// `binding`.
    pub sequence: String,
    /// Last-known annealing footprint on the top strand, AUTHORED relational
    /// state (like a `Feature.range`) — but the load-bearing anchor is the **3'
    /// terminus** (where priming/extension begins), NOT the range length. Rides
    /// a primer-specific shift handler that tracks edits and **never drops** the
    /// primer: an edit destroying the 3' anchor sets `binding = None`
    /// (`Detached`), it does not delete the reagent. `None` = a detached/floating
    /// oligo. Matches a GenBank `primer_bind` location when present.
    pub binding: Option<Range<usize>>,
    /// Extension direction. `Forward` extends toward higher coordinates (3'
    /// anchor at `binding.end`); `Reverse` toward lower (3' anchor at
    /// `binding.start`).
    pub strand: Strand,
    /// Preserve extra GenBank notes; `None` value encodes a flag-style
    /// qualifier. Mirrors [`Feature::qualifiers`].
    pub qualifiers: BTreeMap<String, Option<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Document {
    pub name: String,
    pub sequence: Vec<u8>,
    pub topology: Topology,
    pub features: Vec<Feature>,
    /// Authored primers parsed from the source (GenBank `primer_bind`; decision
    /// 14). Empty for formats that carry none (FASTA). `#[serde(default)]` keeps
    /// older serialized `Document`s loadable.
    #[serde(default)]
    pub primers: Vec<Primer>,
    pub source_path: Option<PathBuf>,
}

impl Document {
    pub fn len(&self) -> usize {
        self.sequence.len()
    }

    pub fn is_empty(&self) -> bool {
        self.sequence.is_empty()
    }
}

#[cfg(test)]
mod location_tests {
    use super::*;

    #[test]
    fn span_of_simple_is_the_range() {
        assert_eq!(Location::simple(3..9).span(), 3..9);
    }

    #[test]
    fn span_of_join_is_the_hull() {
        let j = Location::Join(vec![Location::simple(30..40), Location::simple(11..20)]);
        // Hull spans min start .. max end, regardless of segment order.
        assert_eq!(j.span(), 11..40);
    }

    #[test]
    fn span_of_complement_is_inner_span() {
        let c = Location::Complement(Box::new(Location::simple(5..8)));
        assert_eq!(c.span(), 5..8);
    }

    #[test]
    fn segments_yields_ordered_leaves() {
        let j = Location::Join(vec![Location::simple(0..3), Location::simple(6..9)]);
        let segs: Vec<_> = j.segments().collect();
        assert_eq!(segs, vec![0..3, 6..9]);
    }

    #[test]
    fn fuzzy_ends_reads_spatial_bounds() {
        // before on the lowest-start leaf, after on the highest-end leaf.
        let j = Location::Join(vec![
            Location::Simple {
                span: Span::from_range(0..3),
                before: true,
                after: false,
            },
            Location::Simple {
                span: Span::from_range(6..9),
                before: false,
                after: true,
            },
        ]);
        assert_eq!(j.fuzzy_ends(), (true, true));
        assert!(j.is_fuzzy());
        assert_eq!(Location::simple(0..3).fuzzy_ends(), (false, false));
    }
}
