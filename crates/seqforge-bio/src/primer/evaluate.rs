//! Primer QC thermodynamics (Phase 1.2): monomer Tm/GC, self-structure ΔG, and
//! orientation-safe primer:template annealing Tm.

use std::ops::Range;

use seqforge_core::{Primer, Strand};
use seqforge_thermo::{
    DEFAULT_FOLD_TEMP_C, FoldError, TmError, duplex_tm, gc, hairpin_dg, self_dimer_dg, tm,
};

use crate::dna::{complement, reverse_complement};

/// Monomer and self-structure QC for a primer oligo.
#[derive(Debug, Clone)]
pub struct PrimerQc {
    pub tm: Result<f64, TmError>,
    pub gc: f64,
    pub hairpin_dg: Result<f64, FoldError>,
    pub self_dimer_dg: Result<f64, FoldError>,
}

/// [`PrimerQc`] plus optional annealing Tm when the primer has a binding site.
#[derive(Debug, Clone)]
pub struct PrimerQcPlusAnneal {
    pub qc: PrimerQc,
    pub anneal_tm: Option<Result<f64, TmError>>,
}

/// Tm (°C) of `oligo` annealed to `template` at `binding` on `strand`.
///
/// Feeds seqfold heteroduplex `tm` with the correct antiparallel template sense
/// (the same orientation footgun [`super::decompose_primer`] guards).
pub fn anneal_tm(
    oligo: &str,
    binding: &Range<usize>,
    strand: Strand,
    template: &[u8],
) -> Result<f64, TmError> {
    let oligo: String = oligo.bytes().map(|b| b.to_ascii_uppercase() as char).collect();
    let start = binding.start;
    let end = binding.end.min(template.len());
    if oligo.len() < 2 || start >= end {
        return Err(TmError("sequence too short".to_string()));
    }
    let region: String = template[start..end]
        .iter()
        .map(|&b| b.to_ascii_uppercase() as char)
        .collect();
    let partner: String = complement(region.as_bytes())
        .iter()
        .map(|&b| b as char)
        .collect();
    match strand {
        Strand::Forward => duplex_tm(&oligo, &partner),
        Strand::Reverse => {
            let top_sense: String = reverse_complement(oligo.as_bytes())
                .iter()
                .map(|&b| b as char)
                .collect();
            duplex_tm(&top_sense, &partner)
        }
        Strand::Both | Strand::None => Err(TmError(
            "anneal_tm requires Forward or Reverse primer strand".to_string(),
        )),
    }
}

/// Monomer Tm, %GC, and self-structure ΔG at [`DEFAULT_FOLD_TEMP_C`].
pub fn primer_qc(oligo: &str) -> PrimerQc {
    PrimerQc {
        tm: tm(oligo),
        gc: gc(oligo),
        hairpin_dg: hairpin_dg(oligo, DEFAULT_FOLD_TEMP_C),
        self_dimer_dg: self_dimer_dg(oligo, DEFAULT_FOLD_TEMP_C),
    }
}

/// [`primer_qc`] plus [`anneal_tm`] when `primer.binding` is present.
pub fn primer_qc_with_anneal(primer: &Primer, template: &[u8]) -> PrimerQcPlusAnneal {
    let qc = primer_qc(&primer.sequence);
    let anneal_tm = primer
        .binding
        .as_ref()
        .map(|b| anneal_tm(&primer.sequence, b, primer.strand, template));
    PrimerQcPlusAnneal { qc, anneal_tm }
}

#[cfg(test)]
mod tests {
    use super::*;
    use seqforge_core::PrimerId;

    const T: &[u8] = b"ATGCGTACCA";

    #[test]
    fn forward_anneal_tm_matches_perfect_duplex() {
        let at = anneal_tm("GCGTAC", &(2..8), Strand::Forward, T).unwrap();
        let comp: String = complement(b"GCGTAC")
            .iter()
            .map(|&b| b as char)
            .collect();
        let duplex = duplex_tm("GCGTAC", &comp).unwrap();
        assert!(
            (at - duplex).abs() < 0.5,
            "forward anneal should match perfect duplex: at={at}, duplex={duplex}"
        );
    }

    #[test]
    fn reverse_anneal_tm_succeeds_for_revcomp_oligo() {
        let at = anneal_tm("GTACGC", &(2..8), Strand::Reverse, T).unwrap();
        assert!(at > 0.0, "reverse perfect match should yield sensible Tm; got {at}");
    }

    #[test]
    fn reverse_top_strand_oligo_differs_from_revcomp_anneal() {
        let correct = anneal_tm("GTACGC", &(2..8), Strand::Reverse, T).unwrap();
        let wrong = anneal_tm("GCGTAC", &(2..8), Strand::Reverse, T).unwrap();
        assert!(
            (correct - wrong).abs() > 1.0,
            "top-strand bases on a reverse primer should not match revcomp anneal: \
             correct={correct}, wrong={wrong}"
        );
    }

    #[test]
    fn primer_qc_on_fold_test_oligo() {
        let seq = "GGGAGGTCGTTACATCTGGGTAACACCGGTACTGATCCGGTGACCTCCC";
        let qc = primer_qc(seq);
        assert!(qc.tm.unwrap() > 50.0);
        assert!(qc.gc > 40.0);
        assert!(qc.hairpin_dg.unwrap() <= 0.0);
        assert!(qc.self_dimer_dg.unwrap() < 0.0);
    }

    #[test]
    fn primer_qc_with_anneal_when_bound() {
        let primer = Primer {
            id: PrimerId(1),
            name: "fwd".into(),
            sequence: "GCGTAC".into(),
            binding: Some(2..8),
            strand: Strand::Forward,
            qualifiers: Default::default(),
        };
        let out = primer_qc_with_anneal(&primer, T);
        assert!(out.anneal_tm.is_some());
        assert!(out.anneal_tm.unwrap().is_ok());
    }
}
