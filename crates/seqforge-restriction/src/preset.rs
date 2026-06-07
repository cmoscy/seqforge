//! Named enzyme presets — the GUI overlay and CLI both consume these.

use crate::enzyme::{Enzyme, EnzymeType, Site};
use crate::scan::{count_sites_per_enzyme, find_all_sites};

/// Built-in presets. `EnzymeQuery` (in `seqforge-bio`) maps user-typed
/// keywords to these.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Preset {
    /// Enzymes with exactly 1 site in the sequence.
    Unique,
    /// Enzymes with 1 or 2 sites.
    UniqueOrDual,
    /// Enzymes in the library with 0 sites. Useful for designing fresh
    /// cloning sites — `sites` is empty by definition.
    NonCutters,
    /// Every Type IIs enzyme in the library that cuts at least once.
    TypeIIs,
    /// Golden Gate canonical set: BsaI, BsmBI/Esp3I, BbsI/BpiI, SapI.
    GoldenGate,
    /// MoClo / GreenGate destination enzymes: BsaI + BsmBI.
    MoClo,
}

/// Resolved preset → enzyme names + sites for rendering. `enzymes` is the
/// list to record into `view.active_enzymes`; `sites` is what the renderer
/// draws.
#[derive(Debug, Clone, Default)]
pub struct PresetResult {
    pub enzymes: Vec<String>,
    pub sites: Vec<Site>,
}

/// Golden Gate canonical enzyme names. Match against the library by case-
/// insensitive name. We hardcode the set rather than infer from `EnzymeType`
/// because not every Type IIs enzyme is a *Golden Gate* enzyme — these four
/// are specifically chosen for their 4-base 5′ overhangs and clean
/// star-activity profiles.
const GOLDEN_GATE_NAMES: &[&str] = &["BsaI", "BsmBI", "Esp3I", "BbsI", "BpiI", "SapI"];

/// MoClo standard subset.
const MOCLO_NAMES: &[&str] = &["BsaI", "BsmBI", "Esp3I"];

/// Resolve a preset against `seq`. Returns the enzyme name list (for
/// `view.active_enzymes` bookkeeping) and the sites to render.
pub fn resolve_preset(preset: Preset, seq: &[u8], circular: bool) -> PresetResult {
    let lib = crate::all_enzymes();
    let refs: Vec<&'static Enzyme> = lib.iter().collect();

    match preset {
        Preset::Unique => filter_by_count(&refs, seq, circular, |c| c == 1),
        Preset::UniqueOrDual => filter_by_count(&refs, seq, circular, |c| c == 1 || c == 2),
        Preset::NonCutters => {
            let counts = count_sites_per_enzyme(seq, &refs, circular);
            let names = counts
                .into_iter()
                .filter(|(_, c)| *c == 0)
                .map(|(e, _)| e.name.to_string())
                .collect();
            PresetResult {
                enzymes: names,
                sites: Vec::new(),
            }
        }
        Preset::TypeIIs => {
            let iis: Vec<&'static Enzyme> = lib
                .iter()
                .filter(|e| e.enzyme_type == EnzymeType::TypeIIs)
                .collect();
            let sites = find_all_sites(seq, &iis, circular);
            // Filter to enzymes that actually cut at least once — TypeIIs
            // preset is "show me Type IIs sites", not "list all Type IIs".
            let mut names: Vec<String> = sites.iter().map(|s| s.enzyme.to_string()).collect();
            names.sort();
            names.dedup();
            PresetResult {
                enzymes: names,
                sites,
            }
        }
        Preset::GoldenGate => resolve_named_subset(seq, circular, GOLDEN_GATE_NAMES),
        Preset::MoClo => resolve_named_subset(seq, circular, MOCLO_NAMES),
    }
}

fn filter_by_count(
    refs: &[&'static Enzyme],
    seq: &[u8],
    circular: bool,
    predicate: impl Fn(usize) -> bool,
) -> PresetResult {
    let counts = count_sites_per_enzyme(seq, refs, circular);
    let keep: Vec<&'static Enzyme> = counts
        .iter()
        .filter(|(_, c)| predicate(*c))
        .map(|(e, _)| *e)
        .collect();
    let names: Vec<String> = keep.iter().map(|e| e.name.to_string()).collect();
    let sites = find_all_sites(seq, &keep, circular);
    PresetResult {
        enzymes: names,
        sites,
    }
}

fn resolve_named_subset(seq: &[u8], circular: bool, names: &[&str]) -> PresetResult {
    let lib: Vec<&'static Enzyme> = names
        .iter()
        .filter_map(|n| crate::enzyme_by_name(n))
        .collect();
    let sites = find_all_sites(seq, &lib, circular);
    let actual_names: Vec<String> = lib.iter().map(|e| e.name.to_string()).collect();
    PresetResult {
        enzymes: actual_names,
        sites,
    }
}
