use std::path::PathBuf;

use clap::Subcommand;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{Annotations, Buffer, CutSite, Document, SearchHit, View, ViewId};

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
        Self { anchor: pos, focus: pos }
    }

    pub fn range(start: usize, end: usize) -> Self {
        Self { anchor: start, focus: end }
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
    /// Show restriction cut sites for the given enzymes.
    Enzymes {
        enzymes: Vec<String>,
        #[arg(long)]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        view: Option<ViewId>,
    },
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
}

// ── Dispatch ──────────────────────────────────────────────────────────────────

/// Dispatch a **view-scoped** `ViewerRequest` against a mutable [`View`],
/// a read-only [`Buffer`], and mutable [`Annotations`].
///
/// `Open` and `Close` are **workspace-scoped** — they create or destroy
/// views/buffers — and are handled by the caller before invoking
/// `dispatch`. Calling this with `Open` / `Close` panics with a clear
/// message; that path is unreachable when called from `command::apply`.
///
/// Buffer is `&Buffer` (read-only) because nothing in MVP scope mutates
/// the underlying sequence here; Tier 3d (transactional edits) will
/// switch this signature to `&mut Buffer` once the rope and history land.
pub fn dispatch<B: BioOps>(
    view: &mut View,
    buffer: &Buffer,
    _annotations: &mut Annotations,
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
            Ok(ViewerResponse::Navigated { position })
        }

        ViewerRequest::Find { pattern, mismatches, view: _ } => {
            if pattern.is_empty() {
                // Empty pattern is a "clear search" affordance. Drop
                // search hits AND the selection (which was likely
                // pointing at the first hit) so the user lands on a
                // clean state — consistent with `Open` / `Close`.
                // Tier 2 #10.
                view.search_hits.clear();
                view.selection = None;
                return Ok(ViewerResponse::SearchResults { count: 0, hits: vec![] });
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

        ViewerRequest::Enzymes { enzymes, view: _ } => {
            if enzymes.is_empty() {
                view.cut_sites.clear();
                view.active_enzymes.clear();
                return Ok(ViewerResponse::CutSites { count: 0, sites: vec![] });
            }
            let circular = buffer.is_circular();
            let enzyme_refs: Vec<&str> = enzymes.iter().map(String::as_str).collect();
            let sites = bio.find_cut_sites(&buffer.text, &enzyme_refs, circular);
            let count = sites.len();
            view.cut_sites = sites.clone();
            view.active_enzymes = enzymes;
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
            b"TACGTACG".to_vec(),
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
            Self { hits: vec![], sites: vec![], find_calls: std::cell::RefCell::new(vec![]) }
        }
        fn with_hit(mut self, start: usize, end: usize) -> Self {
            self.hits.push(SearchHit { start, end, strand: crate::Strand::Forward });
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
            self.find_calls.borrow_mut().push((pattern.to_vec(), mismatches));
            self.hits.clone()
        }
        fn find_cut_sites(
            &self,
            _seq: &[u8],
            _enzymes: &[&str],
            _circular: bool,
        ) -> Vec<CutSite> {
            self.sites.clone()
        }
    }

    // ── ViewerRequest serde round-trips ───────────────────────────────────────

    #[test]
    fn viewer_request_serde_round_trip_goto() {
        let req = ViewerRequest::GoTo { position: 100, view: None };
        let json = serde_json::to_string(&req).unwrap();
        assert_eq!(json, r#"{"method":"goto","position":100}"#);
        let back: ViewerRequest = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, ViewerRequest::GoTo { position: 100, view: None }));
    }

    #[test]
    fn viewer_request_serde_round_trip_find() {
        let req = ViewerRequest::Find { pattern: "ATGC".into(), mismatches: 2, view: None };
        let json = serde_json::to_string(&req).unwrap();
        let back: ViewerRequest = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, ViewerRequest::Find { ref pattern, mismatches: 2, .. } if pattern == "ATGC"));
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
        let req = ViewerRequest::GoTo { position: 5, view: None };
        let json = serde_json::to_string(&req).unwrap();
        assert!(!json.contains("\"view\""), "view should be omitted when None: {json}");
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
        let req = ViewerRequest::Open { path: PathBuf::from("plasmid.gb") };
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
        let req = ViewerRequest::Enzymes { enzymes: vec!["EcoRI".into(), "BamHI".into()], view: None };
        let json = serde_json::to_string(&req).unwrap();
        let back: ViewerRequest = serde_json::from_str(&json).unwrap();
        assert!(
            matches!(back, ViewerRequest::Enzymes { ref enzymes, .. } if enzymes == &["EcoRI", "BamHI"])
        );
    }

    // ── dispatch correctness ──────────────────────────────────────────────────
    //
    // Open and Close are not dispatch-level operations after the Stage 2.5a
    // model split — they're workspace-scoped (allocate/free buffers and
    // views) and tested in `seqforge_app::workspace::tests`.

    #[test]
    fn dispatch_goto_mutates_view() {
        let (mut view, buf, mut ann) = fixture();
        let resp =
            dispatch(&mut view, &buf, &mut ann, &FakeBio::new(), ViewerRequest::GoTo { position: 3, view: None })
                .unwrap();
        assert_eq!(view.scroll_to, Some(2));
        assert!(matches!(view.selection, Some(sel) if sel.anchor == 2 && sel.is_cursor()));
        assert!(matches!(resp, ViewerResponse::Navigated { position: 3 }));
    }

    #[test]
    fn dispatch_goto_out_of_range_returns_error() {
        let (mut view, buf, mut ann) = fixture(); // seq len = 8
        let err =
            dispatch(&mut view, &buf, &mut ann, &FakeBio::new(), ViewerRequest::GoTo { position: 9, view: None })
                .unwrap_err();
        assert!(matches!(err, DispatchError::OutOfRange { position: 9, seq_len: 8 }));
    }

    #[test]
    fn dispatch_goto_position_zero_returns_error() {
        let (mut view, buf, mut ann) = fixture();
        let err =
            dispatch(&mut view, &buf, &mut ann, &FakeBio::new(), ViewerRequest::GoTo { position: 0, view: None })
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
            ViewerRequest::Find { pattern: "ATGC".into(), mismatches: 1, view: None },
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
            ViewerRequest::Enzymes { enzymes: vec![], view: None },
        )
        .unwrap();
        assert!(view.cut_sites.is_empty());
        assert!(matches!(resp, ViewerResponse::CutSites { count: 0, .. }));
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
            ViewerRequest::Find { pattern: "ATGC".into(), mismatches: 0, view: None },
        )
        .unwrap();
        assert!(matches!(resp, ViewerResponse::SearchResults { count: 1, .. }));
        if let ViewerResponse::SearchResults { hits, .. } = resp {
            assert_eq!(hits[0].start, 2);
        }
    }

    #[test]
    fn dispatch_find_empty_pattern_clears() {
        let (mut view, buf, mut ann) = fixture();
        view.search_hits.push(SearchHit { start: 0, end: 4, strand: crate::Strand::Forward });
        // Pre-populate a selection so we can verify clear-on-empty behavior.
        view.selection = Some(Selection::range(0, 4));
        let resp = dispatch(
            &mut view,
            &buf,
            &mut ann,
            &FakeBio::new(),
            ViewerRequest::Find { pattern: "".into(), mismatches: 0, view: None },
        )
        .unwrap();
        assert!(view.search_hits.is_empty());
        // Tier 2 #10: empty pattern also drops the selection.
        assert!(view.selection.is_none(), "selection should be cleared on empty Find");
        assert!(matches!(resp, ViewerResponse::SearchResults { count: 0, .. }));
    }
}
