//! Seed-and-extend primer binding-site find + attachment-state classification
//! (Phase 1.1). Own result type [`PrimerBinding`] — never [`seqforge_core::SearchHit`].

use std::ops::Range;

use seqforge_core::{Primer, Strand};

use super::{PrimerDecomposition, decompose_primer};
use crate::dna::reverse_complement;

/// Binding-stringency tolerances (ROADMAP decision 7: defaulted settings,
/// exposed later via app config / CLI flags — not persisted on `core`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AnnealSettings {
    /// Exact-match run required at the 3' terminus to seed a candidate
    /// (also gates Detached). Clamped to the oligo length for short primers.
    pub min_three_prime_match: usize,
    /// Max mismatches tolerated across the full footprint to still count as
    /// a binding (gates Detached).
    pub max_mismatches: usize,
}

impl Default for AnnealSettings {
    fn default() -> Self {
        Self {
            min_three_prime_match: 8,
            max_mismatches: 4,
        }
    }
}

/// A primer's alignment to *some* template location — the find pass's own
/// result type (Consistency #4: never `core::SearchHit`, which lacks
/// mismatch/anchor data).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrimerBinding {
    pub range: Range<usize>,
    pub strand: Strand,
    pub mismatches: usize,
    pub three_prime_match: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttachmentState {
    Confirmed,
    Drifted,
    Detached,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrimerAttachment {
    pub state: AttachmentState,
    /// Sites other than the stored/confirmed one. Orthogonal to state.
    pub off_target_sites: Vec<PrimerBinding>,
}

/// Find all binding sites for `oligo` on `template` via 3'-terminal k-mer seeding
/// and full-footprint scoring with [`decompose_primer`].
///
/// For circular sequences, pass `circular = true`; wrap-around hits may have
/// `range.end > template.len()` (same convention as [`crate::find_iupac_matches`]).
pub fn find_primer_binding_sites(
    oligo: &str,
    template: &[u8],
    circular: bool,
    settings: AnnealSettings,
) -> Vec<PrimerBinding> {
    let oligo: Vec<u8> = oligo.bytes().map(|b| b.to_ascii_uppercase()).collect();
    let oligo_len = oligo.len();
    let template_len = template.len();
    if oligo_len == 0 || template_len == 0 {
        return vec![];
    }

    let k = settings.min_three_prime_match.min(oligo_len);
    if k == 0 {
        return vec![];
    }

    let seed_fwd = &oligo[oligo_len - k..];
    let seed_rev = reverse_complement(seed_fwd);

    let extended: Vec<u8>;
    let search_seq: &[u8] = if circular && oligo_len > 1 {
        extended = template
            .iter()
            .chain(&template[..oligo_len - 1])
            .copied()
            .collect();
        &extended
    } else {
        template
    };

    let oligo_str = std::str::from_utf8(&oligo).unwrap_or("");
    let mut candidates = Vec::new();

    // Forward: 3' k-mer seeds at `p..p+k`; footprint ends at `p + k`.
    for p in find_exact_matches(search_seq, seed_fwd) {
        let end = p + k;
        let start = end.saturating_sub(oligo_len);
        let range = report_range(start, oligo_len, template_len, circular);
        try_add_candidate(
            &mut candidates,
            oligo_str,
            range,
            Strand::Forward,
            template,
            circular,
            settings,
        );
    }

    // Reverse: revcomp(3' k-mer) seeds at `p..p+k`; 3' anchor at `p`.
    for p in find_exact_matches(search_seq, &seed_rev) {
        let start = p;
        let range = report_range(start, oligo_len, template_len, circular);
        try_add_candidate(
            &mut candidates,
            oligo_str,
            range,
            Strand::Reverse,
            template,
            circular,
            settings,
        );
    }

    candidates
}

/// Classify a primer's attachment state against the current template.
pub fn classify_attachment(
    primer: &Primer,
    template: &[u8],
    circular: bool,
    settings: AnnealSettings,
) -> PrimerAttachment {
    let Some(binding) = primer.binding.clone() else {
        return PrimerAttachment {
            state: AttachmentState::Detached,
            off_target_sites: vec![],
        };
    };

    let k = settings.min_three_prime_match.min(primer.sequence.len());
    let stored_decomp = decompose_primer(&primer.sequence, &binding, primer.strand, template);
    let stored_ok = three_prime_matches(&stored_decomp, k)
        && stored_decomp.mismatches <= settings.max_mismatches;

    let all_sites = find_primer_binding_sites(&primer.sequence, template, circular, settings);

    if stored_ok {
        let off_target_sites: Vec<_> = all_sites
            .iter()
            .filter(|s| !same_site(s, &binding, primer.strand))
            .cloned()
            .collect();

        let confirmed = stored_decomp.mismatches == 0
            && all_sites
                .iter()
                .any(|s| same_site(s, &binding, primer.strand) && s.mismatches == 0);

        let state = if confirmed {
            AttachmentState::Confirmed
        } else {
            AttachmentState::Drifted
        };

        PrimerAttachment {
            state,
            off_target_sites,
        }
    } else if all_sites.is_empty() {
        PrimerAttachment {
            state: AttachmentState::Detached,
            off_target_sites: vec![],
        }
    } else {
        PrimerAttachment {
            state: AttachmentState::Drifted,
            off_target_sites: all_sites,
        }
    }
}

fn same_site(site: &PrimerBinding, binding: &Range<usize>, strand: Strand) -> bool {
    site.range == *binding && site.strand == strand
}

fn three_prime_matches(decomp: &PrimerDecomposition, k: usize) -> bool {
    if k == 0 {
        return true;
    }
    let annealed = &decomp.annealed;
    if annealed.len() < k {
        return false;
    }
    annealed[annealed.len() - k..].iter().all(|a| a.matches)
}

fn find_exact_matches(haystack: &[u8], needle: &[u8]) -> Vec<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return vec![];
    }
    let limit = haystack.len() - needle.len() + 1;
    let mut hits = Vec::new();
    for i in 0..limit {
        if haystack[i..i + needle.len()]
            .iter()
            .zip(needle)
            .all(|(&a, &b)| a.eq_ignore_ascii_case(&b))
        {
            hits.push(i);
        }
    }
    hits
}

/// Map a search position in the (possibly extended) haystack to a reported range.
fn report_range(
    start: usize,
    oligo_len: usize,
    template_len: usize,
    circular: bool,
) -> Range<usize> {
    if circular {
        let start = start % template_len;
        start..start + oligo_len
    } else {
        let end = (start + oligo_len).min(template_len);
        let start = end.saturating_sub(oligo_len);
        start..end
    }
}

fn try_add_candidate(
    candidates: &mut Vec<PrimerBinding>,
    oligo: &str,
    range: Range<usize>,
    strand: Strand,
    template: &[u8],
    circular: bool,
    settings: AnnealSettings,
) {
    if candidates
        .iter()
        .any(|c| c.range == range && c.strand == strand)
    {
        return;
    }

    let Some(binding) = score_candidate(oligo, &range, strand, template, circular, settings) else {
        return;
    };

    candidates.push(binding);
}

fn score_candidate(
    oligo: &str,
    range: &Range<usize>,
    strand: Strand,
    template: &[u8],
    circular: bool,
    settings: AnnealSettings,
) -> Option<PrimerBinding> {
    let template_len = template.len();
    let k = settings.min_three_prime_match.min(oligo.len());

    let extended_storage;
    let (decomp_template, decomp_binding) = if circular && range.end > template_len {
        let extend = range.end - template_len;
        extended_storage = template
            .iter()
            .chain(&template[..extend.min(template_len)])
            .copied()
            .collect::<Vec<_>>();
        (&extended_storage[..], range.clone())
    } else {
        (template, range.clone())
    };

    let decomp = decompose_primer(oligo, &decomp_binding, strand, decomp_template);
    let three_prime_match = three_prime_matches(&decomp, k);

    if !three_prime_match || decomp.mismatches > settings.max_mismatches {
        return None;
    }

    Some(PrimerBinding {
        range: range.clone(),
        strand,
        mismatches: decomp.mismatches,
        three_prime_match,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use seqforge_core::{Primer, PrimerId};

    // template top strand: ATGCGTACCA (indices 0..10)
    const T: &[u8] = b"ATGCGTACCA";

    fn settings(min_k: usize, max_mm: usize) -> AnnealSettings {
        AnnealSettings {
            min_three_prime_match: min_k,
            max_mismatches: max_mm,
        }
    }

    #[test]
    fn forward_find_exact_match() {
        let sites = find_primer_binding_sites("GCGTAC", T, false, settings(4, 0));
        let fwd = sites
            .iter()
            .find(|s| s.strand == Strand::Forward)
            .expect("forward site");
        assert_eq!(fwd.range, 2..8);
        assert_eq!(fwd.mismatches, 0);
        assert!(fwd.three_prime_match);
    }

    #[test]
    fn forward_find_no_seed_when_three_prime_differs() {
        // top[2..8] = GCGTAC; oligo with wrong 3' end should not seed with k=4.
        let sites = find_primer_binding_sites("GCGTAT", T, false, settings(4, 4));
        assert!(
            sites.iter().all(|s| s.range != (2..8) || s.mismatches > 0),
            "wrong 3' k-mer must not seed a clean hit at 2..8"
        );
    }

    #[test]
    fn reverse_find_revcomp_seed() {
        let sites = find_primer_binding_sites("GTACGC", T, false, settings(4, 0));
        let rev = sites
            .iter()
            .find(|s| s.strand == Strand::Reverse)
            .expect("reverse site");
        assert_eq!(rev.range, 2..8);
        assert_eq!(rev.mismatches, 0);
    }

    #[test]
    fn reverse_top_strand_oligo_not_found_clean() {
        let sites = find_primer_binding_sites("GCGTAC", T, false, settings(4, 0));
        assert!(
            !sites
                .iter()
                .any(|s| s.strand == Strand::Reverse && s.mismatches == 0),
            "top-strand bases must not seed a clean reverse hit"
        );
    }

    #[test]
    fn mismatch_tolerance_filters_candidates() {
        // One mismatch in the 5' portion; 3' k-mer GTAC still seeds at 2..8.
        let oligo = "GAGTAC"; // mismatch at column 3 (oligo A vs template G)
        let strict = find_primer_binding_sites(oligo, T, false, settings(4, 0));
        let lenient = find_primer_binding_sites(oligo, T, false, settings(4, 1));
        assert!(
            strict
                .iter()
                .all(|s| !(s.range == (2..8) && s.strand == Strand::Forward)),
            "strict settings should drop the 1-mismatch site"
        );
        assert!(
            lenient
                .iter()
                .any(|s| s.range == (2..8) && s.strand == Strand::Forward && s.mismatches == 1)
        );
    }

    #[test]
    fn circular_wrap_around() {
        // Circular seq where a 6-mer spans the origin (mirrors search.rs).
        let circ = b"AATTCNNNNNNNNNNG"; // len 16
        let sites = find_primer_binding_sites("GAATTC", circ, true, settings(4, 0));
        let wrap = sites.iter().find(|s| s.range.start == 15);
        assert!(
            wrap.is_some(),
            "should find wrap-around site; got: {sites:?}"
        );
    }

    #[test]
    fn classify_confirmed_clean_binding() {
        let primer = Primer {
            id: PrimerId(1),
            name: "p1".into(),
            sequence: "GCGTAC".into(),
            binding: Some(2..8),
            strand: Strand::Forward,
            qualifiers: Default::default(),
        };
        let att = classify_attachment(&primer, T, false, settings(4, 0));
        assert_eq!(att.state, AttachmentState::Confirmed);
        assert!(att.off_target_sites.is_empty());
    }

    #[test]
    fn classify_drifted_with_mismatches_within_tolerance() {
        let primer = Primer {
            id: PrimerId(1),
            name: "p1".into(),
            sequence: "GAGTAC".into(),
            binding: Some(2..8),
            strand: Strand::Forward,
            qualifiers: Default::default(),
        };
        let att = classify_attachment(&primer, T, false, settings(4, 1));
        assert_eq!(att.state, AttachmentState::Drifted);
    }

    #[test]
    fn classify_drifted_when_binding_moved() {
        // True site at 2..8; stored binding is wrong but still has some overlap.
        let primer = Primer {
            id: PrimerId(1),
            name: "p1".into(),
            sequence: "GCGTAC".into(),
            binding: Some(0..6),
            strand: Strand::Forward,
            qualifiers: Default::default(),
        };
        let att = classify_attachment(&primer, T, false, settings(4, 0));
        assert_eq!(att.state, AttachmentState::Drifted);
        assert!(!att.off_target_sites.is_empty());
    }

    #[test]
    fn classify_detached_no_viable_site() {
        let primer = Primer {
            id: PrimerId(1),
            name: "p1".into(),
            sequence: "ZZZZZZ".into(),
            binding: Some(2..8),
            strand: Strand::Forward,
            qualifiers: Default::default(),
        };
        let att = classify_attachment(&primer, T, false, settings(4, 0));
        assert_eq!(att.state, AttachmentState::Detached);
    }

    #[test]
    fn classify_detached_no_binding() {
        let primer = Primer {
            id: PrimerId(1),
            name: "p1".into(),
            sequence: "GCGTAC".into(),
            binding: None,
            strand: Strand::Forward,
            qualifiers: Default::default(),
        };
        let att = classify_attachment(&primer, T, false, settings(4, 0));
        assert_eq!(att.state, AttachmentState::Detached);
    }

    #[test]
    fn off_target_reported_when_confirmed() {
        // Template with two forward GCGTAC sites.
        let seq = b"GCGTACNNGCGTAC";
        let primer = Primer {
            id: PrimerId(1),
            name: "p1".into(),
            sequence: "GCGTAC".into(),
            binding: Some(0..6),
            strand: Strand::Forward,
            qualifiers: Default::default(),
        };
        let att = classify_attachment(&primer, seq, false, settings(4, 0));
        assert_eq!(att.state, AttachmentState::Confirmed);
        assert_eq!(att.off_target_sites.len(), 1);
        assert_eq!(att.off_target_sites[0].range, 8..14);
    }
}
