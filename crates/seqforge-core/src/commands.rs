use std::path::PathBuf;

use clap::Subcommand;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{Annotations, Buffer, CutSite, Document, FeatureId, SearchHit, Strand, View, ViewId};

// ── Selection model ───────────────────────────────────────────────────────────

/// A selection or cursor position in the sequence.
///
/// When `anchor == focus` the selection is a **cursor** — rendered as a thin
/// vertical line between bases. When they differ it is a **range**. The anchor
/// is where the user first clicked; the focus tracks the current extent.
/// Shift+click (Phase 8) will extend the focus while keeping the anchor fixed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Selection {
    /// Fixed end — where the interaction began.
    pub anchor: usize,
    /// Moving end — equals `anchor` for a cursor.
    pub focus: usize,
}

impl Selection {
    pub fn cursor(pos: usize) -> Self {
        Self {
            anchor: pos,
            focus: pos,
        }
    }

    pub fn range(start: usize, end: usize) -> Self {
        Self {
            anchor: start,
            focus: end,
        }
    }

    pub fn is_cursor(self) -> bool {
        self.anchor == self.focus
    }

    /// Returns `(start, end)` in ascending order regardless of drag direction.
    pub fn ordered(self) -> (usize, usize) {
        (self.anchor.min(self.focus), self.anchor.max(self.focus))
    }
}

// ── File commands ─────────────────────────────────────────────────────────────

/// Commands that operate on sequence files on disk. No running GUI required.
#[derive(Debug, Clone, Subcommand, Serialize, Deserialize)]
pub enum FileCommand {
    /// Print info about a sequence file
    Info { input: PathBuf },
    /// Digest a sequence with restriction enzymes (post-MVP implementation)
    Digest {
        input: PathBuf,
        #[arg(short, long)]
        enzymes: Vec<String>,
        #[arg(short, long)]
        output: PathBuf,
    },
    /// Annotate a sequence file (post-MVP implementation)
    Annotate {
        input: PathBuf,
        #[arg(short, long)]
        output: PathBuf,
    },
}

// ── Errors ────────────────────────────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum DispatchError {
    /// Operation targeted "the active view" but none is active.
    #[error("no active view")]
    NoActiveView,
    /// Operation targeted a specific view by id but it was not found
    /// (e.g. closed between the agent's enumeration and dispatch).
    #[error("view {0} not found")]
    ViewNotFound(crate::ViewId),
    /// A `RwLock` on the buffer was poisoned by a panicking writer.
    /// Practically never observed in the single-threaded UI path; here for
    /// completeness once background tasks land.
    #[error("buffer lock was poisoned")]
    PoisonedLock,
    #[error("position {position} is out of range (sequence length: {seq_len})")]
    OutOfRange { position: usize, seq_len: usize },
    #[error("`{0}` is not yet implemented")]
    Unimplemented(&'static str),
    #[error("bio operation failed: {0}")]
    BioError(String),
    /// A command argument was malformed (e.g. a non-IUPAC base, an empty
    /// paste, a feature index past the end). Distinct from `OutOfRange`
    /// (sequence-position bounds) and `BioError` (a bio op that ran but failed).
    #[error("invalid input: {0}")]
    InvalidInput(String),
    /// A `Save` was blocked because the file changed on disk since it was
    /// loaded/last saved (external-change guard). CLI/agent callers can retry
    /// with `--force`; the GUI raises an Overwrite/Reload/Cancel modal.
    #[error("file changed on disk since load: {0} (re-run with --force to overwrite)")]
    SaveConflict(String),
}

// ── Typed request/response schema ─────────────────────────────────────────────

/// Typed request variants. Serde tag = `"method"` so the JSON wire shape is
/// `{"method":"goto","position":100}` — compatible with JSON-RPC 2.0 framing
/// where method + params are merged into this envelope.
///
/// **View targeting** (Stage 2.5d). View-scoped variants (`GoTo`,
/// `Find`, `Enzymes`) accept an optional `view: ViewId` field. When
/// `None`, the request operates on the active view (default
/// behaviour). When `Some(vid)`, the request is dispatched against
/// that specific view, returning `DispatchError::ViewNotFound` if the
/// view has been closed since the agent enumerated it. There is
/// intentionally no pane targeting — panes are a layout concept
/// owned by the dock, not addressable identity.
/// How an `Enzymes` request mutates `view.active_enzymes` (the source of
/// truth). The resulting `cut_sites` are always re-derived from the new set
/// via `find_cut_sites`, so all three ops share one rendering path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum EnzymeOp {
    /// Replace the active set with the query's result (the historical
    /// behaviour; empty / `none` / `clear` query thus clears all).
    #[default]
    Set,
    /// Union the query's result into the current active set.
    Add,
    /// Remove the query's result from the current active set.
    Remove,
}

#[derive(Debug, Clone, Serialize, Deserialize, Subcommand)]
#[serde(tag = "method", rename_all = "snake_case")]
pub enum ViewerRequest {
    /// Open a sequence file in the viewer.
    Open { path: PathBuf },
    /// Close the current document.
    Close,
    /// Navigate to a sequence position (1-based).
    #[serde(rename = "goto")]
    #[command(name = "goto")]
    GoTo {
        position: usize,
        #[arg(long)]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        view: Option<ViewId>,
    },
    /// Search for a sequence pattern (IUPAC; forward + reverse complement).
    Find {
        pattern: String,
        #[arg(short, long, default_value = "0")]
        #[serde(default)]
        mismatches: u8,
        #[arg(long)]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        view: Option<ViewId>,
    },
    /// Show restriction cut sites. `query` is a free-text expression accepted
    /// by `seqforge_bio::parse_enzyme_query`: a preset keyword (`unique`,
    /// `unique and dual`, `non-cutters`), `all`, `none`/`clear`, or a
    /// whitespace/comma-separated list of enzyme names.
    Enzymes {
        /// Raw query string. For `set`, empty / `none` / `clear` drops all
        /// sites; for `add` / `remove` it names the enzymes to union/subtract.
        #[arg(default_value = "")]
        query: String,
        /// Set (replace, default), add, or remove against the active set.
        #[arg(long, value_enum, default_value_t = EnzymeOp::Set)]
        #[serde(default)]
        op: EnzymeOp,
        #[arg(long)]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        view: Option<ViewId>,
    },

    // ── Editor write-ops (v0.2) ────────────────────────────────────────────
    //
    // These are **workspace/write-scoped**, like `Open`/`Close`: they are
    // intercepted in the app's `command::apply` `Viewer(req)` arm and routed to
    // `command/edit.rs` → `workspace.edit/undo/redo` (the Phase 11 write path).
    // They never flow through `core::dispatch` (read-lock only); `dispatch`
    // `unreachable!`s on them. Every variant carries an optional `view` so an
    // agent can target a specific buffer; GUI / CLI default to the active view.
    /// Insert bases at a position (0-based).
    Insert {
        pos: usize,
        bases: String,
        #[arg(long)]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        view: Option<ViewId>,
    },
    /// Delete the bases in the half-open range `[start, end)`.
    Delete {
        start: usize,
        end: usize,
        #[arg(long)]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        view: Option<ViewId>,
    },
    /// Replace the bases in `[start, end)` with new bases.
    Replace {
        start: usize,
        end: usize,
        bases: String,
        #[arg(long)]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        view: Option<ViewId>,
    },
    /// Reverse-complement the bases in `[start, end)` in place.
    #[command(visible_alias = "rc")]
    ReverseComplement {
        start: usize,
        end: usize,
        #[arg(long)]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        view: Option<ViewId>,
    },
    /// Cut (copy then delete) the bases in `[start, end)`.
    Cut {
        start: usize,
        end: usize,
        #[arg(long)]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        view: Option<ViewId>,
    },
    /// Copy the bases in `[start, end)` to the clipboard.
    Copy {
        start: usize,
        end: usize,
        #[arg(long)]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        view: Option<ViewId>,
    },
    /// Paste the clipboard contents at a position (0-based).
    Paste {
        pos: usize,
        #[arg(long)]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        view: Option<ViewId>,
    },
    /// Add a feature over the half-open range `[start, end)`.
    AddFeature {
        start: usize,
        end: usize,
        /// GenBank feature-type string (e.g. `CDS`, `misc_feature`).
        #[arg(long)]
        kind: String,
        #[arg(long)]
        label: String,
        /// `+`, `-`, or `.` (unstranded).
        #[arg(long, default_value = "+")]
        #[serde(default = "default_strand")]
        strand: String,
        #[arg(long)]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        view: Option<ViewId>,
    },
    /// List the features on the active buffer (id, kind, label, range, strand).
    /// Ids are session-scoped — use them for `remove-feature`/`rename-feature`.
    ListFeatures {
        #[arg(long)]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        view: Option<ViewId>,
    },
    /// Remove the feature with the given id (from `list-features`).
    RemoveFeature {
        #[arg(long)]
        id: FeatureId,
        #[arg(long)]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        view: Option<ViewId>,
    },
    /// Rename the feature with the given id (from `list-features`).
    RenameFeature {
        #[arg(long)]
        id: FeatureId,
        #[arg(long)]
        label: String,
        #[arg(long)]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        view: Option<ViewId>,
    },
    /// Edit a feature's geometry/type in place: only the fields you pass change.
    /// Addressed by id (from `list-features`); validates `start < end <= len`.
    UpdateFeature {
        #[arg(long)]
        id: FeatureId,
        /// New GenBank feature-type string (e.g. `CDS`, `misc_feature`).
        #[arg(long)]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        kind: Option<String>,
        #[arg(long)]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        /// `+`, `-`, or `.` (unstranded).
        #[arg(long)]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        strand: Option<String>,
        /// New 0-based start of the half-open range.
        #[arg(long)]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        start: Option<usize>,
        /// New 0-based exclusive end of the range.
        #[arg(long)]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        end: Option<usize>,
        #[arg(long)]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        view: Option<ViewId>,
    },
    /// Save the active buffer to its source path.
    Save {
        /// Overwrite even if the file changed on disk since it was loaded
        /// (skips the external-change guard). For non-interactive callers.
        #[arg(long)]
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        force: bool,
        #[arg(long)]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        view: Option<ViewId>,
    },
    /// Save the active buffer to a new path.
    SaveAs {
        path: PathBuf,
        #[arg(long)]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        view: Option<ViewId>,
    },
    /// Undo the last edit on the active buffer.
    Undo {
        #[arg(long)]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        view: Option<ViewId>,
    },
    /// Redo the last undone edit on the active buffer.
    Redo {
        #[arg(long)]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        view: Option<ViewId>,
    },
}

/// Serde default for `AddFeature.strand` (clap supplies it via `default_value`).
fn default_strand() -> String {
    "+".to_string()
}

impl ViewerRequest {
    /// Returns the explicit `view` target if the request carries one;
    /// `None` for `Open` / `Close` (workspace-scoped) and for view-
    /// scoped variants where the field was omitted.
    pub fn target_view(&self) -> Option<ViewId> {
        match self {
            ViewerRequest::GoTo { view, .. } => *view,
            ViewerRequest::Find { view, .. } => *view,
            ViewerRequest::Enzymes { view, .. } => *view,
            ViewerRequest::Insert { view, .. } => *view,
            ViewerRequest::Delete { view, .. } => *view,
            ViewerRequest::Replace { view, .. } => *view,
            ViewerRequest::ReverseComplement { view, .. } => *view,
            ViewerRequest::Cut { view, .. } => *view,
            ViewerRequest::Copy { view, .. } => *view,
            ViewerRequest::Paste { view, .. } => *view,
            ViewerRequest::AddFeature { view, .. } => *view,
            ViewerRequest::ListFeatures { view, .. } => *view,
            ViewerRequest::RemoveFeature { view, .. } => *view,
            ViewerRequest::RenameFeature { view, .. } => *view,
            ViewerRequest::UpdateFeature { view, .. } => *view,
            ViewerRequest::Save { view, .. } => *view,
            ViewerRequest::SaveAs { view, .. } => *view,
            ViewerRequest::Undo { view, .. } => *view,
            ViewerRequest::Redo { view, .. } => *view,
            ViewerRequest::Open { .. } | ViewerRequest::Close => None,
        }
    }
}

/// Response returned from `dispatch`. Each variant carries the data relevant
/// to that command so callers (CLI, agents) can act on it without parsing text.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ViewerResponse {
    /// Open or Close succeeded.
    Ok,
    /// GoTo — 1-based position the viewer navigated to.
    Navigated { position: usize },
    /// Find — all matching hits (empty when the pattern was cleared).
    SearchResults { count: usize, hits: Vec<SearchHit> },
    /// Enzymes — all cut sites found (empty when the enzyme list was cleared).
    CutSites { count: usize, sites: Vec<CutSite> },
    /// An editor write-op (insert/delete/replace/RC/cut/paste/undo/redo/feature)
    /// succeeded; `len` is the buffer length after the edit. `changed` is false
    /// for a no-op Undo/Redo (empty history) so callers can report "nothing to
    /// undo" without it being an error.
    Edited { len: usize, changed: bool },
    /// `AddFeature` — the new feature's session-scoped id (use it to
    /// remove/rename), and the buffer length after the add.
    FeatureAdded { id: FeatureId, len: usize },
    /// `ListFeatures` — every feature on the buffer, in definition order.
    Features { features: Vec<FeatureInfo> },
}

/// A feature summary for `ListFeatures` — a by-value projection so CLI/agent
/// callers get id + location without a live handle into editor state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeatureInfo {
    pub id: FeatureId,
    /// Verbatim GenBank feature-type string (e.g. `CDS`, `misc_feature`).
    pub kind: String,
    pub label: String,
    /// 0-based half-open range `[start, end)`.
    pub start: usize,
    pub end: usize,
    pub strand: Strand,
}

// ── BioOps trait ─────────────────────────────────────────────────────────────

/// Abstraction over biological operations so `seqforge-core` can call them
/// without depending on `seqforge-bio`.
pub trait BioOps {
    fn load(&self, path: &std::path::Path) -> Result<Document, String>;
    fn find_matches(
        &self,
        seq: &[u8],
        pattern: &[u8],
        mismatches: u8,
        circular: bool,
    ) -> Vec<SearchHit>;
    fn find_cut_sites(&self, seq: &[u8], enzymes: &[&str], circular: bool) -> Vec<CutSite>;
    /// Resolve a free-text enzyme query to a list of **canonical** enzyme
    /// names (presets resolved against the sequence; explicit names mapped to
    /// their canonical spelling; unknown names dropped; empty for clear).
    ///
    /// This is names-only: the dispatcher combines the result with the view's
    /// current set per `EnzymeOp`, then re-derives `cut_sites` via
    /// `find_cut_sites`. Grammar lives in `seqforge_bio::parse_enzyme_query`;
    /// this trait method is the seqforge-core seam so the dispatcher can call
    /// it without depending on seqforge-bio.
    fn resolve_enzyme_names(&self, seq: &[u8], query: &str, circular: bool) -> Vec<String>;
}

/// Union `add` into `base`, preserving order and skipping case-insensitive
/// duplicates. Canonical names mean exact matches in practice; the
/// case-insensitive guard is belt-and-suspenders.
fn union_names(base: &[String], add: &[String]) -> Vec<String> {
    let mut out = base.to_vec();
    for name in add {
        if !out.iter().any(|n| n.eq_ignore_ascii_case(name)) {
            out.push(name.clone());
        }
    }
    out
}

/// `base` minus any name appearing in `remove` (case-insensitive).
fn difference_names(base: &[String], remove: &[String]) -> Vec<String> {
    base.iter()
        .filter(|n| !remove.iter().any(|r| r.eq_ignore_ascii_case(n)))
        .cloned()
        .collect()
}

// ── Dispatch ──────────────────────────────────────────────────────────────────

/// Dispatch a **view-scoped** `ViewerRequest` against a mutable [`View`],
/// a read-only [`Buffer`], and mutable [`Annotations`].
///
/// `Open`/`Close` and the **editor write-ops** (`Insert`, `Delete`,
/// `Replace`, `ReverseComplement`, `Cut`, `Copy`, `Paste`, `AddFeature`,
/// `RemoveFeature`, `RenameFeature`, `Save`, `SaveAs`, `Undo`, `Redo`) are
/// **workspace/write-scoped** — they allocate/free views or mutate the buffer
/// through history — and are handled by the caller (`command::apply`'s
/// `Viewer(req)` arm → `command/edit.rs` → `workspace.edit/undo/redo`) before
/// invoking `dispatch`. Calling `dispatch` with any of them panics with a
/// clear message; that path is unreachable from `command::apply`.
///
/// Buffer stays `&Buffer` (read-only): the read-scoped requests handled here
/// (`GoTo`/`Find`/`Enzymes`) never mutate the sequence. Editor mutation does
/// not widen this signature — it lives on the Phase 11 `workspace.edit` path
/// (which owns the `BufferStore` history), not here.
pub fn dispatch<B: BioOps>(
    view: &mut View,
    buffer: &Buffer,
    annotations: &mut Annotations,
    bio: &B,
    req: ViewerRequest,
) -> Result<ViewerResponse, DispatchError> {
    match req {
        ViewerRequest::Open { .. } | ViewerRequest::Close => {
            unreachable!(
                "Open/Close are workspace-scoped; the caller must handle them \
                 before invoking dispatch (see command::apply)"
            )
        }

        // Editor write-ops are intercepted in `command::apply`'s `Viewer(req)`
        // arm and routed to `command/edit.rs` (the Phase 11 write path); they
        // never reach `core::dispatch`. Listed explicitly so adding a future
        // write-op forces a compile error here rather than silently falling
        // through.
        ViewerRequest::Insert { .. }
        | ViewerRequest::Delete { .. }
        | ViewerRequest::Replace { .. }
        | ViewerRequest::ReverseComplement { .. }
        | ViewerRequest::Cut { .. }
        | ViewerRequest::Copy { .. }
        | ViewerRequest::Paste { .. }
        | ViewerRequest::AddFeature { .. }
        | ViewerRequest::RemoveFeature { .. }
        | ViewerRequest::RenameFeature { .. }
        | ViewerRequest::UpdateFeature { .. }
        | ViewerRequest::Save { .. }
        | ViewerRequest::SaveAs { .. }
        | ViewerRequest::Undo { .. }
        | ViewerRequest::Redo { .. } => {
            unreachable!(
                "editor write-ops are workspace-scoped; the caller routes them \
                 to command/edit.rs before invoking dispatch (see command::apply)"
            )
        }

        // Note: `view` targeting is handled by the caller before this
        // function is invoked. `dispatch` always operates on whatever
        // (View, Buffer) was passed in.
        ViewerRequest::GoTo { position, view: _ } => {
            let seq_len = buffer.len();
            if position == 0 || position > seq_len {
                return Err(DispatchError::OutOfRange { position, seq_len });
            }
            let idx = position - 1;
            view.scroll_to = Some(idx);
            view.selection = Some(Selection::cursor(idx));
            view.selected_feature = None;
            view.selected_primer = None;
            Ok(ViewerResponse::Navigated { position })
        }

        ViewerRequest::Find {
            pattern,
            mismatches,
            view: _,
        } => {
            if pattern.is_empty() {
                // Empty pattern is a "clear search" affordance. Drop
                // search hits AND the selection (which was likely
                // pointing at the first hit) so the user lands on a
                // clean state — consistent with `Open` / `Close`.
                // Tier 2 #10.
                view.search_hits.clear();
                view.selection = None;
                return Ok(ViewerResponse::SearchResults {
                    count: 0,
                    hits: vec![],
                });
            }
            let circular = buffer.is_circular();
            let hits = bio.find_matches(&buffer.text, pattern.as_bytes(), mismatches, circular);
            let count = hits.len();
            if let Some(first) = hits.first() {
                view.scroll_to = Some(first.start);
                view.selection = Some(Selection::range(first.start, first.end));
            }
            view.search_hits = hits.clone();
            Ok(ViewerResponse::SearchResults { count, hits })
        }

        // Read-op: features are addressed by id, so surface the live id table
        // for CLI/agent callers. Rides `dispatch` (read-only, no history).
        ViewerRequest::ListFeatures { view: _ } => {
            let features = annotations
                .iter()
                .map(|f| FeatureInfo {
                    id: f.id,
                    kind: f.raw_kind.clone(),
                    label: f.label.clone(),
                    start: f.range.start,
                    end: f.range.end,
                    strand: f.strand,
                })
                .collect();
            Ok(ViewerResponse::Features { features })
        }

        ViewerRequest::Enzymes { query, op, view: _ } => {
            let circular = buffer.is_circular();
            // active_enzymes is the source of truth; the op mutates it and
            // cut_sites is always re-derived through the single scanner.
            let resolved = bio.resolve_enzyme_names(&buffer.text, &query, circular);
            let new_set = match op {
                EnzymeOp::Set => resolved,
                EnzymeOp::Add => union_names(&view.active_enzymes, &resolved),
                EnzymeOp::Remove => difference_names(&view.active_enzymes, &resolved),
            };
            let refs: Vec<&str> = new_set.iter().map(String::as_str).collect();
            let sites = bio.find_cut_sites(&buffer.text, &refs, circular);
            let count = sites.len();
            view.active_enzymes = new_set;
            view.cut_sites = sites.clone();
            Ok(ViewerResponse::CutSites { count, sites })
        }
    }
}

/// Dispatch a file command. Runs entirely in the calling process; no GUI needed.
pub fn dispatch_file(cmd: FileCommand) -> Result<(), DispatchError> {
    match cmd {
        FileCommand::Info { .. } => Ok(()), // handled directly by seqforge-cli
        FileCommand::Digest { .. } => Err(DispatchError::Unimplemented("digest")),
        FileCommand::Annotate { .. } => Err(DispatchError::Unimplemented("annotate")),
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    use crate::{BufferId, ViewId, ViewKind};

    /// Build a (View, Buffer, Annotations) triple for dispatch tests.
    /// Buffer text is `ATGCATGC` (length 8), no features.
    fn fixture() -> (View, Buffer, Annotations) {
        let buffer = Buffer::new(
            "test".into(),
            None,
            b"ATGCATGC".to_vec(),
            crate::Topology::Linear,
        );
        let view = View::new(ViewId(1), BufferId(1), ViewKind::TextView);
        (view, buffer, Annotations::default())
    }

    // ── FakeBio ───────────────────────────────────────────────────────────────

    struct FakeBio {
        hits: Vec<SearchHit>,
        sites: Vec<CutSite>,
        find_calls: std::cell::RefCell<Vec<(Vec<u8>, u8)>>,
    }

    impl FakeBio {
        fn new() -> Self {
            Self {
                hits: vec![],
                sites: vec![],
                find_calls: std::cell::RefCell::new(vec![]),
            }
        }
        fn with_hit(mut self, start: usize, end: usize) -> Self {
            self.hits.push(SearchHit {
                start,
                end,
                strand: crate::Strand::Forward,
            });
            self
        }
    }

    impl BioOps for FakeBio {
        fn load(&self, _path: &std::path::Path) -> Result<Document, String> {
            Ok(Document {
                name: "fake".into(),
                sequence: b"ATGCATGC".to_vec(),
                topology: crate::Topology::Linear,
                features: vec![],
                source_path: None,
            })
        }
        fn find_matches(
            &self,
            _seq: &[u8],
            pattern: &[u8],
            mismatches: u8,
            _circular: bool,
        ) -> Vec<SearchHit> {
            self.find_calls
                .borrow_mut()
                .push((pattern.to_vec(), mismatches));
            self.hits.clone()
        }
        fn find_cut_sites(&self, _seq: &[u8], enzymes: &[&str], _circular: bool) -> Vec<CutSite> {
            // Honour the enzyme list so the dispatch's re-derive is testable:
            // an empty set yields no sites; any non-empty set echoes the stub.
            if enzymes.is_empty() {
                vec![]
            } else {
                self.sites.clone()
            }
        }
        fn resolve_enzyme_names(&self, _seq: &[u8], query: &str, _circular: bool) -> Vec<String> {
            if query.trim().is_empty()
                || query.eq_ignore_ascii_case("none")
                || query.eq_ignore_ascii_case("clear")
            {
                return Vec::new();
            }
            // Stub: treat any non-empty query as a verbatim name list.
            query
                .split(|c: char| c.is_whitespace() || c == ',')
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
                .collect()
        }
    }

    // ── ViewerRequest serde round-trips ───────────────────────────────────────

    #[test]
    fn viewer_request_serde_round_trip_goto() {
        let req = ViewerRequest::GoTo {
            position: 100,
            view: None,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert_eq!(json, r#"{"method":"goto","position":100}"#);
        let back: ViewerRequest = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            back,
            ViewerRequest::GoTo {
                position: 100,
                view: None
            }
        ));
    }

    #[test]
    fn viewer_request_serde_round_trip_find() {
        let req = ViewerRequest::Find {
            pattern: "ATGC".into(),
            mismatches: 2,
            view: None,
        };
        let json = serde_json::to_string(&req).unwrap();
        let back: ViewerRequest = serde_json::from_str(&json).unwrap();
        assert!(
            matches!(back, ViewerRequest::Find { ref pattern, mismatches: 2, .. } if pattern == "ATGC")
        );
    }

    #[test]
    fn viewer_request_serde_default_mismatches() {
        let json = r#"{"method":"find","pattern":"ATGC"}"#;
        let req: ViewerRequest = serde_json::from_str(json).unwrap();
        assert!(matches!(req, ViewerRequest::Find { mismatches: 0, .. }));
    }

    #[test]
    fn viewer_request_view_field_default_omitted() {
        // Stage 2.5d: `view` is optional and skip-serialized when None,
        // so the wire format stays clean for the common case (operate
        // on active view). Backwards compatible with pre-2.5d clients.
        let req = ViewerRequest::GoTo {
            position: 5,
            view: None,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(
            !json.contains("\"view\""),
            "view should be omitted when None: {json}"
        );
    }

    #[test]
    fn viewer_request_view_field_round_trip() {
        let req = ViewerRequest::GoTo {
            position: 5,
            view: Some(crate::ViewId(17)),
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"view\":17"));
        let back: ViewerRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.target_view(), Some(crate::ViewId(17)));
    }

    #[test]
    fn target_view_extracts_explicit_id() {
        let r = ViewerRequest::Find {
            pattern: "AT".into(),
            mismatches: 0,
            view: Some(crate::ViewId(42)),
        };
        assert_eq!(r.target_view(), Some(crate::ViewId(42)));
    }

    #[test]
    fn target_view_workspace_scoped_variants_return_none() {
        let close = ViewerRequest::Close;
        assert_eq!(close.target_view(), None);
        let open = ViewerRequest::Open {
            path: std::path::PathBuf::from("/x"),
        };
        assert_eq!(open.target_view(), None);
    }

    #[test]
    fn viewer_request_serde_round_trip_open() {
        let req = ViewerRequest::Open {
            path: PathBuf::from("plasmid.gb"),
        };
        let json = serde_json::to_string(&req).unwrap();
        let back: ViewerRequest = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, ViewerRequest::Open { .. }));
    }

    #[test]
    fn viewer_request_serde_round_trip_close() {
        let req = ViewerRequest::Close;
        let json = serde_json::to_string(&req).unwrap();
        assert_eq!(json, r#"{"method":"close"}"#);
        let back: ViewerRequest = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, ViewerRequest::Close));
    }

    #[test]
    fn viewer_request_serde_round_trip_enzymes() {
        let req = ViewerRequest::Enzymes {
            query: "EcoRI BamHI".into(),
            op: EnzymeOp::Set,
            view: None,
        };
        let json = serde_json::to_string(&req).unwrap();
        let back: ViewerRequest = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, ViewerRequest::Enzymes { ref query, .. } if query == "EcoRI BamHI"));
    }

    // ── Editor write-op serde round-trips (v0.2) ──────────────────────────────

    #[test]
    fn viewer_request_serde_round_trip_insert() {
        let req = ViewerRequest::Insert {
            pos: 10,
            bases: "ATG".into(),
            view: None,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert_eq!(json, r#"{"method":"insert","pos":10,"bases":"ATG"}"#);
        let back: ViewerRequest = serde_json::from_str(&json).unwrap();
        assert!(
            matches!(back, ViewerRequest::Insert { pos: 10, ref bases, view: None } if bases == "ATG")
        );
    }

    #[test]
    fn viewer_request_serde_round_trip_delete() {
        let req = ViewerRequest::Delete {
            start: 5,
            end: 9,
            view: None,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert_eq!(json, r#"{"method":"delete","start":5,"end":9}"#);
        let back: ViewerRequest = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            back,
            ViewerRequest::Delete {
                start: 5,
                end: 9,
                view: None
            }
        ));
    }

    #[test]
    fn viewer_request_serde_round_trip_reverse_complement() {
        let req = ViewerRequest::ReverseComplement {
            start: 0,
            end: 4,
            view: None,
        };
        let json = serde_json::to_string(&req).unwrap();
        // snake_case method tag derived from the variant name.
        assert_eq!(json, r#"{"method":"reverse_complement","start":0,"end":4}"#);
        let back: ViewerRequest = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            back,
            ViewerRequest::ReverseComplement {
                start: 0,
                end: 4,
                ..
            }
        ));
    }

    #[test]
    fn viewer_request_serde_add_feature_strand_defaults() {
        // strand omitted on the wire → defaults to "+".
        let json = r#"{"method":"add_feature","start":0,"end":9,"kind":"CDS","label":"gene"}"#;
        let req: ViewerRequest = serde_json::from_str(json).unwrap();
        assert!(matches!(
            req,
            ViewerRequest::AddFeature { ref kind, ref label, ref strand, .. }
            if kind == "CDS" && label == "gene" && strand == "+"
        ));
    }

    #[test]
    fn viewer_request_serde_round_trip_undo_save() {
        for (req, tag) in [
            (ViewerRequest::Undo { view: None }, "undo"),
            (
                ViewerRequest::Save {
                    force: false,
                    view: None,
                },
                "save",
            ),
        ] {
            let json = serde_json::to_string(&req).unwrap();
            assert_eq!(json, format!(r#"{{"method":"{tag}"}}"#));
            let back: ViewerRequest = serde_json::from_str(&json).unwrap();
            assert_eq!(back.target_view(), None);
        }
    }

    #[test]
    fn viewer_request_editor_view_target_round_trips() {
        let req = ViewerRequest::Insert {
            pos: 3,
            bases: "C".into(),
            view: Some(crate::ViewId(7)),
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"view\":7"));
        let back: ViewerRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.target_view(), Some(crate::ViewId(7)));
    }

    #[test]
    fn viewer_request_serde_round_trip_enzymes_preset() {
        let req = ViewerRequest::Enzymes {
            query: "unique".into(),
            op: EnzymeOp::Set,
            view: None,
        };
        let json = serde_json::to_string(&req).unwrap();
        let back: ViewerRequest = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, ViewerRequest::Enzymes { ref query, .. } if query == "unique"));
    }

    // ── dispatch correctness ──────────────────────────────────────────────────
    //
    // Open and Close are not dispatch-level operations after the Stage 2.5a
    // model split — they're workspace-scoped (allocate/free buffers and
    // views) and tested in `seqforge_app::workspace::tests`.

    #[test]
    fn dispatch_goto_mutates_view() {
        let (mut view, buf, mut ann) = fixture();
        let resp = dispatch(
            &mut view,
            &buf,
            &mut ann,
            &FakeBio::new(),
            ViewerRequest::GoTo {
                position: 3,
                view: None,
            },
        )
        .unwrap();
        assert_eq!(view.scroll_to, Some(2));
        assert!(matches!(view.selection, Some(sel) if sel.anchor == 2 && sel.is_cursor()));
        assert!(matches!(resp, ViewerResponse::Navigated { position: 3 }));
    }

    #[test]
    fn dispatch_goto_out_of_range_returns_error() {
        let (mut view, buf, mut ann) = fixture(); // seq len = 8
        let err = dispatch(
            &mut view,
            &buf,
            &mut ann,
            &FakeBio::new(),
            ViewerRequest::GoTo {
                position: 9,
                view: None,
            },
        )
        .unwrap_err();
        assert!(matches!(
            err,
            DispatchError::OutOfRange {
                position: 9,
                seq_len: 8
            }
        ));
    }

    #[test]
    fn dispatch_list_features_returns_id_table() {
        let (mut view, buf, _) = fixture();
        let mut ann = Annotations::new(vec![crate::Feature {
            id: Default::default(),
            range: 1..4,
            raw_kind: "CDS".into(),
            label: "gene".into(),
            strand: crate::Strand::Forward,
            qualifiers: Default::default(),
            provenance: None,
        }]);
        let minted = ann.iter().next().unwrap().id;
        let resp = dispatch(
            &mut view,
            &buf,
            &mut ann,
            &FakeBio::new(),
            ViewerRequest::ListFeatures { view: None },
        )
        .unwrap();
        match resp {
            ViewerResponse::Features { features } => {
                assert_eq!(features.len(), 1);
                assert_eq!(features[0].id, minted);
                assert_eq!(features[0].kind, "CDS");
                assert_eq!((features[0].start, features[0].end), (1, 4));
            }
            other => panic!("expected Features, got {other:?}"),
        }
    }

    #[test]
    fn remove_feature_request_serde_round_trips_id() {
        let req = ViewerRequest::RemoveFeature {
            id: FeatureId(42),
            view: None,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains(r#""id":42"#), "got {json}");
        let back: ViewerRequest = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            back,
            ViewerRequest::RemoveFeature {
                id: FeatureId(42),
                ..
            }
        ));
    }

    #[test]
    fn dispatch_goto_position_zero_returns_error() {
        let (mut view, buf, mut ann) = fixture();
        let err = dispatch(
            &mut view,
            &buf,
            &mut ann,
            &FakeBio::new(),
            ViewerRequest::GoTo {
                position: 0,
                view: None,
            },
        )
        .unwrap_err();
        assert!(matches!(err, DispatchError::OutOfRange { position: 0, .. }));
    }

    #[test]
    fn dispatch_find_records_call_args() {
        let (mut view, buf, mut ann) = fixture();
        let bio = FakeBio::new().with_hit(2, 6);
        dispatch(
            &mut view,
            &buf,
            &mut ann,
            &bio,
            ViewerRequest::Find {
                pattern: "ATGC".into(),
                mismatches: 1,
                view: None,
            },
        )
        .unwrap();
        let calls = bio.find_calls.borrow();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, b"ATGC");
        assert_eq!(calls[0].1, 1);
        assert_eq!(view.scroll_to, Some(2));
        assert_eq!(view.search_hits.len(), 1);
    }

    #[test]
    fn dispatch_enzymes_empty_clears_cut_sites() {
        let (mut view, buf, mut ann) = fixture();
        view.cut_sites.push(crate::CutSite {
            enzyme: "EcoRI".into(),
            recognition: "GAATTC".into(),
            recognition_start: 0,
            recognition_end: 6,
            cut_pos: 1,
            bottom_cut_pos: 5,
        });
        let resp = dispatch(
            &mut view,
            &buf,
            &mut ann,
            &FakeBio::new(),
            ViewerRequest::Enzymes {
                query: String::new(),
                op: EnzymeOp::Set,
                view: None,
            },
        )
        .unwrap();
        assert!(view.cut_sites.is_empty());
        assert!(view.active_enzymes.is_empty());
        assert!(matches!(resp, ViewerResponse::CutSites { count: 0, .. }));
    }

    /// Run an enzyme op against `view`, returning nothing — callers assert on
    /// `view.active_enzymes` afterward.
    fn enzyme_op(view: &mut View, buf: &Buffer, ann: &mut Annotations, query: &str, op: EnzymeOp) {
        dispatch(
            view,
            buf,
            ann,
            &FakeBio::new(),
            ViewerRequest::Enzymes {
                query: query.into(),
                op,
                view: None,
            },
        )
        .unwrap();
    }

    #[test]
    fn dispatch_enzymes_add_unions_into_active_set() {
        let (mut view, buf, mut ann) = fixture();
        enzyme_op(&mut view, &buf, &mut ann, "EcoRI", EnzymeOp::Set);
        enzyme_op(&mut view, &buf, &mut ann, "BamHI", EnzymeOp::Add);
        assert_eq!(
            view.active_enzymes,
            vec!["EcoRI".to_string(), "BamHI".to_string()]
        );
    }

    #[test]
    fn dispatch_enzymes_add_is_idempotent_case_insensitive() {
        let (mut view, buf, mut ann) = fixture();
        enzyme_op(&mut view, &buf, &mut ann, "EcoRI", EnzymeOp::Set);
        enzyme_op(&mut view, &buf, &mut ann, "ecori", EnzymeOp::Add);
        assert_eq!(view.active_enzymes, vec!["EcoRI".to_string()]);
    }

    #[test]
    fn dispatch_enzymes_remove_subtracts_by_name() {
        let (mut view, buf, mut ann) = fixture();
        enzyme_op(&mut view, &buf, &mut ann, "EcoRI BamHI", EnzymeOp::Set);
        enzyme_op(&mut view, &buf, &mut ann, "EcoRI", EnzymeOp::Remove);
        assert_eq!(view.active_enzymes, vec!["BamHI".to_string()]);
    }

    #[test]
    fn dispatch_find_returns_search_results() {
        let (mut view, buf, mut ann) = fixture();
        let bio = FakeBio::new().with_hit(2, 6);
        let resp = dispatch(
            &mut view,
            &buf,
            &mut ann,
            &bio,
            ViewerRequest::Find {
                pattern: "ATGC".into(),
                mismatches: 0,
                view: None,
            },
        )
        .unwrap();
        assert!(matches!(
            resp,
            ViewerResponse::SearchResults { count: 1, .. }
        ));
        if let ViewerResponse::SearchResults { hits, .. } = resp {
            assert_eq!(hits[0].start, 2);
        }
    }

    #[test]
    fn dispatch_find_empty_pattern_clears() {
        let (mut view, buf, mut ann) = fixture();
        view.search_hits.push(SearchHit {
            start: 0,
            end: 4,
            strand: crate::Strand::Forward,
        });
        // Pre-populate a selection so we can verify clear-on-empty behavior.
        view.selection = Some(Selection::range(0, 4));
        let resp = dispatch(
            &mut view,
            &buf,
            &mut ann,
            &FakeBio::new(),
            ViewerRequest::Find {
                pattern: "".into(),
                mismatches: 0,
                view: None,
            },
        )
        .unwrap();
        assert!(view.search_hits.is_empty());
        // Tier 2 #10: empty pattern also drops the selection.
        assert!(
            view.selection.is_none(),
            "selection should be cleared on empty Find"
        );
        assert!(matches!(
            resp,
            ViewerResponse::SearchResults { count: 0, .. }
        ));
    }
}
