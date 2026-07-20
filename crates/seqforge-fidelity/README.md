# seqforge-fidelity

Overhang ligation-fidelity scoring for SeqForge — Potapov 2018 / Pryor 2020
frequency matrices as committed CSVs. Concrete API, **no non-std
dependencies**. Unpublished workspace crate; extractable later like
`seqforge-restriction` (see `../../plans/fidelity.md`).

## What ships vs. what doesn't

| Path | Tracked? | Role |
|------|----------|------|
| `data/*.csv` | yes | Unaltered tatapov_data / SI count tables (runtime via `include_str!`) |
| `src/fidelity_generated.rs` | yes | Wires each CSV → dense `&[u16]` (`OnceLock` parse) |
| `src/bin/codegen.rs` | yes | Maintainer `--fetch` only (xlsx → CSV); not needed for builds |

Normal builds embed the committed CSVs. No `build.rs`, no binary matrices.

## Refreshing the matrices

```bash
# Re-download upstream xlsx, rewrite data/*.csv, smoke-parse:
cargo run -p seqforge-fidelity --bin codegen -- --fetch
```

Then review and commit the updated `data/*.csv` files.

## Data attribution

Matrices are redistributed **unaltered** under **CC BY-ND 4.0** via
[Edinburgh-Genome-Foundry/tatapov_data](https://github.com/Edinburgh-Genome-Foundry/tatapov_data)
(Potapov 2018 ACS Synth. Biol.; Pryor 2020 PLoS ONE). SapI 3-nt table from
Pryor et al. PLoS ONE S5 (`10.1371/journal.pone.0238592.s005`). See
`ATTRIBUTION.md`.
