//! Enzyme query grammar shared between CLI and GUI overlay.
//!
//! Accepts a single free-text string and produces an `EnzymeQuery` that the
//! dispatcher uses to compute `view.active_enzymes` and `view.cut_sites`.
//! Same grammar is parsed by `seqforge enzymes <args>` and by the GUI's
//! enzyme overlay, so both surfaces share one mental model.
//!
//! Resolution delegates to `seqforge_restriction` — this module is only the
//! grammar + bridge.

use seqforge_core::CutSite;
use seqforge_restriction::Preset as RsPreset;

use crate::search::site_to_cutsite;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnzymePreset {
    Unique,
    UniqueOrDual,
    NonCutters,
    /// Every Type IIs enzyme in the library that cuts at least once.
    TypeIIs,
    /// Golden Gate canonical set (BsaI, BsmBI/Esp3I, BbsI/BpiI, SapI).
    GoldenGate,
    /// MoClo / GreenGate destination enzymes (BsaI, BsmBI).
    MoClo,
}

impl EnzymePreset {
    fn into_restriction(self) -> RsPreset {
        match self {
            EnzymePreset::Unique => RsPreset::Unique,
            EnzymePreset::UniqueOrDual => RsPreset::UniqueOrDual,
            EnzymePreset::NonCutters => RsPreset::NonCutters,
            EnzymePreset::TypeIIs => RsPreset::TypeIIs,
            EnzymePreset::GoldenGate => RsPreset::GoldenGate,
            EnzymePreset::MoClo => RsPreset::MoClo,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnzymeQuery {
    Clear,
    Preset(EnzymePreset),
    Names(Vec<String>),
    All,
}

/// Free-text parse. Never errors — unrecognized input falls through to
/// `Names(...)` (the resolver silently drops names that aren't in the
/// library, matching the historical contract).
pub fn parse_enzyme_query(input: &str) -> EnzymeQuery {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return EnzymeQuery::Clear;
    }
    let normalized = trimmed.to_ascii_lowercase();
    // Normalize separators to a single space so `unique-and-dual`,
    // `unique+dual`, `unique  and  dual` all collapse to one form.
    let collapsed: String = normalized
        .chars()
        .map(|c| match c {
            '-' | '_' | '+' => ' ',
            c => c,
        })
        .collect();
    let collapsed = collapsed.split_whitespace().collect::<Vec<_>>().join(" ");

    match collapsed.as_str() {
        "none" | "clear" => EnzymeQuery::Clear,
        "all" => EnzymeQuery::All,
        "unique" | "unique cutters" | "unique cutter" => EnzymeQuery::Preset(EnzymePreset::Unique),
        "unique and dual"
        | "unique or dual"
        | "unique dual"
        | "unique and dual cutters"
        | "unique or dual cutters" => EnzymeQuery::Preset(EnzymePreset::UniqueOrDual),
        "non cutters" | "noncutters" | "non cutter" | "noncutter" => {
            EnzymeQuery::Preset(EnzymePreset::NonCutters)
        }
        // Type IIs — multiple keyword spellings since users will type whatever.
        "type iis" | "typeiis" | "iis" | "type 2s" | "type2s" => {
            EnzymeQuery::Preset(EnzymePreset::TypeIIs)
        }
        "golden gate" | "goldengate" | "gg" => EnzymeQuery::Preset(EnzymePreset::GoldenGate),
        "moclo" | "mo clo" => EnzymeQuery::Preset(EnzymePreset::MoClo),
        _ => {
            let names: Vec<String> = trimmed
                .split(|c: char| c.is_whitespace() || c == ',')
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
                .collect();
            if names.is_empty() {
                EnzymeQuery::Clear
            } else {
                EnzymeQuery::Names(names)
            }
        }
    }
}

/// Resolve a parsed query against the sequence. Returns the enzyme name
/// list for `view.active_enzymes` and the sites for `view.cut_sites`.
pub fn resolve_query(
    query: &EnzymeQuery,
    seq: &[u8],
    circular: bool,
) -> (Vec<String>, Vec<CutSite>) {
    match query {
        EnzymeQuery::Clear => (Vec::new(), Vec::new()),
        EnzymeQuery::Names(names) => {
            let sites = crate::search::find_cut_sites(
                seq,
                &names.iter().map(String::as_str).collect::<Vec<_>>(),
                circular,
            );
            (names.clone(), sites)
        }
        EnzymeQuery::All => {
            let all_names: Vec<String> = seqforge_restriction::all_enzymes()
                .iter()
                .map(|e| e.name.to_string())
                .collect();
            let sites = crate::search::find_cut_sites(
                seq,
                &all_names.iter().map(String::as_str).collect::<Vec<_>>(),
                circular,
            );
            (all_names, sites)
        }
        EnzymeQuery::Preset(p) => {
            let r = seqforge_restriction::resolve_preset(p.into_restriction(), seq, circular);
            let sites: Vec<CutSite> = r.sites.into_iter().map(site_to_cutsite).collect();
            (r.enzymes, sites)
        }
    }
}

/// Resolve a parsed query to **canonical** enzyme names only (no scanning of
/// the returned set into sites). Presets are still resolved against the
/// sequence; explicit names are mapped to their canonical spelling and unknown
/// names dropped. This is the primitive the GUI/CLI dispatch composes with set
/// operations (add / remove) before re-deriving sites via `find_cut_sites`.
pub fn resolve_query_names(query: &EnzymeQuery, seq: &[u8], circular: bool) -> Vec<String> {
    match query {
        EnzymeQuery::Clear => Vec::new(),
        EnzymeQuery::Names(names) => names
            .iter()
            .filter_map(|n| seqforge_restriction::enzyme_by_name(n).map(|e| e.name.to_string()))
            .collect(),
        EnzymeQuery::All => seqforge_restriction::all_enzymes()
            .iter()
            .map(|e| e.name.to_string())
            .collect(),
        EnzymeQuery::Preset(p) => {
            seqforge_restriction::resolve_preset(p.into_restriction(), seq, circular).enzymes
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_empty_is_clear() {
        assert_eq!(parse_enzyme_query(""), EnzymeQuery::Clear);
        assert_eq!(parse_enzyme_query("   "), EnzymeQuery::Clear);
        assert_eq!(parse_enzyme_query("none"), EnzymeQuery::Clear);
        assert_eq!(parse_enzyme_query("CLEAR"), EnzymeQuery::Clear);
    }

    #[test]
    fn parse_unique_variants() {
        assert_eq!(parse_enzyme_query("unique"), EnzymeQuery::Preset(EnzymePreset::Unique));
        assert_eq!(
            parse_enzyme_query("Unique Cutters"),
            EnzymeQuery::Preset(EnzymePreset::Unique)
        );
    }

    #[test]
    fn parse_unique_and_dual_variants() {
        let expected = EnzymeQuery::Preset(EnzymePreset::UniqueOrDual);
        assert_eq!(parse_enzyme_query("unique and dual"), expected);
        assert_eq!(parse_enzyme_query("unique+dual"), expected);
        assert_eq!(parse_enzyme_query("unique-and-dual"), expected);
        assert_eq!(parse_enzyme_query("Unique  And  Dual"), expected);
        assert_eq!(parse_enzyme_query("unique or dual"), expected);
    }

    #[test]
    fn parse_non_cutters_variants() {
        let expected = EnzymeQuery::Preset(EnzymePreset::NonCutters);
        assert_eq!(parse_enzyme_query("non-cutters"), expected);
        assert_eq!(parse_enzyme_query("noncutters"), expected);
        assert_eq!(parse_enzyme_query("non cutters"), expected);
    }

    #[test]
    fn parse_type_iis_keywords() {
        let expected = EnzymeQuery::Preset(EnzymePreset::TypeIIs);
        assert_eq!(parse_enzyme_query("type iis"), expected);
        assert_eq!(parse_enzyme_query("Type IIs"), expected);
        assert_eq!(parse_enzyme_query("IIS"), expected);
        assert_eq!(parse_enzyme_query("type2s"), expected);
    }

    #[test]
    fn parse_golden_gate_keywords() {
        let expected = EnzymeQuery::Preset(EnzymePreset::GoldenGate);
        assert_eq!(parse_enzyme_query("golden gate"), expected);
        assert_eq!(parse_enzyme_query("GoldenGate"), expected);
        assert_eq!(parse_enzyme_query("gg"), expected);
    }

    #[test]
    fn parse_moclo_keywords() {
        let expected = EnzymeQuery::Preset(EnzymePreset::MoClo);
        assert_eq!(parse_enzyme_query("moclo"), expected);
        assert_eq!(parse_enzyme_query("MoClo"), expected);
    }

    #[test]
    fn parse_all_keyword() {
        assert_eq!(parse_enzyme_query("all"), EnzymeQuery::All);
    }

    #[test]
    fn parse_names_list() {
        assert_eq!(
            parse_enzyme_query("EcoRI BamHI"),
            EnzymeQuery::Names(vec!["EcoRI".into(), "BamHI".into()])
        );
        assert_eq!(
            parse_enzyme_query("EcoRI, BamHI , HindIII"),
            EnzymeQuery::Names(vec!["EcoRI".into(), "BamHI".into(), "HindIII".into()])
        );
    }

    #[test]
    fn resolve_clear_returns_empty() {
        let (names, sites) = resolve_query(&EnzymeQuery::Clear, b"GAATTC", false);
        assert!(names.is_empty());
        assert!(sites.is_empty());
    }

    #[test]
    fn resolve_names_passes_through() {
        let q = EnzymeQuery::Names(vec!["EcoRI".into()]);
        let (names, sites) = resolve_query(&q, b"AAAGAATTCAAA", false);
        assert_eq!(names, vec!["EcoRI".to_string()]);
        assert_eq!(sites.len(), 1);
    }

    #[test]
    fn resolve_golden_gate_finds_bsai() {
        // 30 bases with GGTCTC at position 5; BsaI should cut.
        let seq = b"AAAAAGGTCTCAAAAAAAAAAAAAAAAAAA";
        let (names, sites) =
            resolve_query(&EnzymeQuery::Preset(EnzymePreset::GoldenGate), seq, false);
        assert!(names.iter().any(|n| n == "BsaI"));
        assert!(sites.iter().any(|s| s.enzyme == "BsaI"));
    }

    #[test]
    fn resolve_type_iis_includes_bsai_and_bbsi() {
        // Sequence with both BsaI (GGTCTC) and BbsI (GAAGAC) recognition sites.
        // 50 bases total — well past the cut offsets for both enzymes.
        //          0         1         2         3         4
        //          01234567890123456789012345678901234567890123456789
        let seq = b"AAAAAGGTCTCAAAAAAAAAAGAAGACAAAAAAAAAAAAAAAAAAAAAAA";
        let (names, sites) =
            resolve_query(&EnzymeQuery::Preset(EnzymePreset::TypeIIs), seq, false);
        assert!(names.iter().any(|n| n == "BsaI"), "names: {names:?}");
        assert!(names.iter().any(|n| n == "BbsI"), "names: {names:?}");
        assert!(sites.len() >= 2);
    }
}
