//! Sticky/blunt **ligation** — the traditional `JoinKind::Ligate`. A thin
//! wrapper over the shared [`super::ends::assemble_by_ends`] engine (Golden Gate
//! is the same engine; see `ends.rs`).

use seqforge_core::{Fragment, TopologyIntent};

pub(super) fn ligate(frags: Vec<Fragment>, intent: TopologyIntent) -> Vec<Fragment> {
    super::ends::assemble_by_ends(frags, intent)
}

#[cfg(test)]
mod tests {
    use super::*;
    use seqforge_core::{Annotations, MethylContext, Topology};

    use super::super::ends::canonical;

    /// Digest a circular molecule, then ligate the fragments back — the product
    /// must be the original (up to rotation / strand). The Tier-2 → Tier-3 bridge.
    fn round_trip(seq: &[u8], enzymes: &[&str]) {
        let (frags, _) = crate::digest_fragments(
            seq,
            &Annotations::default(),
            enzymes,
            true,
            "p",
            &MethylContext::NONE,
        );
        assert!(frags.len() >= 2, "need a multi-cut digest to religate");
        let products = ligate(frags, TopologyIntent::Circular);
        let want = canonical(seq, Topology::Circular);
        assert!(
            products.iter().any(|p| p.topology == Topology::Circular
                && canonical(p.bytes(), Topology::Circular) == want),
            "no circular product matched the input for enzymes {enzymes:?}"
        );
    }

    #[test]
    fn digest_ligate_round_trip_single_enzyme() {
        // Two EcoRI sites on a circle → 2 sticky fragments → religate to original.
        round_trip(b"GAATTCAAAAGAATTCAAAA", &["EcoRI"]);
    }

    #[test]
    fn digest_ligate_round_trip_double_digest_directional() {
        // One EcoRI + one BamHI → 2 fragments with DISTINCT overhangs (directional).
        round_trip(b"GAATTCAAAAGGATCCAAAA", &["EcoRI", "BamHI"]);
    }

    #[test]
    fn digest_ligate_round_trip_blunt() {
        // Two SmaI sites → 2 blunt fragments → blunt religation.
        round_trip(b"CCCGGGAAAACCCGGGAAAA", &["SmaI"]);
    }

    #[test]
    fn digest_order_identity_religates_identical_overhangs() {
        // Two EcoRI arcs in digest order: prepared orientation matches without flip.
        // (Both orientations as products require two authored sources — not join DFS.)
        let (vec_frags, _) = crate::digest_fragments(
            b"GAATTCAAAAAAAAAAGAATTCTTTTTTTTTT",
            &Annotations::default(),
            &["EcoRI"],
            true,
            "vector",
            &MethylContext::NONE,
        );
        assert_eq!(vec_frags.len(), 2);
        let products = ligate(vec_frags, TopologyIntent::Circular);
        assert_eq!(
            products.len(),
            1,
            "identity join yields one circular religation, not either-orientation"
        );
        assert!(products.iter().all(|p| p.topology == Topology::Circular));
    }

    #[test]
    fn intent_linear_does_not_return_circular_products() {
        let (frags, _) = crate::digest_fragments(
            b"GAATTCAAAAGAATTCAAAA",
            &Annotations::default(),
            &["EcoRI"],
            true,
            "p",
            &MethylContext::NONE,
        );
        let products = ligate(frags, TopologyIntent::Linear);
        assert!(products.iter().all(|p| p.topology == Topology::Linear));
    }
}
