//! DNA → protein translation (standard genetic code, NCBI table 1).
//!
//! Translation is **derived, read-only data** (ROADMAP decision 8): a pure
//! function of DNA + frame + strand, computed on demand, never stored on
//! `core`, never on the mutation/undo path, never serialized. The GUI shows it
//! for CDS features; the `seqforge translate` CLI command exposes it for files.

use seqforge_core::Strand;

/// Concrete bases an IUPAC nucleotide code stands for (uppercase; `U`→`T`).
/// Returns an empty slice for gaps / unknown bytes so the caller yields `X`.
fn iupac_bases(b: u8) -> &'static [u8] {
    match b.to_ascii_uppercase() {
        b'A' => b"A",
        b'C' => b"C",
        b'G' => b"G",
        b'T' | b'U' => b"T",
        b'R' => b"AG",
        b'Y' => b"CT",
        b'S' => b"CG",
        b'W' => b"AT",
        b'K' => b"GT",
        b'M' => b"AC",
        b'B' => b"CGT",
        b'D' => b"AGT",
        b'H' => b"ACT",
        b'V' => b"ACG",
        b'N' => b"ACGT",
        _ => b"",
    }
}

/// Map a codon to its amino acid, resolving IUPAC ambiguity by consensus: expand
/// each position to its possible bases, translate every concrete combination,
/// and return the amino acid only if they all agree — otherwise `X` (the
/// EMBOSS/BioPython convention). So `GGN`→`G`, `CTN`→`L`, `MGR`→`R`, `TAR`→`*`,
/// but a codon spanning two amino acids (`RAT` = Asn|Asp) → `X`. A base that
/// isn't a valid IUPAC code (gap, `N`-adjacent junk) also yields `X`.
fn codon_to_aa(codon: &[u8]) -> u8 {
    let (b0, b1, b2) = (
        iupac_bases(codon[0]),
        iupac_bases(codon[1]),
        iupac_bases(codon[2]),
    );
    if b0.is_empty() || b1.is_empty() || b2.is_empty() {
        return b'X';
    }
    let mut consensus: Option<u8> = None;
    for &x in b0 {
        for &y in b1 {
            for &z in b2 {
                let aa = concrete_codon_to_aa(x, y, z);
                match consensus {
                    None => consensus = Some(aa),
                    Some(prev) if prev == aa => {}
                    Some(_) => return b'X', // maps to >1 amino acid
                }
            }
        }
    }
    consensus.unwrap_or(b'X')
}

/// Standard genetic code (NCBI transl_table 1) over **concrete** bases only —
/// the three args are unambiguous uppercase `A`/`C`/`G`/`T` (the caller expands
/// IUPAC codes and normalizes `U`→`T` before calling).
fn concrete_codon_to_aa(a: u8, b: u8, c: u8) -> u8 {
    let c = [a, b, c];
    match &c {
        b"TTT" | b"TTC" => b'F',
        b"TTA" | b"TTG" | b"CTT" | b"CTC" | b"CTA" | b"CTG" => b'L',
        b"ATT" | b"ATC" | b"ATA" => b'I',
        b"ATG" => b'M',
        b"GTT" | b"GTC" | b"GTA" | b"GTG" => b'V',
        b"TCT" | b"TCC" | b"TCA" | b"TCG" | b"AGT" | b"AGC" => b'S',
        b"CCT" | b"CCC" | b"CCA" | b"CCG" => b'P',
        b"ACT" | b"ACC" | b"ACA" | b"ACG" => b'T',
        b"GCT" | b"GCC" | b"GCA" | b"GCG" => b'A',
        b"TAT" | b"TAC" => b'Y',
        b"TAA" | b"TAG" | b"TGA" => b'*',
        b"CAT" | b"CAC" => b'H',
        b"CAA" | b"CAG" => b'Q',
        b"AAT" | b"AAC" => b'N',
        b"AAA" | b"AAG" => b'K',
        b"GAT" | b"GAC" => b'D',
        b"GAA" | b"GAG" => b'E',
        b"TGT" | b"TGC" => b'C',
        b"TGG" => b'W',
        b"CGT" | b"CGC" | b"CGA" | b"CGG" | b"AGA" | b"AGG" => b'R',
        b"GGT" | b"GGC" | b"GGA" | b"GGG" => b'G',
        _ => b'X',
    }
}

/// Translate `seq` to a protein string.
///
/// - `strand`: [`Strand::Reverse`] translates the reverse complement (5'→3' on
///   the bottom strand); anything else translates `seq` as given.
/// - `codon_start`: the GenBank convention — `1`, `2`, or `3` — the 1-based
///   position (after strand handling) of the first base of the first codon.
///   Values outside `1..=3` are clamped into range.
///
/// Stop codons render as `*` and are included (callers decide whether to trim).
/// A trailing partial codon is dropped.
pub fn translate(seq: &[u8], strand: Strand, codon_start: usize) -> String {
    let oriented = match strand {
        Strand::Reverse => crate::reverse_complement(seq),
        _ => seq.to_vec(),
    };
    let offset = codon_start.clamp(1, 3) - 1;
    if oriented.len() <= offset {
        return String::new();
    }
    oriented[offset..]
        .chunks_exact(3)
        .map(|codon| codon_to_aa(codon) as char)
        .collect()
}

/// An open reading frame located by [`find_orfs`]. Coordinates are always on
/// the **forward** strand (0-based, half-open) and include the stop codon, so an
/// ORF can be annotated as a CDS feature directly. Translation is derived data
/// (decision 8) — ORFs are *not* stored; this is a pure analysis.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Orf {
    pub start: usize,
    pub end: usize,
    pub strand: Strand,
    /// Reading frame within the oriented strand: 1, 2, or 3.
    pub frame: usize,
    /// Number of amino acids (Met through the last residue before the stop).
    pub aa_len: usize,
}

/// Scan a protein string for ORFs, returning `(start_aa_idx, stop_aa_idx)` pairs
/// where `stop_aa_idx` is the `*` position.
///
/// - `met_to_stop`: each ORF runs from the first `M` after a stop (or sequence
///   start) to the next `*` — the standard "longest ORF from the first Met".
/// - otherwise (`stop_to_stop`): each inter-stop segment is an ORF regardless of
///   a start codon.
fn scan_orf_indices(protein: &str, met_to_stop: bool) -> Vec<(usize, usize)> {
    let mut orfs = Vec::new();
    if met_to_stop {
        let mut current_met: Option<usize> = None;
        for (i, ch) in protein.char_indices() {
            if ch == '*' {
                if let Some(m) = current_met.take() {
                    orfs.push((m, i));
                }
            } else if ch == 'M' && current_met.is_none() {
                current_met = Some(i);
            }
        }
    } else {
        let mut seg_start = 0usize;
        for (i, ch) in protein.char_indices() {
            if ch == '*' {
                orfs.push((seg_start, i));
                seg_start = i + 1;
            }
        }
    }
    orfs
}

/// Find open reading frames in all 3 forward frames (and 3 reverse frames when
/// `include_reverse`). `min_aa` filters by protein length; `met_to_stop`
/// selects Met→stop (default) vs stop→stop. Results are forward-coordinate and
/// sorted by position — ready to render as lanes or annotate as CDS features.
pub fn find_orfs(seq: &[u8], min_aa: usize, met_to_stop: bool, include_reverse: bool) -> Vec<Orf> {
    let len = seq.len();
    let strands: &[Strand] = if include_reverse {
        &[Strand::Forward, Strand::Reverse]
    } else {
        &[Strand::Forward]
    };
    let mut orfs = Vec::new();
    for &strand in strands {
        let oriented = match strand {
            Strand::Reverse => crate::reverse_complement(seq),
            _ => seq.to_vec(),
        };
        for offset in 0..3usize {
            let protein = translate(&oriented, Strand::Forward, offset + 1);
            for (m, t) in scan_orf_indices(&protein, met_to_stop) {
                let aa_len = t - m;
                if aa_len < min_aa {
                    continue;
                }
                // Oriented-strand nt span (include the stop codon).
                let o_start = offset + 3 * m;
                let o_end = (offset + 3 * (t + 1)).min(len);
                // Map back to forward coordinates for reverse-strand ORFs.
                let (start, end) = match strand {
                    Strand::Reverse => (len - o_end, len - o_start),
                    _ => (o_start, o_end),
                };
                orfs.push(Orf {
                    start,
                    end,
                    strand,
                    frame: offset + 1,
                    aa_len,
                });
            }
        }
    }
    orfs.sort_by_key(|o| (o.start, o.end));
    orfs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forward_start_codon_and_stop() {
        // ATG AAA TAA → M K *
        assert_eq!(translate(b"ATGAAATAA", Strand::Forward, 1), "MK*");
    }

    #[test]
    fn trailing_partial_codon_dropped() {
        // ATG AA → M (the dangling "AA" is ignored)
        assert_eq!(translate(b"ATGAA", Strand::Forward, 1), "M");
    }

    #[test]
    fn codon_start_offsets_frame() {
        // codon_start=2 skips the leading G: ATG AAA → M K
        assert_eq!(translate(b"GATGAAA", Strand::Forward, 2), "MK");
    }

    #[test]
    fn reverse_strand_translates_bottom_strand() {
        // revcomp("TTATTTCAT") = "ATGAAATAA" → M K *
        assert_eq!(translate(b"TTATTTCAT", Strand::Reverse, 1), "MK*");
    }

    #[test]
    fn fully_ambiguous_codon_is_x() {
        assert_eq!(translate(b"NNN", Strand::Forward, 1), "X");
    }

    #[test]
    fn unambiguous_degenerate_codons_resolve() {
        // Four-fold degenerate boxes: the wobble base is irrelevant.
        assert_eq!(translate(b"GGN", Strand::Forward, 1), "G"); // Gly
        assert_eq!(translate(b"CTN", Strand::Forward, 1), "L"); // Leu
        assert_eq!(translate(b"TCN", Strand::Forward, 1), "S"); // Ser
        assert_eq!(translate(b"ACN", Strand::Forward, 1), "T"); // Thr
        // Leucine also via YTR (CTA/CTG/TTA/TTG).
        assert_eq!(translate(b"YTR", Strand::Forward, 1), "L");
        // Arginine via MGR (AGA/AGG/CGA/CGG).
        assert_eq!(translate(b"MGR", Strand::Forward, 1), "R");
        // Both stop codons in the box → still a stop.
        assert_eq!(translate(b"TAR", Strand::Forward, 1), "*"); // TAA/TAG
    }

    #[test]
    fn ambiguity_spanning_two_amino_acids_is_x() {
        // RAT = AAT (Asn) | GAT (Asp) → ambiguous → X.
        assert_eq!(translate(b"RAT", Strand::Forward, 1), "X");
    }

    #[test]
    fn find_orf_forward_met_to_stop() {
        // ATG AAA AAA TAA = M K K * → one ORF, 3 aa, frame 1, forward.
        let orfs = find_orfs(b"ATGAAAAAATAA", 1, true, false);
        assert_eq!(orfs.len(), 1);
        let o = &orfs[0];
        assert_eq!((o.start, o.end), (0, 12));
        assert_eq!(o.strand, Strand::Forward);
        assert_eq!(o.frame, 1);
        assert_eq!(o.aa_len, 3);
    }

    #[test]
    fn find_orf_min_length_filters() {
        // The 3-aa ORF above is excluded when min_aa = 4.
        assert!(find_orfs(b"ATGAAAAAATAA", 4, true, false).is_empty());
    }

    #[test]
    fn find_orf_reverse_strand_maps_to_forward_coords() {
        // revcomp = ATGAAATAA (M K *). The ORF lives on the reverse strand but
        // its coordinates are reported on the forward strand.
        let fwd = crate::reverse_complement(b"ATGAAATAA");
        let orfs = find_orfs(&fwd, 1, true, true);
        let rev: Vec<_> = orfs
            .iter()
            .filter(|o| o.strand == Strand::Reverse)
            .collect();
        assert_eq!(rev.len(), 1);
        assert_eq!((rev[0].start, rev[0].end), (0, fwd.len()));
        assert_eq!(rev[0].aa_len, 2); // M K (stop excluded)
    }

    #[test]
    fn u_treated_as_t() {
        assert_eq!(translate(b"AUG", Strand::Forward, 1), "M");
    }
}
