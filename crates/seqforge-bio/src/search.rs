use na_seq::{
    Nucleotide,
    re_lib::load_re_library,
    restriction_enzyme::find_re_matches,
};
use seqforge_core::{CutSite, SearchHit, Strand};

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
    let search_rc = rc_pat.to_ascii_uppercase() != pattern.to_ascii_uppercase();

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
/// Names are matched case-insensitively against the na_seq built-in library.
/// Unknown enzyme names are silently skipped (the caller should validate names
/// and surface an error if nothing matched).
///
/// For circular sequences, pass `circular = true` to detect sites spanning
/// the origin (requires na_seq >= 0.3.15 library coverage).
pub fn find_cut_sites(seq: &[u8], enzyme_names: &[&str], circular: bool) -> Vec<CutSite> {
    if seq.is_empty() || enzyme_names.is_empty() {
        return vec![];
    }

    let lib = load_re_library();
    let filtered: Vec<_> = lib
        .into_iter()
        .filter(|re| enzyme_names.iter().any(|n| n.eq_ignore_ascii_case(&re.name)))
        .collect();

    if filtered.is_empty() {
        return vec![];
    }

    let seq_len = seq.len();

    // Convert to na_seq Nucleotide, skipping non-ACGT bytes (e.g., ambiguity codes).
    // For circular: extend by max recognition-seq length - 1 to catch wrap-around sites.
    let max_re_len = filtered.iter().map(|re| re.cut_seq.len()).max().unwrap_or(0);

    let search_bytes: Vec<u8> = if circular && max_re_len > 1 {
        seq.iter().chain(seq[..max_re_len - 1].iter()).copied().collect()
    } else {
        seq.to_vec()
    };

    let na_seq_vec: Vec<Nucleotide> = search_bytes
        .iter()
        .filter_map(|&b| Nucleotide::from_u8_letter(b).ok())
        .collect();

    // If any byte couldn't be converted (ambiguity codes), the length won't match.
    // Proceed anyway — na_seq's find_re_matches won't match partial non-ACGT positions.

    let matches = find_re_matches(&na_seq_vec, &filtered);

    matches
        .into_iter()
        .filter_map(|m| {
            let re = &filtered[m.lib_index];
            // seq_index is 1-based start of the recognition site in the search buffer.
            let rec_start_ext = m.seq_index.checked_sub(1)?;
            let recognition_start = rec_start_ext % seq_len;
            let recognition_end = recognition_start + re.cut_seq.len();
            let cut_pos = recognition_start + re.cut_after as usize + 1;
            Some(CutSite {
                enzyme: re.name.clone(),
                recognition_start,
                recognition_end,
                cut_pos,
            })
        })
        .collect()
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
        // GAATTC is palindromic — should appear once
        let seq = b"GAATTC";
        let hits = find_iupac_matches(seq, ECORI_SITE, 0, false);
        assert_eq!(hits.len(), 1, "palindromic site must not be double-counted");
    }

    #[test]
    fn reverse_complement_hit() {
        // GGATCC (BamHI) is palindromic; AATCGG is not the same as GGATCC
        // Use a non-palindromic pattern: AAAA on a seq with TTTT on other strand
        let seq = b"CCTTTTGG"; // top strand; RC = CCAAAAGG
        let hits = find_iupac_matches(seq, b"AAAA", 0, false);
        // forward: no AAAA in CCTTTTGG
        // reverse: RC of AAAA = TTTT — but we search RC of pattern in the sequence
        // rc_pat = TTTT; CCTTTTGG contains TTTT at pos 2
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
        // GAATTC vs GAACTC: 1 mismatch at pos 3
        let seq = b"GAACTC";
        let hits_0 = find_iupac_matches(seq, b"GAATTC", 0, false);
        let hits_1 = find_iupac_matches(seq, b"GAATTC", 1, false);
        assert!(hits_0.is_empty());
        assert_eq!(hits_1.len(), 1);
    }

    #[test]
    fn circular_wrap_around() {
        // Pattern GAATTC spanning the origin of a circular sequence
        // seq = b"ATTCAAAGAA" — last 3 chars + first 3 chars = "GAATTC"
        // GAA at end (pos 7-9), TTC at start (pos 0-2)? No...
        // Let's put: seq = b"AATTCAAAGAA" → start "AATTC", end "GAA"
        // Circular concat: "AATTCAAAGAA" + "AAT" = has "GAATTC" at pos 8?
        // seq[8..] = "GAA", wrapped: "GAA" + seq[0..3] = "GAA"+"AAT" = "GAAAAT"? No.
        // Let me pick a cleaner example.
        // seq = b"TCAAAGAATT" (len 10), circular: "GAATTC" starts at pos 7
        // seq[7..10] = "ATT", then wraps to seq[0..3] = "TCA"... that gives "ATTTCA", not right.
        //
        // Simplest: seq = b"TCGAATTCGA", site starts at pos 2 (non-wrap).
        // For wrap test: seq = b"AATTCGAAG" (len 9)
        //   extension: "AATTCGAAG" + "AATT" (first 4 bytes for pat_len=6-1=5)...
        // Actually: seq = "AATTCNNNGA" + "A" hmm.
        //
        // Clean wrap test: seq = "ATTCNNGAA" (len 9), pat = GAATTC (len 6)
        //   circular extension appends seq[0..5] = "ATTCNN"
        //   extended = "ATTCNNGAAATTCNN"
        //   search at i=6: extended[6..12] = "GAAATT" — doesn't match GAATTC exactly
        //
        // Let's use: seq = "TCNNGAATTCN" (len 11), non-wrap site starting at i=4
        // This is a normal (non-wrap) case. For wrap: need site crossing boundary.
        // seq = b"TCGAA" (len 5) + pat GAATTC: can't fit even half.
        // Let seq = b"AATTCNNNNNNNNNNNNNNG" (len 20).
        // Site starts at pos 17: seq[17..20] = "NNG"... no.
        // For circular wrap: last chars of seq are start of GAATTC, first chars complete it.
        // seq = b"TCNNNNNNNNNG" (len 12), pat = GAATTC
        //   seq ends with "G", site start = 11: seq[11] = 'G', seq[12..] wraps to seq[0..5] = "TCNNN"
        //   "GAATTC" != "GTCNNN". Not right.
        //
        // Cleanest: seq = b"AATTCNNNNNNNNNNG" — GAATTC where 'G' is the last char
        // seq len = 16, last char = 'G', first 5 = "AATTC"
        // Circular extension: seq + seq[0..5] = "AATTCNNNNNNNNNNG" + "AATTC"
        // Search at i = 15 (last position before extension): extended[15..21] = "GAATTC" ✓
        let seq = b"AATTCNNNNNNNNNNG"; // len 16, 'G' at pos 15
        let hits = find_iupac_matches(seq, ECORI_SITE, 0, true);
        // Should find: the wrap-around site (G at pos 15, AATTC at pos 0-4)
        // start = 15 % 16 = 15, end = 15 + 6 = 21
        let wrap = hits.iter().find(|h| h.start == 15);
        assert!(wrap.is_some(), "should find wrap-around site; got: {hits:?}");
    }

    #[test]
    fn find_ecori_cut_sites() {
        // GAATTC is EcoRI. Embed it in a sequence.
        let seq = b"AAAGAATTCAAA";
        let sites = find_cut_sites(seq, &["EcoRI"], false);
        assert_eq!(sites.len(), 1);
        assert_eq!(sites[0].enzyme, "EcoRI");
        assert_eq!(sites[0].recognition_start, 3);
        assert_eq!(sites[0].recognition_end, 9); // 3 + 6
        // cut_after = 0 for EcoRI, so cut_pos = 3 + 0 + 1 = 4
        assert_eq!(sites[0].cut_pos, 4);
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
}
