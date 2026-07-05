//! Primer ↔ template decomposition (Phase 1.1). Aligns an authored oligo to the
//! template at its stored binding, **3'-anchored**, and classifies each position
//! as annealed (match / mismatch) or 5' tail. This is the derived interpretation
//! the [`PrimerTrack`](../../seqforge-app) renders: per-column bases + mismatch
//! marks + the off-grid tail.
//!
//! ## Orientation (the seqfold-plan footgun)
//!
//! A primer anneals **antiparallel** to the strand it binds:
//! - A **forward** primer's oligo (5'→3') runs with the top strand; its 3'
//!   terminus is at `binding.end`. Annealed base = oligo base; it matches when
//!   `oligo == top[column]`.
//! - A **reverse** primer's oligo is the reverse-complement of the top strand;
//!   its 3' terminus is at `binding.start`. It matches when
//!   `oligo == complement(top[column])`.
//!
//! The stored binding length is **not** trusted (the shift handler can grow or
//! shrink it independently of the fixed oligo — decision 14): the annealed run
//! is anchored at the 3' terminus and is `min(oligo_len, footprint)` long.

mod anneal;
mod design;
mod evaluate;

use std::ops::Range;

use seqforge_core::Strand;

use crate::dna::complement_byte;

pub use anneal::{
    AnnealSettings, AttachmentState, PrimerAttachment, PrimerBinding, classify_attachment,
    find_primer_binding_sites,
};
pub use design::{DesignError, EnzymeSpec, enzyme_catalog, enzyme_cuts, restriction_tail};
pub use evaluate::{
    PrimerQc, PrimerQcPlusAnneal, anneal_tm, primer_infos, primer_qc, primer_qc_with_anneal,
};

/// One template column covered by a primer's annealed footprint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnnealedBase {
    /// Template column (0-based, top-strand coordinate).
    pub column: usize,
    /// The oligo base annealing here (5'→3' oligo char, upper-case).
    pub base: u8,
    /// True when the oligo base correctly pairs with the template here.
    pub matches: bool,
}

/// A primer's alignment to the template at its stored binding (3'-anchored).
/// **Derived, never stored** — the interpretation of an authored primer against
/// the current template (decision 8 scopes to projections; this is one).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PrimerDecomposition {
    /// Per-column annealed bases, ascending by template column.
    pub annealed: Vec<AnnealedBase>,
    /// 5' tail bases (oligo bases with no template column), in oligo 5'→3'
    /// order. Empty when the oligo fits within its footprint.
    pub tail: Vec<u8>,
    /// Count of mismatched annealed columns (0 = a clean anneal).
    pub mismatches: usize,
}

/// Decompose `oligo` against `template` at `binding` on the given `strand`.
///
/// Returns an empty decomposition for a degenerate input (empty oligo, or a
/// binding whose start is past the clamped template end).
pub fn decompose_primer(
    oligo: &str,
    binding: &Range<usize>,
    strand: Strand,
    template: &[u8],
) -> PrimerDecomposition {
    let oligo: Vec<u8> = oligo.bytes().map(|b| b.to_ascii_uppercase()).collect();
    let l = oligo.len();
    let start = binding.start;
    let end = binding.end.min(template.len());
    if l == 0 || start >= end {
        return PrimerDecomposition::default();
    }
    let footprint = end - start;
    // 3'-anchored: cover the last `annealed_count` oligo bases; the rest is tail.
    let annealed_count = l.min(footprint);
    let tail_len = l - annealed_count;
    let reverse = matches!(strand, Strand::Reverse);

    let mut annealed = Vec::with_capacity(annealed_count);
    let mut mismatches = 0;
    for k in 0..annealed_count {
        // Column walks 3'→ inward, so ascending template column either way.
        let (column, oligo_i) = if reverse {
            // Reverse 3' terminus is at `start`; oligo runs antiparallel, so the
            // 3' oligo base (index l-1) sits at `start`, l-2 at start+1, …
            (start + k, l - 1 - k)
        } else {
            // Forward 3' terminus is at `end`; the annealed run ends at end-1.
            (end - annealed_count + k, tail_len + k)
        };
        let base = oligo[oligo_i];
        let t = template[column];
        let target = if reverse { complement_byte(t) } else { t };
        let matches = base == target;
        if !matches {
            mismatches += 1;
        }
        annealed.push(AnnealedBase {
            column,
            base,
            matches,
        });
    }

    // Tail = the 5' oligo bases (indices 0..tail_len), 5'→3' order.
    let tail = oligo[..tail_len].to_vec();

    PrimerDecomposition {
        annealed,
        tail,
        mismatches,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // template top strand: ATGCGTACCA (indices 0..10)
    const T: &[u8] = b"ATGCGTACCA";

    #[test]
    fn forward_perfect_match() {
        // oligo == top[2..8] = GCGTAC
        let d = decompose_primer("GCGTAC", &(2..8), Strand::Forward, T);
        assert_eq!(d.mismatches, 0);
        assert_eq!(d.tail, b"");
        assert_eq!(d.annealed.len(), 6);
        assert_eq!(d.annealed[0].column, 2);
        assert_eq!(d.annealed[0].base, b'G');
        assert!(d.annealed.iter().all(|a| a.matches));
    }

    #[test]
    fn forward_with_five_prime_tail() {
        // oligo = tail "TTT" + footprint top[2..8] "GCGTAC"
        let d = decompose_primer("TTTGCGTAC", &(2..8), Strand::Forward, T);
        assert_eq!(d.tail, b"TTT");
        assert_eq!(d.mismatches, 0);
        assert_eq!(d.annealed.len(), 6);
        // annealed still fills the footprint, 3'-anchored at column 8.
        assert_eq!(d.annealed[0].column, 2);
        assert_eq!(d.annealed.last().unwrap().column, 7);
    }

    #[test]
    fn forward_detects_mismatch() {
        // top[2..8] = GCGTAC; flip one base → one mismatch.
        let d = decompose_primer("GCGAAC", &(2..8), Strand::Forward, T);
        assert_eq!(d.mismatches, 1);
        let mm: Vec<_> = d.annealed.iter().filter(|a| !a.matches).collect();
        assert_eq!(mm.len(), 1);
        assert_eq!(mm[0].column, 5); // G(4)C(5).. wait: pos 5 template is T, oligo A
    }

    #[test]
    fn reverse_perfect_match_is_revcomp() {
        // top[2..8] = GCGTAC; reverse primer oligo = revcomp = GTACGC
        let d = decompose_primer("GTACGC", &(2..8), Strand::Reverse, T);
        assert_eq!(d.mismatches, 0, "revcomp oligo must anneal clean");
        assert_eq!(d.annealed.len(), 6);
        // Ascending template column, 3' terminus at start (column 2).
        assert_eq!(d.annealed[0].column, 2);
        assert_eq!(d.annealed.last().unwrap().column, 7);
        assert!(d.annealed.iter().all(|a| a.matches));
    }

    #[test]
    fn reverse_non_revcomp_is_all_mismatch() {
        // Feeding the *top-strand* bases to a reverse primer mismatches
        // everywhere (the orientation footgun made visible).
        let d = decompose_primer("GCGTAC", &(2..8), Strand::Reverse, T);
        assert!(d.mismatches > 0);
    }

    #[test]
    fn reverse_with_tail() {
        // 5' tail "AA" + revcomp footprint "GTACGC"
        let d = decompose_primer("AAGTACGC", &(2..8), Strand::Reverse, T);
        assert_eq!(d.tail, b"AA");
        assert_eq!(d.mismatches, 0);
        assert_eq!(d.annealed.len(), 6);
    }

    #[test]
    fn empty_or_degenerate_is_empty() {
        assert_eq!(
            decompose_primer("", &(0..4), Strand::Forward, T),
            PrimerDecomposition::default()
        );
        assert_eq!(
            decompose_primer("ACGT", &(9..9), Strand::Forward, T),
            PrimerDecomposition::default()
        );
    }

    #[test]
    fn binding_longer_than_oligo_anchors_at_three_prime() {
        // footprint 2..10 (len 8) but oligo only 4 long → annealed = 4, 3'-anchored.
        let d = decompose_primer("ACCA", &(2..10), Strand::Forward, T);
        assert_eq!(d.tail, b"");
        assert_eq!(d.annealed.len(), 4);
        // Forward anchors at end: columns 6,7,8,9.
        assert_eq!(d.annealed[0].column, 6);
        assert_eq!(d.annealed.last().unwrap().column, 9);
    }
}
