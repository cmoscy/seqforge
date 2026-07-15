//! Methylation-aware cut-site evaluation.
//!
//! A restriction site is blocked by host methylation only when **both** hold:
//!
//! 1. **Enzyme sensitivity** — REBASE's per-enzyme verdict, carried on
//!    [`Enzyme::methylation`](crate::Enzyme) (sourced by codegen). This says the
//!    enzyme *can* be blocked/impaired by a given system.
//! 2. **Context present in this site** — the methylatable motif actually overlaps
//!    *this* occurrence of the recognition site, given flanking bases. Computed
//!    here from the sequence.
//!
//! The AND is what makes **BamHI** (`GGATCC`, which contains the Dam motif `GATC`)
//! correctly cuttable — REBASE reports `Dam: cut`, so factor 1 vetoes the block —
//! while **MboI** (recognition *is* `GATC`) is always Dam-blocked, and **ClaI**
//! (`ATCGAT`) is blocked only where a flanking base forms an overlapping `GATC`.
//!
//! The three host motifs are all palindromic (`GATC`, `CCWGG`, `CG`), so a single
//! forward-strand scan captures double-strand methylation — no strand bookkeeping
//! needed.

use crate::enzyme::{MethylEffect, MethylSensitivity};

/// Which host methylation systems are active on the molecule being viewed. This
/// is authored/persisted per-molecule (default Dam+Dcm on = standard *E. coli*
/// plasmid prep); it is the context input to [`site_methyl_state`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MethylContext {
    pub dam: bool,
    pub dcm: bool,
    pub cpg: bool,
}

impl Default for MethylContext {
    /// Standard *E. coli*-derived DNA: Dam⁺ Dcm⁺, no CpG. The protective,
    /// common-case default.
    fn default() -> Self {
        MethylContext {
            dam: true,
            dcm: true,
            cpg: false,
        }
    }
}

impl MethylContext {
    /// No methylation — every site cuts. Equivalent to the feature being off.
    pub const NONE: MethylContext = MethylContext {
        dam: false,
        dcm: false,
        cpg: false,
    };

    fn any(&self) -> bool {
        self.dam || self.dcm || self.cpg
    }
}

/// Verdict for one cut site under a methylation context. Ordered
/// `Cuttable < Impaired < Blocked`; the worst across active systems wins.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SiteMethylState {
    Cuttable,
    Impaired,
    Blocked,
}

/// Two-factor verdict for a recognition site at `[rec_start, rec_end)` under a
/// methylation context.
///
/// Takes just the recognition span (not a full `Site`) — cut geometry and strand
/// are irrelevant to methylation, and the three host motifs are palindromic so a
/// single forward scan suffices. `seq` is the full template (flanking bases
/// matter — e.g. ClaI's `GATC` overlap forms only with a specific neighbour).
pub fn site_methyl_state(
    rec_start: usize,
    rec_end: usize,
    methylation: &MethylSensitivity,
    seq: &[u8],
    ctx: &MethylContext,
) -> SiteMethylState {
    if !ctx.any() {
        return SiteMethylState::Cuttable;
    }
    // (enabled, enzyme sensitivity to this system, methylatable motif)
    let systems = [
        (ctx.dam, methylation.dam, DAM),
        (ctx.dcm, methylation.dcm, DCM),
        (ctx.cpg, methylation.cpg, CPG),
    ];

    let mut worst = SiteMethylState::Cuttable;
    for (enabled, effect, motif) in systems {
        if !enabled {
            continue;
        }
        // Factor 1: does the enzyme respond to this methylation at all?
        let candidate = match effect {
            MethylEffect::Blocked => SiteMethylState::Blocked,
            // `Variable` = conflicting reports: surface as a caution, not a hard block.
            MethylEffect::Impaired | MethylEffect::Variable => SiteMethylState::Impaired,
            MethylEffect::Cut | MethylEffect::Untested => continue,
        };
        // Factor 2: is the methylatable base actually inside this site?
        if motif_overlaps_site(seq, rec_start, rec_end, motif) {
            worst = worst.max(candidate);
        }
    }
    worst
}

/// Dam methylates the A in `GATC`; Dcm the inner C in `CCWGG` (W = A/T); CpG the C
/// in `CG`. Bytes are uppercase ASCII with `W` as the sole ambiguity code.
const DAM: &[u8] = b"GATC";
const DCM: &[u8] = b"CCWGG";
const CPG: &[u8] = b"CG";

/// True iff `motif` occurs in `seq` at a position whose span intersects
/// `[rec_start, rec_end)`. Scans a window widened by `motif.len() - 1` on each
/// side so a motif straddling the site edge (the ClaI/Dam case) is caught.
fn motif_overlaps_site(seq: &[u8], rec_start: usize, rec_end: usize, motif: &[u8]) -> bool {
    let m = motif.len();
    if m == 0 || seq.len() < m {
        return false;
    }
    let lo = rec_start.saturating_sub(m - 1);
    let hi = (rec_end + m - 1).min(seq.len()); // exclusive upper bound for the last start
    for p in lo..hi.saturating_sub(m - 1) {
        // overlap of [p, p+m) with [rec_start, rec_end)
        if p < rec_end && p + m > rec_start && motif_matches_at(seq, p, motif) {
            return true;
        }
    }
    false
}

fn motif_matches_at(seq: &[u8], p: usize, motif: &[u8]) -> bool {
    motif.iter().enumerate().all(|(i, &mb)| {
        let b = seq[p + i].to_ascii_uppercase();
        match mb {
            b'W' => b == b'A' || b == b'T',
            other => b == other,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sens(dam: MethylEffect, dcm: MethylEffect, cpg: MethylEffect) -> MethylSensitivity {
        MethylSensitivity { dam, dcm, cpg }
    }

    const DAM_ON: MethylContext = MethylContext {
        dam: true,
        dcm: false,
        cpg: false,
    };

    #[test]
    fn no_context_never_blocks() {
        // Even a Dam-blocked enzyme sitting on a GATC cuts when no system is on.
        let seq = b"AAAGATCAAA";
        let m = sens(MethylEffect::Blocked, MethylEffect::Cut, MethylEffect::Cut);
        assert_eq!(
            site_methyl_state(3, 7, &m, seq, &MethylContext::NONE),
            SiteMethylState::Cuttable
        );
    }

    #[test]
    fn mboi_gatc_always_blocked() {
        // MboI recognition IS the Dam motif → always overlaps → blocked.
        let seq = b"TTTGATCTTT"; // GATC at [3, 7)
        let m = sens(MethylEffect::Blocked, MethylEffect::Cut, MethylEffect::Cut);
        assert_eq!(
            site_methyl_state(3, 7, &m, seq, &DAM_ON),
            SiteMethylState::Blocked
        );
    }

    #[test]
    fn bamhi_contains_gatc_but_cuts() {
        // GGATCC contains GATC, but REBASE says Dam: cut → factor 1 vetoes.
        let seq = b"TTGGATCCTT"; // GGATCC at [2, 8)
        let m = sens(MethylEffect::Cut, MethylEffect::Cut, MethylEffect::Cut);
        assert_eq!(
            site_methyl_state(2, 8, &m, seq, &DAM_ON),
            SiteMethylState::Cuttable
        );
    }

    #[test]
    fn clai_dam_context_dependent() {
        // ClaI = ATCGAT (no internal GATC). Blocked only when a flank forms GATC.
        let m = sens(MethylEffect::Blocked, MethylEffect::Cut, MethylEffect::Cut);

        // Followed by C: ...ATCGAT C... → GATC straddles the 3' edge → blocked.
        assert_eq!(
            site_methyl_state(2, 8, &m, b"TTATCGATCTT", &DAM_ON),
            SiteMethylState::Blocked
        );

        // Neutral flanks (A both sides): no overlapping GATC → cuttable.
        assert_eq!(
            site_methyl_state(2, 8, &m, b"TTATCGATATT", &DAM_ON),
            SiteMethylState::Cuttable
        );

        // Preceded by G: ...G ATCGAT... → GATC straddles the 5' edge → blocked.
        assert_eq!(
            site_methyl_state(3, 9, &m, b"TTGATCGATTT", &DAM_ON), // ATCGAT at [3, 9)
            SiteMethylState::Blocked
        );
    }

    #[test]
    fn dcm_ccwgg_ambiguity() {
        // Dcm motif CCWGG (W = A/T). CCAGG matches; CCGGG (W=G) does not.
        let m = sens(MethylEffect::Cut, MethylEffect::Blocked, MethylEffect::Cut);
        let dcm_on = MethylContext {
            dam: false,
            dcm: true,
            cpg: false,
        };
        assert_eq!(
            site_methyl_state(2, 7, &m, b"TTCCAGGTT", &dcm_on),
            SiteMethylState::Blocked
        );
        assert_eq!(
            site_methyl_state(2, 7, &m, b"TTCCGGGTT", &dcm_on),
            SiteMethylState::Cuttable
        );
    }

    #[test]
    fn cpg_blocked_when_cg_in_site() {
        // SmaI-like CCCGGG contains CG → CpG-blocked when CpG on.
        let m = sens(MethylEffect::Cut, MethylEffect::Cut, MethylEffect::Blocked);
        let cpg_on = MethylContext {
            dam: false,
            dcm: false,
            cpg: true,
        };
        assert_eq!(
            site_methyl_state(2, 8, &m, b"TTCCCGGGTT", &cpg_on),
            SiteMethylState::Blocked
        );
    }

    #[test]
    fn variable_surfaces_as_impaired() {
        let m = sens(MethylEffect::Variable, MethylEffect::Cut, MethylEffect::Cut);
        assert_eq!(
            site_methyl_state(3, 7, &m, b"TTTGATCTTT", &DAM_ON),
            SiteMethylState::Impaired
        );
    }

    #[test]
    fn worst_system_wins() {
        // Dam impaired + CpG blocked (both contexts present) → Blocked overall.
        let m = sens(
            MethylEffect::Impaired,
            MethylEffect::Cut,
            MethylEffect::Blocked,
        );
        let all = MethylContext {
            dam: true,
            dcm: false,
            cpg: true,
        };
        // GATCG at [3, 8) contains GATC (Dam) and CG (CpG).
        assert_eq!(
            site_methyl_state(3, 8, &m, b"TTTGATCGTTT", &all),
            SiteMethylState::Blocked
        );
    }
}
