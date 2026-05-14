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
    /// 0-based start of the recognition sequence.
    pub recognition_start: usize,
    /// 0-based exclusive end of the recognition sequence.
    pub recognition_end: usize,
    /// Inter-base position of the top-strand cut (between bases `cut_pos-1` and `cut_pos`).
    pub cut_pos: usize,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Feature {
    pub range: Range<usize>,
    pub kind: FeatureKind,
    pub label: String,
    pub strand: Strand,
    pub qualifiers: BTreeMap<String, String>,
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
