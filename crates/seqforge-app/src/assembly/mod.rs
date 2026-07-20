//! The assembly-recipe **workbench** — the app's `Tab::Recipe` surface. Collects
//! sources into bins, runs the shared `seqforge_bio::assembly` engine over them,
//! and materializes products as buffers. Self-contained: nothing depends on it
//! but the tab dispatcher; it depends inward on the `Fragment` IR + engine.

pub(crate) mod resolver;
pub(crate) mod workbench;

pub(crate) use workbench::show;
