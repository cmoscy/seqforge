//! Primer **construction** (Phase 2.2 — "tail composition") — the third primer
//! concern alongside `evaluate` (QC) and `anneal` (find/classify).
//!
//! Pure, headless, **name-based** builders that assemble oligo tail bytes from a
//! restriction enzyme + user inputs. Name-based on purpose: the app depends on
//! `seqforge-bio` but **not** `seqforge-restriction`, so these functions are the
//! seam through which the Inspector reaches enzyme geometry without ever seeing
//! `Enzyme`/`Iupac`.
//!
//! Scope boundary (ROADMAP decision 16 / primers decision 13): this is
//! **deterministic composition** given user-chosen inputs — safe to ship. Anything
//! *stochastic or rule-driven* (random oligos, barcode sets, Golden Gate overhang
//! sets) belongs to a separate, data-backed generative package, deferred.

use seqforge_restriction::{EnzymeType, OverhangKind, all_enzymes, enzyme_by_name, find_sites};

/// A conservative default 5′ flank prepended before a restriction site so the
/// enzyme has room to bind near the fragment end (NEB's "~6 nt" guidance). A
/// placeholder pending the per-enzyme NEB minimum-flanking table; callers
/// override via `restriction_tail`'s `flank` argument.
const DEFAULT_FLANK: &str = "AACC";

/// IUPAC nucleotide alphabet (DNA + ambiguity codes), matching the command
/// layer's `parse_bases`.
const IUPAC: &str = "ACGTURYSWKMBDHVN";

/// Why a tail could not be composed. Deterministic, user-facing.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DesignError {
    #[error("unknown enzyme: {0}")]
    UnknownEnzyme(String),
    #[error("{enzyme} is a Type IIs enzyme; supply a {expected} nt overhang")]
    OverhangRequired { enzyme: String, expected: u8 },
    #[error("{enzyme} cuts a {expected} nt overhang, but got {got} nt")]
    OverhangLen {
        enzyme: String,
        expected: u8,
        got: usize,
    },
    #[error("{0} defines its own overhang within its recognition site; don't supply one")]
    OverhangOnFixedType(String),
    #[error("{0} cuts within/upstream of its recognition site — unsupported for tail composition")]
    UnsupportedGeometry(String),
    #[error("`{0}` is not a valid IUPAC nucleotide sequence")]
    BadBases(String),
}

/// A serializable, restriction-free projection of an enzyme for the Inspector's
/// insertion-tools dropdown (mirrors the `PrimerInfo` projection pattern). Lets
/// the editor decide when to prompt for an overhang and validate its length
/// without linking `seqforge-restriction`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnzymeSpec {
    pub name: String,
    /// Type IIs (cuts outside its recognition site → user designs the overhang).
    pub type_iis: bool,
    /// Overhang length (nt) this enzyme produces, or `None` for a blunt cutter.
    pub overhang_len: Option<u8>,
}

/// Every known enzyme as an `EnzymeSpec`, definition order. The catalog is static;
/// callers can cache it.
pub fn enzyme_catalog() -> Vec<EnzymeSpec> {
    all_enzymes()
        .iter()
        .map(|e| EnzymeSpec {
            name: e.name.to_string(),
            type_iis: matches!(e.enzyme_type, EnzymeType::TypeIIs),
            overhang_len: overhang_n(e.overhang_kind()),
        })
        .collect()
}

/// Does `enzyme` already cut `template`? An advisory for "this site already
/// occurs in the amplicon" — unknown enzyme names report `false`.
pub fn enzyme_cuts(template: &[u8], enzyme: &str, circular: bool) -> bool {
    enzyme_by_name(enzyme).is_some_and(|e| !find_sites(template, e, circular).is_empty())
}

/// Build a primer 5′ **tail** carrying `enzyme`'s recognition site, ready to
/// prepend to an oligo (`new_oligo = restriction_tail(..)? + old_oligo`).
///
/// - **Type II** (palindromic; cuts inside its site): `flank + recognition`. The
///   enzyme defines its own overhang, so `overhang` must be `None`.
/// - **Type IIs** (cuts downstream): `flank + recognition + spacer + overhang`,
///   where `spacer_len = top_offset − recognition.len()` and the user-supplied
///   `overhang` length must equal the enzyme's overhang. Orientation is automatic
///   — a 5′-tail site on an inward-extending primer always points inward.
///
/// `flank` defaults to [`DEFAULT_FLANK`]; the spacer is filled with `A`s.
pub fn restriction_tail(
    enzyme: &str,
    overhang: Option<&str>,
    flank: Option<&str>,
) -> Result<String, DesignError> {
    let e = enzyme_by_name(enzyme).ok_or_else(|| DesignError::UnknownEnzyme(enzyme.to_string()))?;
    let recognition: String = e.recognition.iter().map(|i| *i as u8 as char).collect();
    let flank = validate_iupac(flank.unwrap_or(DEFAULT_FLANK))?;
    let reclen = e.recognition.len();

    match e.enzyme_type {
        EnzymeType::TypeII => {
            if overhang.is_some() {
                return Err(DesignError::OverhangOnFixedType(e.name.to_string()));
            }
            Ok(format!("{flank}{recognition}"))
        }
        EnzymeType::TypeIIs => {
            // Only downstream cutters compose cleanly as a 5′ tail.
            if e.top_offset < reclen as i16 {
                return Err(DesignError::UnsupportedGeometry(e.name.to_string()));
            }
            let n = overhang_n(e.overhang_kind()).unwrap_or(0);
            let overhang = match overhang {
                Some(o) => {
                    let o = validate_iupac(o)?;
                    if o.len() != n as usize {
                        return Err(DesignError::OverhangLen {
                            enzyme: e.name.to_string(),
                            expected: n,
                            got: o.len(),
                        });
                    }
                    o
                }
                None if n == 0 => String::new(),
                None => {
                    return Err(DesignError::OverhangRequired {
                        enzyme: e.name.to_string(),
                        expected: n,
                    });
                }
            };
            let spacer = "A".repeat(e.top_offset as usize - reclen);
            Ok(format!("{flank}{recognition}{spacer}{overhang}"))
        }
    }
}

fn overhang_n(kind: OverhangKind) -> Option<u8> {
    match kind {
        OverhangKind::FivePrime(n) | OverhangKind::ThreePrime(n) => Some(n),
        OverhangKind::Blunt => None,
    }
}

/// Uppercase + validate an IUPAC nucleotide string (empty is allowed).
fn validate_iupac(s: &str) -> Result<String, DesignError> {
    let up: String = s.chars().map(|c| c.to_ascii_uppercase()).collect();
    if up.chars().all(|c| IUPAC.contains(c)) {
        Ok(up)
    } else {
        Err(DesignError::BadBases(s.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn type_ii_tail_is_flank_plus_recognition() {
        // EcoRI (GAATTC) is palindromic Type II — no user overhang.
        assert_eq!(
            restriction_tail("EcoRI", None, Some("AA")).unwrap(),
            "AAGAATTC"
        );
        // Empty flank isolates the recognition site.
        assert_eq!(restriction_tail("EcoRI", None, Some("")).unwrap(), "GAATTC");
    }

    #[test]
    fn type_ii_rejects_a_user_overhang() {
        assert_eq!(
            restriction_tail("EcoRI", Some("AATG"), Some("")).unwrap_err(),
            DesignError::OverhangOnFixedType("EcoRI".into())
        );
    }

    #[test]
    fn type_iis_tail_places_recognition_spacer_overhang() {
        // BsaI GGTCTC(1/5): 1 nt spacer, 4 nt user overhang.
        assert_eq!(
            restriction_tail("BsaI", Some("AATG"), Some("")).unwrap(),
            "GGTCTCAAATG" // GGTCTC + A(spacer) + AATG
        );
        // Overhang is uppercased + validated.
        assert_eq!(
            restriction_tail("BsaI", Some("aatg"), Some("TT")).unwrap(),
            "TTGGTCTCAAATG"
        );
    }

    #[test]
    fn type_iis_overhang_length_is_enforced() {
        assert_eq!(
            restriction_tail("BsaI", Some("AAT"), Some("")).unwrap_err(),
            DesignError::OverhangLen {
                enzyme: "BsaI".into(),
                expected: 4,
                got: 3,
            }
        );
        assert_eq!(
            restriction_tail("BsaI", None, Some("")).unwrap_err(),
            DesignError::OverhangRequired {
                enzyme: "BsaI".into(),
                expected: 4,
            }
        );
    }

    #[test]
    fn unknown_enzyme_and_bad_bases_error() {
        assert_eq!(
            restriction_tail("NotAnEnzyme", None, None).unwrap_err(),
            DesignError::UnknownEnzyme("NotAnEnzyme".into())
        );
        assert!(matches!(
            restriction_tail("BsaI", Some("AAXG"), Some("")).unwrap_err(),
            DesignError::BadBases(_)
        ));
    }

    #[test]
    fn catalog_classifies_type_ii_and_iis() {
        let cat = enzyme_catalog();
        let ecori = cat
            .iter()
            .find(|s| s.name == "EcoRI")
            .expect("EcoRI present");
        assert!(!ecori.type_iis);
        let bsai = cat.iter().find(|s| s.name == "BsaI").expect("BsaI present");
        assert!(bsai.type_iis);
        assert_eq!(bsai.overhang_len, Some(4));
    }

    #[test]
    fn enzyme_cuts_detects_internal_site() {
        assert!(enzyme_cuts(b"AAAGAATTCAAA", "EcoRI", false));
        assert!(!enzyme_cuts(b"AAAAAAAAAAAA", "EcoRI", false));
        assert!(!enzyme_cuts(b"AAAGAATTCAAA", "NotAnEnzyme", false));
    }
}
