/// IUPAC complement table (ASCII byte → complement byte, uppercase output).
/// Handles A/T/G/C plus all IUPAC ambiguity codes.
fn complement_byte(b: u8) -> u8 {
    match b.to_ascii_uppercase() {
        b'A' => b'T',
        b'T' => b'A',
        b'G' => b'C',
        b'C' => b'G',
        b'U' => b'A',
        b'R' => b'Y', // A|G  →  T|C
        b'Y' => b'R', // C|T  →  G|A
        b'S' => b'S', // G|C  →  C|G
        b'W' => b'W', // A|T  →  T|A
        b'K' => b'M', // G|T  →  C|A
        b'M' => b'K', // A|C  →  T|G
        b'B' => b'V', // C|G|T →  G|C|A
        b'D' => b'H', // A|G|T →  T|C|A
        b'H' => b'D', // A|C|T →  T|G|A
        b'V' => b'B', // A|C|G →  T|G|C
        b'N' => b'N',
        other => other, // pass through gaps, unknown
    }
}

/// Return the reverse complement of a raw DNA byte sequence.
/// Input may be upper or lowercase; output is always uppercase.
pub fn reverse_complement(seq: &[u8]) -> Vec<u8> {
    seq.iter().rev().map(|&b| complement_byte(b)).collect()
}

/// Return the complement of a raw DNA byte sequence (same order, not reversed).
/// Input may be upper or lowercase; output is always uppercase.
pub fn complement(seq: &[u8]) -> Vec<u8> {
    seq.iter().map(|&b| complement_byte(b)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple_revcomp() {
        assert_eq!(reverse_complement(b"ATGC"), b"GCAT");
        assert_eq!(reverse_complement(b"AAAAAA"), b"TTTTTT");
    }

    #[test]
    fn iupac_ambiguity() {
        // N → N, R (A|G) → Y (T|C), Y → R
        assert_eq!(reverse_complement(b"NRYW"), b"WRYN");
    }

    #[test]
    fn lowercase_input() {
        assert_eq!(reverse_complement(b"atgc"), b"GCAT");
    }

    #[test]
    fn empty() {
        assert_eq!(reverse_complement(b""), b"");
    }
}
