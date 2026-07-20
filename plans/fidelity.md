# `seqforge-fidelity`: Overhang Ligation Fidelity Crate

> Canonical cross-track status: [`../ROADMAP.md`](../ROADMAP.md) (v0.3 milestone).
> Consumed by [`assembly.md`](assembly.md) (Golden Gate + any sticky-end
> ligation). **Pipeline:** committed Potapov/Pryor CSVs embedded via
> `include_str!` (zero-dep); maintainer `--fetch` refreshes from tatapov_data.

> **Status — F1 landed (in-tree).** Full 4-nt + 3-nt (SapI) matrices; informational
> per-combo scoring in the assembly workbench / CLI dry-run. Matrix-first API
> matches NEB Ligase Fidelity Viewer (RC-expanded axes; palindromes doubled).
> GetSet/SplitSet = F2; crates.io extraction = F3.

## Goal

Score the ligation **fidelity** of an overhang set as a **quantitative % correct**
— the probability each overhang ligates only to its intended Watson–Crick partner
rather than mis-ligating to another set member. This is the offline, local,
batch-capable analog of NEB's **Ligase Fidelity Viewer** (read-side score).

**Matrix first, % as summary.** The primary artifact is the **subset ligation-frequency
matrix** (Viewer axes). The set percentage is derived from that matrix — not a
separate scoring path.

GetSet / SplitSet analogs (orthogonal-set *design*, as in GoldenHinges clique
search over GC/Hamming/crosstalk filters) are **F2**, not F1. F1 stays
Viewer-shaped for scoring only.

## NEB Viewer scoring (locked)

1. **Expand labels** — unique input overhangs; for each `h` append `h` then
   `rc(h)` **always**, even when `h == rc(h)`. Palindromes (e.g. `AATT`) therefore
   appear **twice** on both axes.
2. **Subset matrix** — `counts[i][j]` = published ligation count for
   `labels[i]` × `labels[j]`.
3. **Per input overhang** (not per expanded label):
   - `on = M[h][rc(h)]` — single Watson–Crick cell
   - `total = Σ_j M[h][labels[j]]` — sum over expanded labels (palindrome
     self-ligation counted twice in the denominator)
   - `junction_fidelity = on / total`
4. **`set_fidelity = Π` junctions** → a single fraction (e.g. 0.998 = 99.8%).

Fixture (Pryor `bsai` table): `AAGG,ACTC,AGGA,AGTG` → ~99%; same + `AATT` → ~50%.
(Viewer UI may show a cycling BsaI-HFv2 condition; our shipped Pryor BsaI matrix
matches order of magnitude.)

Indexed by **overhang sequence**, not enzyme — so it serves *any* sticky-end
ligation (Type II restriction–ligation *and* Type IIS Golden Gate).

## Product rules (locked)

- **Informational only** — never changes `run` / `run_indices`, never filters or
  sorts the combo grid by score, never blocks or disables Run. On and Off execute
  identically.
- **Never in recipe.json / recipe IR** — not a deferred feature. Scoring is only
  (1) a **GUI session** choice on the assembly tab, or (2) a **CLI dry-run overlay**
  (`--dry-run --fidelity-dataset <id>`; optional `--fidelity-matrix` for the
  subset matrix JSON). Save/reload never carries % or dataset id.
- **Per-combo** — each preview / dry-run row scores that combo’s overhang set.
  Workbench shows the **% summary** only (`%` / `%*` when any scored overhang was
  3′-chemistry). Full matrix heatmap = out of scope for F1.
- **Sequence-only (NEB-style)** — any sticky overhang letters are scored regardless
  of 5′/3′ cut chemistry (PstI `TGCA` is scored like a paste into Ligase Fidelity
  Viewer). Matrices still **assume Potapov/Pryor 5′-assay conditions**; blunt /
  wrong length / non-ACGT stay unscorable. Documented in UI hover.
- **Crate shape** — in-tree workspace member `crates/seqforge-fidelity`, zero
  workspace deps, `publish = false`, extractable later like restriction/thermo
  (**not** a separate `.git` in F1).

## Source data

Unaltered counts under **CC BY-ND 4.0** via EGF
[tatapov_data](https://github.com/Edinburgh-Genome-Foundry/tatapov_data)
(+ Pryor PLoS ONE S5 for SapI 3-nt). See crate `ATTRIBUTION.md`.

| Dataset id | Source | Len |
|------------|--------|-----|
| `t4_25c_18h` (default) | Potapov 2018 T4 25 °C 18 h | 4 |
| `t4_25c_01h` / `t4_37c_*` | Potapov 2018 other T4 conditions | 4 |
| `bsai` / `bsmbi` / `esp3i` / `bbsi` | Pryor 2020 GG enzymes | 4 |
| `sapi` | Pryor 2020 SapI (S5) | 3 |

## Data pipeline (methylation / restriction pattern)

1. `data/*.csv` — **committed** unaltered count tables (runtime via `include_str!`).
2. `src/bin/codegen.rs` — maintainer refresh only: `--fetch` downloads upstream
   xlsx → CSV (Python/`openpyxl`), then smoke-parses with `csv_matrix`.
   Not a `build.rs`; normal builds never run it.
3. Zero-dep, `&'static [u16]` matrices — base-4 index lookup after parse-once.

## API surface (concrete, no traits)

```rust
pub struct SubsetMatrix {
    pub labels: Vec<Vec<u8>>,  // RC-expanded; palindrome → two identical labels
    pub counts: Vec<u32>,      // row-major len = n*n
}

pub struct FidelityReport {
    pub set_fidelity: Option<f64>,       // Π of per-junction on/total; None if uncovered
    pub junctions: Vec<JunctionScore>,
    pub worst: Option<usize>,
    pub uncovered: Vec<Vec<u8>>,         // blunt / wrong length / non-ACGT (not 3′-chemistry)
    pub matrix: Option<SubsetMatrix>,    // axes used for the % (when scored)
}

pub fn expand_overhang_labels(overhangs: &[&[u8]]) -> Vec<Vec<u8>>;
pub fn subset_matrix(overhangs: &[&[u8]], dataset: Dataset) -> Option<SubsetMatrix>;
pub fn junction_fidelity(overhangs: &[&[u8]], dataset: Dataset) -> FidelityReport;
pub fn dataset_for_enzyme(name: &str) -> Option<Dataset>;

pub enum Dataset { T4_25C_18h, /* … */, SapI }
impl Dataset {
    pub fn overhang_len(self) -> u8;
    pub fn covers(self, overhangs: &[&[u8]]) -> bool;
}
```

**Dataset selection:** default `T4_25C_18h`; GG enzyme can preselect its Pryor
table; UI length-gates via `covers` / enzyme (SapI ↔ 3-nt tables only).

**CLI:** `--dry-run --fidelity-dataset <id> [--fidelity-matrix]` — combo rows keep
`fidelity` / `fidelity_three_prime`; with `--fidelity-matrix`, dry-run root gains
`fidelity_matrix: { combo_index, labels, counts }` for the first compatible combo
(else the first combo).

## Related tools (roles)

| Tool | Role | Takeaway |
|------|------|----------|
| **NEB Ligase Fidelity Viewer** | Subset matrix + estimated % | Gold standard for *scoring* — RC expand; palindromes twice |
| **tatapov** | `data_subset` / plot of the same matrices | Confirms subset-matrix-first |
| **GoldenHinges** | *Design* orthogonal sets (clique search) | Guides **F2** GetSet — not the Viewer % formula |

## Phasing

- **F1 — Data + matrix API + `junction_fidelity` + assembly wiring.** ✅ Full 4-nt
  + 3-nt tables; NEB palindrome expansion; workbench Join strip + per-combo %;
  CLI `--fidelity-dataset` / `--fidelity-matrix` on dry-run.
- **F2 — `suggest_set` / `split_target`.** GetSet/SplitSet analogs (GoldenHinges-
  style design constraints: forbid palindromes / RC pairs, Hamming/GC filters).
- **F3 — Extraction.** crates.io / own repo alongside `seqforge-restriction`
  Tier 4 (still optional; in-tree remains valid).

## Testing

- Spec-anchored: a published high-fidelity 4-nt set scores >0.95 under T4 25C 18h.
- Viewer fixture: `AAGG,ACTC,AGGA,AGTG` high on `bsai`; +`AATT` → ~50%.
- Expand: palindrome → two identical axis labels.
- Length gate: wrong-length overhangs → `uncovered` / `None`.
- SapI 3-nt matrix loads and scores.

## Out of scope

- recipe.json fidelity fields or dataset ids (**never a feature**).
- Gating / filtering / sorting Run or combo selection by fidelity % (**never**).
- Full matrix heatmap in the GUI (F1 shows % summary only).
- Separate 3′-chemistry ligation matrices (we apply 5′-assay sequence data to any
  sticky letters; blunt / wrong length / non-ACGT remain `uncovered`).
- Gibson homology-arm specificity — assembly A4, not this crate.
- Reaction kinetics / concentration modeling.
