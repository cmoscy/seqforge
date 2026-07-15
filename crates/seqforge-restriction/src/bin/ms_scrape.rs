//! Scrapes REBASE per-enzyme methylation sensitivity into a checked-in TSV
//! snapshot (`data/rebase_methylation.tsv`), the second input to `codegen`.
//!
//! ## Why a scraper (and why `damlist`, not `msget`)
//!
//! Methylation sensitivity is **not** in the bairoch snapshot (its `MS` lines are
//! all on methyltransferase records) nor in any REBASE bulk file — it lives only
//! in per-enzyme CGI records. Of the two, `cgi-bin/msget?<name>` is one row per
//! *experiment* (messy, mixes host methylation with exotic modifications), while
//! `cgi-bin/damlist?e<name>` carries REBASE's own **generated per-enzyme summary**:
//! a `sensitivity?` row with 5 cells in fixed column order Dam / Dcm / CpG / EcoBI
//! / EcoKI. We parse the first three.
//!
//! ## Bounded and reviewable
//!
//! Fetches only the enzymes already in the table (`all_enzymes()`), so the scrape
//! is ~the commercial subset, not all of REBASE. The output TSV is committed and
//! reviewed in the PR diff exactly like `enzymes_generated.rs`. Run on the same
//! quarterly cadence as the bairoch refresh:
//!
//! ```text
//! cargo run -p seqforge-restriction --bin ms_scrape
//! ```
//!
//! Shells out to `curl` (like `codegen --fetch`) to keep the crate zero-dep.
//! A parse miss emits `untested` and logs — never a silent guess.
//!
//! Data attribution: REBASE — http://rebase.neb.com — © Dr. Richard J. Roberts.

use std::fs;
use std::path::Path;
use std::process::Command;
use std::thread;
use std::time::Duration;

use seqforge_restriction::all_enzymes;

const OUT_PATH: &str = "crates/seqforge-restriction/data/rebase_methylation.tsv";
const DAMLIST_URL: &str = "http://rebase.neb.com/cgi-bin/damlist?e";
/// Polite delay between requests — this hits a public REBASE endpoint ~565 times.
const REQUEST_DELAY: Duration = Duration::from_millis(250);

fn main() {
    let out_path = if Path::new("crates/seqforge-restriction/data").is_dir() {
        OUT_PATH
    } else {
        "data/rebase_methylation.tsv"
    };

    let names: Vec<&str> = {
        let mut v: Vec<&str> = all_enzymes().iter().map(|e| e.name).collect();
        v.sort_unstable();
        v.dedup();
        v
    };
    eprintln!(
        "scraping methylation sensitivity for {} enzymes…",
        names.len()
    );

    let mut rows: Vec<String> = Vec::with_capacity(names.len());
    let (mut n_ok, mut n_miss) = (0usize, 0usize);

    for (i, name) in names.iter().enumerate() {
        let html = fetch(&format!("{DAMLIST_URL}{name}"));
        let sens = html.as_deref().and_then(parse_sensitivity);
        match sens {
            Some([dam, dcm, cpg]) => {
                n_ok += 1;
                rows.push(format!("{name}\t{dam}\t{dcm}\t{cpg}"));
            }
            None => {
                n_miss += 1;
                eprintln!(
                    "  [{}/{}] {name}: no sensitivity row — untested",
                    i + 1,
                    names.len()
                );
                rows.push(format!("{name}\tuntested\tuntested\tuntested"));
            }
        }
        thread::sleep(REQUEST_DELAY);
    }

    let mut out = String::new();
    out.push_str(
        "# REBASE methylation sensitivity — Dam / Dcm / CpG effect per enzyme.\n\
         # Source: http://rebase.neb.com/cgi-bin/damlist?e<name> (sensitivity? row).\n\
         # REBASE © Dr. Richard J. Roberts. Used with attribution.\n\
         # Regenerate: cargo run -p seqforge-restriction --bin ms_scrape\n\
         # Columns: name<TAB>dam<TAB>dcm<TAB>cpg — values: cut|impaired|blocked|variable|untested\n",
    );
    for r in &rows {
        out.push_str(r);
        out.push('\n');
    }
    fs::write(out_path, out).expect("write rebase_methylation.tsv");
    eprintln!(
        "wrote {} rows to {out_path}  ({n_ok} sourced, {n_miss} untested)",
        rows.len()
    );
}

/// Fetch a URL to a string via `curl`. Returns `None` on any failure (logged by
/// the caller as a miss) rather than aborting the whole scrape for one bad page.
fn fetch(url: &str) -> Option<String> {
    let out = Command::new("curl")
        .args(["-fsSL", "--max-time", "30", "--retry", "2", url])
        .output()
        .expect("failed to launch curl — is it installed?");
    if !out.status.success() {
        return None;
    }
    String::from_utf8(out.stdout).ok()
}

/// Extract `[dam, dcm, cpg]` from a `damlist` page. The summary block is:
///
/// ```html
/// <b>sensitivity?</b> … <td><font size=1>blocked</font></td> …(5 cells: Dam Dcm CpG EcoBI EcoKI)
/// ```
///
/// Robust to the line-break variation REBASE emits between enzymes: newlines are
/// collapsed, then the first five `<font size=1>…</font>` cells after the
/// `sensitivity?` label are read; we keep the first three.
fn parse_sensitivity(html: &str) -> Option<[String; 3]> {
    let flat: String = html.split_whitespace().collect::<Vec<_>>().join(" ");
    let after = flat.split("sensitivity?").nth(1)?;
    let cells: Vec<String> = FontCells::new(after).take(5).collect();
    if cells.len() < 3 {
        return None;
    }
    Some([
        normalize(&cells[0]),
        normalize(&cells[1]),
        normalize(&cells[2]),
    ])
}

/// Map REBASE's `sensitivity?` vocabulary onto our `MethylEffect` names.
/// `-` (no overlap) and `cut` both mean the enzyme cleaves; the "some"
/// context-dependence is resolved per-site by the evaluator, so `some blocked`
/// collapses to `blocked` and `some impaired` to `impaired` here.
fn normalize(raw: &str) -> String {
    match raw.trim().to_ascii_lowercase().as_str() {
        "cut" | "-" | "" => "cut",
        "blocked" | "some blocked" => "blocked",
        "impaired" | "some impaired" => "impaired",
        "variable" => "variable",
        _ => "untested",
    }
    .to_string()
}

/// Iterator over the text inside successive `<font size=1>…</font>` cells in a
/// whitespace-collapsed HTML fragment.
struct FontCells<'a> {
    rest: &'a str,
}

impl<'a> FontCells<'a> {
    fn new(s: &'a str) -> Self {
        FontCells { rest: s }
    }
}

impl Iterator for FontCells<'_> {
    type Item = String;
    fn next(&mut self) -> Option<String> {
        const OPEN: &str = "<font size=1>";
        let start = self.rest.find(OPEN)? + OPEN.len();
        let tail = &self.rest[start..];
        let end = tail.find("</font>")?;
        let cell = tail[..end].trim().to_string();
        self.rest = &tail[end + "</font>".len()..];
        Some(cell)
    }
}
