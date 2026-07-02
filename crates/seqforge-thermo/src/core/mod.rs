//! Vendored `seqfold` numerical engine (pure Rust, no Python, zero deps).
//!
//! Ported verbatim from `github.com/Lattice-Automation/seqfold` @ v0.10.1 (MIT),
//! files `src/core/{tm,fold,data,energies,pyfloat}.rs`, with the crate's
//! non-std dependencies stripped: `pyo3` dropped, `rayon` → serial, `smallvec`
//! → `Vec`. See this crate's `README.md` and `LICENSE` for attribution.
//!
//! `fold`/`dg` are retained in place (they carry seqfold's Zuker MFE folding for
//! self-hairpin / self-dimer ΔG) but are not part of the thin public API yet —
//! the primers-plan Phase 1.2 surfaces them. The 0.1 public surface is the
//! crate-root [`crate::tm`] / [`crate::gc`] pair.

pub mod data;
pub mod energies;
pub mod fold;
pub mod pyfloat;
pub mod tm;
