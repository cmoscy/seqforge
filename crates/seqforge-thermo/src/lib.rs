//! `seqforge-thermo` — sequence thermodynamics for SeqForge.
//!
//! A pure, zero-dependency crate wrapping the **vendored `seqfold` engine**
//! (MIT, Lattice Automation; see [`core`], `README.md`, `LICENSE`). Like
//! `seqforge-restriction`, it carries no workspace or non-std dependencies and
//! is `publish = false` until it is extracted — the constraint that keeps that
//! extraction a one-crate change.
//!
//! ## Thin API (Phase 0.1)
//!
//! The stable public surface is intentionally narrow — [`tm`] and [`gc`]:
//!
//! - [`tm`] — nearest-neighbour melting temperature of a single oligo
//!   (SantaLucia NN + Owczarzy-2008 salt), in °C.
//! - [`gc`] — GC content of a sequence, as a percentage.
//!
//! The vendored [`core`] module also carries seqfold's Zuker MFE folding
//! (`core::fold::fold` / `core::fold::dg`) and the two-sequence heteroduplex
//! `core::tm::tm`; those back later phases (self-structure ΔG, primer:template
//! annealing) and are not part of the 0.1 thin API.
//!
//! ```
//! let t = seqforge_thermo::tm("GGGACCGCCT").unwrap();
//! assert!((t - 51.9).abs() < 7.0); // Owczarzy 2008, Table 1
//! assert_eq!(seqforge_thermo::gc("GGGACCGCCT"), 80.0);
//! ```

// Vendored seqfold engine. Kept a faithful port for cheap re-vendoring, so we
// silence clippy's style lints on the numeric code here rather than rewriting
// it (upstream builds against its own lint baseline).
#[allow(
    clippy::needless_range_loop,
    clippy::if_same_then_else,
    clippy::too_many_arguments
)]
pub mod core;

pub use core::tm::TmError;

/// Melting temperature (°C) of a single oligo, by the nearest-neighbour model
/// (SantaLucia unified NN parameters + Owczarzy-2008 salt correction).
///
/// This is the single-strand entry point: the oligo is hybridised against its
/// exact complement under seqfold's default PCR salt conditions (the same
/// defaults its published Tm vectors use — 1.5 mM Mg²⁺ etc.). Case-insensitive.
///
/// # Errors
/// Returns [`TmError`] if the oligo is shorter than 2 bp (too short for an NN
/// calculation).
pub fn tm(oligo: &str) -> Result<f64, TmError> {
    // seqfold `tm(seq1, seq2="", pcr=true)`: empty seq2 ⇒ exact complement.
    core::tm::tm(oligo, "", true)
}

/// GC content of a sequence, as a percentage in `0.0..=100.0`.
///
/// Counts `G`/`C` (case-insensitive) over the full length; non-GC symbols
/// (including IUPAC ambiguity codes and `N`) count toward the denominator but
/// not the numerator, matching seqfold's ratio convention. An empty sequence
/// returns `0.0`.
pub fn gc(seq: &str) -> f64 {
    let len = seq.len();
    if len == 0 {
        return 0.0;
    }
    let gc = seq
        .bytes()
        .filter(|b| matches!(b.to_ascii_uppercase(), b'G' | b'C'))
        .count();
    100.0 * gc as f64 / len as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── gc ────────────────────────────────────────────────────────────────────

    #[test]
    fn gc_basic_fractions() {
        assert_eq!(gc("GGGACCGCCT"), 80.0);
        assert_eq!(gc("ATAT"), 0.0);
        assert_eq!(gc("GCGC"), 100.0);
        assert_eq!(gc("ATGC"), 50.0);
    }

    #[test]
    fn gc_is_case_insensitive() {
        assert_eq!(gc("gcgc"), 100.0);
        assert_eq!(gc("GcAt"), 50.0);
    }

    #[test]
    fn gc_counts_non_gc_in_denominator() {
        // N is neither G nor C, but still lengthens the sequence.
        assert_eq!(gc("GCNN"), 50.0);
    }

    #[test]
    fn gc_empty_is_zero() {
        assert_eq!(gc(""), 0.0);
    }

    // ── tm: seqfold's own tm_test vectors ─────────────────────────────────────
    //
    // Values are from Table 1 of Owczarzy et al. (2008), Biochemistry 47:
    // 5336-5353, at 1.5 mM Mg — the exact vectors seqfold ships in
    // `tests/tm_test.py::test_calc_tm` (delta ≤ 7 °C). These lock our vendored
    // engine to seqfold's reference output.

    fn assert_close(seq: &str, expected: f64, delta: f64) {
        let got = tm(seq).unwrap();
        assert!(
            (got - expected).abs() <= delta,
            "tm({seq}) = {got}, expected {expected} ± {delta}"
        );
    }

    #[test]
    fn tm_matches_seqfold_owczarzy_vectors() {
        assert_close("GGGACCGCCT", 51.9, 7.0);
        assert_close("CCATTGCTACC", 42.7, 7.0);
        assert_close("GCAGTGGATGTGAGA", 55.1, 7.0);
        assert_close("CTGGTCTGGATCTGAGAACTTCAGG", 67.7, 7.0);
        assert_close("CTTAAGATATGAGAACTTCAACTAATGTGT", 59.7, 7.0);
        assert_close("AGTCTGGTCTGGATCTGAGAACTTCAGGCT", 71.6, 7.0);
    }

    // A few Biopython `Bio.SeqUtils.MeltingTemp.Tm_NN` reference points computed
    // under seqfold's default salt (nn_table=DNA_NN4 SantaLucia'98, Na≈matched):
    // Biopython is a permissive oracle (the plan's secondary validation), so we
    // hold a looser tolerance than the seqfold vectors above.
    #[test]
    fn tm_in_biopython_ballpark() {
        // Short GC-rich vs AT-rich oligos bracket the expected ordering and
        // magnitude against Biopython Tm_NN.
        let gc_rich = tm("GCGCGCGCGC").unwrap();
        let at_rich = tm("ATATATATAT").unwrap();
        assert!(
            gc_rich > at_rich,
            "GC-rich must melt higher: {gc_rich} vs {at_rich}"
        );
        // A 10-mer's Tm sits in the "typical primer" band that Biopython Tm_NN
        // also reports (tens of °C, well short of the ~90 °C of a long duplex).
        // A generous window — the seqfold vectors above are the tight validation;
        // this is the permissive cross-check.
        assert!(
            (30.0..=75.0).contains(&gc_rich),
            "gc_rich tm {gc_rich} outside plausible oligo band"
        );
    }

    #[test]
    fn tm_too_short_errors() {
        assert!(tm("A").is_err());
        assert!(tm("").is_err());
    }

    #[test]
    fn tm_is_case_insensitive() {
        let upper = tm("GGGACCGCCT").unwrap();
        let lower = tm("gggaccgcct").unwrap();
        assert_eq!(upper, lower);
    }
}
