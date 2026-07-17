//! Primer QC thermodynamics (Phase 1.2): monomer Tm/GC, self-structure ΔG, and
//! orientation-safe primer:template annealing Tm.

use std::ops::Range;

use seqforge_core::{Primer, PrimerInfo, PrimerSiteInfo, PrimerState, Strand};
use seqforge_thermo::{
    DEFAULT_FOLD_TEMP_C, FoldError, TmError, duplex_tm, gc, hairpin_dg, self_dimer_dg, tm,
};

use super::{
    AnnealSettings, AttachmentState, classify_attachment, decompose_primer,
    find_primer_binding_sites,
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
    let oligo: String = oligo
        .bytes()
        .map(|b| b.to_ascii_uppercase() as char)
        .collect();
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
    let anneal_tm = primer.binding.as_ref().map(|b| {
        anneal_tm(
            &primer.sequence,
            &(b.start..b.start + b.len),
            primer.strand,
            template,
        )
    });
    PrimerQcPlusAnneal { qc, anneal_tm }
}

/// Build the [`PrimerInfo`] projection (attachment state + QC) for each primer
/// against `template`. The `seqforge_core::BioOps::primer_infos` seam — the one
/// shape the Inspector pane and CLI `primers list` share (decision 10).
pub fn primer_infos(template: &[u8], primers: &[&Primer], circular: bool) -> Vec<PrimerInfo> {
    let settings = AnnealSettings::default();
    primers
        .iter()
        .map(|p| primer_info(p, template, circular, settings))
        .collect()
}

fn primer_info(
    primer: &Primer,
    template: &[u8],
    circular: bool,
    settings: AnnealSettings,
) -> PrimerInfo {
    let attachment = classify_attachment(primer, template, circular, settings);
    let state = match attachment.state {
        AttachmentState::Confirmed => PrimerState::Confirmed,
        AttachmentState::Drifted => PrimerState::Drifted,
        AttachmentState::Detached => PrimerState::Detached,
    };
    // Mismatches within the *stored* footprint (0 when detached).
    let mismatches = primer
        .binding
        .as_ref()
        .map(|b| {
            decompose_primer(
                &primer.sequence,
                &(b.start..b.start + b.len),
                primer.strand,
                template,
            )
            .mismatches
        })
        .unwrap_or(0);
    let qc = primer_qc_with_anneal(primer, template);

    // Every place the oligo anneals — scanned independently of the authored
    // binding so a floating oligo still surfaces its candidate sites (drives the
    // Inspector site list + rescan). Each site is tagged `attached` when it
    // coincides with the authored footprint.
    let sites: Vec<PrimerSiteInfo> =
        find_primer_binding_sites(&primer.sequence, template, circular, settings)
            .into_iter()
            .map(|s| {
                let attached = primer
                    .binding
                    .is_some_and(|b| b == s.span && s.strand == primer.strand);
                // Linear range into the template for the thermo engine (the
                // documented linear-engine `Range` survivor).
                let binding = s.span.start..s.span.start + s.span.len;
                PrimerSiteInfo {
                    anneal_tm: anneal_tm(&primer.sequence, &binding, s.strand, template).ok(),
                    span: s.span,
                    strand: s.strand,
                    mismatches: s.mismatches,
                    attached,
                }
            })
            .collect();
    let off_targets = sites.iter().filter(|s| !s.attached).count();

    PrimerInfo {
        id: primer.id,
        name: primer.name.clone(),
        sequence: primer.sequence.clone(),
        binding: primer.binding,
        strand: primer.strand,
        len: primer.sequence.len(),
        tm: qc.qc.tm.ok(),
        gc: qc.qc.gc,
        hairpin_dg: qc.qc.hairpin_dg.ok(),
        self_dimer_dg: qc.qc.self_dimer_dg.ok(),
        anneal_tm: qc.anneal_tm.and_then(Result::ok),
        state,
        mismatches,
        off_targets,
        sites,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use seqforge_core::{PrimerId, Span};

    const T: &[u8] = b"ATGCGTACCA";

    #[test]
    fn forward_anneal_tm_matches_perfect_duplex() {
        let at = anneal_tm("GCGTAC", &(2..8), Strand::Forward, T).unwrap();
        let comp: String = complement(b"GCGTAC").iter().map(|&b| b as char).collect();
        let duplex = duplex_tm("GCGTAC", &comp).unwrap();
        assert!(
            (at - duplex).abs() < 0.5,
            "forward anneal should match perfect duplex: at={at}, duplex={duplex}"
        );
    }

    #[test]
    fn reverse_anneal_tm_succeeds_for_revcomp_oligo() {
        let at = anneal_tm("GTACGC", &(2..8), Strand::Reverse, T).unwrap();
        assert!(
            at > 0.0,
            "reverse perfect match should yield sensible Tm; got {at}"
        );
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
            binding: Some(seqforge_core::Span::from_range(2..8)),
            strand: Strand::Forward,
            qualifiers: Default::default(),
        };
        let out = primer_qc_with_anneal(&primer, T);
        assert!(out.anneal_tm.is_some());
        assert!(out.anneal_tm.unwrap().is_ok());
    }

    #[test]
    fn primer_infos_lists_all_sites_marking_attached_and_off_target() {
        // GCGTAC occurs twice on the top strand → one attached, one off-target.
        let template = b"GCGTACAAGCGTAC";
        let primer = Primer {
            id: PrimerId(1),
            name: "fwd".into(),
            sequence: "GCGTAC".into(),
            binding: Some(seqforge_core::Span::from_range(0..6)),
            strand: Strand::Forward,
            qualifiers: Default::default(),
        };
        let infos = primer_infos(template, &[&primer], false);
        let sites = &infos[0].sites;
        assert!(sites.len() >= 2, "both occurrences found: {sites:?}");
        let attached: Vec<_> = sites.iter().filter(|s| s.attached).collect();
        assert_eq!(
            attached.len(),
            1,
            "exactly one site is the attached footprint"
        );
        assert_eq!(attached[0].span, Span::from_range(0..6));
        assert_eq!(
            infos[0].off_targets,
            sites.iter().filter(|s| !s.attached).count(),
            "off_targets counts the non-attached sites"
        );
        assert!(
            infos[0].off_targets >= 1,
            "the second occurrence is off-target"
        );
        assert!(
            sites.iter().all(|s| s.anneal_tm.is_some()),
            "each site carries an annealing Tm"
        );
    }

    #[test]
    fn primer_infos_lists_sites_for_a_floating_oligo() {
        // A detached primer still surfaces candidate sites (none attached) so the
        // Inspector can offer rescan/attach.
        let template = b"ATGCGTACCA";
        let primer = Primer {
            id: PrimerId(1),
            name: "float".into(),
            sequence: "GCGTAC".into(),
            binding: None,
            strand: Strand::Forward,
            qualifiers: Default::default(),
        };
        let infos = primer_infos(template, &[&primer], false);
        assert_eq!(infos[0].state, PrimerState::Detached);
        assert!(
            !infos[0].sites.is_empty(),
            "candidate site listed for rescan"
        );
        assert!(
            infos[0].sites.iter().all(|s| !s.attached),
            "none attached while floating"
        );
    }

    #[test]
    fn primer_infos_projects_confirmed_and_detached() {
        use seqforge_core::PrimerState;

        // Confirmed: oligo == top[2..8], clean forward anneal.
        let confirmed = Primer {
            id: PrimerId(1),
            name: "fwd".into(),
            sequence: "GCGTAC".into(),
            binding: Some(seqforge_core::Span::from_range(2..8)),
            strand: Strand::Forward,
            qualifiers: Default::default(),
        };
        // Detached: no binding (floating oligo).
        let detached = Primer {
            id: PrimerId(2),
            name: "float".into(),
            sequence: "GCGTAC".into(),
            binding: None,
            strand: Strand::Forward,
            qualifiers: Default::default(),
        };
        let refs = [&confirmed, &detached];
        let infos = primer_infos(T, &refs, false);

        assert_eq!(infos.len(), 2);
        assert_eq!(infos[0].id, PrimerId(1));
        assert_eq!(infos[0].state, PrimerState::Confirmed);
        assert_eq!(
            infos[0].binding,
            Some(seqforge_core::Span::from_range(2..8))
        );
        assert_eq!(infos[0].len, 6);
        assert_eq!(infos[0].mismatches, 0);
        assert!(infos[0].tm.is_some());
        assert!(infos[0].anneal_tm.is_some());

        assert_eq!(infos[1].state, PrimerState::Detached);
        assert_eq!(infos[1].binding, None);
        // No binding → no annealing Tm, but monomer QC still computes.
        assert!(infos[1].anneal_tm.is_none());
        assert!(infos[1].tm.is_some());
    }
}
