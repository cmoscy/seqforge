//! Primer QC thermodynamics (Phase 1.2): monomer Tm/GC, self-structure ΔG, and
//! orientation-safe primer:template annealing Tm.

use std::ops::Range;

use seqforge_core::span::Pieces;
use seqforge_core::{Primer, PrimerInfo, PrimerSiteInfo, PrimerState, Span, Strand};
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
/// Anneal Tm for a binding [`Span`], which may wrap the origin of a circular
/// template.
///
/// [`anneal_tm`] takes a linear `Range` and clamps it to the template, which is
/// right for a linear region and silently wrong for a wrapping one: it scores
/// only the bases before the origin, and the 3'-anchoring then trims the oligo
/// to match, yielding a believable Tm for a much shorter duplex. Since
/// `find_primer_binding_sites` reports a wrap-around hit as one wrapping `Span`,
/// every origin-crossing primer on a plasmid hit that path.
///
/// Gathering through [`Span::linear_pieces`] — the codebase's lossless wrap
/// projection — makes the duplex the real one, and makes the origin stop
/// mattering to the answer.
pub fn anneal_tm_span(
    oligo: &str,
    span: Span,
    strand: Strand,
    template: &[u8],
) -> Result<f64, TmError> {
    let pieces = span.linear_pieces(template.len());
    // One run is the common case and needs no copy.
    if let Pieces::One(r) = &pieces {
        return anneal_tm(oligo, r, strand, template);
    }
    let region: Vec<u8> = pieces.iter().flat_map(|r| template[r].to_vec()).collect();
    if region.is_empty() {
        return Err(TmError("sequence too short".to_string()));
    }
    let n = region.len();
    anneal_tm(oligo, &(0..n), strand, &region)
}

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
    // 3'-anchored, matching `decompose_primer`: only the bases inside the
    // footprint anneal, so a cloning primer's 5' tail is excluded from the
    // duplex. Without this, any tailed primer yields a length mismatch against
    // its template partner and no Tm at all.
    let oligo: String = {
        let footprint = end - start;
        match oligo.len().checked_sub(footprint) {
            Some(tail_len) if tail_len > 0 => oligo[tail_len..].to_string(),
            _ => oligo,
        }
    };
    if oligo.len() < 2 {
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

/// Every place `oligo` anneals on `template`, projected to [`PrimerSiteInfo`].
///
/// `is_attached` marks the site coinciding with an authored footprint; an ad-hoc
/// oligo has none. Extracted so an authored primer and a `find-primer-sites`
/// query are scored by one implementation — in particular the wrap-aware
/// [`anneal_tm_span`], which a parallel copy got wrong for origin-crossing sites.
fn binding_sites(
    oligo: &str,
    template: &[u8],
    circular: bool,
    settings: AnnealSettings,
    is_attached: impl Fn(&super::anneal::PrimerBinding) -> bool,
) -> Vec<PrimerSiteInfo> {
    find_primer_binding_sites(oligo, template, circular, settings)
        .into_iter()
        .map(|s| {
            let tail_len = oligo.len().saturating_sub(s.span.len);
            PrimerSiteInfo {
                // Wrap-aware: a site crossing the origin is scored over the
                // whole duplex, not truncated at the origin.
                anneal_tm: anneal_tm_span(oligo, s.span, s.strand, template).ok(),
                attached: is_attached(&s),
                span: s.span,
                strand: s.strand,
                mismatches: s.mismatches,
                tail: oligo[..tail_len].to_string(),
                tail_len,
            }
        })
        .collect()
}

/// Where an ad-hoc oligo anneals — the `find-primer-sites` projection.
///
/// No authored primer is involved, so no site is `attached`.
pub fn primer_sites(oligo: &str, template: &[u8], circular: bool) -> Vec<PrimerSiteInfo> {
    binding_sites(oligo, template, circular, AnnealSettings::default(), |_| {
        false
    })
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
    let sites = binding_sites(&primer.sequence, template, circular, settings, |s| {
        // One predicate for "is this the stored priming event", shared with
        // `classify_attachment` — a second copy here would let the site list
        // disagree with the state it is supposed to explain.
        primer
            .binding
            .is_some_and(|b| super::anneal::same_site(s, b, primer.strand))
    });
    let off_targets = sites.iter().filter(|s| !s.attached).count();

    PrimerInfo {
        id: primer.id,
        name: primer.name.clone(),
        sequence: primer.sequence.clone(),
        binding: primer.binding,
        strand: primer.strand,
        len: primer.sequence.len(),
        // Taken straight off the oligo rather than from `decompose_primer`,
        // which clamps to the template end and would over-report the tail for a
        // binding that crosses the origin. The footprint is the annealed span by
        // definition, so whatever the oligo has beyond it is the tail.
        tail: primer
            .binding
            .map(|b| {
                let tail_len = primer.sequence.len().saturating_sub(b.len);
                primer.sequence[..tail_len].to_string()
            })
            .unwrap_or_default(),
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

    /// The user-visible half of the same bug: `primer_infos` is the projection
    /// behind the Inspector's Primers tab, `seqforge primers list`, and every
    /// socket caller. On a circular plasmid it reported the clamped Tm — a
    /// believable number for a duplex several bases shorter than the real one.
    ///
    /// Only `seqforge primers find` was right, because it open-coded the
    /// origin extension. That copy is now gone; both go through
    /// `anneal_tm_span`.
    #[test]
    fn primer_infos_reports_a_real_tm_for_an_origin_crossing_site() {
        // 24 bp circular template, and an 12-mer footprint that starts 6 bases
        // before the origin so it wraps.
        let template: &[u8] = b"ATGCGTACCAGGTTACGCATGCAT";
        let span = Span {
            start: template.len() - 6,
            len: 12,
        };
        let oligo: String = span
            .linear_pieces(template.len())
            .iter()
            .flat_map(|r| template[r].to_vec())
            .map(|b| b as char)
            .collect();
        assert_eq!(oligo.len(), 12, "the footprint wraps the origin");

        let primer = Primer {
            id: PrimerId(1),
            name: "wraps".into(),
            sequence: oligo.clone(),
            binding: Some(span),
            strand: Strand::Forward,
            qualifiers: Default::default(),
        };

        let infos = primer_infos(template, &[&primer], true);
        let site = infos[0]
            .sites
            .iter()
            .find(|s| s.span == span)
            .expect("the wrapping site is found");

        let tm = site.anneal_tm.expect("a wrapping site has a real duplex");

        // It must equal the whole 12-mer duplex, not the 6-base prefix the
        // clamp used to score.
        let whole = anneal_tm(&oligo, &(0..12), Strand::Forward, oligo.as_bytes()).unwrap();
        assert!(
            (tm - whole).abs() < 0.01,
            "projection disagrees with the real duplex: tm={tm}, whole={whole}"
        );

        let clamped = anneal_tm(
            &oligo,
            &(span.start..span.start + span.len),
            Strand::Forward,
            template,
        )
        .unwrap();
        assert!(
            (tm - clamped).abs() > 1.0,
            "the old clamped answer should be visibly different: tm={tm}, clamped={clamped}"
        );
    }

    /// A binding site that crosses the origin of a circular template must be
    /// scored over the **whole** duplex, not the part before the origin.
    ///
    /// `anneal_tm` clamps `binding.end` to the template length, so a wrapping
    /// span used to be truncated silently — and because the 3'-anchoring then
    /// trims the oligo to that same short footprint, the result was a
    /// plausible-looking Tm for a much shorter duplex rather than an error.
    #[test]
    fn origin_crossing_site_is_scored_over_the_whole_duplex() {
        // 10 bp circular template; an 8-mer starting at 6 wraps: 4 before the
        // origin, 4 after.
        let span = Span { start: 6, len: 8 };

        // Build the oligo from the template so the duplex is perfect.
        let gathered: Vec<u8> = span
            .linear_pieces(T.len())
            .iter()
            .flat_map(|r| T[r].to_vec())
            .collect();
        let oligo: String = gathered.iter().map(|&b| b as char).collect();
        assert_eq!(oligo.len(), 8, "the wrapping footprint is 8 bases");

        let wrapped = anneal_tm_span(&oligo, span, Strand::Forward, T)
            .expect("a wrapping site has a real duplex");

        // The same 8-mer scored against a linear template that already contains
        // it contiguously must agree — the origin is not supposed to matter.
        let linear: Vec<u8> = gathered.clone();
        let straight = anneal_tm(&oligo, &(0..8), Strand::Forward, &linear).unwrap();

        assert!(
            (wrapped - straight).abs() < 0.01,
            "origin crossing changed the answer: wrapped={wrapped}, straight={straight}"
        );

        // And it must differ from the truncated answer the old clamp produced.
        let truncated = anneal_tm(&oligo, &(6..14), Strand::Forward, T).unwrap();
        assert!(
            (wrapped - truncated).abs() > 1.0,
            "the clamped path should be visibly wrong: wrapped={wrapped}, truncated={truncated}"
        );
    }

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

    /// The site list must agree with the state it is meant to explain: a longer
    /// anneal at the stored 3' anchor is *the* attached site, not an off-target.
    /// Regression for the PCR-product case, where the product contains the
    /// primer's own tail so the oligo pairs over its whole length.
    #[test]
    fn a_longer_anneal_at_the_stored_anchor_is_marked_attached() {
        let anneal = "GGCATTACGCAGGATCCAAG";
        let product = format!("TTTTT{anneal}");
        let primer = Primer {
            id: PrimerId(1),
            name: "tailed".into(),
            sequence: product.clone(), // oligo == tail + anneal
            binding: Some(seqforge_core::Span::from_range(5..25)),
            strand: Strand::Forward,
            qualifiers: Default::default(),
        };
        let infos = primer_infos(product.as_bytes(), &[&primer], false);
        let info = &infos[0];
        assert_eq!(info.state, PrimerState::Confirmed);
        assert_eq!(info.off_targets, 0, "sites: {:?}", info.sites);
        assert!(
            info.sites.iter().any(|s| s.attached && s.span.len == 25),
            "the full-length site should be the attached one: {:?}",
            info.sites
        );
        assert_eq!(
            info.tail.len(),
            5,
            "tail still reads from the stored binding"
        );
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
