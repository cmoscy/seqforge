use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::ops::Range;
use std::path::PathBuf;

// ── Result types (computed from a Document) ───────────────────────────────────

/// A pattern match hit — 0-based half-open range, indicates which strand was matched.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchHit {
    pub start: usize,
    pub end: usize,
    pub strand: Strand,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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

/// Lineage of a feature across edits / cloning operations. Round-trips
/// through GenBank as a single JSON-valued `/seqforge_provenance` qualifier
/// so it survives save/reload without committing to any cloning shape now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Provenance {
    pub source_doc: String,
    pub source_range: Range<usize>,
    pub operation: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Feature {
    /// Session-scoped handle. Never persisted (`#[serde(skip)]`); re-minted
    /// by [`crate::Annotations`] on load. See [`FeatureId`].
    #[serde(skip)]
    pub id: FeatureId,
    pub range: Range<usize>,
    /// Verbatim GenBank feature-type string (the authoritative type).
    /// Display kind is derived via [`FeatureKind::classify`].
    pub raw_kind: String,
    pub label: String,
    pub strand: Strand,
    /// `None` value encodes a flag-style qualifier (`/pseudo`, `/partial`).
    pub qualifiers: BTreeMap<String, Option<String>>,
    pub provenance: Option<Provenance>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Document {
    pub name: String,
    pub sequence: Vec<u8>,
    pub topology: Topology,
    pub features: Vec<Feature>,
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
