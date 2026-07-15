//! Table-backed methylation tests: the parity/coverage gate + canonical
//! per-enzyme sensitivity checks against the joined `enzymes_generated.rs`.
//!
//! The pure two-factor evaluator logic is unit-tested in `src/methylation.rs`;
//! here we assert the *data* that codegen joined from `rebase_methylation.tsv`.

use seqforge_restriction::{
    all_enzymes, enzyme_by_name, find_sites, site_methyl_state, MethylContext, MethylEffect,
    SiteMethylState,
};

/// The reviewed allowlist of enzymes permitted to carry no sourced sensitivity.
const ALLOW: &str = include_str!("fixtures/ms_untested_allow.txt");

fn allowlist() -> Vec<&'static str> {
    ALLOW
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .collect()
}

fn fully_untested(e: &seqforge_restriction::Enzyme) -> bool {
    matches!(e.methylation.dam, MethylEffect::Untested)
        && matches!(e.methylation.dcm, MethylEffect::Untested)
        && matches!(e.methylation.cpg, MethylEffect::Untested)
}

/// PARITY / COVERAGE GATE. Every enzyme in the table must carry sourced
/// methylation sensitivity (at least one non-`Untested` system), unless it is on
/// the reviewed allowlist. A snapshot refresh that adds an unsourced enzyme trips
/// this — the "don't silently miss one" guarantee, enforced under `cargo test`.
#[test]
fn every_enzyme_has_sourced_methylation() {
    let allow = allowlist();
    let missing: Vec<&str> = all_enzymes()
        .iter()
        .filter(|e| fully_untested(e))
        .map(|e| e.name)
        .filter(|n| !allow.contains(n))
        .collect();
    assert!(
        missing.is_empty(),
        "{} enzyme(s) have no sourced methylation sensitivity and are not \
         allowlisted (re-run `ms_scrape`, or add to tests/fixtures/ms_untested_allow.txt): {:?}",
        missing.len(),
        missing
    );
}

// ── Canonical per-enzyme verdicts (from REBASE damlist) ─────────────────────────

#[test]
fn clai_blocked_by_dam_and_cpg() {
    let e = enzyme_by_name("ClaI").expect("ClaI in table");
    assert_eq!(e.methylation.dam, MethylEffect::Blocked);
    assert_eq!(e.methylation.cpg, MethylEffect::Blocked);
}

#[test]
fn mboi_blocked_by_dam() {
    // MboI recognition IS the Dam site (GATC).
    let e = enzyme_by_name("MboI").expect("MboI in table");
    assert_eq!(e.methylation.dam, MethylEffect::Blocked);
}

#[test]
fn bamhi_not_dam_blocked_despite_containing_gatc() {
    // GGATCC contains GATC, but REBASE reports Dam: cut — the whole point.
    let e = enzyme_by_name("BamHI").expect("BamHI in table");
    assert_eq!(e.methylation.dam, MethylEffect::Cut);
}

// ── Integration: table verdict × real found site under a context ─────────────────

#[test]
fn bamhi_site_cuts_under_dam_even_with_internal_gatc() {
    let bamhi = enzyme_by_name("BamHI").unwrap();
    let seq = b"AAAAGGATCCAAAA";
    let sites = find_sites(seq, bamhi, false);
    assert_eq!(sites.len(), 1);
    // Dam on: factor 1 (Cut) vetoes the internal-GATC block → cuttable.
    let state = site_methyl_state(
        sites[0].recognition_start,
        sites[0].recognition_end,
        &bamhi.methylation,
        seq,
        &MethylContext::default(),
    );
    assert_eq!(state, SiteMethylState::Cuttable);
}

#[test]
fn mboi_site_blocked_under_dam() {
    let mboi = enzyme_by_name("MboI").unwrap();
    let seq = b"AAAAGATCAAAA";
    let sites = find_sites(seq, mboi, false);
    assert!(!sites.is_empty());
    let dam_on = MethylContext {
        dam: true,
        dcm: false,
        cpg: false,
    };
    let (rs, re) = (sites[0].recognition_start, sites[0].recognition_end);
    let state = site_methyl_state(rs, re, &mboi.methylation, seq, &dam_on);
    assert_eq!(state, SiteMethylState::Blocked);
    // …but cuts when nothing is methylated.
    let off = site_methyl_state(rs, re, &mboi.methylation, seq, &MethylContext::NONE);
    assert_eq!(off, SiteMethylState::Cuttable);
}
