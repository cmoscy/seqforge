//! The closed assembly IR — `Fragment` and its join `End`s.
//!
//! A `Fragment` is the **transport carrier** ([`SeqSlice`] — bytes + re-homed
//! features/primers) plus two typed `End`s and a derived topology. It is an
//! **in-memory value**, computed on demand from a source + a prepare op
//! (digest today; PCR/homology later): intermediate fragments are never written
//! to a buffer (`plans/assembly.md` "Fragments are virtual", ROADMAP decision
//! 25). `type Product = Fragment` — a product **is** a fragment, so hierarchical
//! assembly falls out (closure).
//!
//! `seqforge-core` owns this type as the composition of `SeqSlice`, [`Lineage`],
//! and [`Topology`]; it carries **no** `seqforge-restriction` dependency. The
//! digest geometry lives in that zero-dep crate and is bridged here by
//! `seqforge-bio` (`digest_fragments`).

use crate::document::{Lineage, LineageOp, Topology};
use crate::transport::SeqSlice;

/// Which strand carries a sticky end's single-stranded extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverhangSide {
    FivePrime,
    ThreePrime,
}

/// One join interface of a [`Fragment`].
///
/// The overhang length is `seq.len()`. There is **no** `cut_by` field: the
/// cutting enzyme is read off the fragment's [`Lineage`] op (see
/// [`Fragment::left_cut_by`]) — one source of truth, not a second copy of
/// enzyme identity (`docs/architecture.md` "Lineage").
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum End {
    Blunt,
    Overhang {
        side: OverhangSide,
        /// The single-stranded overhang, read 5′→3′.
        seq: Vec<u8>,
    },
}

impl End {
    /// True for a free molecule terminus (no sticky end).
    pub fn is_blunt(&self) -> bool {
        matches!(self, End::Blunt)
    }

    /// Whether these two ends can ligate (the `ends_compatible` predicate).
    ///
    /// Two blunt ends always join. Two sticky ends join iff they are the **same
    /// side** (5′↔5′ / 3′↔3′) and their single-stranded overhangs are reverse
    /// complements — the annealing condition. Blunt↔sticky never join.
    /// Compatibility reads only `side`+`seq`; the cutting enzyme is irrelevant.
    pub fn compatible_with(&self, other: &End) -> bool {
        match (self, other) {
            (End::Blunt, End::Blunt) => true,
            (End::Overhang { side: sa, seq: qa }, End::Overhang { side: sb, seq: qb }) => {
                sa == sb && *qa == revcomp(qb)
            }
            _ => false,
        }
    }
}

/// Reverse complement of an uppercase ACGT byte string (ambiguity codes pass
/// through). Local to avoid a `seqforge-bio` dependency from `core`.
pub(crate) fn revcomp(s: &[u8]) -> Vec<u8> {
    s.iter()
        .rev()
        .map(|&b| match b.to_ascii_uppercase() {
            b'A' => b'T',
            b'T' => b'A',
            b'C' => b'G',
            b'G' => b'C',
            other => other,
        })
        .collect()
}

/// A virtual piece of a molecule — the currency every assembly verb reads.
#[derive(Debug, Clone)]
pub struct Fragment {
    /// The transport carrier: top-strand bytes + re-homed features/primers.
    pub slice: SeqSlice,
    /// 5′-side join interface.
    pub left: End,
    /// 3′-side join interface.
    pub right: End,
    /// Derived topology: `Circular` = an uncut whole plasmid (no free ends);
    /// any cut piece is `Linear`.
    pub topology: Topology,
    /// This fragment's slice of the composed lineage map (op = `Digest`).
    pub lineage: Lineage,
}

/// Closure: a product **is** a fragment, so a product can re-enter any verb.
pub type Product = Fragment;

impl Fragment {
    /// Top-strand bytes.
    pub fn bytes(&self) -> &[u8] {
        self.slice.bytes()
    }

    pub fn len(&self) -> usize {
        self.slice.len()
    }

    pub fn is_empty(&self) -> bool {
        self.slice.is_empty()
    }

    /// The enzyme that cut the 5′ (left) boundary, read off `lineage.op` — not a
    /// stored `End` field. `None` = a free molecule terminus (or non-digest op).
    pub fn left_cut_by(&self) -> Option<&str> {
        match &self.lineage.op {
            LineageOp::Digest { left, .. } => left.as_deref(),
            _ => None,
        }
    }

    /// The enzyme that cut the 3′ (right) boundary; see [`Fragment::left_cut_by`].
    pub fn right_cut_by(&self) -> Option<&str> {
        match &self.lineage.op {
            LineageOp::Digest { right, .. } => right.as_deref(),
            _ => None,
        }
    }

    /// Project to the serializable [`FragmentInfo`](crate::commands::FragmentInfo)
    /// display shape shared by the Fragments view and the CLI/agent (so they
    /// cannot drift — the `PrimerInfo` pattern).
    pub fn to_info(&self, index: usize) -> crate::commands::FragmentInfo {
        crate::commands::FragmentInfo {
            index,
            length: self.len(),
            topology: self.topology,
            left: end_info(&self.left, self.left_cut_by()),
            right: end_info(&self.right, self.right_cut_by()),
            source_span: crate::Span::from_range(self.lineage.source_range.clone()),
        }
    }
}

fn end_info(end: &End, cut_by: Option<&str>) -> crate::commands::EndInfo {
    let (kind, seq) = match end {
        End::Blunt => ("blunt".to_string(), String::new()),
        End::Overhang { side, seq } => {
            let k = match side {
                OverhangSide::FivePrime => "5'",
                OverhangSide::ThreePrime => "3'",
            };
            (k.to_string(), String::from_utf8_lossy(seq).into_owned())
        }
    };
    crate::commands::EndInfo {
        kind,
        seq,
        cut_by: cut_by.map(str::to_string),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frag(op: LineageOp) -> Fragment {
        Fragment {
            slice: SeqSlice {
                bytes: b"ACGTACGT".to_vec(),
                features: Vec::new(),
                primers: Vec::new(),
            },
            left: End::Overhang {
                side: OverhangSide::FivePrime,
                seq: b"AATT".to_vec(),
            },
            right: End::Blunt,
            topology: Topology::Linear,
            lineage: Lineage {
                source_doc: "src".into(),
                source_range: 0..8,
                op,
            },
        }
    }

    #[test]
    fn cut_by_reads_off_lineage_op() {
        let f = frag(LineageOp::Digest {
            left: Some("EcoRI".into()),
            right: None,
        });
        assert_eq!(f.left_cut_by(), Some("EcoRI"));
        assert_eq!(f.right_cut_by(), None);
        assert_eq!(f.bytes(), b"ACGTACGT");
        assert_eq!(f.len(), 8);
    }

    #[test]
    fn cut_by_is_none_for_non_digest_op() {
        let f = frag(LineageOp::Extract);
        assert_eq!(f.left_cut_by(), None);
        assert_eq!(f.right_cut_by(), None);
    }

    #[test]
    fn ends_ligate_when_overhangs_are_reverse_complements() {
        let five = |s: &[u8]| End::Overhang {
            side: OverhangSide::FivePrime,
            seq: s.to_vec(),
        };
        let three = |s: &[u8]| End::Overhang {
            side: OverhangSide::ThreePrime,
            seq: s.to_vec(),
        };
        // Blunt ↔ blunt.
        assert!(End::Blunt.compatible_with(&End::Blunt));
        // 5' AATT is self-complementary (palindrome) → compatible with itself.
        assert!(five(b"AATT").compatible_with(&five(b"AATT")));
        // Non-palindromic: AGGT ligates to its revcomp ACCT, not to itself.
        assert!(five(b"AGGT").compatible_with(&five(b"ACCT")));
        assert!(!five(b"AGGT").compatible_with(&five(b"AGGT")));
        // Side must match (5' never anneals to 3') and blunt never to sticky.
        assert!(!five(b"AATT").compatible_with(&three(b"AATT")));
        assert!(!End::Blunt.compatible_with(&five(b"AATT")));
        // 3' overhangs follow the same revcomp rule.
        assert!(three(b"TGCA").compatible_with(&three(b"TGCA"))); // palindrome
    }

    #[test]
    fn digest_op_serde_round_trips_as_snake_case() {
        let op = LineageOp::Digest {
            left: Some("BsaI".into()),
            right: Some("EcoRI".into()),
        };
        let json = serde_json::to_string(&op).unwrap();
        assert!(json.contains("\"digest\""), "got {json}");
        let back: LineageOp = serde_json::from_str(&json).unwrap();
        assert_eq!(back, op);
    }
}
