//! The assembly **recipe** — the serde document that *is* the provenance
//! (ROADMAP decisions 21 + 25). A recipe is *n* **bins**; each bin is a set of
//! sources plus a **prepare op** that turns them into fragments; a **join** verb
//! combines the prepared fragments into products. GUI gesture, CLI text, and this
//! JSON are three faces of the same value — parity by construction.
//!
//! Prepare is one ordered **5′→3′** span (decision 26): walk the template from
//! the fragment's 5′ end to its 3′ end; bin index is join order. No separate
//! keep/select dialect.

use std::path::PathBuf;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::model::BufferId;

/// A full assembly recipe: bins + the join method + intended topology.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Recipe {
    pub bins: Vec<Bin>,
    pub join: JoinKind,
    #[serde(default)]
    pub intent: TopologyIntent,
    #[serde(default)]
    pub expand: Expand,
    /// Product name template (`{0}`, `{1}` = bin roles); `None` = auto.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name_template: Option<String>,
}

impl Recipe {
    /// A blank recipe with `n` empty bins (default prepare = empty Digest span).
    pub fn with_bins(n: usize) -> Self {
        Recipe {
            bins: (0..n).map(Bin::empty).collect(),
            join: JoinKind::Ligate,
            intent: TopologyIntent::Circular,
            expand: Expand::AllToAll,
            name_template: None,
        }
    }
}

/// One bin: a labeled tray of sources + how to derive fragments from them. The
/// `role` is a UX label only — the join is **role-blind** (decision 25).
///
/// Authoring is **batch-first** (decision 26): `prepare` is one ordered 5′→3′
/// span, inherited by every source. A per-input [`Source::span`] override is the
/// exception (e.g. `@pos` when a cutter is ambiguous).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bin {
    pub role: String,
    pub sources: Vec<Source>,
    pub prepare: PrepareKind,
}

impl Bin {
    pub fn empty(i: usize) -> Self {
        Bin {
            role: default_role(i),
            sources: Vec::new(),
            prepare: PrepareKind::Digest {
                five_prime: Boundary::EnzymeSite {
                    enzyme: String::new(),
                    at: None,
                },
                three_prime: Boundary::EnzymeSite {
                    enzyme: String::new(),
                    at: None,
                },
            },
        }
    }
}

/// Conventional role label for the `i`-th bin (vector first, then inserts).
pub fn default_role(i: usize) -> String {
    if i == 0 {
        "Vector".to_string()
    } else {
        format!("Insert {i}")
    }
}

/// A recipe input. `pin` is a stable content hash (FNV-1a of the resolved bytes)
/// recorded at authoring time so a replay can warn on input drift.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Source {
    #[serde(flatten)]
    pub ref_: SourceRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pin: Option<u64>,
    /// Per-input 5′→3′ **override** (decision 26). When present it wins over
    /// the bin's [`PrepareKind::Digest`] span for *this* source only (e.g.
    /// pinning `EcoRI@410` when the enzyme cuts >1×).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub span: Option<SpanEnds>,
}

/// An ordered 5′→3′ pair — the single fragment-choice language (CLI `A..B`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpanEnds {
    pub five_prime: Boundary,
    pub three_prime: Boundary,
}

impl SpanEnds {
    pub fn new(five_prime: Boundary, three_prime: Boundary) -> Self {
        SpanEnds {
            five_prime,
            three_prime,
        }
    }

    /// Swap 5′ and 3′ (GUI ⇄ / CLI rewrite of `B..A`).
    pub fn flipped(self) -> Self {
        SpanEnds {
            five_prime: self.three_prime,
            three_prime: self.five_prime,
        }
    }
}

impl std::fmt::Display for SpanEnds {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}..{}", self.five_prime, self.three_prime)
    }
}

impl FromStr for SpanEnds {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        let (a, b) = s
            .split_once("..")
            .ok_or_else(|| format!("bad span {s:?} (want 5′..3′)"))?;
        Ok(SpanEnds {
            five_prime: a.parse()?,
            three_prime: b.parse()?,
        })
    }
}

/// Where a source's bytes come from: a live open buffer (by handle) or a file at
/// rest (by path — referenced, not opened).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "ref")]
pub enum SourceRef {
    Buffer(BufferId),
    Path(PathBuf),
}

/// The per-bin **prepare op**: sources → assemblable fragments.
///
/// Digest and PCR are both ordered 5′→3′ spans. Enzymes for a digest are derived
/// from [`Boundary::EnzymeSite`] endpoints.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "op")]
pub enum PrepareKind {
    /// Restriction digest; keep the fragment bounded by 5′ → 3′.
    Digest {
        five_prime: Boundary,
        three_prime: Boundary,
    },
    /// PCR amplification between two primers, named on the source (durable —
    /// primer *ids* are session-scoped, so we key on name). `fwd` = 5′ of the
    /// amplicon sense, `rev` = 3′.
    Pcr { fwd: String, rev: String },
    /// The source is already a fragment; pass it through with its native ends.
    AsIs,
}

impl PrepareKind {
    /// The Digest 5′→3′ span, if this prepare is a digest.
    pub fn digest_span(&self) -> Option<SpanEnds> {
        match self {
            PrepareKind::Digest {
                five_prime,
                three_prime,
            } => Some(SpanEnds::new(five_prime.clone(), three_prime.clone())),
            _ => None,
        }
    }

    /// Enzyme names named by Digest endpoints (deduped, order preserved).
    pub fn digest_enzymes(&self) -> Vec<String> {
        let PrepareKind::Digest {
            five_prime,
            three_prime,
        } = self
        else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for b in [five_prime, three_prime] {
            if let Boundary::EnzymeSite { enzyme, .. } = b {
                if !enzyme.is_empty() && !out.iter().any(|e| e == enzyme) {
                    out.push(enzyme.clone());
                }
            }
        }
        out
    }
}

/// The per-recipe **join** verb — the only method-specific code.
///
/// `GoldenGate` shares the overhang end-matching mechanics with `Ligate` (the
/// join engine is one helper); the distinct variant records the reaction's
/// author intent and its Type IIS enzyme (Workbench / CLI may preselect that
/// enzyme's Pryor fidelity table for the informational combo %). It is **not**
/// `Copy` — `GoldenGate` carries an owned enzyme name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JoinKind {
    Ligate,
    GoldenGate { enzyme: String },
}

/// Intended product topology; filters the derived-topology product set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TopologyIntent {
    #[default]
    Circular,
    Linear,
    Any,
}

/// How candidate fragments combine across bins.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Expand {
    /// Cartesian product (one fragment per bin); count = ∏|binᵢ|.
    #[default]
    AllToAll,
    /// Positional pairing (all bins must hold the same count).
    Zip,
}

// ── Boundaries — polymorphic 5′/3′ endpoints (decision 26) ────────────────────

/// Which edge of a feature a [`Boundary::FeatureEdge`] refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FeatureSide {
    Five,
    Three,
}

impl std::fmt::Display for FeatureSide {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FeatureSide::Five => f.write_str("5"),
            FeatureSide::Three => f.write_str("3"),
        }
    }
}

impl FromStr for FeatureSide {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s.trim().trim_end_matches('\'') {
            "5" | "five" => Ok(FeatureSide::Five),
            "3" | "three" => Ok(FeatureSide::Three),
            other => Err(format!("bad feature side: {other:?} (want 5 or 3)")),
        }
    }
}

/// One endpoint of a fragment (decision 26): a fragment is bounded by **exactly
/// two** boundaries (5′ and 3′). Polymorphic so a boundary can be an enzyme cut
/// (with an optional occurrence tiebreaker), a raw coordinate, a feature edge,
/// or a free molecule terminus. Its `Display`/`FromStr` is the endpoint text
/// grammar used inside `5′..3′` CLI tokens.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Boundary {
    /// An enzyme cut. `at` (a cut position) is the **occurrence tiebreaker**:
    /// implicit for a unique cutter; required when that endpoint enzyme cuts >1×
    /// and the default two-site same-enzyme walk does not apply.
    EnzymeSite {
        enzyme: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        at: Option<usize>,
    },
    /// A raw coordinate — the universal escape hatch.
    Coordinate(usize),
    /// A feature edge — semantic and edit-stable (reserved; not a keep dialect).
    FeatureEdge { feature: String, side: FeatureSide },
    /// A free molecule terminus (PCR product / linear part end).
    Terminus,
}

impl Boundary {
    pub fn enzyme(name: impl Into<String>) -> Self {
        Boundary::EnzymeSite {
            enzyme: name.into(),
            at: None,
        }
    }

    pub fn enzyme_at(name: impl Into<String>, at: usize) -> Self {
        Boundary::EnzymeSite {
            enzyme: name.into(),
            at: Some(at),
        }
    }
}

impl std::fmt::Display for Boundary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Boundary::EnzymeSite { enzyme, at: None } => write!(f, "{enzyme}"),
            Boundary::EnzymeSite {
                enzyme,
                at: Some(p),
            } => write!(f, "{enzyme}@{p}"),
            Boundary::Coordinate(n) => write!(f, "{n}"),
            Boundary::FeatureEdge { feature, side } => write!(f, "{feature}^{side}"),
            Boundary::Terminus => f.write_str("*"),
        }
    }
}

impl FromStr for Boundary {
    type Err = String;
    /// Parses one endpoint token: `EcoRI` · `EcoRI@1201` · `100` · `ori^5` · `*`.
    fn from_str(s: &str) -> Result<Self, String> {
        let s = s.trim();
        if s.is_empty() {
            return Err("empty boundary".into());
        }
        if s == "*" {
            return Ok(Boundary::Terminus);
        }
        if let Some((feat, side)) = s.split_once('^') {
            return Ok(Boundary::FeatureEdge {
                feature: feat.trim().to_string(),
                side: side.parse()?,
            });
        }
        if let Some((enzyme, at)) = s.split_once('@') {
            let pos = at
                .trim()
                .parse::<usize>()
                .map_err(|_| format!("bad occurrence in {s:?}"))?;
            return Ok(Boundary::EnzymeSite {
                enzyme: enzyme.trim().to_string(),
                at: Some(pos),
            });
        }
        if let Ok(n) = s.parse::<usize>() {
            return Ok(Boundary::Coordinate(n));
        }
        Ok(Boundary::EnzymeSite {
            enzyme: s.to_string(),
            at: None,
        })
    }
}

/// Stable content hash (FNV-1a, deterministic across processes — unlike
/// `DefaultHasher`) for pinning a source's bytes. Used to warn on input drift.
pub fn content_pin(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enzyme(name: &str) -> Boundary {
        Boundary::enzyme(name)
    }

    #[test]
    fn recipe_serde_round_trips() {
        let mut r = Recipe::with_bins(2);
        r.bins[0].sources.push(Source {
            ref_: SourceRef::Path(PathBuf::from("pUC19.gb")),
            pin: Some(content_pin(b"ACGT")),
            span: None,
        });
        r.bins[0].prepare = PrepareKind::Digest {
            five_prime: enzyme("EcoRI"),
            three_prime: enzyme("BamHI"),
        };
        r.bins[1].sources.push(Source {
            ref_: SourceRef::Buffer(BufferId(3)),
            pin: None,
            span: Some(SpanEnds::new(
                Boundary::enzyme_at("BsaI", 410),
                Boundary::Terminus,
            )),
        });
        r.bins[1].prepare = PrepareKind::Pcr {
            fwd: "f1".into(),
            rev: "r1".into(),
        };
        r.join = JoinKind::GoldenGate {
            enzyme: "BsaI".into(),
        };
        let json = serde_json::to_string(&r).unwrap();
        let back: Recipe = serde_json::from_str(&json).unwrap();
        assert_eq!(back, r);
    }

    #[test]
    fn span_ends_grammar_round_trips() {
        let cases = [
            (
                "EcoRI..BamHI",
                SpanEnds::new(enzyme("EcoRI"), enzyme("BamHI")),
            ),
            (
                "EcoRI@1201..BamHI",
                SpanEnds::new(Boundary::enzyme_at("EcoRI", 1201), enzyme("BamHI")),
            ),
            (
                "100..*",
                SpanEnds::new(Boundary::Coordinate(100), Boundary::Terminus),
            ),
            (
                "BamHI@1201..EcoRI",
                SpanEnds::new(Boundary::enzyme_at("BamHI", 1201), enzyme("EcoRI")),
            ),
        ];
        for (text, want) in cases {
            let span: SpanEnds = text.parse().unwrap();
            assert_eq!(span, want, "parsing {text:?}");
            assert_eq!(span.to_string(), text, "display of {text:?}");
        }
    }

    #[test]
    fn digest_enzymes_dedupes_same_enzyme_twice() {
        let p = PrepareKind::Digest {
            five_prime: enzyme("BsaI"),
            three_prime: enzyme("BsaI"),
        };
        assert_eq!(p.digest_enzymes(), vec!["BsaI".to_string()]);
    }

    #[test]
    fn feature_edge_boundary_round_trips() {
        for text in ["ori^5", "ori^3"] {
            let b: Boundary = text.parse().unwrap();
            assert_eq!(b.to_string(), text);
        }
    }

    #[test]
    fn content_pin_is_deterministic() {
        assert_eq!(content_pin(b"ACGTACGT"), content_pin(b"ACGTACGT"));
        assert_ne!(content_pin(b"ACGTACGT"), content_pin(b"ACGTACGA"));
    }
}
