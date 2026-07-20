//! Combination expansion — the batch axis. `AllToAll` is the Cartesian product
//! (one candidate per bin → ∏|binᵢ| combos); `Zip` pairs positionally. Individual
//! vs. library is emergent from candidate cardinality, not a mode (decision 7).

use seqforge_core::{Expand, Fragment};

/// Expand per-bin candidate pools into combos (each combo = one fragment per bin).
pub(super) fn expand(pools: &[Vec<Fragment>], mode: Expand) -> Vec<Vec<Fragment>> {
    if pools.iter().any(|p| p.is_empty()) {
        return Vec::new(); // an empty bin means nothing can assemble
    }
    match mode {
        Expand::AllToAll => cartesian(pools),
        Expand::Zip => zip(pools),
    }
}

fn cartesian(pools: &[Vec<Fragment>]) -> Vec<Vec<Fragment>> {
    let mut combos: Vec<Vec<Fragment>> = vec![Vec::new()];
    for pool in pools {
        let mut next = Vec::with_capacity(combos.len() * pool.len());
        for prefix in &combos {
            for frag in pool {
                let mut c = prefix.clone();
                c.push(frag.clone());
                next.push(c);
            }
        }
        combos = next;
    }
    combos
}

fn zip(pools: &[Vec<Fragment>]) -> Vec<Vec<Fragment>> {
    let k = pools.iter().map(Vec::len).min().unwrap_or(0);
    (0..k)
        .map(|i| pools.iter().map(|p| p[i].clone()).collect())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use seqforge_core::document::{Lineage, LineageOp};
    use seqforge_core::{End, SeqSlice, Topology};

    fn frag(tag: u8) -> Fragment {
        Fragment {
            slice: SeqSlice {
                bytes: vec![tag],
                features: vec![],
                primers: vec![],
            },
            left: End::Blunt,
            right: End::Blunt,
            topology: Topology::Linear,
            lineage: Lineage {
                source_doc: "s".into(),
                source_range: 0..1,
                op: LineageOp::Extract,
            },
        }
    }

    #[test]
    fn all_to_all_count_is_product_of_pool_sizes() {
        let pools = vec![vec![frag(1), frag(2)], vec![frag(3), frag(4), frag(5)]];
        let combos = expand(&pools, Expand::AllToAll);
        assert_eq!(combos.len(), 6); // 2 * 3
        assert!(combos.iter().all(|c| c.len() == 2));
    }

    #[test]
    fn zip_pairs_positionally() {
        let pools = vec![vec![frag(1), frag(2)], vec![frag(3), frag(4)]];
        let combos = expand(&pools, Expand::Zip);
        assert_eq!(combos.len(), 2);
        assert_eq!(combos[0][0].bytes(), &[1]);
        assert_eq!(combos[0][1].bytes(), &[3]);
    }

    #[test]
    fn empty_bin_yields_no_combos() {
        let pools = vec![vec![frag(1)], vec![]];
        assert!(expand(&pools, Expand::AllToAll).is_empty());
    }
}
