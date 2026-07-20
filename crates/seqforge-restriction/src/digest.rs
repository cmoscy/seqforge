//! Multi-enzyme digestion — Tier 2.
//!
//! Turns the sites found by [`crate::find_all_sites`] into the pieces a molecule
//! breaks into. This is **pure geometry over owned values** — no `seqforge-core`
//! types — so the crate stays zero-dep and extractable (the assembly-side
//! [`crate::Site`] → `core::Fragment` bridge lives in `seqforge-bio`, mirroring
//! `site_to_cutsite`).
//!
//! ## Model
//!
//! A cut severs the **top strand** at `Site::top_cut`. Fragments therefore
//! partition the molecule at top-cut positions, so concatenating fragment
//! `bytes` in boundary order always reconstructs the original top strand — the
//! Tier-2 reassembly invariant (the seed of Tier-3 `ligate`).
//!
//! Each end's overhang is the single-stranded extension, reported **5′→3′**.
//! For a cut with top-strand footprint `R` (the bases between the two cut
//! points), the two ends it creates are reverse complements of each other:
//!
//! | kind | fragment on the **right** (its left end) | fragment on the **left** (its right end) |
//! |------|------------------------------------------|------------------------------------------|
//! | 5′   | `R`                                      | `revcomp(R)`                             |
//! | 3′   | `revcomp(R)`                             | `R`                                      |
//!
//! ## Known Tier-2 simplifications
//!
//! - Methylation-**Blocked** sites are dropped from the cut set (default
//!   context Dam⁺ Dcm⁺); **Impaired** sites still cut but emit a warning.
//! - Two enzymes cutting at the **same** top-strand position collapse to one
//!   boundary (first wins).
//! - Cuts closer together than an overhang length still each form a boundary;
//!   no partial-digest or star-activity modelling.

use std::collections::HashMap;

use crate::enzyme::{Enzyme, OverhangKind};
use crate::methylation::{site_methyl_state, MethylContext, SiteMethylState};
use crate::scan::find_all_sites;

/// Fragment topology. `Circular` = an uncut circular molecule (no free ends);
/// every cut piece is `Linear`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Topology {
    Linear,
    Circular,
}

/// One join interface of a fragment. Pure geometry; `seqforge-bio` maps this to
/// `core::End`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndGeom {
    pub kind: OverhangKind,
    /// This end's single-stranded overhang, read 5′→3′ (empty when blunt).
    pub seq: Vec<u8>,
    /// The enzyme that produced this end; `None` = a free molecule terminus.
    pub enzyme: Option<&'static str>,
}

impl EndGeom {
    fn blunt() -> Self {
        EndGeom {
            kind: OverhangKind::Blunt,
            seq: Vec::new(),
            enzyme: None,
        }
    }
}

/// One virtual piece of a digested molecule, in **source coordinates**.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestrictionFragment {
    /// Top-strand start (a top-cut position, or `0`/native terminus).
    pub span_start: usize,
    /// Top-strand length. For a circular molecule the span may wrap the origin;
    /// `span_start + span_len` is taken mod the molecule length.
    pub span_len: usize,
    /// Top strand between the two boundary cuts (wrap already resolved).
    pub bytes: Vec<u8>,
    pub left: EndGeom,
    pub right: EndGeom,
    pub topology: Topology,
}

/// Result of a digest: the fragment set plus any methylation warnings.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DigestResult {
    pub fragments: Vec<RestrictionFragment>,
    pub warnings: Vec<String>,
}

/// Digest `seq` with `enzymes`. Methylation-`Blocked` sites are excluded from
/// the cut set under `methyl` (pass [`MethylContext::NONE`] to ignore
/// methylation); `Impaired` sites still cut but add a warning.
pub fn digest(
    seq: &[u8],
    enzymes: &[&'static Enzyme],
    circular: bool,
    methyl: &MethylContext,
) -> DigestResult {
    let l = seq.len();
    let mut warnings = Vec::new();
    if l == 0 {
        return DigestResult::default();
    }

    // Name → enzyme, for `overhang_kind` + methylation sensitivity lookup.
    let by_name: HashMap<&str, &'static Enzyme> = enzymes.iter().map(|e| (e.name, *e)).collect();

    let mut boundaries: Vec<CutBoundary> = Vec::new();
    for s in &find_all_sites(seq, enzymes, circular) {
        let Some(enz) = by_name
            .get(s.enzyme)
            .copied()
            .or_else(|| crate::enzyme_by_name(s.enzyme))
        else {
            continue;
        };

        match site_methyl_state(
            s.recognition_start,
            s.recognition_end,
            &enz.methylation,
            seq,
            methyl,
        ) {
            SiteMethylState::Blocked => {
                warnings.push(format!(
                    "{} site at {} not cut (methylation-blocked)",
                    s.enzyme, s.recognition_start
                ));
                continue;
            }
            SiteMethylState::Impaired => warnings.push(format!(
                "{} site at {} may be impaired by methylation",
                s.enzyme, s.recognition_start
            )),
            SiteMethylState::Cuttable => {}
        }

        let kind = enz.overhang_kind();
        boundaries.push(CutBoundary {
            top_cut: s.top_cut % l,
            kind,
            footprint: overhang_footprint(seq, s.top_cut, s.bottom_cut, kind, l, circular),
            enzyme: s.enzyme,
        });
    }

    // One boundary per top-cut position (coincident cuts collapse — first wins).
    boundaries.sort_by_key(|b| b.top_cut);
    boundaries.dedup_by_key(|b| b.top_cut);

    let fragments = if boundaries.is_empty() {
        vec![RestrictionFragment {
            span_start: 0,
            span_len: l,
            bytes: uppercase(seq),
            left: EndGeom::blunt(),
            right: EndGeom::blunt(),
            topology: if circular {
                Topology::Circular
            } else {
                Topology::Linear
            },
        }]
    } else if circular {
        build_circular(seq, &boundaries, l)
    } else {
        build_linear(seq, &boundaries, l)
    };

    DigestResult {
        fragments,
        warnings,
    }
}

struct CutBoundary {
    top_cut: usize,
    kind: OverhangKind,
    /// Top-strand bases spanning the overhang (empty when blunt).
    footprint: Vec<u8>,
    enzyme: &'static str,
}

/// Left end of the fragment lying to the **right** of a cut.
fn right_frag_left_end(b: &CutBoundary) -> EndGeom {
    let seq = match b.kind {
        OverhangKind::Blunt => Vec::new(),
        OverhangKind::FivePrime(_) => b.footprint.clone(),
        OverhangKind::ThreePrime(_) => revcomp(&b.footprint),
    };
    EndGeom {
        kind: b.kind,
        seq,
        enzyme: Some(b.enzyme),
    }
}

/// Right end of the fragment lying to the **left** of a cut.
fn left_frag_right_end(b: &CutBoundary) -> EndGeom {
    let seq = match b.kind {
        OverhangKind::Blunt => Vec::new(),
        OverhangKind::FivePrime(_) => revcomp(&b.footprint),
        OverhangKind::ThreePrime(_) => b.footprint.clone(),
    };
    EndGeom {
        kind: b.kind,
        seq,
        enzyme: Some(b.enzyme),
    }
}

fn build_linear(seq: &[u8], boundaries: &[CutBoundary], l: usize) -> Vec<RestrictionFragment> {
    // Boundary points = the two native termini plus every cut, sorted-unique.
    let mut points: Vec<usize> = Vec::with_capacity(boundaries.len() + 2);
    points.push(0);
    points.extend(boundaries.iter().map(|b| b.top_cut));
    points.push(l);
    points.sort_unstable();
    points.dedup();

    let mut frags = Vec::new();
    for w in points.windows(2) {
        let (a, b) = (w[0], w[1]);
        if b <= a {
            continue; // empty / degenerate segment
        }
        let left = boundary_at(boundaries, a)
            .map(right_frag_left_end)
            .unwrap_or_else(EndGeom::blunt);
        let right = boundary_at(boundaries, b)
            .map(left_frag_right_end)
            .unwrap_or_else(EndGeom::blunt);
        frags.push(RestrictionFragment {
            span_start: a,
            span_len: b - a,
            bytes: uppercase(&seq[a..b]),
            left,
            right,
            topology: Topology::Linear,
        });
    }
    frags
}

fn build_circular(seq: &[u8], boundaries: &[CutBoundary], l: usize) -> Vec<RestrictionFragment> {
    let k = boundaries.len();
    let mut frags = Vec::with_capacity(k);
    for i in 0..k {
        let b_start = &boundaries[i];
        let b_end = &boundaries[(i + 1) % k];
        let start = b_start.top_cut;
        // Wrap-aware length; a lone cut (k == 1) sweeps the whole circle.
        let raw = (b_end.top_cut + l - start) % l;
        let span_len = if raw == 0 { l } else { raw };
        frags.push(RestrictionFragment {
            span_start: start,
            span_len,
            bytes: sweep(seq, start, span_len, l, true),
            left: right_frag_left_end(b_start),
            right: left_frag_right_end(b_end),
            topology: Topology::Linear,
        });
    }
    frags
}

fn boundary_at(boundaries: &[CutBoundary], pos: usize) -> Option<&CutBoundary> {
    boundaries.iter().find(|b| b.top_cut == pos)
}

/// Top-strand bases spanning the overhang between the two cut points.
fn overhang_footprint(
    seq: &[u8],
    top_cut: usize,
    bottom_cut: usize,
    kind: OverhangKind,
    l: usize,
    circular: bool,
) -> Vec<u8> {
    let (start, n) = match kind {
        OverhangKind::Blunt => return Vec::new(),
        OverhangKind::FivePrime(n) => (top_cut % l, n as usize),
        OverhangKind::ThreePrime(n) => (bottom_cut % l, n as usize),
    };
    sweep(seq, start, n, l, circular)
}

/// `n` bases from `start`, forward, uppercased; wrapping mod `l` when `circular`.
fn sweep(seq: &[u8], start: usize, n: usize, l: usize, circular: bool) -> Vec<u8> {
    let mut v = Vec::with_capacity(n);
    for i in 0..n {
        let idx = if circular { (start + i) % l } else { start + i };
        if let Some(&b) = seq.get(idx) {
            v.push(b.to_ascii_uppercase());
        }
    }
    v
}

fn uppercase(seq: &[u8]) -> Vec<u8> {
    seq.iter().map(|b| b.to_ascii_uppercase()).collect()
}

fn revcomp(s: &[u8]) -> Vec<u8> {
    s.iter()
        .rev()
        .map(|&b| match b.to_ascii_uppercase() {
            b'A' => b'T',
            b'T' => b'A',
            b'C' => b'G',
            b'G' => b'C',
            other => other,
        })
        .collect()
}

/// Reassemble digest fragments by concatenating their top strands in boundary
/// order — the Tier-2 seed of Tier-3 `ligate`. Returns the reconstructed top
/// strand (a rotation of the original for a circular digest).
pub fn reassemble(fragments: &[RestrictionFragment]) -> Vec<u8> {
    let mut out = Vec::new();
    for f in fragments {
        out.extend_from_slice(&f.bytes);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::enzyme_by_name;

    fn enz(name: &str) -> &'static Enzyme {
        enzyme_by_name(name).unwrap()
    }

    #[test]
    fn ecori_single_cut_gives_two_fragments_with_aatt_overhangs() {
        // ...G^AATTC...  → 5' AATT overhang on both new ends.
        let seq = b"AAAGAATTCTTT";
        let r = digest(seq, &[enz("EcoRI")], false, &MethylContext::NONE);
        assert_eq!(r.fragments.len(), 2);

        let (l, rt) = (&r.fragments[0], &r.fragments[1]);
        // Left fragment: native 5' terminus (blunt), cut 3' end.
        assert_eq!(l.left, EndGeom::blunt());
        assert_eq!(l.right.kind, OverhangKind::FivePrime(4));
        assert_eq!(l.right.seq, b"AATT");
        assert_eq!(l.right.enzyme, Some("EcoRI"));
        // Right fragment: cut 5' end, native 3' terminus (blunt).
        assert_eq!(rt.left.kind, OverhangKind::FivePrime(4));
        assert_eq!(rt.left.seq, b"AATT");
        assert_eq!(rt.right, EndGeom::blunt());

        // Top-strand partition reconstructs the input exactly.
        assert_eq!(reassemble(&r.fragments), seq);
    }

    #[test]
    fn pst_i_gives_three_prime_tgca_overhang() {
        // CTGCA^G → 3' TGCA overhang.
        let seq = b"AAACTGCAGTTT";
        let r = digest(seq, &[enz("PstI")], false, &MethylContext::NONE);
        assert_eq!(r.fragments.len(), 2);
        let cut_end = &r.fragments[0].right;
        assert_eq!(cut_end.kind, OverhangKind::ThreePrime(4));
        assert_eq!(cut_end.seq, b"TGCA");
        assert_eq!(r.fragments[1].left.seq, b"TGCA");
        assert_eq!(reassemble(&r.fragments), seq);
    }

    #[test]
    fn sma_i_is_blunt() {
        let seq = b"AAACCCGGGTTT";
        let r = digest(seq, &[enz("SmaI")], false, &MethylContext::NONE);
        assert_eq!(r.fragments.len(), 2);
        assert_eq!(r.fragments[0].right.kind, OverhangKind::Blunt);
        assert!(r.fragments[0].right.seq.is_empty());
        assert_eq!(r.fragments[0].right.enzyme, Some("SmaI"));
        assert_eq!(reassemble(&r.fragments), seq);
    }

    #[test]
    fn bsa_i_typeiis_reverse_strand_overhang_is_complementary() {
        // BsaI GGTCTC(1/5): the two ends of one cut are reverse complements.
        let seq = b"AAAAAGGTCTCACGTGAAAAAAAAAA";
        let r = digest(seq, &[enz("BsaI")], false, &MethylContext::NONE);
        assert!(r.fragments.len() >= 2);
        // Find the cut end pair and check RC-complementarity.
        for w in r.fragments.windows(2) {
            if let (OverhangKind::FivePrime(_), OverhangKind::FivePrime(_)) =
                (w[0].right.kind, w[1].left.kind)
            {
                assert_eq!(w[0].right.seq, revcomp(&w[1].left.seq));
            }
        }
        assert_eq!(reassemble(&r.fragments), seq);
    }

    #[test]
    fn linear_no_cut_is_single_linear_fragment() {
        let seq = b"ACGTACGTACGT";
        let r = digest(seq, &[enz("EcoRI")], false, &MethylContext::NONE);
        assert_eq!(r.fragments.len(), 1);
        assert_eq!(r.fragments[0].topology, Topology::Linear);
        assert_eq!(r.fragments[0].left, EndGeom::blunt());
        assert_eq!(r.fragments[0].right, EndGeom::blunt());
    }

    #[test]
    fn circular_no_cut_is_single_circular_fragment() {
        let seq = b"ACGTACGTACGT";
        let r = digest(seq, &[enz("EcoRI")], true, &MethylContext::NONE);
        assert_eq!(r.fragments.len(), 1);
        assert_eq!(r.fragments[0].topology, Topology::Circular);
    }

    #[test]
    fn circular_one_cut_is_one_linear_fragment_full_length() {
        let seq = b"AAAGAATTCTTT";
        let r = digest(seq, &[enz("EcoRI")], true, &MethylContext::NONE);
        assert_eq!(r.fragments.len(), 1);
        assert_eq!(r.fragments[0].topology, Topology::Linear);
        assert_eq!(r.fragments[0].span_len, seq.len());
        // Both ends come from the same EcoRI cut.
        assert_eq!(r.fragments[0].left.seq, b"AATT");
        assert_eq!(r.fragments[0].right.seq, b"AATT");
    }

    #[test]
    fn circular_two_cuts_give_two_linear_fragments_covering_the_circle() {
        let seq = b"AAAGAATTCTTTGGGGAATTCCCC"; // two EcoRI sites
        let r = digest(seq, &[enz("EcoRI")], true, &MethylContext::NONE);
        assert_eq!(r.fragments.len(), 2);
        let total: usize = r.fragments.iter().map(|f| f.span_len).sum();
        assert_eq!(total, seq.len());
    }

    #[test]
    fn circular_origin_spanning_site_found_only_when_circular() {
        // EcoRI site straddling the origin: "TTC...GAA".
        let seq = b"TTCAAAAAAAAAAGAA";
        let linear = digest(seq, &[enz("EcoRI")], false, &MethylContext::NONE);
        assert_eq!(linear.fragments.len(), 1); // no cut
        let circ = digest(seq, &[enz("EcoRI")], true, &MethylContext::NONE);
        assert_eq!(circ.fragments.len(), 1); // one cut on a circle → one linear frag
        assert_eq!(circ.fragments[0].topology, Topology::Linear);
    }

    #[test]
    fn dam_methylation_blocks_a_cut() {
        // MboI recognizes GATC and is Dam-blocked; flank with GATC so the
        // context is present. Under Dam+ the site does not cut.
        let mbo = enz("MboI");
        let seq = b"AAAAGATCAAAA";
        let blocked = digest(seq, &[mbo], false, &MethylContext::default());
        let cut = digest(seq, &[mbo], false, &MethylContext::NONE);
        assert!(
            blocked.fragments.len() < cut.fragments.len(),
            "Dam+ must suppress at least one MboI boundary: {} vs {}",
            blocked.fragments.len(),
            cut.fragments.len()
        );
        assert!(blocked.warnings.iter().any(|w| w.contains("blocked")));
    }
}
