# seqforge-restriction

REBASE-derived restriction enzyme database + scanner for SeqForge. Concrete
API, no traits, no `Box<dyn …>`, **no non-std dependencies**. Unpublished
workspace crate; stabilises through real SeqForge use before any crates.io
extraction (see `../../plans/restriction.md`).

## What ships vs. what doesn't

| Path | Tracked? | Role |
|------|----------|------|
| `src/enzymes_generated.rs` (~160 KB, 565 enzymes) | ✅ committed | The enzyme table the build and runtime actually use (recognition + methylation) |
| `data/rebase_bairoch.txt` (~4.5 MB) | ❌ git-ignored | Raw REBASE snapshot — input to codegen only |
| `data/rebase_methylation.tsv` (~12 KB) | ❌ git-ignored | Per-enzyme methylation sensitivity scraped from REBASE `damlist` — input to codegen only |
| `tests/fixtures/ms_untested_allow.txt` | ✅ committed | Reviewed allowlist for the coverage-gate test (enzymes permitted to have no sourced sensitivity) |

The normal `cargo build` and all runtime lookups read **only** the generated
file. Raw snapshots are never compiled in (`no include_str!`, no `build.rs`)
and are not in git.

## Refreshing the enzyme set (manual, intentional)

Codegen is **not** automated — not a `build.rs`, not part of CI. Run it by hand
only when you mean to: bumping the REBASE version, or building a larger table.

```bash
# Fetch the latest REBASE bairoch snapshot, then regenerate the table:
cargo run -p seqforge-restriction --bin codegen -- --fetch

# Or pin a specific REBASE release URL:
cargo run -p seqforge-restriction --bin codegen -- --fetch <url>

# Regenerate from a snapshot already present in data/ (no download):
cargo run -p seqforge-restriction --bin codegen
```

Then review the diff to `src/enzymes_generated.rs` and commit **only that file**.
The downloaded `data/rebase_bairoch.txt` stays local (git-ignored).

### Refreshing methylation sensitivity

Methylation sensitivity (Dam / Dcm / CpG) is scraped from REBASE's per-enzyme
`damlist` CGI endpoint and joined into the same generated table by codegen.

```bash
# Scrape sensitivity for the ~565 kept enzymes (writes data/rebase_methylation.tsv):
cargo run -p seqforge-restriction --bin ms_scrape

# Then regenerate the table (joins bairoch + methylation):
cargo run -p seqforge-restriction --bin codegen
```

Run on the same quarterly cadence as the bairoch refresh. The TSV stays local
(git-ignored); the joined data ships inside `enzymes_generated.rs`.

### Want a larger / more inclusive table?

The default filter (`filter_and_classify` in `src/bin/codegen.rs`) keeps
commercially-available Type II / Type IIs enzymes with a known cut position
(~565 enzymes, >99% of real workflows). To include more — e.g. non-commercial
enzymes — relax that filter and regenerate. REBASE has ~5000 entries raw.

## Data attribution

Enzyme data is from REBASE — <http://rebase.neb.com> — © Dr. Richard J.
Roberts. The generated file retains the attribution header. REBASE's license
requires attribution on use of its data.
