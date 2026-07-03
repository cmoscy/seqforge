//! Memoized primer attachment-state (Phase 1.1). Rebuilt on `buffer.version`
//! change — not per frame — so the seed-and-extend find pass stays change-scoped.

use seqforge_bio::{
    AnnealSettings, PrimerAttachment, classify_attachment,
};

/// Per-buffer primer attachment classification, aligned positionally with
/// `Annotations::primers()`.
#[derive(Debug, Clone)]
pub(crate) struct PrimerAnnealCache {
    pub version: u64,
    pub attachments: Vec<PrimerAttachment>,
}

pub(crate) fn build_primer_anneal_cache(
    template: &[u8],
    primers: &[&seqforge_core::Primer],
    circular: bool,
    version: u64,
) -> PrimerAnnealCache {
    let settings = AnnealSettings::default();
    let attachments = primers
        .iter()
        .map(|p| classify_attachment(p, template, circular, settings))
        .collect();
    PrimerAnnealCache {
        version,
        attachments,
    }
}
