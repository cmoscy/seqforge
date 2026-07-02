# seqforge-thermo

Sequence thermodynamics for SeqForge: melting temperature (`tm`), GC content
(`gc`), and — reserved for later phases — nucleic-acid MFE folding (`fold`/`dg`)
for self-hairpin / self-dimer ΔG and two-sequence heteroduplex Tm.

This crate is **pure and zero-dependency** (no workspace or non-std deps) and
`publish = false`, mirroring `seqforge-restriction`. That constraint is what
keeps a future crates.io extraction a one-crate change. It is reached only via
`seqforge-bio` (`bio → thermo` is the single new cross-crate edge); `seqforge-core`
never depends on it — Tm/GC are *derived* data, never stored on the model.

## Public API (Phase 0.1)

- `tm(oligo) -> Result<f64, TmError>` — nearest-neighbour Tm (°C) of a single
  oligo (SantaLucia unified NN + Owczarzy-2008 salt), under seqfold's default
  PCR salt conditions.
- `gc(seq) -> f64` — GC content as a percentage (`0.0..=100.0`).

The vendored `core` module additionally carries seqfold's Zuker MFE folding and
the two-sequence `core::tm::tm(seq1, seq2, pcr)` heteroduplex path; those back
later phases (self-structure ΔG, primer:template annealing) and are not part of
the 0.1 thin API.

## Vendored from seqfold (MIT)

The numerical engine in `src/core/` is vendored from
[**seqfold**](https://github.com/Lattice-Automation/seqfold) @ **v0.10.1**
(Lattice Automation, MIT), files `src/core/{tm,fold,data,energies,pyfloat}.rs`.

Per the MIT terms, seqfold's `LICENSE` and copyright are retained verbatim in
[`LICENSE`](./LICENSE) (Copyright © 2019 Lattice Automation).

**Modifications from upstream** (to keep the crate pure and zero-dep):

- **`pyo3` dropped** — the optional Python-extension feature and `python.rs`
  bindings are not vendored; only the pure-Rust `core` engine.
- **`rayon` → serial** — the folding DP anti-diagonal fill (`fold::fill`) ran in
  parallel upstream; here it is serial. At primer/short-window sizes the serial
  fill is instant.
- **`smallvec` → `Vec`** — the inline per-cell basepair lists (`SmallVec`) are
  plain `Vec`.

The numerics are otherwise a faithful port; the vendored `tm` unit test and this
crate's `lib.rs` tests validate against seqfold's own reference vectors
(Owczarzy 2008, Table 1) plus a permissive Biopython `MeltingTemp` cross-check.

`primer3` / `ntthal` (GPL) is **not** a dependency and never linked — it is
retained only as an optional *offline* validation oracle for the deferred
gapped-heteroduplex work (see `plans/primers.md`).
