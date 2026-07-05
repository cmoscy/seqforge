//! Primer **construction** (Phase 2.2 — "tail composition") — the third primer
//! concern alongside `evaluate` (QC) and `anneal` (find/classify).
//!
//! This module owns the pure *builder* functions that assemble oligo bytes from
//! constraints: constrained-random oligos (rejection sampling over the `evaluate`
//! QC) and restriction-site primer tails (recognition + NEB flanking + — for
//! Type IIS — a user-supplied overhang placed at the enzyme's cut offset, using
//! `seqforge-restriction`'s `top_offset`/`bottom_offset`/`overhang_kind`).
//!
//! Scope boundary (ROADMAP decision 16 / primers decision 13): this is
//! *construction* given a user-chosen overhang — deterministic and safe to ship.
//! Overhang **set design** (fidelity/uniqueness) and barcode **set design**
//! (distance + colour balance) are a separate, data-backed `design` package,
//! deferred. Builders here are pure and headless so both the CLI verbs and the
//! Inspector's insertion-tools UI call one source of truth.
//
// Intentionally empty until Phase 2.2 lands the first builder — this file is the
// pre-placed seam so construction logic never bloats `evaluate.rs`/`anneal.rs`.
