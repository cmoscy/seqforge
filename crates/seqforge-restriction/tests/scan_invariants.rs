//! Property / invariant tests for the scanner.

use seqforge_restriction::{enzyme_by_name, find_sites, SiteStrand};

/// Helper: simple reverse complement of ASCII ACGT bytes.
fn revcomp(seq: &[u8]) -> Vec<u8> {
    seq.iter()
        .rev()
        .map(|&b| match b.to_ascii_uppercase() {
            b'A' => b'T',
            b'T' => b'A',
            b'C' => b'G',
            b'G' => b'C',
            other => other,
        })
        .collect()
}

#[test]
fn site_count_commutes_with_revcomp_for_palindromes() {
    // EcoRI's recognition is palindromic — same count on forward and
    // reverse-complement of the same sequence.
    let ecori = enzyme_by_name("EcoRI").unwrap();
    let seq = b"AAAGAATTCAAATCGAATTCNNN";
    let n_forward = find_sites(seq, ecori, false).len();
    let n_revcomp = find_sites(&revcomp(seq), ecori, false).len();
    assert_eq!(n_forward, n_revcomp);
}

#[test]
fn site_count_commutes_with_revcomp_for_type_iis() {
    // A Type IIs enzyme: count on forward seq equals count on its
    // reverse complement (sites swap strands but the total is the same).
    let bsai = enzyme_by_name("BsaI").unwrap();
    let seq = b"AAAGGTCTCAAAATTTTGAGACCAAAAAAA";
    let n_forward = find_sites(seq, bsai, false).len();
    let n_revcomp = find_sites(&revcomp(seq), bsai, false).len();
    assert_eq!(
        n_forward, n_revcomp,
        "site count must be invariant under revcomp"
    );
}

#[test]
fn forward_and_reverse_strands_swap_under_revcomp() {
    // Specifically: a forward-strand site at position p in `seq` becomes a
    // reverse-strand site at position (seq.len() - p - rec_len) in
    // revcomp(seq).
    let bsai = enzyme_by_name("BsaI").unwrap();
    let seq = b"AAAGGTCTCAAAAAAAAAAAA";
    let rc = revcomp(seq);
    let fwd = find_sites(seq, bsai, false);
    let bwd = find_sites(&rc, bsai, false);
    assert_eq!(fwd.len(), 1);
    assert_eq!(bwd.len(), 1);
    assert_eq!(fwd[0].strand, SiteStrand::Forward);
    assert_eq!(bwd[0].strand, SiteStrand::Reverse);
    let rec_len = bsai.recognition.len();
    let expected_rc_start = seq.len() - fwd[0].recognition_start - rec_len;
    assert_eq!(bwd[0].recognition_start, expected_rc_start);
}

#[test]
fn no_phantom_sites_in_pure_aaaa() {
    // A homopolymer should produce no sites for any of the common
    // 6-cutters with mixed-base recognition.
    let seq = vec![b'A'; 100];
    for name in ["EcoRI", "BamHI", "HindIII", "PstI", "SmaI", "BsaI", "BsmBI"] {
        let e = enzyme_by_name(name).unwrap();
        let n = find_sites(&seq, e, false).len();
        assert_eq!(n, 0, "{name} should not cut homopolymer A");
    }
}

#[test]
fn linear_drops_off_end_sites_circular_keeps_them() {
    // A site that would span the origin only appears with `circular = true`.
    let ecori = enzyme_by_name("EcoRI").unwrap();
    //                  0         1
    //                  0123456789012345
    let seq = b"AATTCNNNNNNNNNNG"; // GAATTC spans pos 15 → wrap
    assert_eq!(find_sites(seq, ecori, false).len(), 0);
    assert_eq!(find_sites(seq, ecori, true).len(), 1);
}

#[test]
fn type_iis_cut_in_range_for_short_seq() {
    // BsaI's cut is 11 bases past recognition start. A short sequence with
    // GGTCTC at position 0 and only 11 total bases should NOT report a
    // site — the cuts would land at or past seq end.
    let bsai = enzyme_by_name("BsaI").unwrap();
    let too_short = find_sites(b"GGTCTCNNNN", bsai, false); // 10 bases
    assert!(too_short.is_empty(), "cuts past seq end must be dropped");

    let exact = find_sites(b"GGTCTCNNNNNN", bsai, false); // 12 bases — bottom cut at 11 fits
    assert_eq!(exact.len(), 1);
}

#[test]
fn count_per_enzyme_matches_find_sites() {
    // The fast `count_sites_per_enzyme` path must agree with the full
    // `find_sites` path for the same enzyme set.
    use seqforge_restriction::count_sites_per_enzyme;
    let seq = b"AAAGAATTCAAAGGATCCAAATTTAAGCTTAAA";
    let enzymes: Vec<_> = ["EcoRI", "BamHI", "HindIII"]
        .iter()
        .map(|n| enzyme_by_name(n).unwrap())
        .collect();
    let counted = count_sites_per_enzyme(seq, &enzymes, false);
    for (e, c) in &counted {
        let n = find_sites(seq, e, false).len();
        assert_eq!(n, *c, "count mismatch for {}", e.name);
    }
}
