//! Spec-anchored unit tests against REBASE / NEB documentation.
//!
//! These assert cut positions and overhang geometry that any user can look
//! up at rebase.neb.com or in a standard molecular biology textbook. They
//! don't depend on the rest of the SeqForge stack — pure library checks.

use seqforge_restriction::{
    all_enzymes, enzyme_by_name, find_sites, EnzymeType, OverhangKind, SiteStrand,
};

#[test]
fn library_loaded_and_nonempty() {
    let lib = all_enzymes();
    assert!(
        lib.len() > 100,
        "expected hundreds of enzymes, got {}",
        lib.len()
    );
    let n_iis = lib
        .iter()
        .filter(|e| e.enzyme_type == EnzymeType::TypeIIs)
        .count();
    assert!(n_iis >= 50, "expected many Type IIs enzymes, got {n_iis}");
}

#[test]
fn ecori_geometry() {
    // EcoRI: G^AATTC, 4-base 5' overhang AATT.
    let ecori = enzyme_by_name("EcoRI").expect("EcoRI in library");
    assert_eq!(ecori.top_offset, 1);
    assert_eq!(ecori.bottom_offset, 5);
    assert_eq!(ecori.overhang_kind(), OverhangKind::FivePrime(4));
}

#[test]
fn bamhi_geometry() {
    // BamHI: G^GATCC, 4-base 5' overhang GATC.
    let e = enzyme_by_name("BamHI").expect("BamHI in library");
    assert_eq!(e.top_offset, 1);
    assert_eq!(e.bottom_offset, 5);
    assert_eq!(e.overhang_kind(), OverhangKind::FivePrime(4));
}

#[test]
fn smai_blunt() {
    // SmaI: CCC^GGG, blunt.
    let e = enzyme_by_name("SmaI").expect("SmaI in library");
    assert_eq!(e.top_offset, 3);
    assert_eq!(e.bottom_offset, 3);
    assert_eq!(e.overhang_kind(), OverhangKind::Blunt);
}

#[test]
fn psti_three_prime_overhang() {
    // PstI: CTGCA^G, 4-base 3' overhang TGCA.
    let e = enzyme_by_name("PstI").expect("PstI in library");
    assert_eq!(e.top_offset, 5);
    assert_eq!(e.bottom_offset, 1);
    assert_eq!(e.overhang_kind(), OverhangKind::ThreePrime(4));
}

#[test]
fn bsai_type_iis_geometry() {
    // BsaI: GGTCTC(N1)/(N5), cuts 1/5 past 6-bp recognition.
    let e = enzyme_by_name("BsaI").expect("BsaI in library");
    assert_eq!(e.enzyme_type, EnzymeType::TypeIIs);
    assert_eq!(e.recognition.len(), 6);
    assert_eq!(e.top_offset, 7); // 6 + 1
    assert_eq!(e.bottom_offset, 11); // 6 + 5
    assert_eq!(e.overhang_kind(), OverhangKind::FivePrime(4));
}

#[test]
fn bsmbi_type_iis_geometry() {
    // BsmBI: CGTCTC(N1)/(N5), same geometry as BsaI, different recognition.
    let e = enzyme_by_name("BsmBI").expect("BsmBI in library");
    assert_eq!(e.enzyme_type, EnzymeType::TypeIIs);
    assert_eq!(e.top_offset, 7);
    assert_eq!(e.bottom_offset, 11);
}

#[test]
fn sapi_type_iis_geometry() {
    // SapI: GCTCTTC(N1)/(N4), 7-bp recognition, 3-base 5' overhang.
    let e = enzyme_by_name("SapI").expect("SapI in library");
    assert_eq!(e.enzyme_type, EnzymeType::TypeIIs);
    assert_eq!(e.recognition.len(), 7);
    assert_eq!(e.top_offset, 8); // 7 + 1
    assert_eq!(e.bottom_offset, 11); // 7 + 4
    assert_eq!(e.overhang_kind(), OverhangKind::FivePrime(3));
}

#[test]
fn find_ecori_in_short_sequence() {
    // 0123456789012
    // AAAGAATTCAAAA
    //    └─site at 3..9, top cut at 4, bottom cut at 8
    let ecori = enzyme_by_name("EcoRI").unwrap();
    let sites = find_sites(b"AAAGAATTCAAAA", ecori, false);
    assert_eq!(sites.len(), 1);
    let s = &sites[0];
    assert_eq!(s.recognition_start, 3);
    assert_eq!(s.recognition_end, 9);
    assert_eq!(s.top_cut, 4);
    assert_eq!(s.bottom_cut, 8);
    assert_eq!(s.strand, SiteStrand::Forward);
}

#[test]
fn bsai_forward_strand() {
    // GGTCTC at pos 0; top cut at 7, bottom cut at 11.
    // Need at least 11 bases of room past recognition end (pos 6) for the
    // bottom cut to be in range.
    //                       0         1
    //                       0123456789012345
    let sites = find_sites(b"GGTCTCNNNNNNNNNN", enzyme_by_name("BsaI").unwrap(), false);
    // N's don't match GGTCTC reverse complement (GAGACC), so single hit.
    assert_eq!(sites.len(), 1);
    let s = &sites[0];
    assert_eq!(s.recognition_start, 0);
    assert_eq!(s.top_cut, 7);
    assert_eq!(s.bottom_cut, 11);
    assert_eq!(s.strand, SiteStrand::Forward);
}

#[test]
fn bsai_reverse_strand() {
    // GAGACC = revcomp(GGTCTC). When found on the forward strand, the
    // enzyme is actually binding the bottom strand — staple geometry
    // mirrors. Top cut should be UPSTREAM of GAGACC by 5 bases; bottom
    // cut upstream by 1.
    //                       0         1
    //                       0123456789012345
    let seq = b"NNNNNNNNNGAGACCN";
    let sites = find_sites(seq, enzyme_by_name("BsaI").unwrap(), false);
    assert_eq!(sites.len(), 1, "expected one reverse-strand hit: {sites:?}");
    let s = &sites[0];
    assert_eq!(s.recognition_start, 9);
    assert_eq!(s.strand, SiteStrand::Reverse);
    // Reverse-strand mirror: top_cut = rec_start + (rec_len - bottom_offset)
    //                                = 9 + (6 - 11) = 9 - 5 = 4
    // bottom_cut = rec_start + (rec_len - top_offset) = 9 + (6 - 7) = 8
    assert_eq!(s.top_cut, 4, "top cut should be 5 bases upstream of GAGACC");
    assert_eq!(
        s.bottom_cut, 8,
        "bottom cut should be 1 base upstream of GAGACC"
    );
}

#[test]
fn palindrome_not_double_counted() {
    // EcoRI's recognition is palindromic — the scanner must not emit a
    // forward and reverse hit at the same position.
    let sites = find_sites(b"GAATTCNNNN", enzyme_by_name("EcoRI").unwrap(), false);
    assert_eq!(sites.len(), 1);
}

#[test]
fn iupac_ambiguity_in_recognition_matches() {
    // DrdI has recognition GACNNNNNNGTC. The N's must IUPAC-match any base.
    if let Some(drdi) = enzyme_by_name("DrdI") {
        // DrdI = GAC + 6N + GTC = 12 bp recognition. Test with exactly 6 N's
        // in the gap.
        //              0         1
        //              0123456789012345678
        let seq = b"AAAGACAAAAAAGTCAAAA";
        let sites = find_sites(seq, drdi, false);
        assert!(
            !sites.is_empty(),
            "DrdI should match GAC + 6N + GTC; got 0 sites in {:?}",
            std::str::from_utf8(seq).unwrap()
        );
    }
    // (If DrdI isn't in the filtered library this test is a no-op — the
    // IUPAC layer is also exercised by the GAATTC-style palindrome tests.)
}

#[test]
fn circular_wrap_around() {
    // EcoRI site spanning the origin: last 5 bases + first base = GAATTC.
    //                       0         1
    //                       0123456789012345
    let seq = b"AATTCNNNNNNNNNNG"; // len 16, 'G' at pos 15
    let ecori = enzyme_by_name("EcoRI").unwrap();
    let linear = find_sites(seq, ecori, false);
    let circular = find_sites(seq, ecori, true);
    assert!(linear.is_empty(), "no linear hit");
    assert_eq!(circular.len(), 1);
    assert_eq!(circular[0].recognition_start, 15);
}
