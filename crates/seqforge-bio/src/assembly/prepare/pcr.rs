//! `PrepareKind::Pcr` — amplify between two primers named on the source. The
//! amplicon (with inherited annotations) becomes one blunt linear fragment; 5′
//! tails bake into the product ends. Mirrors `command::file::apply_pcr`.

use seqforge_core::document::{Lineage, LineageOp};
use seqforge_core::{
    Annotations, End, Fragment, Orient, PartialPolicy, SeqSlice, Topology, transport,
};

use crate::assembly::ResolvedSource;

pub(super) fn prepare(src: &ResolvedSource, fwd: &str, rev: &str) -> Result<Vec<Fragment>, String> {
    let circular = matches!(src.topology, Topology::Circular);
    let fwd_p = src
        .ann
        .primers()
        .find(|p| p.name == fwd)
        .ok_or_else(|| format!("no primer named \"{fwd}\" on {}", src.name))?;
    let rev_p = src
        .ann
        .primers()
        .find(|p| p.name == rev)
        .ok_or_else(|| format!("no primer named \"{rev}\" on {}", src.name))?;

    let prod = crate::pcr(&src.bytes, fwd_p, rev_p, circular).map_err(|e| e.to_string())?;

    // Inherit amplicon annotations at the forward-tail offset (as apply_pcr does).
    let mut slice = transport::extract(
        &src.bytes,
        &src.ann,
        prod.amplicon,
        PartialPolicy::TruncatePartials,
        &src.name,
    );
    slice.primers.retain(|p| p.binding.is_some());

    let mut prod_ann = Annotations::default();
    transport::place(
        &mut prod_ann,
        &slice,
        prod.tail_f_len,
        Orient::Identity,
        false,
        prod.bytes.len(),
    );

    let len = prod.bytes.len();
    let product_slice = SeqSlice {
        bytes: prod.bytes,
        features: prod_ann.iter().cloned().collect(),
        primers: prod_ann.primers().cloned().collect(),
    };

    Ok(vec![Fragment {
        slice: product_slice,
        left: End::Blunt,
        right: End::Blunt,
        topology: Topology::Linear,
        lineage: Lineage {
            source_doc: src.name.clone(),
            source_range: 0..len,
            op: LineageOp::Extract,
        },
    }])
}
