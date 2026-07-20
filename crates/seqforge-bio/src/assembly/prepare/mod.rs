//! The per-bin **prepare** verb: a bin's sources → candidate fragments. This is
//! where "different treatments per bin" lives — dispatch on the closed
//! [`PrepareKind`], one submodule per op (plugin cards).
//!
//! Prepare is applied **per source** so the batch-first authoring model works:
//! the bin's 5′→3′ prepare is inherited by every source; a per-input
//! [`Source::span`] override then narrows *that source's* digest pool
//! (decision 26). The bin resolver ([`super::resolve_bin`]) drives this loop.

mod as_is;
mod digest;
mod pcr;

use seqforge_core::{Fragment, PrepareKind};

use super::ResolvedSource;

/// Apply a bin's prepare op to **one** resolved source, yielding its candidate
/// fragments (full digest pool for Digest; one amplicon for PCR; one piece for
/// AsIs). The 5′→3′ span pick is applied by the caller for Digest.
pub(super) fn prepare_source(
    prepare: &PrepareKind,
    resolved: &ResolvedSource,
) -> Result<Vec<Fragment>, String> {
    Ok(match prepare {
        PrepareKind::Digest {
            five_prime,
            three_prime,
        } => {
            let enzymes = PrepareKind::Digest {
                five_prime: five_prime.clone(),
                three_prime: three_prime.clone(),
            }
            .digest_enzymes();
            if enzymes.is_empty() {
                return Ok(Vec::new());
            }
            digest::prepare(resolved, &enzymes.join(" "))
        }
        PrepareKind::Pcr { fwd, rev } => pcr::prepare(resolved, fwd, rev)?,
        PrepareKind::AsIs => as_is::prepare(resolved),
    })
}
