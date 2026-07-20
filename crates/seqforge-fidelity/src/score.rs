//! Score an overhang set against a published ligation-frequency matrix.
//!
//! NEB Ligase Fidelity Viewer logic: expand each unique input with its reverse
//! complement (**always** append RC, so palindromes appear twice on the axes),
//! build the subset count matrix, then
//! `set_fidelity = Π_h  M[h][rc(h)] / Σ_j M[h][labels[j]]`.

use crate::fidelity_generated;
use crate::types::{Dataset, FidelityReport, JunctionScore, SubsetMatrix};

/// Map a Golden Gate enzyme name to its published Pryor table when we have one.
pub fn dataset_for_enzyme(name: &str) -> Option<Dataset> {
    let n = name.trim();
    if n.eq_ignore_ascii_case("BsaI") || n.eq_ignore_ascii_case("BsaI-HFv2") {
        Some(Dataset::BsaI)
    } else if n.eq_ignore_ascii_case("BsmBI") || n.eq_ignore_ascii_case("BsmBI-v2") {
        Some(Dataset::BsmBI)
    } else if n.eq_ignore_ascii_case("Esp3I") {
        Some(Dataset::Esp3I)
    } else if n.eq_ignore_ascii_case("BbsI") || n.eq_ignore_ascii_case("BbsI-HF") {
        Some(Dataset::BbsI)
    } else if n.eq_ignore_ascii_case("SapI") {
        Some(Dataset::SapI)
    } else {
        None
    }
}

/// Expand unique input overhangs to Viewer axis labels: for each `h`, append
/// `h` then `rc(h)` even when `h == rc(h)` (palindrome → two identical labels).
pub fn expand_overhang_labels(overhangs: &[&[u8]]) -> Vec<Vec<u8>> {
    let mut labels = Vec::new();
    let mut seen = Vec::new();
    for o in overhangs {
        let h = normalize(o);
        if seen.iter().any(|e: &Vec<u8>| e == &h) {
            continue;
        }
        seen.push(h.clone());
        let rc = revcomp(&h);
        labels.push(h);
        labels.push(rc);
    }
    labels
}

/// Subset ligation-frequency matrix for an overhang set (NEB Viewer axes).
///
/// Returns `None` when any overhang is wrong-length / non-ACGT for `dataset`
/// (never fabricates a partial matrix).
pub fn subset_matrix(overhangs: &[&[u8]], dataset: Dataset) -> Option<SubsetMatrix> {
    let (inputs, uncovered) = classify_inputs(overhangs, dataset);
    if inputs.is_empty() || !uncovered.is_empty() {
        return None;
    }
    Some(build_subset_matrix(&inputs, dataset))
}

/// Predict set ligation fidelity (NEB Ligase Fidelity Viewer-style).
///
/// Builds the RC-expanded subset matrix (palindromes doubled), then for each
/// unique input overhang `h`:
/// `junction = M[h][rc(h)] / Σ_j M[h][labels[j]]`,
/// `set_fidelity = Π junctions`.
///
/// Wrong-length / non-ACGT overhangs land in `uncovered` and yield
/// `set_fidelity = None` (never a fabricated percentage).
pub fn junction_fidelity(overhangs: &[&[u8]], dataset: Dataset) -> FidelityReport {
    let (inputs, uncovered) = classify_inputs(overhangs, dataset);

    if inputs.is_empty() || !uncovered.is_empty() {
        return FidelityReport {
            set_fidelity: None,
            junctions: Vec::new(),
            worst: None,
            uncovered,
            matrix: None,
        };
    }

    let matrix = build_subset_matrix(&inputs, dataset);
    let want = dataset.overhang_len() as usize;
    let full = fidelity_generated::matrix(dataset);
    let nfull = 1usize << (2 * want);

    let mut junctions = Vec::with_capacity(inputs.len());
    let mut product = 1.0_f64;
    let mut worst: Option<usize> = None;
    let mut worst_fid = f64::INFINITY;

    for h in &inputs {
        let intended = revcomp(h);
        let hi = encode(h, want);
        // Single WC cell (not summed over duplicate palindrome columns).
        let on = full[hi * nfull + encode(&intended, want)] as u32;
        let mut total = 0u32;
        for s in &matrix.labels {
            total = total.saturating_add(full[hi * nfull + encode(s, want)] as u32);
        }
        let off = total.saturating_sub(on);
        let fidelity = if total == 0 {
            0.0
        } else {
            f64::from(on) / f64::from(total)
        };
        if fidelity < worst_fid {
            worst_fid = fidelity;
            worst = Some(junctions.len());
        }
        product *= fidelity;
        junctions.push(JunctionScore {
            overhang: h.clone(),
            fidelity,
            on_target: on,
            off_target: off,
        });
    }

    FidelityReport {
        set_fidelity: Some(product),
        junctions,
        worst,
        uncovered,
        matrix: Some(matrix),
    }
}

fn classify_inputs(overhangs: &[&[u8]], dataset: Dataset) -> (Vec<Vec<u8>>, Vec<Vec<u8>>) {
    let want = dataset.overhang_len() as usize;
    let mut uncovered = Vec::new();
    let mut inputs = Vec::new();
    for o in overhangs {
        if o.len() != want || !o.iter().all(|&b| is_acgt(b)) {
            uncovered.push(o.to_vec());
            continue;
        }
        let h = normalize(o);
        if !inputs.iter().any(|e: &Vec<u8>| e == &h) {
            inputs.push(h);
        }
    }
    (inputs, uncovered)
}

fn build_subset_matrix(inputs: &[Vec<u8>], dataset: Dataset) -> SubsetMatrix {
    let refs: Vec<&[u8]> = inputs.iter().map(|s| s.as_slice()).collect();
    let labels = expand_overhang_labels(&refs);
    let want = dataset.overhang_len() as usize;
    let full = fidelity_generated::matrix(dataset);
    let nfull = 1usize << (2 * want);
    let n = labels.len();
    let mut counts = vec![0u32; n * n];
    for (i, row) in labels.iter().enumerate() {
        let ri = encode(row, want);
        for (j, col) in labels.iter().enumerate() {
            let cj = encode(col, want);
            counts[i * n + j] = full[ri * nfull + cj] as u32;
        }
    }
    SubsetMatrix { labels, counts }
}

fn normalize(seq: &[u8]) -> Vec<u8> {
    seq.iter().map(|b| b.to_ascii_uppercase()).collect()
}

fn is_acgt(b: u8) -> bool {
    matches!(b.to_ascii_uppercase(), b'A' | b'C' | b'G' | b'T')
}

fn revcomp(seq: &[u8]) -> Vec<u8> {
    seq.iter()
        .rev()
        .map(|&b| match b.to_ascii_uppercase() {
            b'A' => b'T',
            b'T' => b'A',
            b'C' => b'G',
            b'G' => b'C',
            x => x,
        })
        .collect()
}

/// Base-4 encode A/C/G/T overhang → row/col index.
fn encode(seq: &[u8], len: usize) -> usize {
    debug_assert_eq!(seq.len(), len);
    let mut idx = 0usize;
    for &b in seq {
        let d = match b.to_ascii_uppercase() {
            b'A' => 0,
            b'C' => 1,
            b'G' => 2,
            b'T' => 3,
            _ => 0,
        };
        idx = (idx << 2) | d;
    }
    idx
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn high_fidelity_4nt_set_scores_high() {
        let ohs: &[&[u8]] = &[b"GGAG", b"CGAA", b"AGAC", b"ATCG", b"TAAT"];
        let r = junction_fidelity(ohs, Dataset::T4_25C_18h);
        let f = r.set_fidelity.expect("scored");
        assert!(f > 0.95, "got {f}");
        assert!(r.uncovered.is_empty());
        assert_eq!(r.junctions.len(), 5);
        assert!(r.matrix.is_some());
    }

    #[test]
    fn wrong_length_uncovered() {
        let r = junction_fidelity(&[b"GGAG", b"AAA"], Dataset::T4_25C_18h);
        assert!(r.set_fidelity.is_none());
        assert!(!r.uncovered.is_empty());
        assert!(r.matrix.is_none());
    }

    #[test]
    fn sapi_covers_3nt_only() {
        assert!(Dataset::SapI.covers(&[b"AAA", b"GGG"]));
        assert!(!Dataset::SapI.covers(&[b"AAAA"]));
        assert!(Dataset::BsaI.covers(&[b"AAAA"]));
        assert!(!Dataset::BsaI.covers(&[b"AAA"]));
    }

    #[test]
    fn dataset_for_enzyme_maps() {
        assert_eq!(dataset_for_enzyme("BsaI"), Some(Dataset::BsaI));
        assert_eq!(dataset_for_enzyme("SapI"), Some(Dataset::SapI));
        assert_eq!(dataset_for_enzyme("EcoRI"), None);
    }

    #[test]
    fn sapi_3nt_scores() {
        let ohs: &[&[u8]] = &[b"ATG", b"GAC"];
        let r = junction_fidelity(ohs, Dataset::SapI);
        assert!(r.set_fidelity.is_some(), "{r:?}");
        let f = r.set_fidelity.unwrap();
        assert!((0.0..=1.0).contains(&f));
    }

    #[test]
    fn expand_doubles_palindrome() {
        let labels = expand_overhang_labels(&[b"AATT"]);
        assert_eq!(labels.len(), 2);
        assert_eq!(labels[0], b"AATT");
        assert_eq!(labels[1], b"AATT");
    }

    #[test]
    fn expand_adds_rc_for_non_palindrome() {
        let labels = expand_overhang_labels(&[b"AAGG"]);
        assert_eq!(labels, vec![b"AAGG".to_vec(), b"CCTT".to_vec()]);
    }

    #[test]
    fn viewer_set_high_without_aatt() {
        // NEB example: AAGG,ACTC,AGGA,AGTG → ~99% (BsaI-HFv2 cycling on Viewer;
        // our Pryor BsaI table is the closest shipped matrix).
        let ohs: &[&[u8]] = &[b"AAGG", b"ACTC", b"AGGA", b"AGTG"];
        let r = junction_fidelity(ohs, Dataset::BsaI);
        let f = r.set_fidelity.expect("scored");
        assert!(f > 0.95, "expected high fidelity, got {f}");
        let m = r.matrix.expect("matrix");
        assert_eq!(m.labels.len(), 8);
    }

    #[test]
    fn viewer_set_drops_with_palindrome_aatt() {
        // Same set + AATT → Viewer ~50%; palindrome doubles in the denominator.
        let ohs: &[&[u8]] = &[b"AAGG", b"ACTC", b"AGGA", b"AGTG", b"AATT"];
        let r = junction_fidelity(ohs, Dataset::BsaI);
        let f = r.set_fidelity.expect("scored");
        assert!(
            (0.40..0.60).contains(&f),
            "expected ~50% with AATT palindrome, got {f}"
        );
        let m = r.matrix.expect("matrix");
        assert_eq!(m.labels.len(), 10);
        let aatt = m.labels.iter().filter(|l| l.as_slice() == b"AATT").count();
        assert_eq!(aatt, 2, "palindrome must appear twice on axes");
        let aatt_j = r
            .junctions
            .iter()
            .find(|j| j.overhang == b"AATT")
            .expect("AATT junction");
        assert!(
            (0.45..0.55).contains(&aatt_j.fidelity),
            "AATT junction ~0.5, got {}",
            aatt_j.fidelity
        );
    }

    #[test]
    fn subset_matrix_matches_report() {
        let ohs: &[&[u8]] = &[b"AAGG", b"AATT"];
        let m = subset_matrix(ohs, Dataset::BsaI).expect("matrix");
        let r = junction_fidelity(ohs, Dataset::BsaI);
        assert_eq!(Some(m.labels), r.matrix.as_ref().map(|x| x.labels.clone()));
    }
}
