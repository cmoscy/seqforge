//! Headless session layer: the editor's document state and the dispatch that
//! mutates it, with no renderer in scope.
//!
//! ## Why this crate exists
//!
//! ROADMAP decision 9 forbids `core ──► bio`, so bio-derived write operations
//! have to live *above* `bio`. `docs/architecture.md` puts it as "it belongs
//! where both crates are already in scope" — and for a long time the only crate
//! satisfying that was `seqforge-app`, so the session model landed inside the
//! GUI. That was a dependency-graph accident, not a design intent: it made every
//! verb that touched a workspace GUI-only, and parity had to be restored by
//! duplicating commands (see ROADMAP decision 27).
//!
//! This crate is that missing address. It sits above `core` *and* `bio` and
//! below both shells, so `seqforge-cli` and `seqforge-app` drive one
//! implementation.
//!
//! ## The rule
//!
//! **Nothing here may depend on egui.** `cargo tree -p seqforge-session` is
//! expected to be renderer-free, and that is what keeps the CLI able to run a
//! real session without linking a GUI. State that is genuinely visual — the
//! per-view render cache, dock layout, overlays — stays in `seqforge-app`.

pub mod bases;
pub mod edit;
pub mod host;
pub mod resolver;
pub mod workspace;

pub use host::{Host, Level, NullHost};
pub use resolver::WorkspaceResolver;
pub use workspace::{BufferStore, Workspace, display_name, hash_file_bytes};
