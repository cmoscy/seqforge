//! The per-recipe **join** verb — the only method-specific code. A method is one
//! `fn(Vec<Fragment>) -> Vec<Product>` with **topology derived inside it**; the
//! recipe's `intent` filters the result.
//!
//! `Ligate` and `GoldenGate` share the same overhang-driven end-matching engine
//! ([`ends::assemble_by_ends`]) — Golden Gate assembly *is* sticky-end ligation
//! of Type IIS-digested fragments, so there is one join engine, not two. The
//! distinct `GoldenGate` variant records author intent + its enzyme (Workbench /
//! CLI may preselect that enzyme's Pryor fidelity table). Gibson slots in as a
//! sibling module.

mod ends;
mod golden_gate;
mod ligate;

pub use ends::{
    HarvestedOverhang, JoinProbe, JunctionReport, harvest_junction_overhangs, probe_join,
};

use seqforge_core::{Fragment, JoinKind, TopologyIntent};

/// Join one combo of fragments into product(s), filtered by topology intent.
pub(super) fn join(kind: &JoinKind, frags: Vec<Fragment>, intent: TopologyIntent) -> Vec<Fragment> {
    match kind {
        JoinKind::Ligate => ligate::ligate(frags, intent),
        JoinKind::GoldenGate { .. } => golden_gate::golden_gate(frags, intent),
    }
}
