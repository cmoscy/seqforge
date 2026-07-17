# `seqforge-fidelity`: Overhang Ligation Fidelity Crate

> Canonical cross-track status: [`../ROADMAP.md`](../ROADMAP.md) (v0.3 milestone).
> Consumed by [`assembly.md`](assembly.md) (Golden Gate + any sticky-end
> ligation). **Pipeline mirrors [`methylation.md`](methylation.md)** (decision 18):
> gitignored snapshot → codegen → committed static table, zero-dep engine crate.

> **Status — NOT STARTED (design of record).** One ⚠ hard gate blocks Tier 1:
> the **data license** (below). Resolve before vendoring any matrix.

## Goal

Score the ligation **fidelity** of an overhang set as a **quantitative % correct**
— the probability each overhang ligates only to its intended Watson–Crick partner
rather than mis-ligating to another set member. This is the offline, local,
batch-capable analog of NEB's **Ligase Fidelity Viewer / GetSet / SplitSet** web
tools, which return the same percentage.

**The metric (from the frequency matrix, not a category):** Potapov's data is the
raw **ligation-frequency matrix** (observed counts of every overhang *i* joining
every overhang *j*). For a given set, per junction:

```
junction_fidelity = on_target / (on_target + Σ off_target_within_the_set)
set_fidelity      = Π over all junctions   →  a single fraction (e.g. 0.998 = 99.8%)
```

So a designed all-distinct Type IIS set scores high (≈0.99+), and a set of N
identical self-complementary overhangs scores toward **~1/N** — a real number, not
a "promiscuous" flag.

Indexed by **overhang sequence**, not enzyme — so it serves *any* sticky-end
ligation (Type II restriction–ligation *and* Type IIS Golden Gate), which is why
the assembly op needs no enzyme "mode" (see [`assembly.md`](assembly.md),
decision 2 there).

## Source data

Potapov et al. 2018, *ACS Synth. Biol.* 7(11):2665–2674 — high-throughput
profiling of T4 DNA ligase joining for **all 3-base and 4-base overhang pairs**,
under defined conditions. Public artifacts:
- `github.com/potapovneb/ligase-fidelity` (tools + data)
- figshare datasets per enzyme/condition (BsaI, Esp3I/BsmBI, SapI, various
  temp/time) — ligation-frequency matrices over overhang pairs.
- The NEB Ligase Fidelity Viewer/GetSet/SplitSet are *tools over this data*
  (separate from the published data itself).

> ⚠ **License gate (Tier 1 blocker).** Confirm the figshare/repo data license
> permits redistribution **before** baking a matrix into a committed table. NEB's
> web *tools* are separate from the *data* license — check the data terms
> specifically. Fallback if redistribution is barred: (a) GoldenHinges (EGF, open)
> as an alternative overhang-set design reference; (b) ship the codegen + a
> fetch-at-ingest snapshot (like REBASE `bairoch`) rather than committing the raw
> matrix, if that satisfies the terms.

## Data pipeline (the methylation pattern, exactly)

1. `data/ligase_fidelity_*.tsv` — gitignored, fetched-at-ingest snapshot(s), one
   per enzyme/condition dataset, keyed by (overhang length, enzyme, temp, time).
2. `src/bin/codegen.rs` — parses snapshot(s) → emits committed
   `src/fidelity_generated.rs` as `const` matrices (the reviewable artifact).
   Not a `build.rs`. CI runs the codegen check (snapshot ↔ generated in sync),
   same as `seqforge-restriction`.
3. Zero-dep, `&'static` tables — pointer-arithmetic lookup, no startup alloc.

## API surface (concrete, no traits)

```rust
pub struct FidelityReport {
    pub set_fidelity: f64,               // % correct as a fraction 0..1 (Π of per-junction on/(on+off))
    pub junctions: Vec<JunctionScore>,   // per intended pair: on- vs off-target
    pub worst: Option<usize>,            // index of the weakest junction
    pub uncovered: Vec<Overhang>,        // no dataset (blunt / 2-base / 3' overhang)
}

// The universal metric the assembly op always calls.
pub fn junction_fidelity(overhangs: &[Overhang], dataset: Dataset) -> FidelityReport;

// Design/partition analogs of GetSet / SplitSet (assembly A3+/A5).
pub fn suggest_set(n: usize, constraints: SetConstraints) -> Vec<Overhang>;      // GetSet
pub fn split_target(seq: &[u8], n_junctions: usize) -> Vec<Junction>;           // SplitSet

pub enum Dataset {           // enzyme + condition; each is a separate published table
    T4_25C_18h,              // DEFAULT — general T4 ligase, 25 °C, 18 h (NEB Viewer default)
    Bsa_37C,   Esp3I_37C,    // 4-base overhang enzyme tables
    Sap_37C,                 // 3-base overhang enzyme table
    /* … as licensed */
}
impl Dataset {
    fn overhang_len(self) -> u8;     // 4 for Bsa/Esp3I/T4-4nt tables, 3 for Sap
    fn covers(self, junctions: &[Overhang]) -> bool;  // relevance gate (length/geometry match)
}

// Enzyme → its specific table when we have one, else the T4_16C default.
pub fn dataset_for(enzyme: Option<&Enzyme>) -> Dataset;
```

**Dataset selection (enzyme informs the table, not the algorithm):**
- **Default = `T4_25C_18h`** — NEB's own Viewer default: a long near-equilibrium
  T4 incubation that mirrors a standard overnight restriction–ligation **and** is
  validated to predict cycled Golden Gate (16/37 °C). One default serves normal
  ligation and GG; the metric math is overhang-keyed, so it always applies.
- **Auto-select by enzyme** — if the assembly uses a recognized GG enzyme
  (BsaI/BsmBI·Esp3I/SapI) with its own published table, `dataset_for` picks it;
  the empirical frequencies were measured in that enzyme's reaction context.
- **Dropdown override, relevance-gated** — the UI only offers datasets whose
  `overhang_len`/geometry **match the current junctions** (`covers`); e.g. a
  3-base SapI assembly never shows a 4-base table. Selecting an irrelevant table
  is unrepresentable, not just discouraged.

Scope limits baked into the type (never fabricate a number): overhangs outside the
chosen dataset's length/geometry (blunt, 2-base, 3′) land in `uncovered`, not in
the score. Gibson has no overhangs → this crate is not called for Gibson.

## Phasing

- **F1 — Data + `junction_fidelity`.** Snapshot + codegen + the read-side score
  over one dataset (4-base 5′). Consumed by assembly **A3** for the Golden Gate
  readout. Spec-anchored tests against published Viewer numbers for a known set.
- **F2 — `suggest_set` / `split_target`.** GetSet/SplitSet analogs — used by
  assembly A5 (batch) and, later, the primers 2.2b generative package (overhang
  **sets**). Property tests: suggested sets score above a threshold; partitions
  reassemble.
- **F3 — Extraction.** Additional datasets (3-base, other enzymes/conditions),
  README/attribution, crates.io alongside `seqforge-restriction` (Tier 4 there).

## Testing

- **Spec-anchored:** a published high-fidelity set scores high; a set containing a
  palindromic/self-complementary overhang scores low (matches the biology — see
  assembly.md); Viewer-documented example numbers reproduced.
- **Coverage-gate** (methylation's `every_enzyme_has_sourced_methylation` analog):
  every overhang of the supported length is present in the generated matrix, or in
  a reviewed allowlist.
- **Property:** `set_fidelity ∈ [0,1]`; symmetry where the assay is symmetric;
  `suggest_set` outputs pass `junction_fidelity` above its own threshold.

## Out of scope

- 3′-overhang / 2-base / blunt fidelity (no dataset) — reported as `uncovered`.
- Gibson homology-arm specificity — lives in `seqforge-bio` assembly (A4), not
  here (different mechanism, not overhangs).
- Reaction kinetics / concentration modeling — the metric is set specificity, not
  yield.
