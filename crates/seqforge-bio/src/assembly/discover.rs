//! Cut-site **discovery** — the source of truth behind the workbench's per-row
//! enzyme/site dropdowns and the multi-cut endpoint ambiguity flag (decision 26). Given a
//! source's bytes and the bin's enzyme query, report which enzymes actually cut
//! and at what top-strand positions. These positions are the `at` occurrence
//! tiebreaker on a [`seqforge_core::Boundary::EnzymeSite`] (they coincide with a
//! digest fragment's boundary coordinate, so a picked site round-trips into a
//! selector that `select` can resolve).
//!
//! Pure projection over the shipped scanner — reuses `resolve_query_names`
//! (enzymes present in this query that cut this input) and `find_cut_sites`.

use crate::{find_cut_sites, parse_enzyme_query, resolve_query_names};

/// One enzyme from the bin query and its cut positions in a given source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnzymeSites {
    pub enzyme: String,
    /// Top-strand cut positions, ascending.
    pub positions: Vec<usize>,
}

impl EnzymeSites {
    /// A unique cutter needs no site picker; cutting >1× makes that endpoint
    /// ambiguous (site dropdown on that end only). Same-enzyme two-site walks
    /// default without a picker; >2 sites still need `@pos`.
    pub fn is_ambiguous(&self) -> bool {
        self.positions.len() > 1
    }
}

/// Resolve the bin's enzyme query against `seq` into the enzymes that cut and
/// their cut positions. Enzymes with no site are dropped (they contribute no
/// boundary). Order follows the resolved query.
pub fn cut_boundaries(seq: &[u8], enzymes: &str, circular: bool) -> Vec<EnzymeSites> {
    let names = resolve_query_names(&parse_enzyme_query(enzymes), seq, circular);
    names
        .iter()
        .filter_map(|name| {
            let mut positions: Vec<usize> = find_cut_sites(seq, &[name.as_str()], circular)
                .into_iter()
                .map(|s| s.cut_pos)
                .collect();
            positions.sort_unstable();
            positions.dedup();
            if positions.is_empty() {
                None
            } else {
                Some(EnzymeSites {
                    enzyme: name.clone(),
                    positions,
                })
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unique_cutter_reports_one_site_and_is_unambiguous() {
        // One EcoRI site → a single cut position, no picker needed.
        let sites = cut_boundaries(b"AAAGAATTCTTT", "EcoRI", false);
        assert_eq!(sites.len(), 1);
        assert_eq!(sites[0].enzyme, "EcoRI");
        assert_eq!(sites[0].positions.len(), 1);
        assert!(!sites[0].is_ambiguous());
    }

    #[test]
    fn multi_cutter_flags_ambiguous_when_more_than_one_site() {
        let sites = cut_boundaries(b"GAATTCgggGAATTCgggGAATTCggg", "EcoRI", false);
        assert_eq!(sites.len(), 1);
        assert_eq!(sites[0].positions.len(), 3);
        assert!(sites[0].is_ambiguous());

        let two = cut_boundaries(b"GAATTCgggGAATTCggg", "EcoRI", false);
        assert_eq!(two[0].positions.len(), 2);
        assert!(two[0].is_ambiguous());
    }

    #[test]
    fn non_cutting_enzyme_is_dropped() {
        let sites = cut_boundaries(b"AAAAAAAAAAAA", "EcoRI", false);
        assert!(sites.is_empty());
    }
}
