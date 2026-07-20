//! Hand-written types for ligation-fidelity scoring.

/// Published frequency matrix (enzyme / condition / overhang length).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[allow(non_camel_case_types)] // match published condition labels (T4_25C_18h, …)
pub enum Dataset {
    /// Potapov 2018 T4, 25 °C, 18 h — NEB Viewer default (4-nt).
    T4_25C_18h,
    /// Potapov 2018 T4, 25 °C, 1 h (4-nt).
    T4_25C_01h,
    /// Potapov 2018 T4, 37 °C, 18 h (4-nt).
    T4_37C_18h,
    /// Potapov 2018 T4, 37 °C, 1 h (4-nt).
    T4_37C_01h,
    /// Pryor 2020 Golden Gate with BsaI (4-nt).
    BsaI,
    /// Pryor 2020 Golden Gate with BsmBI (4-nt).
    BsmBI,
    /// Pryor 2020 Golden Gate with Esp3I (4-nt).
    Esp3I,
    /// Pryor 2020 Golden Gate with BbsI (4-nt).
    BbsI,
    /// Pryor 2020 Golden Gate with SapI (3-nt).
    SapI,
}

impl Dataset {
    /// Overhang length this matrix covers (3 or 4).
    pub fn overhang_len(self) -> u8 {
        match self {
            Dataset::SapI => 3,
            _ => 4,
        }
    }

    /// Stable CLI / UI id (snake-ish).
    pub fn id(self) -> &'static str {
        match self {
            Dataset::T4_25C_18h => "t4_25c_18h",
            Dataset::T4_25C_01h => "t4_25c_01h",
            Dataset::T4_37C_18h => "t4_37c_18h",
            Dataset::T4_37C_01h => "t4_37c_01h",
            Dataset::BsaI => "bsai",
            Dataset::BsmBI => "bsmbi",
            Dataset::Esp3I => "esp3i",
            Dataset::BbsI => "bbsi",
            Dataset::SapI => "sapi",
        }
    }

    /// Short label for dropdowns.
    pub fn label(self) -> &'static str {
        match self {
            Dataset::T4_25C_18h => "T4 25C 18h (default)",
            Dataset::T4_25C_01h => "T4 25C 01h",
            Dataset::T4_37C_18h => "T4 37C 18h",
            Dataset::T4_37C_01h => "T4 37C 01h",
            Dataset::BsaI => "BsaI",
            Dataset::BsmBI => "BsmBI",
            Dataset::Esp3I => "Esp3I",
            Dataset::BbsI => "BbsI",
            Dataset::SapI => "SapI (3-base)",
        }
    }

    /// Parse a CLI / UI id (case-insensitive). Also accepts enzyme display names.
    pub fn parse(s: &str) -> Option<Self> {
        let t = s.trim();
        if t.eq_ignore_ascii_case("t4") || t.eq_ignore_ascii_case("default") {
            return Some(Dataset::T4_25C_18h);
        }
        for d in Dataset::ALL {
            if t.eq_ignore_ascii_case(d.id()) || t.eq_ignore_ascii_case(d.label()) {
                return Some(d);
            }
        }
        // Enzyme-style names without the 3-base suffix.
        if t.eq_ignore_ascii_case("SapI") {
            return Some(Dataset::SapI);
        }
        None
    }

    /// Every shipped dataset (UI enumeration order).
    pub const ALL: [Dataset; 9] = [
        Dataset::T4_25C_18h,
        Dataset::T4_25C_01h,
        Dataset::T4_37C_18h,
        Dataset::T4_37C_01h,
        Dataset::BsaI,
        Dataset::BsmBI,
        Dataset::Esp3I,
        Dataset::BbsI,
        Dataset::SapI,
    ];

    /// True when every overhang has this dataset's length (and is A/C/G/T).
    pub fn covers(self, overhangs: &[&[u8]]) -> bool {
        let n = self.overhang_len() as usize;
        !overhangs.is_empty()
            && overhangs
                .iter()
                .all(|o| o.len() == n && o.iter().all(|&b| is_acgt(b)))
    }
}

fn is_acgt(b: u8) -> bool {
    matches!(b.to_ascii_uppercase(), b'A' | b'C' | b'G' | b'T')
}

/// Per-junction on-target fraction within the set.
#[derive(Debug, Clone, PartialEq)]
pub struct JunctionScore {
    /// Overhang scored (5′→3′).
    pub overhang: Vec<u8>,
    /// `M[h][rc(h)] / Σ_j M[h][labels[j]]` (NEB Viewer; palindrome self counted twice in denom).
    pub fidelity: f64,
    pub on_target: u32,
    pub off_target: u32,
}

/// Result of [`crate::junction_fidelity`].
#[derive(Debug, Clone, PartialEq)]
pub struct FidelityReport {
    /// Product of per-junction fidelities (0..1). `None` if any junction was uncovered.
    pub set_fidelity: Option<f64>,
    pub junctions: Vec<JunctionScore>,
    /// Index into `junctions` of the weakest scored junction.
    pub worst: Option<usize>,
    /// Overhangs that could not be scored (wrong length / non-ACGT).
    pub uncovered: Vec<Vec<u8>>,
    /// RC-expanded subset matrix the % was computed from (NEB Viewer axes).
    pub matrix: Option<SubsetMatrix>,
}

/// Subset ligation-frequency matrix (inputs + reverse complements as axis labels).
///
/// Palindromic overhangs appear **twice** on the axes (NEB Ligase Fidelity Viewer).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubsetMatrix {
    pub labels: Vec<Vec<u8>>,
    /// Row-major counts, length `labels.len()²`.
    pub counts: Vec<u32>,
}

impl SubsetMatrix {
    pub fn dim(&self) -> usize {
        self.labels.len()
    }

    pub fn get(&self, row: usize, col: usize) -> u32 {
        self.counts[row * self.dim() + col]
    }
}
