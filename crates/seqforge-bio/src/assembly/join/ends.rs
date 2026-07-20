//! The shared **overhang end-matching** join engine. Given a combo of fragments
//! in **bin order** with **prepared orientation** (identity only — no flip
//! search), check that adjacent ends are compatible and build the product(s).
//! Bin order seals connectivity; 5′→3′ prepare seals orientation. Both
//! orientations are authored as multiple sources / complementary walks, never
//! invented by the join. **Topology is derived**: circular iff the chain's two
//! terminal ends also close. Byte assembly is concatenation of top strands —
//! the Tier-2 partition already assigns each overhang base to exactly one
//! fragment, so a junction's shared overhang appears once.
//!
//! Both `Ligate` and `GoldenGate` are this engine (Golden Gate assembly is
//! sticky-end ligation of Type IIS fragments); the `JoinKind` variant carries
//! author intent + enzyme. Informational fidelity % is scored outside the join
//! (session / CLI dry-run overlay — never gates assembly).

use seqforge_core::document::{Lineage, LineageOp};
use seqforge_core::{
    Annotations, End, Fragment, Orient, SeqSlice, Topology, TopologyIntent, transport,
};

use crate::reverse_complement;

/// Above this fragment count the arrangement is skipped (returns nothing);
/// A1 targets small joins (vector + a few inserts).
const MAX_FRAGMENTS: usize = 8;

/// One junction in a [`JoinProbe`] (adjacent bins, or circular close).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JunctionReport {
    /// Index of the fragment whose **right** end feeds this junction
    /// (`from` → `to`). For circular close, `from` is the last fragment.
    pub from: usize,
    /// Index of the fragment whose **left** end feeds this junction.
    pub to: usize,
    pub ok: bool,
    /// Short human detail (e.g. enzyme names / blunt / side mismatch).
    pub detail: String,
}

/// End-compatibility probe — same predicate as join, **no** byte concat or
/// annotation place. Used by CLI `--dry-run` and the workbench pre-Run status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JoinProbe {
    /// Every adjacent junction in bin order (length `n-1` for `n` fragments).
    pub junctions: Vec<JunctionReport>,
    /// Whether the two terminal ends close a circle (meaningful for circular intent).
    pub closes: bool,
    /// True when all adjacent junctions succeed (linear chain is assemblable).
    pub chain_ok: bool,
}

impl JoinProbe {
    /// Whether this combo yields a product under `intent` (identity orientation).
    pub fn compatible_for(&self, intent: TopologyIntent) -> bool {
        if !self.chain_ok {
            return false;
        }
        match intent {
            TopologyIntent::Linear => true,
            TopologyIntent::Circular => self.closes,
            TopologyIntent::Any => true,
        }
    }
}

/// Probe end compatibility for a combo in **prepared orientation** (no flips).
/// Does not build product bytes or place annotations.
pub fn probe_join(frags: &[Fragment]) -> JoinProbe {
    let n = frags.len();
    if n == 0 {
        return JoinProbe {
            junctions: Vec::new(),
            closes: false,
            chain_ok: false,
        };
    }
    if n == 1 {
        let closes = frags[0].left.compatible_with(&frags[0].right);
        return JoinProbe {
            junctions: Vec::new(),
            closes,
            chain_ok: true,
        };
    }
    if n > MAX_FRAGMENTS {
        return JoinProbe {
            junctions: Vec::new(),
            closes: false,
            chain_ok: false,
        };
    }

    let mut junctions = Vec::with_capacity(n - 1);
    let mut chain_ok = true;
    for i in 0..n - 1 {
        let ok = frags[i].right.compatible_with(&frags[i + 1].left);
        if !ok {
            chain_ok = false;
        }
        junctions.push(JunctionReport {
            from: i,
            to: i + 1,
            ok,
            detail: junction_detail(&frags[i].right, &frags[i + 1].left),
        });
    }
    let closes = frags[0].left.compatible_with(&frags[n - 1].right);
    JoinProbe {
        junctions,
        closes,
        chain_ok,
    }
}

/// One sticky overhang harvested for fidelity scoring.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HarvestedOverhang {
    pub seq: Vec<u8>,
    /// True when the end was [`OverhangSide::ThreePrime`] chemistry.
    pub three_prime: bool,
}

/// Harvest one sticky overhang per junction (bin-order adjacent, plus circular
/// close when `circular`). Used for informational fidelity scoring.
///
/// Sequence-only (NEB Ligase Fidelity Viewer style): any [`End::Overhang`] is
/// harvested regardless of 5′/3′ chemistry. Blunt → `None`; callers treat a
/// partial harvest as unscorable. Matrices assume Potapov/Pryor 5′-assay data;
/// `three_prime` lets the UI mark extrapolated scores with `*`.
pub fn harvest_junction_overhangs(
    frags: &[Fragment],
    circular: bool,
) -> Vec<Option<HarvestedOverhang>> {
    let n = frags.len();
    if n < 2 {
        return Vec::new();
    }
    let mut out = Vec::with_capacity(n);
    for frag in frags.iter().take(n - 1) {
        out.push(sticky_overhang(&frag.right));
    }
    if circular {
        out.push(sticky_overhang(&frags[n - 1].right));
    }
    out
}

fn sticky_overhang(end: &End) -> Option<HarvestedOverhang> {
    match end {
        End::Overhang { side, seq } => Some(HarvestedOverhang {
            seq: seq.clone(),
            three_prime: matches!(side, seqforge_core::OverhangSide::ThreePrime),
        }),
        End::Blunt => None,
    }
}

fn junction_detail(right: &End, left: &End) -> String {
    format!("{} ↔ {}", end_label(right), end_label(left))
}

fn end_label(e: &End) -> String {
    match e {
        End::Blunt => "blunt".into(),
        End::Overhang { side, seq } => {
            let side = match side {
                seqforge_core::OverhangSide::FivePrime => "5′",
                seqforge_core::OverhangSide::ThreePrime => "3′",
            };
            let s = String::from_utf8_lossy(seq);
            format!("{side} {s}")
        }
    }
}

/// Assemble a combo of fragments by matching compatible ends in **prepared
/// orientation**, returning every distinct product allowed by `intent`. Combos
/// arrive in bin order; that order and each fragment's orientation are fixed.
pub(super) fn assemble_by_ends(frags: Vec<Fragment>, intent: TopologyIntent) -> Vec<Fragment> {
    let n = frags.len();
    if n == 0 || n > MAX_FRAGMENTS {
        return Vec::new();
    }

    let probe = probe_join(&frags);
    if !probe.chain_ok {
        return Vec::new();
    }

    let bytes = product_bytes(&frags);
    let mut out = Vec::new();
    let mut seen: Vec<(Topology, Vec<u8>)> = Vec::new();

    let mut candidates = Vec::new();
    if matches!(intent, TopologyIntent::Linear | TopologyIntent::Any) {
        candidates.push(Topology::Linear);
    }
    if probe.closes && matches!(intent, TopologyIntent::Circular | TopologyIntent::Any) {
        candidates.push(Topology::Circular);
    }

    for topo in candidates {
        let key = (topo, canonical(&bytes, topo));
        if seen.contains(&key) {
            continue;
        }
        seen.push(key);
        out.push(build_product(&frags, &bytes, topo));
    }
    out
}

fn product_bytes(frags: &[Fragment]) -> Vec<u8> {
    let mut out = Vec::new();
    for f in frags {
        out.extend_from_slice(f.bytes());
    }
    out
}

/// Assemble the product `Fragment`: concatenated bytes + re-homed annotations
/// (each fragment `place`d at its cumulative offset in prepared orientation,
/// `merge=true` so features split across a junction/origin rejoin by lineage).
fn build_product(frags: &[Fragment], bytes: &[u8], topo: Topology) -> Fragment {
    let total = bytes.len();
    let mut ann = Annotations::default();
    let mut offset = 0usize;
    for f in frags {
        transport::place(&mut ann, &f.slice, offset, Orient::Identity, true, total);
        offset += f.len();
    }

    let (left, right) = match topo {
        Topology::Circular => (End::Blunt, End::Blunt),
        Topology::Linear => (frags[0].left.clone(), frags[frags.len() - 1].right.clone()),
    };

    Fragment {
        slice: SeqSlice {
            bytes: bytes.to_vec(),
            features: ann.iter().cloned().collect(),
            primers: ann.primers().cloned().collect(),
        },
        left,
        right,
        topology: topo,
        lineage: Lineage {
            source_doc: "assembly".to_string(),
            source_range: 0..total,
            op: LineageOp::Extract,
        },
    }
}

/// Canonical form for dedup: linear → `min(seq, revcomp)`; circular → the
/// lexicographically minimal rotation of that (rotation-and-strand invariant).
pub(super) fn canonical(bytes: &[u8], topo: Topology) -> Vec<u8> {
    let rc = reverse_complement(bytes);
    match topo {
        Topology::Linear => bytes.to_vec().min(rc),
        Topology::Circular => min_rotation(bytes).min(min_rotation(&rc)),
    }
}

fn min_rotation(s: &[u8]) -> Vec<u8> {
    let n = s.len();
    let mut best = s.to_vec();
    for i in 1..n {
        let rot: Vec<u8> = s[i..].iter().chain(s[..i].iter()).copied().collect();
        if rot < best {
            best = rot;
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;
    use seqforge_core::OverhangSide;
    use seqforge_core::SeqSlice;
    use seqforge_core::document::{Lineage, LineageOp};

    fn frag(bytes: &[u8]) -> Fragment {
        Fragment {
            slice: SeqSlice {
                bytes: bytes.to_vec(),
                features: Vec::new(),
                primers: Vec::new(),
            },
            left: End::Blunt,
            right: End::Blunt,
            topology: Topology::Linear,
            lineage: Lineage {
                source_doc: "t".into(),
                source_range: 0..bytes.len(),
                op: LineageOp::Extract,
            },
        }
    }

    fn sticky(side: OverhangSide, seq: &[u8]) -> End {
        End::Overhang {
            side,
            seq: seq.to_vec(),
        }
    }

    #[test]
    fn bin_order_is_preserved_not_permuted() {
        let products = assemble_by_ends(vec![frag(b"AAAA"), frag(b"BBBB")], TopologyIntent::Linear);
        assert!(!products.is_empty());
        let has_ab = products.iter().any(|p| p.bytes() == b"AAAABBBB");
        assert!(has_ab, "bin order A then B must appear as AAAABBBB");
    }

    #[test]
    fn probe_rejects_incompatible_adjacent_ends() {
        let mut a = frag(b"AAAA");
        let mut b = frag(b"BBBB");
        a.right = sticky(OverhangSide::ThreePrime, b"TGCA");
        b.left = sticky(OverhangSide::FivePrime, b"AATT");
        let probe = probe_join(&[a.clone(), b.clone()]);
        assert!(!probe.chain_ok);
        assert!(!probe.junctions[0].ok);
        assert!(!probe.compatible_for(TopologyIntent::Circular));
        assert!(
            assemble_by_ends(vec![a, b], TopologyIntent::Any).is_empty(),
            "incompatible ends must not assemble"
        );
    }

    #[test]
    fn probe_agrees_with_assemble_on_compatible_blunts() {
        let frags = vec![frag(b"AAAA"), frag(b"BBBB")];
        let probe = probe_join(&frags);
        assert!(probe.chain_ok);
        assert!(probe.closes, "blunt↔blunt closes");
        let products = assemble_by_ends(frags, TopologyIntent::Circular);
        assert_eq!(products.len(), 1);
        assert!(probe.compatible_for(TopologyIntent::Circular));
    }

    #[test]
    fn identity_only_does_not_flip_to_rescue_mismatch() {
        // Same sticky on both rights/lefts in a way that only a flip would fix:
        // A.right = 5′ AATT, B.left = 3′ AATT → incompatible; flip would RC.
        let mut a = frag(b"AAAA");
        let mut b = frag(b"TTTT");
        a.right = sticky(OverhangSide::FivePrime, b"AATT");
        b.left = sticky(OverhangSide::ThreePrime, b"AATT");
        assert!(assemble_by_ends(vec![a, b], TopologyIntent::Any).is_empty());
    }

    #[test]
    fn harvest_scores_three_prime_overhang_seqs() {
        // Sequence-only: 3′ chemistry letters still enter the 5′-assay matrix.
        let mut a = frag(b"AAAA");
        let mut b = frag(b"BBBB");
        a.right = sticky(OverhangSide::ThreePrime, b"TGCA");
        b.right = sticky(OverhangSide::FivePrime, b"AATT");
        let h = harvest_junction_overhangs(&[a, b], true);
        assert_eq!(
            h,
            vec![
                Some(HarvestedOverhang {
                    seq: b"TGCA".to_vec(),
                    three_prime: true,
                }),
                Some(HarvestedOverhang {
                    seq: b"AATT".to_vec(),
                    three_prime: false,
                }),
            ]
        );
        assert!(h.iter().flatten().any(|o| o.three_prime));
        let owned: Vec<Vec<u8>> = h.into_iter().flatten().map(|o| o.seq).collect();
        let refs: Vec<&[u8]> = owned.iter().map(|s| s.as_slice()).collect();
        let report =
            seqforge_fidelity::junction_fidelity(&refs, seqforge_fidelity::Dataset::T4_25C_18h);
        assert!(
            report.set_fidelity.is_some(),
            "3′ sticky letters must score under T4 25C 18h, got {report:?}"
        );
    }

    #[test]
    fn harvest_five_prime_only_has_no_three_prime_flag() {
        let mut a = frag(b"AAAA");
        let mut b = frag(b"BBBB");
        a.right = sticky(OverhangSide::FivePrime, b"AATT");
        b.right = sticky(OverhangSide::FivePrime, b"GATC");
        let h = harvest_junction_overhangs(&[a, b], true);
        assert!(h.iter().flatten().all(|o| !o.three_prime));
    }
}
