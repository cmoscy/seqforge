//! **Golden Gate** one-pot assembly (`JoinKind::GoldenGate`). Mechanically it is
//! sticky-end ligation of Type IIS-digested fragments — the enzyme (e.g. BsaI)
//! cuts *outside* its recognition site leaving author-designed 4-nt overhangs,
//! the recognition-site-bearing flanking pieces fail to close, and the topology
//! filter keeps the assembled circle. So the join reuses the shared
//! [`super::ends::assemble_by_ends`] engine; the distinct `JoinKind` variant only
//! records intent + the enzyme (GG tables can preselect a Pryor dataset in the
//! workbench / CLI overlay; scoring never runs inside this join).

use seqforge_core::{Fragment, TopologyIntent};

pub(super) fn golden_gate(frags: Vec<Fragment>, intent: TopologyIntent) -> Vec<Fragment> {
    // Join mechanics are identical to ligation; fidelity is informational and
    // lives on the combo preview / dry-run path, not here.
    super::ends::assemble_by_ends(frags, intent)
}

#[cfg(test)]
mod tests {
    use seqforge_core::{Annotations, MethylContext, Topology, TopologyIntent};

    use super::super::ends::canonical;
    use super::golden_gate;

    /// A synthetic 2-part Golden Gate: two fragments cut by BsaI whose designed
    /// overhangs are mutually compatible ligate into a circle. We build the
    /// substrate by digesting a circular molecule with two BsaI sites and feeding
    /// the resulting fragments back through the GG join (the digest→GG analogue of
    /// the ligate round-trip).
    #[test]
    fn golden_gate_closes_a_bsai_digest_into_a_circle() {
        // BsaI = GGTCTC(1/5): cuts leaving 4-nt 5′ overhangs at a distance. Two
        // sites on a circle → two fragments with distinct designed overhangs.
        let seq = b"GGTCTCACCCCAAAAAAAAAAGGTCTCAGGGGTTTTTTTTTT";
        let (frags, _warn) = crate::digest_fragments(
            seq,
            &Annotations::default(),
            &["BsaI"],
            true,
            "gg",
            &MethylContext::NONE,
        );
        assert!(
            frags.len() >= 2,
            "need a multi-cut BsaI digest, got {}",
            frags.len()
        );
        let products = golden_gate(frags, TopologyIntent::Circular);
        assert!(
            products.iter().any(|p| p.topology == Topology::Circular),
            "Golden Gate should close at least one circular product"
        );
    }

    #[test]
    fn golden_gate_matches_ligate_mechanics_on_shared_overhangs() {
        // With identical designed overhangs the GG join behaves exactly like
        // ligation (shared engine) — a circular product exists and is canonical.
        let (frags, _) = crate::digest_fragments(
            b"GAATTCAAAAGAATTCAAAA",
            &Annotations::default(),
            &["EcoRI"],
            true,
            "p",
            &MethylContext::NONE,
        );
        let products = golden_gate(frags, TopologyIntent::Circular);
        let want = canonical(b"GAATTCAAAAGAATTCAAAA", Topology::Circular);
        assert!(
            products
                .iter()
                .any(|p| canonical(p.bytes(), Topology::Circular) == want),
            "GG engine must reproduce the closed circle"
        );
    }
}
