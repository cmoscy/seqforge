//! REBASE-derived restriction enzyme database + scanner for SeqForge.
//!
//! Unpublished workspace crate. The public API is intentionally concrete:
//! no traits, no extension points, no `Box<dyn …>`. Stabilises through
//! real SeqForge use before any crates.io extraction (see plans/restriction.md).
//!
//! ## Quick tour
//!
//! ```ignore
//! use seqforge_restriction::{enzyme_by_name, find_sites};
//!
//! let seq = b"AAAGAATTCAAA";
//! let ecori = enzyme_by_name("EcoRI").unwrap();
//! let sites = find_sites(seq, ecori, /* circular = */ false);
//! assert_eq!(sites.len(), 1);
//! ```
//!
//! ## Data attribution
//!
//! Enzyme data is parsed from REBASE — http://rebase.neb.com — © Dr. Richard
//! J. Roberts. The snapshot under `data/` retains the original copyright
//! header. Codegen emits the same attribution into `enzymes_generated.rs`.

mod enzyme;
mod enzymes_generated;
mod methylation;
mod preset;
mod scan;

pub use enzyme::{
    Enzyme, EnzymeType, Iupac, MethylEffect, MethylSensitivity, OverhangKind, Site, SiteStrand,
};
pub use methylation::{site_methyl_state, MethylContext, SiteMethylState};
pub use preset::{resolve_preset, Preset, PresetResult};
pub use scan::{count_sites_per_enzyme, find_all_sites, find_sites};

/// Full enzyme table. Static, zero-allocation lookup.
pub fn all_enzymes() -> &'static [Enzyme] {
    enzymes_generated::ENZYMES
}

/// Case-insensitive name lookup. Returns the first match (REBASE lists
/// some isoschizomers under separate names, so callers wanting both should
/// iterate `all_enzymes` instead).
pub fn enzyme_by_name(name: &str) -> Option<&'static Enzyme> {
    enzymes_generated::ENZYMES
        .iter()
        .find(|e| e.name.eq_ignore_ascii_case(name))
}
