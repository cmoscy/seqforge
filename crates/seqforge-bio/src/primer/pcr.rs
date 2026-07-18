//! PCR amplification (Primers Phase 3.1a). Given a template and two attached
//! primers, compute the amplicon product's bytes + the geometric facts the
//! applier needs to re-home the template's annotations onto the product.
//!
//! This module is **pure biology**: it produces the product bytes and the
//! template-frame amplicon [`Span`] but does **not** touch `core::Annotations`
//! transport — that (extract/place onto a fresh buffer) stays in the app-layer
//! applier so bio owns biology and the app owns buffers.
//!
//! ## What PCR is here
//!
//! The product reads, 5'→3' on the top strand:
//!
//! ```text
//!   fwd.sequence  ++  template[fwd_footprint_end .. rev_footprint_start]  ++  revcomp(rev.sequence)
//!   └ tail + annealed ┘   └────────── interior between footprints ──────┘   └ annealed + tail ┘
//! ```
//!
//! Consequences that fall out for free:
//! - **Mismatches bake in** — the product carries the *primer's* bases at each
//!   footprint (not the template's), so a mismatched primer is site-directed
//!   mutagenesis.
//! - **5' tails become the product ends** — overhangs, for free.
//! - **Circular templates** give around-the-horn / whole-plasmid amplification
//!   (outward-facing primers → Q5/KLD site-directed mutagenesis): the amplicon
//!   [`Span`] wraps the origin, handled by [`Span::between`].
//!
//! A PCR product is always **linear**, even off a circular template.

use seqforge_core::{Primer, PrimerId, Span, Strand};

use super::{AnnealSettings, decompose_primer, find_primer_binding_sites};
use crate::dna::reverse_complement;

/// The result of a successful [`pcr`] run — product bytes plus the geometry the
/// applier uses to inherit the template's annotations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PcrProduct {
    /// Product sequence, 5'→3' on the top strand (blunt, linear).
    pub bytes: Vec<u8>,
    /// The region on the **template** the product spans (`fwd 5'-anchor →
    /// rev 5'-anchor`), wrap-aware. The applier `extract`s this from the
    /// template annotations, then `place`s it at [`Self::tail_f_len`].
    pub amplicon: Span,
    /// The forward primer's 5'-tail length = the product-coordinate offset at
    /// which the amplicon's template annotations land (the tail prepends bases
    /// ahead of the first template column).
    pub tail_f_len: usize,
    /// Non-fatal advisories (mispriming: >1 binding site for a primer). Reported,
    /// never blocking.
    pub warnings: Vec<String>,
}

/// Why a [`pcr`] run could not produce a product.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PcrError {
    /// A primer has no binding (`binding = None`) — attach or rescan it first.
    Detached(PrimerId),
    /// The primers are not one Forward + one Reverse (can't flank a product).
    Orientation,
    /// The primers don't flank an amplifiable region on this template (e.g. a
    /// linear template with the reverse primer upstream of the forward one, or
    /// overlapping footprints).
    NoProduct,
    /// The template is empty.
    EmptyTemplate,
}

impl std::fmt::Display for PcrError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PcrError::Detached(id) => write!(
                f,
                "primer {id} is not attached to the template — attach or rescan it first"
            ),
            PcrError::Orientation => write!(
                f,
                "PCR needs one forward and one reverse primer flanking the product"
            ),
            PcrError::NoProduct => {
                write!(f, "the primers do not flank an amplifiable product region")
            }
            PcrError::EmptyTemplate => write!(f, "cannot amplify an empty template"),
        }
    }
}

impl std::error::Error for PcrError {}

/// Amplify between `fwd` and `rev` on `template`. See the module docs for the
/// product shape. Both primers must be attached (`binding = Some`); `fwd` must
/// be Forward and `rev` Reverse.
pub fn pcr(
    template: &[u8],
    fwd: &Primer,
    rev: &Primer,
    circular: bool,
) -> Result<PcrProduct, PcrError> {
    let total = template.len();
    if total == 0 {
        return Err(PcrError::EmptyTemplate);
    }
    if fwd.strand != Strand::Forward || rev.strand != Strand::Reverse {
        return Err(PcrError::Orientation);
    }
    let bf = fwd.binding.ok_or(PcrError::Detached(fwd.id))?;
    let br = rev.binding.ok_or(PcrError::Detached(rev.id))?;

    // Top-strand footprint coordinates. Forward 3' anchor is at `fe`; reverse
    // 3' anchor is at `rs` (its footprint runs rs..re on the top strand).
    let fs = bf.start;
    let fe = bf.start + bf.len;
    let rs = br.start;
    let re = br.start + br.len;

    // Amplicon (fwd 5'-anchor → rev 5'-anchor) and the interior between the two
    // footprints. `Span::between` gives the wrap-aware sweep for circular
    // templates; for linear ones we require the forward primer upstream and
    // non-overlapping footprints (else there is no linear product).
    let (amplicon, interior) = if circular {
        (Span::between(fs, re, total), Span::between(fe, rs, total))
    } else {
        if fs > rs || fe > rs || re > total {
            return Err(PcrError::NoProduct);
        }
        (Span::from_range(fs..re), Span::from_range(fe..rs))
    };
    if amplicon.is_empty() {
        return Err(PcrError::NoProduct);
    }

    // Product = fwd oligo (tail + annealed) + interior template + revcomp(rev oligo).
    let mut bytes = Vec::with_capacity(fwd.sequence.len() + interior.len + rev.sequence.len());
    bytes.extend(fwd.sequence.bytes().map(|b| b.to_ascii_uppercase()));
    for run in interior.linear_pieces(total).iter() {
        bytes.extend_from_slice(&template[run.start..run.end.min(total)]);
    }
    bytes.extend_from_slice(&reverse_complement(
        &rev.sequence
            .bytes()
            .map(|b| b.to_ascii_uppercase())
            .collect::<Vec<u8>>(),
    ));

    let tail_f_len = decompose_primer(&fwd.sequence, &bf.range(), Strand::Forward, template)
        .tail
        .len();

    // Mispriming advisory: more than one binding site for either primer.
    let settings = AnnealSettings::default();
    let mut warnings = Vec::new();
    let n_fwd = find_primer_binding_sites(&fwd.sequence, template, circular, settings).len();
    if n_fwd > 1 {
        warnings.push(format!(
            "forward primer binds {n_fwd} sites on this template (possible mispriming)"
        ));
    }
    let n_rev = find_primer_binding_sites(&rev.sequence, template, circular, settings).len();
    if n_rev > 1 {
        warnings.push(format!(
            "reverse primer binds {n_rev} sites on this template (possible mispriming)"
        ));
    }

    Ok(PcrProduct {
        bytes,
        amplicon,
        tail_f_len,
        warnings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    // Template top strand (index 0..30):
    //   0         1         2
    //   0123456789012345678901234567890
    //   ATGCGTACCAGGTTCAAGGCATTGGCCTAAG
    const T: &[u8] = b"ATGCGTACCAGGTTCAAGGCATTGGCCTAAG";

    fn primer(name: &str, seq: &str, binding: Option<Span>, strand: Strand) -> Primer {
        Primer {
            id: PrimerId(0),
            name: name.to_string(),
            sequence: seq.to_string(),
            binding,
            strand,
            qualifiers: BTreeMap::new(),
        }
    }

    /// Forward primer = top[2..8]; reverse primer = revcomp(top[22..28]).
    fn perfect_pair() -> (Primer, Primer) {
        let fwd_seq = std::str::from_utf8(&T[2..8]).unwrap().to_string(); // GTACCA
        let rev_top = &T[22..28]; // GGCCTA
        let rev_seq = String::from_utf8(reverse_complement(rev_top)).unwrap();
        (
            primer("F", &fwd_seq, Some(Span::new(2, 6)), Strand::Forward),
            primer("R", &rev_seq, Some(Span::new(22, 6)), Strand::Reverse),
        )
    }

    #[test]
    fn linear_perfect_product_is_the_amplicon() {
        let (f, r) = perfect_pair();
        let p = pcr(T, &f, &r, false).unwrap();
        // No tails → product == template[2..28].
        assert_eq!(p.bytes, T[2..28].to_vec());
        assert_eq!(p.amplicon, Span::new(2, 26));
        assert_eq!(p.tail_f_len, 0);
        assert!(p.warnings.is_empty());
    }

    #[test]
    fn tails_become_product_ends() {
        let (mut f, mut r) = perfect_pair();
        f.sequence = format!("AAAA{}", f.sequence); // 4-base 5' tail
        r.sequence = format!("CGCG{}", r.sequence); // 4-base 5' tail (bottom strand)
        let p = pcr(T, &f, &r, false).unwrap();
        assert_eq!(p.tail_f_len, 4);
        // Product starts with the forward tail...
        assert!(p.bytes.starts_with(b"AAAA"));
        // ...and ends with the revcomp of the reverse tail (CGCG -> CGCG).
        assert!(p.bytes.ends_with(&reverse_complement(b"CGCG")));
        // Length = fwd oligo + interior + rev oligo.
        assert_eq!(
            p.bytes.len(),
            f.sequence.len() + (22 - 8) + r.sequence.len()
        );
    }

    #[test]
    fn mismatch_bakes_into_the_product() {
        let (mut f, r) = perfect_pair();
        // Change the 5' base of the annealed forward primer (keeps the 3' anchor).
        f.sequence = format!("T{}", &f.sequence[1..]); // was G..
        let p = pcr(T, &f, &r, false).unwrap();
        // Product carries the primer's mutated base, not the template's.
        assert_eq!(p.bytes[0], b'T');
    }

    #[test]
    fn detached_primer_errors() {
        let (mut f, r) = perfect_pair();
        f.binding = None;
        assert_eq!(pcr(T, &f, &r, false), Err(PcrError::Detached(f.id)));
    }

    #[test]
    fn wrong_orientation_errors() {
        let (f, mut r) = perfect_pair();
        r.strand = Strand::Forward;
        assert_eq!(pcr(T, &f, &r, false), Err(PcrError::Orientation));
    }

    #[test]
    fn linear_reverse_upstream_has_no_product() {
        let (f, r) = perfect_pair();
        // Swap roles: reverse primer sits upstream of forward → no linear product.
        assert_eq!(pcr(T, &r, &f, false), Err(PcrError::Orientation));
        // Same-orientation-but-crossed still NoProduct once orientation is ok:
        let (mut f2, r2) = perfect_pair();
        f2.binding = Some(Span::new(24, 6)); // forward now downstream of reverse
        assert_eq!(pcr(T, &f2, &r2, false), Err(PcrError::NoProduct));
    }

    #[test]
    fn circular_around_the_horn_wraps() {
        // Outward-facing primers on a circle: forward near the end, reverse near
        // the start, so the amplicon wraps the origin (whole-plasmid).
        let fwd_seq = std::str::from_utf8(&T[24..30]).unwrap().to_string();
        let rev_top = &T[2..8];
        let rev_seq = String::from_utf8(reverse_complement(rev_top)).unwrap();
        let f = primer("F", &fwd_seq, Some(Span::new(24, 6)), Strand::Forward);
        let r = primer("R", &rev_seq, Some(Span::new(2, 6)), Strand::Reverse);
        let p = pcr(T, &f, &r, true).unwrap();
        // Amplicon = between fs=24 and re=8 → wraps: length (8 + 31 - 24) = 15.
        assert_eq!(p.amplicon, Span::between(24, 8, T.len()));
        assert!(p.amplicon.wraps(T.len()));
        assert_eq!(
            p.bytes.len(),
            fwd_seq.len() + p.amplicon.len - 6 - 6 + rev_seq.len()
        );
    }
}
