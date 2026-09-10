//! The production [`BioOps`] implementation.
//!
//! See [`Bio`]. Kept in its own module so the seam decision 9 creates has one
//! obvious home, reachable from every shell (ROADMAP decision 27).

use std::path::Path;

use seqforge_core::{
    BioOps, CutSite, Document, MethylContext, MethylState, Primer, PrimerInfo, SearchHit,
};

/// The production [`BioOps`] implementation — a ZST forwarding to `seqforge_bio`
/// free functions.
///
/// `BioOps` exists because decision 9 forbids `core ──► bio`: `seqforge-core`
/// declares the trait and reaches sequence logic through it. Something above
/// both crates has to supply the implementation, and for a long time the only
/// crate there was the GUI — so this lived in `seqforge-app` as `AppBio` and was
/// unreachable from the CLI, which is why five test modules each grew their own
/// stub. It is not GUI code; there is not an `egui` symbol in it.
pub struct Bio;

impl BioOps for Bio {
    fn load(&self, path: &Path) -> Result<Document, String> {
        seqforge_bio::load(path).map_err(|e| e.to_string())
    }

    fn find_matches(
        &self,
        seq: &[u8],
        pattern: &[u8],
        mismatches: u8,
        circular: bool,
    ) -> Vec<SearchHit> {
        seqforge_bio::find_iupac_matches(seq, pattern, mismatches, circular)
    }

    fn find_cut_sites(&self, seq: &[u8], enzymes: &[&str], circular: bool) -> Vec<CutSite> {
        seqforge_bio::find_cut_sites(seq, enzymes, circular)
    }

    fn resolve_enzyme_names(&self, seq: &[u8], query: &str, circular: bool) -> Vec<String> {
        let parsed = seqforge_bio::parse_enzyme_query(query);
        seqforge_bio::resolve_query_names(&parsed, seq, circular)
    }

    fn primer_infos(&self, seq: &[u8], primers: &[&Primer], circular: bool) -> Vec<PrimerInfo> {
        seqforge_bio::primer_infos(seq, primers, circular)
    }

    fn methyl_states_for_sites(
        &self,
        sites: &[CutSite],
        seq: &[u8],
        methylation: &MethylContext,
    ) -> Vec<MethylState> {
        seqforge_bio::methyl_states_for_sites(sites, seq, methylation)
    }
}
