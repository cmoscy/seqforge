//! `PrepareKind::AsIs` — the source is already a fragment. The whole molecule
//! becomes one fragment with its native ends (blunt) and topology.

use seqforge_core::document::{Lineage, LineageOp};
use seqforge_core::{End, Fragment, PartialPolicy, Span, transport};

use crate::assembly::ResolvedSource;

pub(super) fn prepare(src: &ResolvedSource) -> Vec<Fragment> {
    let len = src.bytes.len();
    let slice = transport::extract(
        &src.bytes,
        &src.ann,
        Span::full(len),
        PartialPolicy::TruncatePartials,
        &src.name,
    );
    vec![Fragment {
        slice,
        left: End::Blunt,
        right: End::Blunt,
        topology: src.topology,
        lineage: Lineage {
            source_doc: src.name.clone(),
            source_range: 0..len,
            op: LineageOp::Extract,
        },
    }]
}
