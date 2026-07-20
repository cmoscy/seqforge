//! Overhang ligation fidelity — Potapov/Pryor frequency matrices.
//!
//! Zero-dep extractable crate. Committed `data/*.csv` tables are embedded via
//! [`fidelity_generated`] (`include_str!` + parse-once). Refresh upstream with
//! `cargo run -p seqforge-fidelity --bin codegen -- --fetch`. See `README.md`
//! and `ATTRIBUTION.md`.

pub mod csv_matrix;
mod fidelity_generated;
mod score;
mod types;

pub use score::{dataset_for_enzyme, expand_overhang_labels, junction_fidelity, subset_matrix};
pub use types::{Dataset, FidelityReport, JunctionScore, SubsetMatrix};
