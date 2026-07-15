//! Enzyme data model.
//!
//! The codegen output (`enzymes_generated.rs`) populates a static `&[Enzyme]`
//! table using these types. All recognition sequences and offsets are
//! expressed in **forward-strand coordinates** so the scanner can apply the
//! same arithmetic to forward and reverse-complement orientations.

/// IUPAC-coded base. `#[repr(u8)]` so the byte form is the ASCII letter,
/// which keeps `recognition` literals readable in the generated file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Iupac {
    A = b'A',
    C = b'C',
    G = b'G',
    T = b'T',
    R = b'R', // A or G
    Y = b'Y', // C or T
    S = b'S', // G or C
    W = b'W', // A or T
    K = b'K', // G or T
    M = b'M', // A or C
    B = b'B', // not A
    D = b'D', // not C
    H = b'H', // not G
    V = b'V', // not T
    N = b'N', // any
}

impl Iupac {
    /// True iff `base` (an ASCII A/C/G/T byte) matches this IUPAC class.
    /// Non-ACGT input bases are treated as non-matching — the scanner only
    /// feeds in concrete bases from the sequence, so ambiguity codes don't
    /// appear on the sequence side.
    #[inline]
    pub fn matches(self, base: u8) -> bool {
        let b = base.to_ascii_uppercase();
        match self {
            Iupac::A => b == b'A',
            Iupac::C => b == b'C',
            Iupac::G => b == b'G',
            Iupac::T => b == b'T',
            Iupac::R => b == b'A' || b == b'G',
            Iupac::Y => b == b'C' || b == b'T',
            Iupac::S => b == b'G' || b == b'C',
            Iupac::W => b == b'A' || b == b'T',
            Iupac::K => b == b'G' || b == b'T',
            Iupac::M => b == b'A' || b == b'C',
            Iupac::B => matches!(b, b'C' | b'G' | b'T'),
            Iupac::D => matches!(b, b'A' | b'G' | b'T'),
            Iupac::H => matches!(b, b'A' | b'C' | b'T'),
            Iupac::V => matches!(b, b'A' | b'C' | b'G'),
            Iupac::N => matches!(b, b'A' | b'C' | b'G' | b'T'),
        }
    }

    /// Reverse complement.
    #[inline]
    pub fn complement(self) -> Iupac {
        match self {
            Iupac::A => Iupac::T,
            Iupac::T => Iupac::A,
            Iupac::C => Iupac::G,
            Iupac::G => Iupac::C,
            Iupac::R => Iupac::Y,
            Iupac::Y => Iupac::R,
            Iupac::S => Iupac::S,
            Iupac::W => Iupac::W,
            Iupac::K => Iupac::M,
            Iupac::M => Iupac::K,
            Iupac::B => Iupac::V,
            Iupac::V => Iupac::B,
            Iupac::D => Iupac::H,
            Iupac::H => Iupac::D,
            Iupac::N => Iupac::N,
        }
    }
}

/// REBASE-classified enzyme type. Restricted to the subset we surface in
/// SeqForge — Type II (palindromic, cuts inside the recognition site) and
/// Type IIs (asymmetric, cuts outside the recognition site).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnzymeType {
    /// Palindromic recognition; cut is symmetric about the midpoint.
    TypeII,
    /// Asymmetric recognition; enzyme cuts outside the recognition site at
    /// fixed offsets. Used for Golden Gate / MoClo / scarless assembly.
    TypeIIs,
}

/// Effect of one methylation system on cleavage, normalized from REBASE's
/// `damlist` `sensitivity?` summary (`cut` / `blocked` / `impaired` / `some
/// blocked` / `some impaired` / `variable` / `-`). The "some"/context dependence
/// REBASE reports at the enzyme level is resolved per-site by the evaluator
/// (`methylation::site_methyl_state`), which ANDs this with whether the
/// methylatable base actually falls inside a given occurrence of the site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MethylEffect {
    /// Cleaves normally under this methylation (REBASE `cut`, or `-` = no overlap).
    Cut,
    /// Cleavage slowed but not abolished (REBASE `impaired` / `some impaired`).
    Impaired,
    /// Cleavage abolished where the context is present (REBASE `blocked` / `some blocked`).
    Blocked,
    /// Conflicting reports (REBASE `variable`) — surfaced as a caution, not a hard block.
    Variable,
    /// No sourced data for this enzyme/system.
    Untested,
}

/// Per-system methylation sensitivity for one enzyme. A **required** field on
/// `Enzyme` (never `Option`) so codegen must assign every enzyme a value —
/// "enzyme missing its sensitivity data" is unrepresentable; the coverage-gate
/// test rejects `Untested` outside a reviewed allowlist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MethylSensitivity {
    pub dam: MethylEffect,
    pub dcm: MethylEffect,
    pub cpg: MethylEffect,
}

/// One restriction enzyme.
///
/// `top_offset` and `bottom_offset` are signed positions measured from the
/// **5′ end of `recognition` on the forward strand**, in the "cut between
/// offset and offset+1" convention (matching REBASE's 1-based position
/// encoding):
///
///   * For a palindromic 6-cutter like EcoRI (`GAATTC`, `RS = "GAATTC, 1"`):
///     `top_offset = 1`, `bottom_offset = 6 - 1 = 5`.
///   * For a Type IIs cutter like BsaI (`GGTCTC`, `RS = "GGTCTC, 7; GAGACC, -5"`):
///     `top_offset = 7`, `bottom_offset = recognition.len() - (-5) = 11`.
///     The top strand is cut 1 base past the recognition end; the bottom
///     strand is cut 5 bases past — giving a 4-base 5′ overhang.
///
/// This single convention works for blunt cutters (top == bottom),
/// 5′ overhangs (bottom > top), and 3′ overhangs (bottom < top).
#[derive(Debug, Clone, Copy)]
pub struct Enzyme {
    pub name: &'static str,
    pub recognition: &'static [Iupac],
    pub top_offset: i16,
    pub bottom_offset: i16,
    pub enzyme_type: EnzymeType,
    pub methylation: MethylSensitivity,
}

impl Enzyme {
    #[inline]
    pub fn len(&self) -> usize {
        self.recognition.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.recognition.is_empty()
    }

    /// Overhang produced by this enzyme. Computed from `top_offset` vs
    /// `bottom_offset`; doesn't depend on the recognition site.
    pub fn overhang_kind(&self) -> OverhangKind {
        use std::cmp::Ordering;
        match self.top_offset.cmp(&self.bottom_offset) {
            Ordering::Equal => OverhangKind::Blunt,
            Ordering::Less => OverhangKind::FivePrime((self.bottom_offset - self.top_offset) as u8),
            Ordering::Greater => {
                OverhangKind::ThreePrime((self.top_offset - self.bottom_offset) as u8)
            }
        }
    }
}

/// Overhang geometry classification (overhang sequence itself is not
/// captured here — it depends on the specific site found in a sequence, and
/// Tier 2 `digest()` will return it on `Fragment`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverhangKind {
    Blunt,
    /// 5′ overhang of N bases.
    FivePrime(u8),
    /// 3′ overhang of N bases.
    ThreePrime(u8),
}

// ── Site ──────────────────────────────────────────────────────────────────────

/// A found restriction site.
///
/// Positions are 0-based offsets into the input sequence. `recognition_end`
/// is exclusive (Rust range convention). `top_cut` and `bottom_cut` are the
/// absolute sequence positions where the top and bottom strands are cleaved.
///
/// `strand` records whether the enzyme matched the forward sequence or its
/// reverse complement — important for non-palindromic Type IIs enzymes whose
/// staple geometry mirrors when found on the reverse strand.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Site {
    pub enzyme: &'static str,
    pub recognition_start: usize,
    pub recognition_end: usize,
    pub top_cut: usize,
    pub bottom_cut: usize,
    pub strand: SiteStrand,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SiteStrand {
    Forward,
    Reverse,
}
