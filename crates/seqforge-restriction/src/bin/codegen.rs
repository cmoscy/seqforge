//! REBASE bairoch-format parser → static enzyme table generator.
//!
//! Reads the REBASE bairoch snapshot, applies SeqForge's filter (Type II /
//! Type IIs, commercially available, known cut position), and writes
//! `src/enzymes_generated.rs` containing a `const ENZYMES: &[Enzyme]` table.
//!
//! ## This is a manual, intentional step — never automated
//!
//! The generated file (`enzymes_generated.rs`, ~98 KB) is **committed** and is
//! the only thing the normal build or runtime ever reads. The raw snapshot
//! (`data/rebase_bairoch.txt`, ~4.5 MB) is **git-ignored** and only consumed
//! here. Codegen is run by hand, on purpose, in two situations:
//!
//!   * a maintainer bumps the REBASE version, or
//!   * a user wants a larger / more inclusive table (relax the filter in
//!     `filter_and_classify` below).
//!
//! It is deliberately NOT a `build.rs` and NOT part of CI — regular builds
//! stay fast and offline, and the committed table is reviewable in PRs.
//!
//! ## Usage
//!
//! ```text
//! # Regenerate from an already-present snapshot:
//! cargo run -p seqforge-restriction --bin codegen
//!
//! # Fetch the latest REBASE bairoch snapshot first, then regenerate:
//! cargo run -p seqforge-restriction --bin codegen -- --fetch
//!
//! # Fetch from a specific URL (e.g. a pinned version) then regenerate:
//! cargo run -p seqforge-restriction --bin codegen -- --fetch <url>
//! ```

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::process::Command;

/// Codegen-local mirror of `enzyme::MethylEffect`. Kept separate from the
/// library type on purpose: codegen must build *before* the generated table it
/// writes, so it cannot depend on the library crate compiling. The emitted
/// literals still reference the real `crate::enzyme::MethylEffect` by name.
#[derive(Debug, Clone, Copy)]
enum MethEffect {
    Cut,
    Impaired,
    Blocked,
    Variable,
    Untested,
}

#[derive(Debug, Clone, Copy)]
struct Meth {
    dam: MethEffect,
    dcm: MethEffect,
    cpg: MethEffect,
}

const BAIROCH_PATH: &str = "crates/seqforge-restriction/data/rebase_bairoch.txt";
const METH_PATH: &str = "crates/seqforge-restriction/data/rebase_methylation.tsv";
const OUT_PATH: &str = "crates/seqforge-restriction/src/enzymes_generated.rs";

/// REBASE's "always latest" bairoch-format file. REBASE publishes `link_*`
/// files that track the current version; override via `--fetch <url>` to pin a
/// specific release. Snapshot license/attribution: REBASE © Dr. R. J. Roberts.
const DEFAULT_BAIROCH_URL: &str = "http://rebase.neb.com/rebase/link_bairoch";

#[derive(Debug)]
struct Entry {
    name: String,
    recognition: Vec<u8>, // raw bairoch recognition string for first descriptor
    top_offset: i16,
    bottom_offset: i16,
    is_type_iis: bool,
    /// Methylation sensitivity, joined from `data/rebase_methylation.tsv` by
    /// `join_methylation`. Starts `Untested`; set during the join.
    methylation: Meth,
}

const UNTESTED: Meth = Meth {
    dam: MethEffect::Untested,
    dcm: MethEffect::Untested,
    cpg: MethEffect::Untested,
};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // Resolve paths once: run either from the workspace root or from inside
    // the crate directory. The snapshot may not exist yet (first --fetch), so
    // we key off the data *directory*, not the file.
    let from_root = Path::new("crates/seqforge-restriction/data").is_dir();
    let (data_path, meth_path, out_path) = if from_root {
        (BAIROCH_PATH, METH_PATH, OUT_PATH)
    } else {
        (
            "data/rebase_bairoch.txt",
            "data/rebase_methylation.tsv",
            "src/enzymes_generated.rs",
        )
    };

    // --fetch [url]: manual, intentional refresh of the REBASE snapshot.
    if let Some(i) = args.iter().position(|a| a == "--fetch") {
        let url = args
            .get(i + 1)
            .filter(|a| !a.starts_with("--"))
            .map(String::as_str)
            .unwrap_or(DEFAULT_BAIROCH_URL);
        fetch_snapshot(url, data_path);
    }

    let body = fs::read_to_string(data_path).unwrap_or_else(|_| {
        panic!(
            "REBASE snapshot not found at {data_path}.\n\
             It is git-ignored; download it first:\n  \
             cargo run -p seqforge-restriction --bin codegen -- --fetch"
        )
    });
    let entries = parse_all(&body);
    let mut kept = filter_and_classify(entries);

    // Join methylation sensitivity (Phase 3). Missing file is non-fatal — the
    // table then ships fully `Untested` (as Phase 1 did), and the coverage-gate
    // test flags it. This keeps a fresh checkout buildable before the scrape.
    let meth_body = fs::read_to_string(meth_path).unwrap_or_default();
    if meth_body.is_empty() {
        eprintln!("note: {meth_path} not found — emitting all enzymes as Untested.\n  populate it with: cargo run -p seqforge-restriction --bin ms_scrape");
    }
    let n_sourced = join_methylation(&mut kept, &meth_body);

    let out_text = emit_generated(&kept);
    fs::write(out_path, out_text).expect("write enzymes_generated.rs");
    eprintln!("wrote {} enzymes to {}", kept.len(), out_path);
    let n_iis = kept.iter().filter(|e| e.is_type_iis).count();
    eprintln!("  Type II: {}", kept.len() - n_iis);
    eprintln!("  Type IIs: {}", n_iis);
    eprintln!(
        "  methylation: {} sourced, {} untested",
        n_sourced,
        kept.len() - n_sourced
    );
}

/// Join methylation verdicts from a `rebase_methylation.tsv` body onto the kept
/// enzymes. Primary key is enzyme name; enzymes with no definite verdict then
/// inherit from a same-recognition enzyme that has one (isoschizomers share the
/// methylated recognition context). Returns the count that ended up with at
/// least one sourced (non-`Untested`) system.
///
/// Recognition-level conflicts (two enzymes, same recognition, different
/// definite verdicts) are logged, not fatal: direct-by-name sourcing is
/// authoritative per enzyme, so a conflict is a data note for the maintainer,
/// and the fallback only ever fills a fully-`Untested` enzyme.
fn join_methylation(kept: &mut [Entry], tsv: &str) -> usize {
    let by_name = parse_methylation_tsv(tsv);

    // Build a recognition → verdict index from enzymes that have a definite
    // verdict, flagging conflicts.
    let mut by_recognition: BTreeMap<Vec<u8>, Meth> = BTreeMap::new();
    for e in kept.iter() {
        let Some(m) = by_name.get(&e.name).copied() else {
            continue;
        };
        if is_fully_untested(&m) {
            continue;
        }
        match by_recognition.get(&e.recognition) {
            Some(prev) if !meth_eq(prev, &m) => {
                let rec: String = e.recognition.iter().map(|b| *b as char).collect();
                eprintln!(
                    "  methylation conflict for recognition {rec}: {} differs from an earlier isoschizomer — keeping first",
                    e.name
                );
            }
            None => {
                by_recognition.insert(e.recognition.clone(), m);
            }
            _ => {}
        }
    }

    let mut n_sourced = 0;
    for e in kept.iter_mut() {
        let direct = by_name.get(&e.name).copied().unwrap_or(UNTESTED);
        let m = if is_fully_untested(&direct) {
            // Fall back to a same-recognition isoschizomer's verdict, if any.
            by_recognition
                .get(&e.recognition)
                .copied()
                .unwrap_or(UNTESTED)
        } else {
            direct
        };
        e.methylation = m;
        if !is_fully_untested(&m) {
            n_sourced += 1;
        }
    }
    n_sourced
}

/// Parse `rebase_methylation.tsv` (`name<TAB>dam<TAB>dcm<TAB>cpg`, `#` comments)
/// into a name → verdict map.
fn parse_methylation_tsv(body: &str) -> BTreeMap<String, Meth> {
    let mut map = BTreeMap::new();
    for line in body.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let cols: Vec<&str> = line.split('\t').collect();
        if cols.len() < 4 {
            continue;
        }
        map.insert(
            cols[0].to_string(),
            Meth {
                dam: meth_from_str(cols[1]),
                dcm: meth_from_str(cols[2]),
                cpg: meth_from_str(cols[3]),
            },
        );
    }
    map
}

fn meth_from_str(s: &str) -> MethEffect {
    match s.trim().to_ascii_lowercase().as_str() {
        "cut" => MethEffect::Cut,
        "impaired" => MethEffect::Impaired,
        "blocked" => MethEffect::Blocked,
        "variable" => MethEffect::Variable,
        _ => MethEffect::Untested,
    }
}

fn is_fully_untested(m: &Meth) -> bool {
    matches!(m.dam, MethEffect::Untested)
        && matches!(m.dcm, MethEffect::Untested)
        && matches!(m.cpg, MethEffect::Untested)
}

fn meth_eq(a: &Meth, b: &Meth) -> bool {
    fn code(e: MethEffect) -> u8 {
        match e {
            MethEffect::Cut => 0,
            MethEffect::Impaired => 1,
            MethEffect::Blocked => 2,
            MethEffect::Variable => 3,
            MethEffect::Untested => 4,
        }
    }
    code(a.dam) == code(b.dam) && code(a.dcm) == code(b.dcm) && code(a.cpg) == code(b.cpg)
}

/// Download the REBASE bairoch snapshot to `dest` via `curl`. Shells out
/// rather than pulling in an HTTP crate — keeps the crate's zero-dependency
/// policy intact (the snapshot fetch is a manual maintainer step, not part of
/// any build). Aborts on failure so a partial/empty file never reaches codegen.
fn fetch_snapshot(url: &str, dest: &str) {
    if let Some(parent) = Path::new(dest).parent() {
        fs::create_dir_all(parent).expect("create data dir");
    }
    eprintln!("fetching REBASE snapshot:\n  {url}\n  -> {dest}");
    let status = Command::new("curl")
        .args(["-fSL", "--retry", "2", "-o", dest, url])
        .status()
        .expect("failed to launch curl — is it installed?");
    if !status.success() {
        panic!(
            "curl failed ({status}); snapshot not updated. Check the URL or pass --fetch <url>."
        );
    }
}

// ── Parsing ───────────────────────────────────────────────────────────────────

/// Walk the bairoch text; accumulate raw fields per `//`-terminated record.
/// We keep only the fields we actually need (ID, ET, RS, CR). Everything
/// else — references, methylation lines, accession numbers — is discarded.
fn parse_all(body: &str) -> Vec<RawRecord> {
    let mut out = Vec::new();
    let mut cur = RawRecord::default();
    for line in body.lines() {
        if line.starts_with("//") {
            if !cur.id.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            cur = RawRecord::default();
            continue;
        }
        // CC lines are licence / header comments outside records.
        if line.starts_with("CC") {
            continue;
        }
        let (tag, rest) = match line.split_once("   ") {
            Some(x) => x,
            None => continue,
        };
        let rest = rest.trim();
        match tag {
            "ID" => cur.id = rest.to_string(),
            "ET" => cur.et = rest.to_string(),
            "RS" => cur.rs = rest.to_string(),
            "CR" => cur.cr = rest.to_string(),
            _ => {}
        }
    }
    out
}

#[derive(Default, Debug)]
struct RawRecord {
    id: String,
    et: String,
    rs: String,
    cr: String,
}

// ── Filter + classify ─────────────────────────────────────────────────────────

fn filter_and_classify(raw: Vec<RawRecord>) -> Vec<Entry> {
    let mut by_name: BTreeMap<String, Entry> = BTreeMap::new();

    for r in raw {
        // ET must be R2 (Type II). Skip methyltransferases (M*), homing
        // endonucleases (I-prefix → ET starts with anything that isn't R2),
        // Type I/III restriction enzymes (R1, R3), control proteins,
        // specificity subunits, etc.
        if r.et != "R2" {
            continue;
        }
        // Skip ID prefixes that indicate non-restriction proteins even
        // within R2 (rare but defensive).
        let id = r.id.trim();
        if id.starts_with("M.")
            || id.starts_with("M1.")
            || id.starts_with("M2.")
            || id.starts_with("S.")
            || id.starts_with("C.")
            || id.starts_with("I-")
        {
            continue;
        }
        // Must have at least one commercial source letter on the CR line.
        // Bairoch encodes "no commercial source" as `.` alone.
        let cr_clean = r.cr.trim().trim_end_matches('.');
        if cr_clean.is_empty() {
            continue;
        }
        // Parse the RS line. Skip entries without a known cut position.
        let Some((rec_bytes, top_off, bot_off, is_iis)) = parse_rs(&r.rs) else {
            continue;
        };

        // Dedup: prefer the first occurrence (REBASE often lists multiple
        // entries for the same canonical name — the first is typically the
        // prototype).
        by_name.entry(id.to_string()).or_insert(Entry {
            name: id.to_string(),
            recognition: rec_bytes,
            top_offset: top_off,
            bottom_offset: bot_off,
            is_type_iis: is_iis,
            methylation: UNTESTED,
        });
    }

    by_name.into_values().collect()
}

/// Parse an RS line body. Returns `(recognition_bytes, top_offset,
/// bottom_offset, is_type_iis)` or `None` if the entry should be skipped.
///
/// Bairoch RS examples:
///   `GAATTC, 1;`                         (Type II palindrome — EcoRI)
///   `GGTCTC, 7; GAGACC, -5;`             (Type IIs — BsaI)
///   `GACNNNNNNGTC, 7;`                   (Type II with IUPAC N gap — DrdI)
///   `?, ?;`                              (unknown — skipped)
///   `A, ?;`                              (unknown cut — skipped)
fn parse_rs(rs: &str) -> Option<(Vec<u8>, i16, i16, bool)> {
    // Split on `;`, then within each part split on `,`.
    let parts: Vec<&str> = rs
        .split(';')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    if parts.is_empty() {
        return None;
    }

    let mut descriptors: Vec<(Vec<u8>, i16)> = Vec::new();
    for part in &parts {
        let (rec, pos) = part.split_once(',')?;
        let rec = rec.trim();
        let pos = pos.trim();
        if rec.is_empty() || pos == "?" || rec == "?" {
            return None;
        }
        // Validate recognition: must be IUPAC letters only.
        let rec_bytes: Vec<u8> = rec.bytes().map(|b| b.to_ascii_uppercase()).collect();
        if !rec_bytes.iter().all(|&b| {
            matches!(
                b,
                b'A' | b'C'
                    | b'G'
                    | b'T'
                    | b'R'
                    | b'Y'
                    | b'S'
                    | b'W'
                    | b'K'
                    | b'M'
                    | b'B'
                    | b'D'
                    | b'H'
                    | b'V'
                    | b'N'
            )
        }) {
            return None;
        }
        let cut_pos: i16 = pos.parse().ok()?;
        descriptors.push((rec_bytes, cut_pos));
    }

    match descriptors.len() {
        1 => {
            // Palindromic Type II (or close enough): single cut descriptor.
            // Bottom cut = recognition_len - top_cut.
            let (rec, top) = descriptors.into_iter().next().unwrap();
            let rec_len = rec.len() as i16;
            let bot = rec_len - top;
            Some((rec, top, bot, false))
        }
        2 => {
            // Type IIs: two descriptors, one per strand orientation. The
            // second descriptor's recognition is the reverse complement of
            // the first; we record only the forward one and convert the
            // bottom cut to forward-strand coords:
            //   bottom_offset = recognition_len - second_cut_pos
            // (See enzyme.rs docs for the derivation.)
            let (rec, top) = descriptors[0].clone();
            let second_cut = descriptors[1].1;
            let rec_len = rec.len() as i16;
            let bot = rec_len - second_cut;
            Some((rec, top, bot, true))
        }
        _ => None,
    }
}

// ── Emit ──────────────────────────────────────────────────────────────────────

fn emit_generated(entries: &[Entry]) -> String {
    let mut s = String::new();
    s.push_str(
        "// ╔══════════════════════════════════════════════════════════════════╗\n\
         // ║ AUTO-GENERATED from data/rebase_bairoch.txt. Do not edit.        ║\n\
         // ║ Regenerate with:                                                 ║\n\
         // ║   cargo run -p seqforge-restriction --bin codegen                ║\n\
         // ║                                                                   ║\n\
         // ║ Source: REBASE — The Restriction Enzyme Database                 ║\n\
         // ║   http://rebase.neb.com                                          ║\n\
         // ║   Copyright (c) Dr. Richard J. Roberts. Used with attribution.   ║\n\
         // ╚══════════════════════════════════════════════════════════════════╝\n\n",
    );
    s.push_str(
        "use crate::enzyme::{Enzyme, EnzymeType, Iupac, MethylEffect, MethylSensitivity};\n\n",
    );
    // `rustfmt::skip` keeps the compact one-line-per-enzyme layout: without it
    // rustfmt explodes each entry across ~9 lines and `fmt --check` would fail
    // after any regen.
    s.push_str("#[rustfmt::skip]\npub const ENZYMES: &[Enzyme] = &[\n");
    for e in entries {
        let rec = e
            .recognition
            .iter()
            .map(|b| format!("Iupac::{}", *b as char))
            .collect::<Vec<_>>()
            .join(", ");
        let kind = if e.is_type_iis { "TypeIIs" } else { "TypeII" };
        // `e.methylation` is the per-system verdict joined from
        // `data/rebase_methylation.tsv` by `join_methylation` (Untested if unsourced).
        let meth = format_methylation(&e.methylation);
        s.push_str(&format!(
            "    Enzyme {{ name: {:?}, recognition: &[{rec}], top_offset: {}, bottom_offset: {}, enzyme_type: EnzymeType::{}, methylation: {meth} }},\n",
            e.name, e.top_offset, e.bottom_offset, kind,
        ));
    }
    s.push_str("];\n");
    s
}

/// Render a `MethylSensitivity` as the Rust literal emitted into the generated
/// table. Kept as a single source of truth so the Phase 3 join only has to fill
/// `Entry.methylation` — the emit shape doesn't change.
fn format_methylation(m: &Meth) -> String {
    let v = |e: MethEffect| match e {
        MethEffect::Cut => "Cut",
        MethEffect::Impaired => "Impaired",
        MethEffect::Blocked => "Blocked",
        MethEffect::Variable => "Variable",
        MethEffect::Untested => "Untested",
    };
    format!(
        "MethylSensitivity {{ dam: MethylEffect::{}, dcm: MethylEffect::{}, cpg: MethylEffect::{} }}",
        v(m.dam),
        v(m.dcm),
        v(m.cpg),
    )
}
