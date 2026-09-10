//! Seed-and-extend primer binding-site find + attachment-state classification
//! (Phase 1.1). Own result type [`PrimerBinding`] — never [`seqforge_core::SearchHit`].

use seqforge_core::{Primer, Span, Strand};

use super::{PrimerDecomposition, decompose_primer};

use crate::dna::{complement_byte, reverse_complement};

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
    /// Shortest footprint accepted when trimming a 5' tail. The 3'-anchored
    /// walk stops here rather than at [`min_three_prime_match`], because a
    /// seed-length "anneal" is noise, not a binding site — 4 nt matches
    /// somewhere in almost any template. Clamped up to `min_three_prime_match`
    /// and down to the oligo's length, so an oligo no longer than this floor is
    /// only ever tried at full length (and behaves exactly as it did before
    /// tail-trimming existed).
    ///
    /// [`min_three_prime_match`]: AnnealSettings::min_three_prime_match
    pub min_anneal_len: usize,
    /// How far the 3'→5' extension may fall below its best score before it
    /// stops (the X-drop of seed-and-extend). Scoring is +1 per paired base and
    /// −1 per mismatch, so a non-pairing 5' tail drives the score down and the
    /// footprint settles at the last base that still paid its way, while a
    /// single internal mismatch in an otherwise-annealing primer is absorbed
    /// and the extension continues.
    pub extend_drop_off: usize,
}

impl Default for AnnealSettings {
    fn default() -> Self {
        Self {
            min_three_prime_match: 8,
            max_mismatches: 4,
            min_anneal_len: 12,
            extend_drop_off: 5,
        }
    }
}

/// A primer's alignment to *some* template location — the find pass's own
/// result type (Consistency #4: never `core::SearchHit`, which lacks
/// mismatch/anchor data).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrimerBinding {
    /// Footprint on the template as a wrap-aware [`Span`] (a site crossing the
    /// origin is one wrapping span, not an `end > len` overflow range). The
    /// linear thermo engine ([`decompose_primer`] / [`super::anneal_tm`]) derives
    /// its contiguous `Range` from this at the call boundary — a documented
    /// linear-engine survivor per the three-tier rule (`docs/architecture.md`).
    pub span: Span,
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

/// Find all binding sites for `oligo` on `template` by seeding on the 3' k-mer
/// and extending 5'-ward for as long as the alignment scores.
///
/// The footprint is **not** assumed to be the oligo's length. A cloning primer
/// carries a 5' tail — a restriction site, an overhang, a homology arm — that
/// anneals to nothing, and scoring the tail as mismatches is what previously
/// made every such primer unfindable (a 24 nt BsaI primer with an 8 nt tail
/// scores 8 mismatches against a 4-mismatch budget). Each seed hit therefore
/// tries footprints from the full oligo down to the seed and keeps the longest
/// that scores; [`decompose_primer`] accounts for the remainder as
/// [`PrimerDecomposition::tail`](super::PrimerDecomposition::tail).
///
/// Longest-first makes this strictly additive: an untailed primer still binds
/// over its whole length with the same span and mismatch count as before.
///
/// For circular sequences, pass `circular = true`; a wrap-around hit is reported
/// as one wrapping [`Span`] (`span.wraps(template.len())`), not an `end > len`
/// overflow range.
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

    // Forward: 3' k-mer seeds at `p..p+k`; the footprint *ends* at `p + k` and
    // grows 5'-ward, so a length `l` starts `l` before that anchor.
    for p in find_exact_matches(search_seq, seed_fwd) {
        let anchor_end = p + k;
        try_add_best_candidate(
            &mut candidates,
            oligo_str,
            Anchor {
                at: anchor_end,
                strand: Strand::Forward,
            },
            template,
            circular,
            settings,
        );
    }

    // Reverse: revcomp(3' k-mer) seeds at `p..p+k`; the 3' anchor is `p` and the
    // footprint grows rightward in template coordinates.
    for p in find_exact_matches(search_seq, &seed_rev) {
        try_add_best_candidate(
            &mut candidates,
            oligo_str,
            Anchor {
                at: p,
                strand: Strand::Reverse,
            },
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
    let Some(binding) = primer.binding else {
        return PrimerAttachment {
            state: AttachmentState::Detached,
            off_target_sites: vec![],
        };
    };

    // The authored footprint as a linear range for the (linear) anneal engine —
    // primers don't yet anneal across the origin.
    let binding_range = binding.start..binding.start + binding.len;
    let k = settings.min_three_prime_match.min(primer.sequence.len());
    let stored_decomp = decompose_primer(&primer.sequence, &binding_range, primer.strand, template);
    let stored_ok = three_prime_matches(&stored_decomp, k)
        && stored_decomp.mismatches <= settings.max_mismatches;

    let all_sites = find_primer_binding_sites(&primer.sequence, template, circular, settings);

    if stored_ok {
        let off_target_sites: Vec<_> = all_sites
            .iter()
            .filter(|s| !same_site(s, binding, primer.strand))
            .cloned()
            .collect();

        let confirmed = stored_decomp.mismatches == 0
            && all_sites
                .iter()
                .any(|s| same_site(s, binding, primer.strand) && s.mismatches == 0);

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

/// Whether a found site is the *same priming event* as the stored binding.
///
/// Compared on the **3' anchor**, not the whole span — per the decomposition
/// rule ("anchors on the 3' terminus, never `binding.len()`"; see
/// `plans/primers.md` § Consistency, item 2). One oligo can anneal over
/// different lengths on different templates while priming from the same base:
/// a cloning primer whose 5' tail is untemplated anneals over its footprint on
/// the original template, but over its **whole length** on the PCR product it
/// made — the tail is templated there, because the product contains it.
///
/// Comparing spans instead flags that product's own primers as `Drifted` with an
/// off-target, which is how a correct primer ends up wearing a "moved" badge.
/// Drift is a change of *where* the oligo primes; the anchor is what encodes it.
pub(crate) fn same_site(site: &PrimerBinding, binding: Span, strand: Strand) -> bool {
    site.strand == strand
        && three_prime_anchor(site.span, strand) == three_prime_anchor(binding, strand)
}

/// The template column the oligo's 3' terminus sits at: the far end of the
/// footprint for a forward primer, its start for a reverse one (which runs
/// antiparallel).
fn three_prime_anchor(span: Span, strand: Strand) -> usize {
    match strand {
        Strand::Reverse => span.start,
        _ => span.start + span.len,
    }
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

/// Where a seed pinned the oligo's 3' terminus, in (possibly extended)
/// template coordinates. Forward anchors are exclusive ends, reverse anchors
/// are the 3' base's own column — the footprint grows away from `at` in
/// opposite directions, which is the only thing that differs per strand.
#[derive(Clone, Copy)]
struct Anchor {
    at: usize,
    strand: Strand,
}

impl Anchor {
    /// Template column of the base `len` positions in from the 3' terminus
    /// (`len` is 1-based). `None` once the footprint would run off a linear end.
    fn column(&self, len: usize, template_len: usize, circular: bool) -> Option<usize> {
        match self.strand {
            Strand::Reverse => {
                let c = self.at + len - 1;
                (circular || c < template_len).then(|| c % template_len)
            }
            _ => {
                let c = self.at.checked_sub(len)?;
                Some(c % template_len)
            }
        }
    }

    /// The span a footprint of `len` occupies, 3' anchor held fixed.
    fn span(&self, len: usize, template_len: usize) -> Span {
        let start = match self.strand {
            Strand::Reverse => self.at % template_len,
            _ => (self.at + template_len - len % template_len.max(1)) % template_len,
        };
        Span::new(start, len)
    }
}

/// Record the best footprint for one seed hit, found by 3'→5' extension.
fn try_add_best_candidate(
    candidates: &mut Vec<PrimerBinding>,
    oligo: &str,
    anchor: Anchor,
    template: &[u8],
    circular: bool,
    settings: AnnealSettings,
) {
    // The extension proposes every length at which the alignment reached a new
    // best score, worst-to-best. Take the best that also passes the mismatch
    // gate: a long tail can drift the running score to a new high while
    // accumulating more mismatches than `max_mismatches` allows, and giving up
    // there would lose the shorter, clean footprint underneath it.
    for len in extend_footprint(oligo, anchor, template, circular, settings)
        .into_iter()
        .rev()
    {
        let span = anchor.span(len, template.len());
        if candidates
            .iter()
            .any(|c| c.span == span && c.strand == anchor.strand)
        {
            return;
        }
        if let Some(binding) = score_candidate(oligo, span, anchor.strand, template, settings) {
            candidates.push(binding);
            return;
        }
    }
}

/// Extend the alignment 3'→5' from the seed, returning every footprint length
/// at which the running score (+1 per paired base, −1 per mismatch) reached a
/// new high — ascending, so the last entry is the best-scoring. Lengths below
/// [`AnnealSettings::min_anneal_len`] are dropped.
///
/// This is what separates a primer's annealing region from its 5' tail. A tail
/// pairs at roughly background rate, so the running score falls away and the
/// best length is the tail boundary; a fully-annealing oligo scores
/// monotonically upward and the best length is its whole length. One internal
/// mismatch costs 1 and is repaid by the *following* paired bases, so a
/// mutagenic primer whose mismatch is interior still extends to its full
/// footprint (the score climbs past the previous best a base later).
///
/// Ties keep the **shorter** footprint: bases are only claimed as annealed when
/// they strictly improve the alignment. Without that, the last base or two of a
/// tail get absorbed whenever they happen to pair — a 20 nt BsaI/SapI tail whose
/// 3' end reads `…CA` against a template `…CC` scores mismatch-then-match, nets
/// zero, and would otherwise be reported as a 31 nt footprint with a spurious
/// mismatch instead of the designed 29 nt clean anneal.
fn extend_footprint(
    oligo: &str,
    anchor: Anchor,
    template: &[u8],
    circular: bool,
    settings: AnnealSettings,
) -> Vec<usize> {
    let oligo_len = oligo.len();
    let template_len = template.len();
    if oligo_len == 0 || template_len == 0 {
        return Vec::new();
    }
    let floor = settings
        .min_anneal_len
        .max(settings.min_three_prime_match)
        .min(oligo_len)
        .max(1);

    let mut score: isize = 0;
    let mut best_score: isize = isize::MIN;
    let mut peaks: Vec<usize> = Vec::new();
    let drop_off = settings.extend_drop_off as isize;

    for len in 1..=oligo_len {
        let Some(column) = anchor.column(len, template_len, circular) else {
            break; // ran off a linear template end
        };
        let oligo_base = oligo.as_bytes()[oligo_len - len].to_ascii_uppercase();
        let target = match anchor.strand {
            Strand::Reverse => complement_byte(template[column]).to_ascii_uppercase(),
            _ => template[column].to_ascii_uppercase(),
        };

        score += if oligo_base == target { 1 } else { -1 };
        if score > best_score {
            best_score = score;
            if len >= floor {
                peaks.push(len);
            }
        } else if best_score - score > drop_off {
            break;
        }
    }

    peaks
}

fn score_candidate(
    oligo: &str,
    span: Span,
    strand: Strand,
    template: &[u8],
    settings: AnnealSettings,
) -> Option<PrimerBinding> {
    let template_len = template.len();
    let k = settings.min_three_prime_match.min(oligo.len());

    // Derive the linear binding `Range` the thermo engine needs. A wrapping span
    // (`start + len > template_len`) reads through the origin, so extend the
    // template by its tail and index it as one contiguous `start..start+len`.
    let end = span.start + span.len;
    let extended_storage;
    let (decomp_template, decomp_binding) = if end > template_len {
        let extend = end - template_len;
        extended_storage = template
            .iter()
            .chain(&template[..extend.min(template_len)])
            .copied()
            .collect::<Vec<_>>();
        (&extended_storage[..], span.start..end)
    } else {
        (template, span.start..end)
    };

    let decomp = decompose_primer(oligo, &decomp_binding, strand, decomp_template);
    let three_prime_match = three_prime_matches(&decomp, k);

    if !three_prime_match || decomp.mismatches > settings.max_mismatches {
        return None;
    }

    Some(PrimerBinding {
        span,
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
            ..Default::default()
        }
    }

    #[test]
    fn forward_find_exact_match() {
        let sites = find_primer_binding_sites("GCGTAC", T, false, settings(4, 0));
        let fwd = sites
            .iter()
            .find(|s| s.strand == Strand::Forward)
            .expect("forward site");
        assert_eq!(fwd.span, Span::from_range(2..8));
        assert_eq!(fwd.mismatches, 0);
        assert!(fwd.three_prime_match);
    }

    #[test]
    fn forward_find_no_seed_when_three_prime_differs() {
        // top[2..8] = GCGTAC; oligo with wrong 3' end should not seed with k=4.
        let sites = find_primer_binding_sites("GCGTAT", T, false, settings(4, 4));
        assert!(
            sites
                .iter()
                .all(|s| s.span != Span::from_range(2..8) || s.mismatches > 0),
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
        assert_eq!(rev.span, Span::from_range(2..8));
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
        // A mutagenic primer: one interior mismatch, clean on both sides, so
        // the extension carries through it and the whole oligo is the footprint.
        // `max_mismatches` is then what decides whether the site is reported.
        let template = b"TTTTTGGCATTACGCAGGATCCAAGTGCTAAAAA";
        let oligo = "GGCATTACGCAGGTTCCAAG"; // template[5..25] with A->T at index 13
        let strict = find_primer_binding_sites(oligo, template, false, settings(4, 0));
        let lenient = find_primer_binding_sites(oligo, template, false, settings(4, 1));
        assert!(
            strict
                .iter()
                .all(|s| !(s.span == Span::from_range(5..25) && s.strand == Strand::Forward)),
            "strict settings should drop the 1-mismatch site"
        );
        assert!(
            lenient.iter().any(|s| s.span == Span::from_range(5..25)
                && s.strand == Strand::Forward
                && s.mismatches == 1),
            "an interior mismatch must not truncate the footprint: {lenient:?}"
        );
    }

    /// A tail whose outermost bases net to zero against the template must not
    /// be absorbed into the footprint. This is the real 6H8-VH-1F shape: reading
    /// outward from the anneal boundary the template gives a mismatch and then a
    /// match, so a longest-on-ties rule would report a 31 nt footprint with a
    /// spurious mismatch instead of the designed 29 nt clean anneal.
    #[test]
    fn coincidental_tail_pairing_is_not_claimed_as_annealed() {
        let anneal = "ATGGCGGAAGTGCAGCTGTTAGAGTCTGG"; // 29 nt
        let tail = "AGGTCTCAGGAGGCTCTTCA"; // 20 nt: BsaI + overhang + SapI
        let template = format!("AAACC{anneal}TTTTT");
        let oligo = format!("{tail}{anneal}");

        let sites = find_primer_binding_sites(
            &oligo,
            template.as_bytes(),
            false,
            AnnealSettings::default(),
        );
        let fwd = sites
            .iter()
            .find(|s| s.strand == Strand::Forward)
            .expect("should bind");
        assert_eq!(fwd.mismatches, 0, "no spurious mismatch: {fwd:?}");
        assert_eq!(
            fwd.span,
            Span::from_range(5..34),
            "the designed 29 nt anneal"
        );
        assert_eq!(oligo.len() - fwd.span.len, tail.len());
    }

    #[test]
    fn circular_wrap_around() {
        // Circular seq where a 6-mer spans the origin (mirrors search.rs).
        let circ = b"AATTCNNNNNNNNNNG"; // len 16
        let sites = find_primer_binding_sites("GAATTC", circ, true, settings(4, 0));
        let wrap = sites.iter().find(|s| s.span.start == 15);
        assert!(
            wrap.is_some(),
            "should find wrap-around site; got: {sites:?}"
        );
        // P5c: the wrap site is one wrapping Span (start 15, len 6 on L=16), not an
        // `end > len` overflow range.
        let wrap = wrap.unwrap();
        assert_eq!(wrap.span, Span::new(15, 6));
        assert!(wrap.span.wraps(16), "site crosses the origin");
    }

    #[test]
    fn wrapping_span_flows_through_same_site_matching() {
        // P5c representational guarantee: a wrap-around binding is one wrapping
        // Span that flows through the `same_site`/attached predicate by Span
        // equality — no `end > len` overflow encoding. (Full across-origin
        // *thermo* of a stored wrapping binding remains a documented follow-up.)
        let circ = b"AATTCNNNNNNNNNNG"; // len 16; GAATTC wraps 15..16 ∪ 0..5
        let sites = find_primer_binding_sites("GAATTC", circ, true, settings(4, 0));
        let binding = Span::new(15, 6);
        // Exactly one found site is the wrapping footprint, matched by Span.
        let matched: Vec<_> = sites
            .iter()
            .filter(|s| same_site(s, binding, Strand::Forward))
            .collect();
        assert_eq!(matched.len(), 1, "wrap site matched by span: {sites:?}");
        assert!(matched[0].span.wraps(16));
    }

    #[test]
    fn classify_confirmed_clean_binding() {
        let primer = Primer {
            id: PrimerId(1),
            name: "p1".into(),
            sequence: "GCGTAC".into(),
            binding: Some(seqforge_core::Span::from_range(2..8)),
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
            binding: Some(seqforge_core::Span::from_range(2..8)),
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
            binding: Some(seqforge_core::Span::from_range(0..6)),
            strand: Strand::Forward,
            qualifiers: Default::default(),
        };
        let att = classify_attachment(&primer, T, false, settings(4, 0));
        assert_eq!(att.state, AttachmentState::Drifted);
        assert!(!att.off_target_sites.is_empty());
    }

    /// A cloning primer on the product it amplified: its 5' tail is templated
    /// there, so the oligo anneals over its **whole** length while the stored
    /// binding is the footprint it had on the original template. Same 3' anchor,
    /// so it is one priming event — `Confirmed`, not `Drifted` with a phantom
    /// off-target. This is what `pcr::prepare` hands every amplicon.
    #[test]
    fn full_length_anneal_over_a_stored_footprint_is_the_same_site() {
        // Product layout: 5 nt tail + 20 nt that annealed on the old template.
        let anneal = "GGCATTACGCAGGATCCAAG";
        let product = format!("TTTTT{anneal}");
        let oligo = format!("TTTTT{anneal}");
        let primer = Primer {
            id: PrimerId(1),
            name: "tailed".into(),
            sequence: oligo.clone(),
            // The footprint as re-homed from the template: tail excluded.
            binding: Some(Span::from_range(5..25)),
            strand: Strand::Forward,
            qualifiers: Default::default(),
        };

        let sites =
            find_primer_binding_sites(&oligo, product.as_bytes(), false, AnnealSettings::default());
        assert_eq!(
            sites[0].span,
            Span::from_range(0..25),
            "on the product the whole oligo pairs"
        );

        let att = classify_attachment(
            &primer,
            product.as_bytes(),
            false,
            AnnealSettings::default(),
        );
        assert_eq!(att.state, AttachmentState::Confirmed);
        assert!(
            att.off_target_sites.is_empty(),
            "the longer anneal is the same priming event, not an off-target: {:?}",
            att.off_target_sites
        );
    }

    /// The reverse-strand equivalent: a reverse primer's 3' anchor is its
    /// footprint's *start*, so a longer anneal grows away from it.
    #[test]
    fn full_length_reverse_anneal_is_the_same_site() {
        // revcomp(oligo) sits at the product's 3' end: anneal then tail.
        let product = "GGCATTACGCAGGATCCAAGAAAAA";
        let oligo = "TTTTTCTTGGATCCTGCGTAATGCC";
        let primer = Primer {
            id: PrimerId(1),
            name: "tailed_rev".into(),
            sequence: oligo.into(),
            binding: Some(Span::from_range(0..20)),
            strand: Strand::Reverse,
            qualifiers: Default::default(),
        };
        let att = classify_attachment(
            &primer,
            product.as_bytes(),
            false,
            AnnealSettings::default(),
        );
        assert_eq!(att.state, AttachmentState::Confirmed);
        assert!(
            att.off_target_sites.is_empty(),
            "{:?}",
            att.off_target_sites
        );
    }

    /// The anchor rule must not blur two genuinely different priming events that
    /// happen to overlap — drift is a change of *where* the oligo primes.
    #[test]
    fn a_site_at_a_different_anchor_is_still_an_off_target() {
        // GCGTAC twice: anchors at 6 and 14.
        let template = b"GCGTACNNGCGTAC";
        let primer = Primer {
            id: PrimerId(1),
            name: "p1".into(),
            sequence: "GCGTAC".into(),
            binding: Some(Span::from_range(0..6)),
            strand: Strand::Forward,
            qualifiers: Default::default(),
        };
        let att = classify_attachment(&primer, template, false, settings(4, 0));
        assert_eq!(att.state, AttachmentState::Confirmed);
        assert_eq!(att.off_target_sites.len(), 1);
        assert_eq!(att.off_target_sites[0].span, Span::from_range(8..14));
    }

    #[test]
    fn classify_detached_no_viable_site() {
        let primer = Primer {
            id: PrimerId(1),
            name: "p1".into(),
            sequence: "ZZZZZZ".into(),
            binding: Some(seqforge_core::Span::from_range(2..8)),
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

    /// A Golden Gate primer: 8 nt BsaI tail + overhang, then 20 nt that anneal.
    /// Before tail-trimming this found nothing at all — the tail scored as 8
    /// mismatches against a 4-mismatch budget.
    #[test]
    fn tailed_cloning_primer_reports_only_its_annealed_footprint() {
        let template = b"CCCCCCATGGCGGAAGTGCAGCTGTTAGAGTCTGGAGGCTTTTTT";
        // 8 nt tail (BsaI + overhang) + template[6..30].
        let oligo = "GGTCTCAGATGGCGGAAGTGCAGCTGTTAGAG";
        let sites = find_primer_binding_sites(oligo, template, false, AnnealSettings::default());
        let fwd = sites
            .iter()
            .find(|s| s.strand == Strand::Forward)
            .expect("tailed primer should bind");
        assert_eq!(fwd.mismatches, 0);
        assert_eq!(fwd.span, Span::from_range(6..30), "annealed region only");
        // The 5' tail is exactly the bases the footprint left over.
        assert_eq!(oligo.len() - fwd.span.len, 8);
    }

    /// The reverse-strand equivalent, anchored at the other end.
    #[test]
    fn tailed_reverse_primer_reports_only_its_annealed_footprint() {
        let template = b"CCCCCCATGGCGGAAGTGCAGCTGTTAGAGTCTGGAGGCTTTTTT";
        // 8 nt tail + revcomp(template[6..30]): anneals to the same window on
        // the bottom strand, so the reported span matches the forward case.
        let oligo = "GGTCTCAGCTCTAACAGCTGCACTTCCGCCAT";
        let sites = find_primer_binding_sites(oligo, template, false, AnnealSettings::default());
        let rev = sites
            .iter()
            .find(|s| s.strand == Strand::Reverse)
            .expect("tailed reverse primer should bind");
        assert_eq!(rev.mismatches, 0);
        assert_eq!(rev.span, Span::from_range(6..30));
        assert_eq!(oligo.len() - rev.span.len, 8);
    }

    /// The trimming walk must not manufacture a site out of a seed-length
    /// coincidence: nothing shorter than `min_anneal_len` counts.
    #[test]
    fn does_not_report_a_footprint_below_the_anneal_floor() {
        let template = b"CCCCCCCCCCCCCCCCCCCCGTACCCCCCCCCCCCCCCCCCCC";
        // Only the 3' "GTAC" can pair; everything 5' of it is a mismatch run.
        let oligo = "AAAAAAAAAAAAAAAAGTAC";
        let sites = find_primer_binding_sites(
            oligo,
            template,
            false,
            AnnealSettings {
                min_three_prime_match: 4,
                max_mismatches: 0,
                min_anneal_len: 12,
                ..Default::default()
            },
        );
        assert!(sites.is_empty(), "4 nt of pairing is not a binding site");
    }

    /// An oligo no longer than the floor is only tried at full length, so the
    /// pre-trimming result is preserved exactly.
    #[test]
    fn short_oligo_behaviour_is_unchanged_by_trimming() {
        let sites = find_primer_binding_sites("GCGTAC", T, false, settings(4, 0));
        assert_eq!(sites.len(), 1);
        assert_eq!(sites[0].span, Span::from_range(2..8));
        assert_eq!(sites[0].span.len, 6);
    }

    #[test]
    fn off_target_reported_when_confirmed() {
        // Template with two forward GCGTAC sites.
        let seq = b"GCGTACNNGCGTAC";
        let primer = Primer {
            id: PrimerId(1),
            name: "p1".into(),
            sequence: "GCGTAC".into(),
            binding: Some(seqforge_core::Span::from_range(0..6)),
            strand: Strand::Forward,
            qualifiers: Default::default(),
        };
        let att = classify_attachment(&primer, seq, false, settings(4, 0));
        assert_eq!(att.state, AttachmentState::Confirmed);
        assert_eq!(att.off_target_sites.len(), 1);
        assert_eq!(att.off_target_sites[0].span, Span::from_range(8..14));
    }
}
