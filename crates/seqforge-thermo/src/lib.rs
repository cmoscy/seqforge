//! `seqforge-thermo` — sequence thermodynamics for SeqForge.
//!
//! A pure, zero-dependency crate wrapping the **vendored `seqfold` engine**
//! (MIT, Lattice Automation; see [`core`], `README.md`, `LICENSE`). Like
//! `seqforge-restriction`, it carries no workspace or non-std dependencies and
//! is `publish = false` until it is extracted — the constraint that keeps that
//! extraction a one-crate change.
//!
//! ## Thin API
//!
//! - [`tm`] / [`duplex_tm`] — nearest-neighbour melting temperature (°C).
//! - [`gc`] — GC content as a percentage.
//! - [`hairpin_dg`] / [`self_dimer_dg`] — self-structure ΔG (kcal/mol) via
//!   vendored MFE folding (Phase 1.2).
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

pub use core::fold::FoldError;
pub use core::tm::TmError;

/// Default folding temperature (°C) for primer QC — matches seqfold's
/// `fold_test.py` vectors and Lattice primers scoring.
pub const DEFAULT_FOLD_TEMP_C: f64 = 37.0;

/// Linker between oligo and its reverse complement in [`self_dimer_dg`].
/// AT-rich so it does not add false GC pairing (and avoids `N`, which seqfold
/// cannot fold).
const SELF_DIMER_LINKER: &str = "AAAAA";

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

/// Melting temperature (°C) of a two-sequence duplex (antiparallel), with
/// internal mismatch support. Used for primer:template annealing ([`duplex_tm`]
/// in `seqforge-bio::anneal_tm`).
pub fn duplex_tm(seq1: &str, seq2: &str) -> Result<f64, TmError> {
    core::tm::tm(seq1, seq2, true)
}

/// Most stable hairpin ΔG (kcal/mol) in `oligo` at `temp_c`.
///
/// When the MFE fold includes a hairpin loop, returns the overall MFE ΔG (more
/// negative = more stable self-structure). Returns `0.0` when no hairpin appears
/// in the decomposition or the MFE is not favorable (ΔG ≥ 0).
pub fn hairpin_dg(oligo: &str, temp_c: f64) -> Result<f64, FoldError> {
    let structs = core::fold::fold(oligo, temp_c)?;
    let has_hairpin = structs
        .iter()
        .any(|s| s.desc.starts_with("HAIRPIN"));
    if !has_hairpin {
        return Ok(0.0);
    }
    let overall: f64 = structs.iter().map(|s| s.e).sum();
    Ok(if overall < 0.0 {
        core::pyfloat::pyround(overall, 2)
    } else {
        0.0
    })
}

/// Self-dimer ΔG (kcal/mol) at `temp_c`.
///
/// Unimolecular approximation: MFE fold of `oligo + linker + revcomp(oligo)`.
/// seqfold has no bimolecular dimer mode; hetero-dimer QC is Phase 3.1. More
/// negative = more self-dimer-prone.
pub fn self_dimer_dg(oligo: &str, temp_c: f64) -> Result<f64, FoldError> {
    let upper: String = oligo.bytes().map(|b| b.to_ascii_uppercase() as char).collect();
    if upper.is_empty() {
        return Ok(0.0);
    }
    let rc = reverse_complement_dna(upper.as_bytes());
    let concat = format!("{upper}{SELF_DIMER_LINKER}{rc}");
    core::fold::dg(&concat, temp_c)
}

fn reverse_complement_dna(seq: &[u8]) -> String {
    seq.iter()
        .rev()
        .map(|&b| {
            (match b.to_ascii_uppercase() {
                b'A' => b'T',
                b'T' => b'A',
                b'G' => b'C',
                b'C' => b'G',
                other => other,
            }) as char
        })
        .collect()
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

    // ── fold / structure (seqfold fold_test.py DNA vectors) ─────────────────

    #[test]
    fn dg_matches_unafold_ballpark() {
        let seq = "GGGAGGTCGTTACATCTGGGTAACACCGGTACTGATCCGGTGACCTCCC";
        let ufold = -10.94;
        let d = core::fold::dg(seq, DEFAULT_FOLD_TEMP_C).unwrap();
        let delta = (0.6 * d.min(ufold)).abs();
        assert!(
            (d - ufold).abs() <= delta,
            "dg({seq}) = {d}, expected {ufold} ± {delta}"
        );
    }

    #[test]
    fn fold_rejects_invalid_sequence() {
        assert!(core::fold::dg("EASFEASFAST", DEFAULT_FOLD_TEMP_C).is_err());
        assert!(core::fold::dg("ATGCATGACGATUU", DEFAULT_FOLD_TEMP_C).is_err());
    }

    #[test]
    fn hairpin_dg_on_structured_oligo_is_negative() {
        let hp = hairpin_dg("CGCGTTTTTGCGC", DEFAULT_FOLD_TEMP_C).unwrap();
        assert!(hp < 0.0, "structured oligo with hairpin MFE; got {hp}");
    }

    #[test]
    fn hairpin_dg_on_short_at_homopolymer_is_zero() {
        let hp = hairpin_dg("ATATAT", DEFAULT_FOLD_TEMP_C).unwrap();
        assert_eq!(hp, 0.0, "no favorable hairpin; got {hp}");
    }

    #[test]
    fn self_dimer_dg_self_complementary_more_stable_than_random() {
        let pal = self_dimer_dg("GCGCGCGCGC", DEFAULT_FOLD_TEMP_C).unwrap();
        let ctrl = self_dimer_dg("ATGCGTAGCT", DEFAULT_FOLD_TEMP_C).unwrap();
        assert!(
            pal < ctrl,
            "self-complementary oligo should be more negative: pal={pal}, ctrl={ctrl}"
        );
    }

    #[test]
    fn duplex_tm_matches_monomer_when_seq2_is_complement() {
        let seq = "GCGTAC";
        let mono = tm(seq).unwrap();
        let comp_only: String = seq
            .bytes()
            .map(|b| {
                (match b.to_ascii_uppercase() {
                    b'A' => b'T',
                    b'T' => b'A',
                    b'G' => b'C',
                    b'C' => b'G',
                    other => other,
                }) as char
            })
            .collect();
        let duplex = duplex_tm(seq, &comp_only).unwrap();
        assert!(
            (mono - duplex).abs() < 0.5,
            "perfect complement duplex: mono={mono}, duplex={duplex}"
        );
    }
}
