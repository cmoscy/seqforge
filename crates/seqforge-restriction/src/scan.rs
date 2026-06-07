//! Restriction site scanner.
//!
//! Naive O(n·m·k) scan: for each enzyme, walk the sequence and test the
//! IUPAC pattern at each position, on both strands. For plasmid-scale work
//! (<100 kb) this is microseconds even with the full ~300-enzyme table.
//! Aho-Corasick is a future optimisation if it ever matters; the existing
//! scan is rendering-bound, not search-bound.

use crate::enzyme::{Enzyme, EnzymeType, Iupac, Site, SiteStrand};

/// Find sites for a single enzyme in `seq`.
///
/// For circular sequences, pass `circular = true` to detect sites spanning
/// the origin. Returned positions are normalized to `0..seq.len()`; a
/// wrap-around site has `recognition_start` near the end of the sequence and
/// `recognition_end > seq.len()` is wrapped via modular arithmetic by
/// `top_cut` / `bottom_cut`.
pub fn find_sites(seq: &[u8], enzyme: &'static Enzyme, circular: bool) -> Vec<Site> {
    let mut out = Vec::new();
    scan_one_enzyme(seq, enzyme, circular, &mut out);
    out
}

/// Find sites for many enzymes in `seq`. Results are flattened across all
/// enzymes; callers that need per-enzyme grouping can sort/group by the
/// `enzyme` field.
pub fn find_all_sites(seq: &[u8], enzymes: &[&'static Enzyme], circular: bool) -> Vec<Site> {
    let mut out = Vec::new();
    for enzyme in enzymes {
        scan_one_enzyme(seq, enzyme, circular, &mut out);
    }
    out
}

fn scan_one_enzyme(seq: &[u8], enzyme: &'static Enzyme, circular: bool, out: &mut Vec<Site>) {
    let rec_len = enzyme.recognition.len();
    let seq_len = seq.len();
    if rec_len == 0 || seq_len < rec_len {
        return;
    }

    // For circular sequences, extend by `rec_len - 1` bases so origin-
    // spanning sites are caught in one pass. Positions modulo `seq_len`
    // map back to the canonical range.
    let extended: Vec<u8>;
    let search: &[u8] = if circular && rec_len > 1 {
        extended = seq
            .iter()
            .chain(seq[..rec_len - 1].iter())
            .copied()
            .collect();
        &extended
    } else {
        seq
    };
    let end = search.len().saturating_sub(rec_len) + 1;

    // Reverse complement of the recognition (used for Type IIs reverse
    // strand and any non-palindromic case). For palindromic Type II
    // enzymes the RC matches the same positions as the forward; we suppress
    // the duplicate emission below.
    let rc_pat: Vec<Iupac> = enzyme
        .recognition
        .iter()
        .rev()
        .map(|i| i.complement())
        .collect();
    let is_palindromic = rc_pat
        .iter()
        .zip(enzyme.recognition.iter())
        .all(|(a, b)| a == b);

    for i in 0..end {
        let window = &search[i..i + rec_len];

        if iupac_match(enzyme.recognition, window) {
            let rec_start = i % seq_len;
            push_site(
                out,
                enzyme,
                rec_start,
                SiteStrand::Forward,
                seq_len,
                circular,
            );
        }
        if !is_palindromic && iupac_match(&rc_pat, window) {
            let rec_start = i % seq_len;
            push_site(
                out,
                enzyme,
                rec_start,
                SiteStrand::Reverse,
                seq_len,
                circular,
            );
        }
    }
}

#[inline]
fn iupac_match(pattern: &[Iupac], window: &[u8]) -> bool {
    pattern
        .iter()
        .zip(window.iter())
        .all(|(p, &b)| p.matches(b))
}

fn push_site(
    out: &mut Vec<Site>,
    enzyme: &'static Enzyme,
    rec_start: usize,
    strand: SiteStrand,
    seq_len: usize,
    circular: bool,
) {
    let rec_len = enzyme.recognition.len();
    // Cut positions depend on strand orientation. On the forward strand,
    // top_offset / bottom_offset are added to rec_start directly. On the
    // reverse strand the enzyme is matching the RC of its recognition, so
    // the top of the enzyme corresponds to the bottom of our sequence:
    // mirror the offsets about the recognition midpoint.
    let (top_o, bot_o) = match strand {
        SiteStrand::Forward => (enzyme.top_offset, enzyme.bottom_offset),
        SiteStrand::Reverse => {
            let mirror = |o: i16| (rec_len as i16) - o;
            (mirror(enzyme.bottom_offset), mirror(enzyme.top_offset))
        }
    };

    let abs_top = signed_add(rec_start, top_o);
    let abs_bot = signed_add(rec_start, bot_o);
    let (top_cut, bottom_cut) = if circular {
        (
            abs_top.rem_euclid(seq_len as isize) as usize,
            abs_bot.rem_euclid(seq_len as isize) as usize,
        )
    } else {
        // For linear sequences, drop sites whose cuts fall outside [0,
        // seq_len]. Type IIs enzymes whose recognition is near the end can
        // produce cuts past the sequence — those are biologically irrelevant
        // because the enzyme can't cleave what isn't there.
        if abs_top < 0 || abs_bot < 0 || abs_top > seq_len as isize || abs_bot > seq_len as isize {
            return;
        }
        (abs_top as usize, abs_bot as usize)
    };

    out.push(Site {
        enzyme: enzyme.name,
        recognition_start: rec_start,
        recognition_end: rec_start + rec_len,
        top_cut,
        bottom_cut,
        strand,
    });
}

#[inline]
fn signed_add(base: usize, delta: i16) -> isize {
    base as isize + delta as isize
}

/// Count sites per enzyme over `seq`. Useful for the `Unique` / `UniqueOrDual`
/// / `NonCutters` presets without materialising every site.
pub fn count_sites_per_enzyme(
    seq: &[u8],
    enzymes: &[&'static Enzyme],
    circular: bool,
) -> Vec<(&'static Enzyme, usize)> {
    enzymes
        .iter()
        .map(|e| {
            let n = find_sites(seq, e, circular).len();
            (*e, n)
        })
        .collect()
}

// Silence "unused" warning if `EnzymeType` becomes useful only at preset
// resolution; keep the import here so the module is the single place that
// reasons about the type.
#[allow(dead_code)]
fn _enzyme_type_is_used(_: EnzymeType) {}
