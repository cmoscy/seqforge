# Methylation-Aware Cut Sites

> Canonical cross-track status: [`../ROADMAP.md`](../ROADMAP.md). This builds on
> [`restriction.md`](restriction.md) (the enzyme table + scanner) and surfaces in
> the viewer via the Cut-sites Inspector tab (decision 15/17). It is the **pre-`v0.2`
> correctness feature** — the last item before the `v0.2.0` tag.

> **Status — data pipeline + evaluator + integration complete (Phases 0–4),
> GUI-confirmed. Derive-on-change model: `View.methylation` persisted; verdicts
> cached in `View.methyl_states` (parallel to `cut_sites`), recomputed only on
> enzyme/context change and read by render/CLI — not evaluated per frame.**

## Why this is a correctness issue, not a nicety

Standard *E. coli* plasmid preps are **Dam⁺ / Dcm⁺**: the DNA a user actually
holds is methylated at every `GATC` (Dam, m6A) and `CCWGG` (Dcm, m5C). A
restriction enzyme whose recognition site overlaps one of those contexts **will
not cut**, even though a naive scan shows a site. Mammalian / in-vitro material
adds **CpG** (m5C at `CG`). Every serious tool (SnapGene, NEBcutter, Benchling)
accounts for this because picking a blocked enzyme is a silent experimental
failure. SeqForge currently shows all sites as cuttable — the one place a
"correct-looking" answer is actively wrong.

## The model: two factors, one verdict

A site is only truly blocked when **both** hold:

1. **Enzyme sensitivity** — an empirical property of the enzyme: is it
   `Blocked` / `Impaired` / `Cut` (unaffected) by methylation of a base in its
   recognition context? Not computable — must be sourced (REBASE, below).
2. **Context present in *this* site** — does the methylatable base actually fall
   inside this specific occurrence of the recognition sequence, given flanking
   bases? Purely computable from the sequence.

`sensitivity ∧ context-present → blocked-here`. This recovers the
context-dependent ("Some Blocked") cases — e.g. ClaI (`ATCGAT`) is Dam-sensitive
but only blocked where a `GATC` overlaps (`…ATCGATC…` / `GATCGAT…`), not at every
ClaI site — **without** storing REBASE's full double-strand context matrix.

The methylation *state* is a **per-sequence** property (which host the DNA came
from), toggled by the user; it is the context input, not enzyme data. This
mirrors SnapGene / OpenVectorEditor, which store `damMethylated` / `dcmMethylated`
/ `ecoKIMethylated` bits on the sequence (`examples/tg-oss/.../snapgeneToJson.js`).

## Decision of record (roadmap decision 18)

**Methylation sensitivity rides the *same* ingestion pipeline as the enzyme
table, joined onto the *same* `Enzyme` record, so every enzyme provably carries
both its recognition and its sensitivity data — parity is enforced by the
compiler and CI, not by discipline.** See ROADMAP decision 18 for the one-line
form; this doc owns the mechanics.

## Data sourcing — the honest picture

The sensitivity data **is authoritative in REBASE** ([rebms](http://rebase.neb.com/rebase/rebms.html))
— Dam, Dcm, CpG, EcoBI, EcoKI, with standardized verdicts (**Cut / Impaired /
Blocked / Some Blocked / Variable / Untested**), shown in double-strand form. But:

- It is **not** in the `bairoch` snapshot we vendor, nor in **any** of REBASE's
  ~41 bulk download formats (`withref`, `withrefm`, `type2`, EMBOSS, GCG, …). All
  of those are recognition / cleavage / refs / isoschizomers / commercial-source.
- It is exposed **only** through per-enzyme CGI records (indexed by `/cgi-bin/mslist`).
  Two endpoints exist, and **the source is `damlist`, not `msget`** (Phase 0 finding):
  - `cgi-bin/msget?<name>` — one `<tr>` per *experiment* (a specific modification
    symbol at a specific position, outcome by font colour), mixing host methylation
    with exotic modifications (glucosyl/hydroxymethyl/uracil). Messy to reduce to a
    scalar — **not used**.
  - `cgi-bin/damlist?e<name>` — REBASE's own **generated per-enzyme summary**: a
    two-row block (`overlaps?` / `sensitivity?`) of 5 cells each, fixed column order
    **Dam / Dcm / CpG / EcoBI / EcoKI**. This is the clean, uniform source. Parse =
    collapse newlines, find the `sensitivity?` label, take the next 5
    `<font size=1>…</font>` cells.
- **Value vocabulary** (observed across a diverse sample): `cut`, `blocked`,
  `impaired`, `some blocked`, `some impaired`, `variable`, `-`. Normalize →
  `MethylEffect`: `cut`/`-`→`Cut`; `blocked`/`some blocked`→`Blocked`;
  `impaired`/`some impaired`→`Impaired`; `variable`→`Variable`. The "some"/context
  dependence is then resolved per-site by factor (2).
- Biopython `Bio.Restriction` does **not** carry it. tg-oss / OVE do **not**
  carry an enzyme-sensitivity table (only the per-sequence state bits). So there
  is no ready-made machine table to borrow — sourcing is genuinely required.

**Two-factor model empirically validated (Phase 0):** `damlist` reports **BamHI**
(`GGATCC`, which *contains* the Dam site `GATC`) as `Dam: cut` — a pure
context-presence check would wrongly flag it as blocked, but the curated sensitivity
says it cuts. So the sourced scalar (factor 1) is genuinely necessary, and the AND
with computed context (factor 2) is what makes BamHI correctly cuttable while MboI
(recognition *is* `GATC`) is always blocked.

**NEB chart is not programmatically usable** (Cloudflare 403 to both `curl` and
WebFetch), and moot anyway — `damlist` is the REBASE superset NEB itself derives
from. Thermo / Promega tables remain a manual cross-check for any supplier-specific
enzyme `damlist` doesn't cover.

## Pipeline — one flow, two snapshots

Today the enzyme table flows:
`data/rebase_bairoch.txt → codegen generate → enzymes_generated.rs (committed) → codegen check (CI)`.
Per `.gitignore` + the crate README, the raw `data/` snapshots are **fetched at
ingest and never committed** — the generated `enzymes_generated.rs` is the single
committed source of truth (`no include_str!`, no `build.rs`).

Add a **second regenerated snapshot** that feeds the **same** codegen and the
**same** output record (same fetch-at-ingest treatment as `bairoch`):

```
data/rebase_bairoch.txt      ─┐
data/rebase_methylation.tsv  ─┴─►  codegen (join on name)  ─►  enzymes_generated.rs
```

New pieces (siblings of the existing `src/bin/codegen.rs`):

- **`data/rebase_methylation.tsv`** — gitignored, regenerated snapshot (like
  `bairoch`). One row per enzyme: `name<TAB>dam<TAB>dcm<TAB>cpg` with values in
  `{cut, impaired, blocked, variable, untested}`. Header preserves REBASE attribution.
- **`src/bin/ms_scrape.rs`** — bounded scraper. Reads the *kept* enzyme names from
  the current table, fetches `damlist?e<name>` for **only those** (~300 pages),
  parses the `sensitivity?` summary row (Dam/Dcm/CpG columns), normalizes per the
  value-vocabulary map above, writes the `.tsv`. Run on the same quarterly cadence
  as the `bairoch` refresh. The reviewable artifact is the resulting
  `enzymes_generated.rs` **diff** (where the joined verdicts land), reviewed exactly
  like any codegen output — the `.tsv` itself is a throwaway build input. From
  codegen's viewpoint it is identical to `bairoch` — read a fetched file, emit.
- **`codegen`** — extended: after building the kept set from `bairoch`, parse the
  `.tsv` into a `name → MethylSensitivity` map, **join**, emit sensitivity into the
  *same* `Enzyme { … }` literal.

### The scraper's one asymmetry, stated plainly

Recognition data is one clean flat file; sensitivity must be assembled from
per-enzyme HTML records that REBASE notes are "not rigidly aligned." The scraper
absorbs that irregularity and emits a clean flat `.tsv`, so the asymmetry stops at
acquisition. If a `msget` record fails to parse, the scraper emits `untested` for
that enzyme and logs it — never a silent guess.

## Parity — enforced in three layers, not by care

1. **Type layer — one record, required field.** Add to `Enzyme`:

   ```rust
   pub struct Enzyme {
       // …existing…
       pub methylation: MethylSensitivity,   // NOT Option — every enzyme carries one
   }

   #[derive(Debug, Clone, Copy, PartialEq, Eq)]
   pub struct MethylSensitivity {
       pub dam: MethylEffect,
       pub dcm: MethylEffect,
       pub cpg: MethylEffect,
   }

   #[derive(Debug, Clone, Copy, PartialEq, Eq)]
   pub enum MethylEffect { Cut, Impaired, Blocked, Variable, Untested }
   ```

   A non-`Option` field means the compiler **forces** every emitted enzyme literal
   to carry a value. "Two parallel arrays that can drift" becomes unrepresentable
   (same structural move as decisions 12 / 17).

2. **Join layer — exact key + biological fallback.** Primary join key is the REBASE
   canonical name, identical across `bairoch` and `msget`, so the join is exact.
   **Fallback:** propagate by identical recognition sequence to fill isoschizomer
   gaps (sensitivity tracks the methylated recognition context, so isoschizomers
   share it), with a **conflict detector** that fails codegen if two enzymes with
   the same recognition report different verdicts.

3. **Coverage-gate layer — CI catches the silent miss.** **Fail if any kept
   enzyme is `Untested` outside a small reviewed allowlist.** A quarterly refresh
   that introduces a new enzyme without sensitivity data then **trips CI** instead
   of shipping a silent gap. This is the "don't silently miss one" guarantee.
   *As shipped:* this landed as the `every_enzyme_has_sourced_methylation` **test**
   in `tests/methylation.rs` (runs under `cargo test`, so it gates CI
   unconditionally), not a `codegen check` subcommand. The allowlist is read via
   `include_str!("fixtures/ms_untested_allow.txt")`.

## Evaluation — the computable half

New in `seqforge-restriction` (`src/methylation.rs`):

```rust
pub struct MethylContext { pub dam: bool, pub dcm: bool, pub cpg: bool }

pub enum SiteMethylState { Cuttable, Impaired, Blocked }

/// Two-factor verdict for one found site under a methylation context.
pub fn site_methyl_state(
    site: &Site,
    enzyme: &'static Enzyme,
    seq: &[u8],          // for flanking context around the site
    ctx: &MethylContext,
) -> SiteMethylState;
```

For each enabled system in `ctx`, factor (2) checks whether a methylatable base
(`GATC` A / `CCWGG` inner C / `CG` C) lies inside `site.recognition_start
..recognition_end`, **considering flank** (Dam's `GATC` can overlap the site edge).
If context present **and** `enzyme.methylation.<system>` is `Blocked` → `Blocked`;
if `Impaired` (or context present with a `Some Blocked`-class enzyme) → `Impaired`;
else the system doesn't apply. The worst verdict across enabled systems wins.

`MethylContext` default: **Dam + Dcm on, CpG off** — matches real *E. coli*
plasmid DNA, the common case, so the honest default is protective.

## SeqForge integration (derive-on-change, cached on the View)

**Principle:** `CutSite` stays geometry-only. Verdicts are **derived, not stored
on the site** — but they *are* cached on the `View` as `methyl_states`, a `Vec`
parallel to `cut_sites` with the **same lifecycle** (recomputed only when the
enzyme set or the methylation context changes, never per frame, and a toggle
re-evaluates verdicts without re-scanning for sites). `View.methylation`
(`MethylContext`) is the persisted toggle state; `View.methyl_states` is a derived
`#[serde(skip)]` cache. Industry convention: sites stay visible; blocked/impaired
sites grey out and carry `*` / `(blocked)` / `(impaired)` cues.

- **`seqforge-bio`** — `find_cut_sites` returns geometry only;
  `methyl_states_for_sites(sites, seq, ctx)` (batch) derives verdicts for a site
  slice (the single-site evaluator behind it is crate-private).
- **Persistence / cache** — `methylation: MethylContext` on `View` (default
  Dam+Dcm on); `methyl_states: Vec<MethylState>` cache, written by the `Enzymes`
  dispatch and by `SetMethylation`.
- **Viewer** — Dam / Dcm / CpG toggles in the Cut-sites Inspector tab; the map
  track dims labels/ticks by **indexing `BlockCtx.methyl_states`** (the cache,
  blanked while staging), not by evaluating per frame.
- **CLI** — `seqforge enzymes … --dam/--dcm/--cpg`; `CutSites` response carries
  parallel `methyl_states` (derived at dispatch time, not stored on `CutSite`).

## Testing

- **Layer 1 — spec-anchored** (`tests/methylation.rs`): assert against
  known biology — ClaI blocked by Dam only in overlapping context but not
  otherwise; XbaI blocked by Dam in `TCTAGATC`; MboI (`GATC`) blocked by Dam
  everywhere; Dcm-sensitive (e.g. StuI in `CCWGG` overlap); a CpG-blocked cutter
  (e.g. SmaI-class in `CG` context); an unaffected control (BamHI).
- **Layer 2 — invariants**: `MethylContext { all false }` ⇒ every site `Cuttable`
  (feature is off-by-context, never changes the site set); context-presence
  detection commutes with reverse-complement; parity — **every enzyme in the
  emitted table has a `MethylSensitivity`** (guaranteed by the type, asserted as a
  belt-and-braces test that none are `Untested` outside the allowlist).
- **Codegen** — `codegen check` re-run yields byte-identical output on a clean
  snapshot; the conflict detector and coverage gate have unit tests over small
  synthetic inputs.

## Verification

1. `cargo run -p seqforge-restriction --bin ms_scrape -- generate` (re-)produces
   `data/rebase_methylation.tsv`; `codegen generate` yields byte-identical
   `enzymes_generated.rs`.
2. `cargo test -p seqforge-restriction` — Layer 1 + 2 green; coverage gate green.
3. Open pUC19 fixture, Cut-sites tab, Dam **on**: ClaI/XbaI sites overlapping
   `GATC` render blocked; toggling Dam **off** restores them. Matches a
   SnapGene / NEBcutter screenshot manually.
4. CLI: `seqforge enzymes unique --dam` count matches the GUI with Dam on.

## Out of scope

- EcoBI / EcoKI systems (rare; REBASE has the data — additive later behind the same
  `MethylEffect`/`MethylContext` shape).
- Enzyme-methyltransferase pairing / de-novo methylation prediction beyond the
  three host systems.
- Star activity (tracked separately in restriction "out of scope").
- Editing/authoring methylation on sub-ranges (only whole-sequence host state now).

## Files touched

New (committed):
- `plans/methylation.md` (this file)
- `crates/seqforge-restriction/src/bin/ms_scrape.rs`
- `crates/seqforge-restriction/src/methylation.rs`
- `crates/seqforge-restriction/tests/methylation.rs`
- `crates/seqforge-restriction/tests/fixtures/ms_untested_allow.txt` (reviewed allowlist read by the coverage-gate test — the one curated, committed input)

New (gitignored, regenerated at ingest — not committed):
- `crates/seqforge-restriction/data/rebase_methylation.tsv` (produced by `ms_scrape`; joined into the committed `enzymes_generated.rs`)

Modified:
- `crates/seqforge-restriction/src/enzyme.rs` (`MethylSensitivity`/`MethylEffect`, `Enzyme` field)
- `crates/seqforge-restriction/src/bin/codegen.rs` (join + coverage gate + conflict detector)
- `crates/seqforge-restriction/src/enzymes_generated.rs` (regenerated with sensitivity)
- `crates/seqforge-bio/src/search.rs` (`MethylContext` thread-through)
- `seqforge-core` view/buffer DTO (persisted `MethylContext`)
- `seqforge-app` Cut-sites tab (toggle row + blocked/impaired rendering)
- `plans/restriction.md`, `ROADMAP.md` (status + decision 18)
