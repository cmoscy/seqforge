//! `PrepareKind::Digest` — cut a source with restriction enzymes. Wraps the
//! shipped `digest_fragments`; the molecule's methylation defaults to Dam⁺ Dcm⁺.

use seqforge_core::{Fragment, MethylContext, Topology};

use crate::assembly::ResolvedSource;

pub(super) fn prepare(src: &ResolvedSource, enzymes: &str) -> Vec<Fragment> {
    let circular = matches!(src.topology, Topology::Circular);
    let names =
        crate::resolve_query_names(&crate::parse_enzyme_query(enzymes), &src.bytes, circular);
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let (frags, _warnings) = crate::digest_fragments(
        &src.bytes,
        &src.ann,
        &refs,
        circular,
        &src.name,
        &MethylContext::default(),
    );
    frags
}
