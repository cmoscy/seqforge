use seqforge_core::{CutSite, SearchHit, Strand};
use seqforge_restriction::find_all_sites;

// ── IUPAC matching ────────────────────────────────────────────────────────────

fn iupac_matches(pattern_byte: u8, seq_byte: u8) -> bool {
    let p = pattern_byte.to_ascii_uppercase();
    let s = seq_byte.to_ascii_uppercase();
    match p {
        b'A' => s == b'A',
        b'T' => s == b'T',
        b'G' => s == b'G',
        b'C' => s == b'C',
        b'R' => matches!(s, b'A' | b'G'),
        b'Y' => matches!(s, b'C' | b'T'),
        b'S' => matches!(s, b'G' | b'C'),
        b'W' => matches!(s, b'A' | b'T'),
        b'K' => matches!(s, b'G' | b'T'),
        b'M' => matches!(s, b'A' | b'C'),
        b'B' => matches!(s, b'C' | b'G' | b'T'),
        b'D' => matches!(s, b'A' | b'G' | b'T'),
        b'H' => matches!(s, b'A' | b'C' | b'T'),
        b'V' => matches!(s, b'A' | b'C' | b'G'),
        b'N' | b'X' => true,
        _ => s == p,
    }
}

fn hamming_iupac(pattern: &[u8], seq_slice: &[u8]) -> u8 {
    pattern
        .iter()
        .zip(seq_slice)
        .filter(|&(&p, &s)| !iupac_matches(p, s))
        .count() as u8
}

// ── Sequence search ───────────────────────────────────────────────────────────

/// Find all IUPAC pattern matches in `seq`, on both strands.
///
/// For circular sequences, pass `circular = true` to detect sites that span
/// the origin. Positions are 0-based; `end` may exceed `seq.len()` for
/// wrap-around hits — the renderer clamps to visible range.
///
/// Note: uses a simple O(n·m) scan; adequate for plasmid-scale sequences.
pub fn find_iupac_matches(
    seq: &[u8],
    pattern: &[u8],
    mismatches: u8,
    circular: bool,
) -> Vec<SearchHit> {
    let pat_len = pattern.len();
    let seq_len = seq.len();
    if pat_len == 0 || seq_len == 0 || pat_len > seq_len {
        return vec![];
    }

    let rc_pat = crate::reverse_complement(pattern);
    // Palindromic patterns have the same RC — search them only once.
    let search_rc = !rc_pat.eq_ignore_ascii_case(pattern);

    // For circular sequences, append the first `pat_len - 1` bases so sites
    // spanning the origin are found in one pass.
    let extended: Vec<u8>;
    let search_seq: &[u8] = if circular && pat_len > 1 {
        extended = seq.iter().chain(seq[..pat_len - 1].iter()).copied().collect();
        &extended
    } else {
        seq
    };

    let search_end = search_seq.len().saturating_sub(pat_len) + 1;
    let mut hits = Vec::new();

    for i in 0..search_end {
        let slice = &search_seq[i..i + pat_len];
        let start = i % seq_len;
        let end = start + pat_len;

        if hamming_iupac(pattern, slice) <= mismatches {
            hits.push(SearchHit { start, end, strand: Strand::Forward });
        }
        if search_rc && hamming_iupac(&rc_pat, slice) <= mismatches {
            hits.push(SearchHit { start, end, strand: Strand::Reverse });
        }
    }

    hits
}

// ── Restriction site finding ──────────────────────────────────────────────────

/// Find cut sites for the named restriction enzymes in `seq`.
///
/// Names are matched case-insensitively against the embedded REBASE table.
/// Unknown enzyme names are silently skipped (the caller should validate
/// names and surface an error if nothing matched).
///
/// For circular sequences, pass `circular = true` to detect sites spanning
/// the origin.
pub fn find_cut_sites(seq: &[u8], enzyme_names: &[&str], circular: bool) -> Vec<CutSite> {
    if seq.is_empty() || enzyme_names.is_empty() {
        return vec![];
    }
    let lookups: Vec<_> = enzyme_names
        .iter()
        .filter_map(|n| seqforge_restriction::enzyme_by_name(n))
        .collect();
    if lookups.is_empty() {
        return vec![];
    }
    let sites = find_all_sites(seq, &lookups, circular);
    sites.into_iter().map(site_to_cutsite).collect()
}

/// Bridge: convert the new `seqforge_restriction::Site` to the existing
/// `seqforge_core::CutSite` shape the renderer and view state already know
/// how to consume. Keeps the `na_seq → seqforge-restriction` migration
/// invisible to upstream callers.
pub(crate) fn site_to_cutsite(s: seqforge_restriction::Site) -> CutSite {
    CutSite {
        enzyme: s.enzyme.to_string(),
        recognition_start: s.recognition_start,
        recognition_end: s.recognition_end,
        cut_pos: s.top_cut,
        bottom_cut_pos: s.bottom_cut,
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // EcoRI recognition: GAATTC, cuts after G (cut_after=0) → cut at pos 1
    const ECORI_SITE: &[u8] = b"GAATTC";

    #[test]
    fn exact_forward_hit() {
        let seq = b"AAAGAATTCAAA";
        let hits = find_iupac_matches(seq, b"GAATTC", 0, false);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].start, 3);
        assert_eq!(hits[0].end, 9);
        assert_eq!(hits[0].strand, Strand::Forward);
    }

    #[test]
    fn palindrome_not_double_counted() {
        let seq = b"GAATTC";
        let hits = find_iupac_matches(seq, ECORI_SITE, 0, false);
        assert_eq!(hits.len(), 1, "palindromic site must not be double-counted");
    }

    #[test]
    fn reverse_complement_hit() {
        let seq = b"CCTTTTGG";
        let hits = find_iupac_matches(seq, b"AAAA", 0, false);
        assert!(hits.iter().any(|h| h.strand == Strand::Reverse));
    }

    #[test]
    fn iupac_n_wildcard() {
        let seq = b"GAANTC";
        let hits = find_iupac_matches(seq, b"GAANTC", 0, false);
        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn mismatch_allowance() {
        let seq = b"GAACTC";
        let hits_0 = find_iupac_matches(seq, b"GAATTC", 0, false);
        let hits_1 = find_iupac_matches(seq, b"GAATTC", 1, false);
        assert!(hits_0.is_empty());
        assert_eq!(hits_1.len(), 1);
    }

    #[test]
    fn circular_wrap_around() {
        let seq = b"AATTCNNNNNNNNNNG"; // len 16, 'G' at pos 15
        let hits = find_iupac_matches(seq, ECORI_SITE, 0, true);
        let wrap = hits.iter().find(|h| h.start == 15);
        assert!(wrap.is_some(), "should find wrap-around site; got: {hits:?}");
    }

    #[test]
    fn find_ecori_cut_sites() {
        let seq = b"AAAGAATTCAAA";
        let sites = find_cut_sites(seq, &["EcoRI"], false);
        assert_eq!(sites.len(), 1);
        assert_eq!(sites[0].enzyme, "EcoRI");
        assert_eq!(sites[0].recognition_start, 3);
        assert_eq!(sites[0].recognition_end, 9);
        assert_eq!(sites[0].cut_pos, 4);
        assert_eq!(sites[0].bottom_cut_pos, 8);
    }

    #[test]
    fn unknown_enzyme_returns_empty() {
        let seq = b"AAAGAATTCAAA";
        let sites = find_cut_sites(seq, &["NotAnEnzyme"], false);
        assert!(sites.is_empty());
    }

    #[test]
    fn enzyme_name_case_insensitive() {
        let seq = b"AAAGAATTCAAA";
        let sites = find_cut_sites(seq, &["ecori"], false);
        assert_eq!(sites.len(), 1);
    }

    #[test]
    fn multiple_enzymes() {
        // EcoRI: GAATTC, BamHI: GGATCC
        let seq = b"AAAGAATTCAAAGGATCCAAA";
        let sites = find_cut_sites(seq, &["EcoRI", "BamHI"], false);
        assert_eq!(sites.len(), 2);
    }

    #[test]
    fn bsai_type_iis_found_via_find_cut_sites() {
        // 30 bases; GGTCTC at position 5 — well within range for the
        // bottom cut at position 5 + 11 = 16.
        //          0         1         2
        //          0123456789012345678901234567890
        let seq = b"AAAAAGGTCTCAAAAAAAAAAAAAAAAAAA";
        let sites = find_cut_sites(seq, &["BsaI"], false);
        assert_eq!(sites.len(), 1, "BsaI Type IIs should be found via bridge");
        assert_eq!(sites[0].cut_pos, 12);    // 5 + 7
        assert_eq!(sites[0].bottom_cut_pos, 16); // 5 + 11
    }
}
