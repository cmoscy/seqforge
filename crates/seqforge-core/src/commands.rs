use std::path::PathBuf;

use clap::Subcommand;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{CutSite, Document, SearchHit, Topology};

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

// ── Viewer state ──────────────────────────────────────────────────────────────

/// Pure viewer/document state — no GUI deps.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct ViewerState {
    /// Currently open document. Skipped in persistence; restored via recent files (Phase 8).
    #[serde(skip)]
    pub open_doc: Option<Document>,
    /// Cursor position or selected range. `None` = nothing selected.
    pub selection: Option<Selection>,
    /// Index into `open_doc.features` for the feature-bar selection.
    pub selected_feature: Option<usize>,
    /// If set, the viewer should scroll to bring this position into view.
    #[serde(skip)]
    pub scroll_to: Option<usize>,
    /// Active sequence search results. Cleared on new doc load.
    #[serde(skip)]
    pub search_hits: Vec<SearchHit>,
    /// Active restriction site results. Cleared on new doc load.
    #[serde(skip)]
    pub cut_sites: Vec<CutSite>,
    /// Which enzymes are currently shown (mirrors the last `Enzymes` command).
    #[serde(skip)]
    pub active_enzymes: Vec<String>,
}

impl ViewerState {
    pub fn clear_selection(&mut self) {
        self.selection = None;
        self.selected_feature = None;
    }

    pub fn clear_results(&mut self) {
        self.search_hits.clear();
        self.cut_sites.clear();
        self.active_enzymes.clear();
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
    #[error("no document is open")]
    NoDocument,
    #[error("`{0}` is not yet implemented")]
    Unimplemented(&'static str),
    #[error("bio operation failed: {0}")]
    BioError(String),
}

// ── Typed request/response schema ─────────────────────────────────────────────

/// Typed request variants. Serde tag = `"method"` so the JSON wire shape is
/// `{"method":"goto","position":100}` — compatible with JSON-RPC 2.0 framing
/// where method + params are merged into this envelope.
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
    GoTo { position: usize },
    /// Search for a sequence pattern (IUPAC; forward + reverse complement).
    Find {
        pattern: String,
        #[arg(short, long, default_value = "0")]
        #[serde(default)]
        mismatches: u8,
    },
    /// Show restriction cut sites for the given enzymes.
    Enzymes { enzymes: Vec<String> },
}

/// Response returned from `dispatch`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ViewerResponse {
    /// Operation succeeded with no notable output.
    Ok,
    /// Operation succeeded with a human-readable status message.
    Message { text: String },
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

/// Dispatch a `ViewerRequest` against mutable viewer state, calling into `bio`
/// for any operation that requires sequence computation.
pub fn dispatch<B: BioOps>(
    state: &mut ViewerState,
    bio: &B,
    req: ViewerRequest,
) -> Result<ViewerResponse, DispatchError> {
    match req {
        ViewerRequest::Open { path } => {
            let doc = bio.load(&path).map_err(DispatchError::BioError)?;
            state.open_doc = Some(doc);
            state.clear_selection();
            state.clear_results();
            state.scroll_to = None;
            Ok(ViewerResponse::Ok)
        }

        ViewerRequest::Close => {
            state.open_doc = None;
            state.clear_selection();
            state.scroll_to = None;
            Ok(ViewerResponse::Message { text: "Document closed.".into() })
        }

        ViewerRequest::GoTo { position } => {
            if state.open_doc.is_none() {
                return Err(DispatchError::NoDocument);
            }
            let idx = position.saturating_sub(1);
            state.scroll_to = Some(idx);
            state.selection = Some(Selection::cursor(idx));
            state.selected_feature = None;
            Ok(ViewerResponse::Message { text: format!("Navigated to position {position}.") })
        }

        ViewerRequest::Find { pattern, mismatches } => {
            if state.open_doc.is_none() {
                return Err(DispatchError::NoDocument);
            }
            if pattern.is_empty() {
                state.search_hits.clear();
                return Ok(ViewerResponse::Message { text: "Cleared search results.".into() });
            }
            let doc = state.open_doc.as_ref().unwrap();
            let circular = matches!(doc.topology, Topology::Circular);
            let hits = bio.find_matches(&doc.sequence, pattern.as_bytes(), mismatches, circular);
            let count = hits.len();
            if let Some(first) = hits.first() {
                state.scroll_to = Some(first.start);
            }
            state.search_hits = hits;
            Ok(ViewerResponse::Message { text: format!("Found {count} match(es) for '{pattern}'.") })
        }

        ViewerRequest::Enzymes { enzymes } => {
            if state.open_doc.is_none() {
                return Err(DispatchError::NoDocument);
            }
            if enzymes.is_empty() {
                state.cut_sites.clear();
                state.active_enzymes.clear();
                return Ok(ViewerResponse::Message { text: "Cleared restriction sites.".into() });
            }
            let doc = state.open_doc.as_ref().unwrap();
            let circular = matches!(doc.topology, Topology::Circular);
            let enzyme_refs: Vec<&str> = enzymes.iter().map(String::as_str).collect();
            let sites = bio.find_cut_sites(&doc.sequence, &enzyme_refs, circular);
            let count = sites.len();
            state.cut_sites = sites;
            state.active_enzymes = enzymes;
            Ok(ViewerResponse::Message { text: format!("Found {count} restriction site(s).") })
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

// ── CLI wrapper for terminal intercept ───────────────────────────────────────

/// Parser wrapper so `:goto 100` in the terminal can be parsed as a subcommand.
#[derive(Debug, clap::Parser)]
#[command(name = "seqforge")]
pub struct ViewerCli {
    #[command(subcommand)]
    pub command: ViewerRequest,
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    fn fixture_state_with_doc() -> ViewerState {
        ViewerState {
            open_doc: Some(Document {
                name: "test".into(),
                sequence: b"ATGCATGC".to_vec(),
                topology: crate::Topology::Linear,
                features: vec![],
                source_path: None,
            }),
            ..Default::default()
        }
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
                topology: Topology::Linear,
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
        let req = ViewerRequest::GoTo { position: 100 };
        let json = serde_json::to_string(&req).unwrap();
        assert_eq!(json, r#"{"method":"goto","position":100}"#);
        let back: ViewerRequest = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, ViewerRequest::GoTo { position: 100 }));
    }

    #[test]
    fn viewer_request_serde_round_trip_find() {
        let req = ViewerRequest::Find { pattern: "ATGC".into(), mismatches: 2 };
        let json = serde_json::to_string(&req).unwrap();
        let back: ViewerRequest = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, ViewerRequest::Find { ref pattern, mismatches: 2 } if pattern == "ATGC"));
    }

    #[test]
    fn viewer_request_serde_default_mismatches() {
        let json = r#"{"method":"find","pattern":"ATGC"}"#;
        let req: ViewerRequest = serde_json::from_str(json).unwrap();
        assert!(matches!(req, ViewerRequest::Find { mismatches: 0, .. }));
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
        let req = ViewerRequest::Enzymes { enzymes: vec!["EcoRI".into(), "BamHI".into()] };
        let json = serde_json::to_string(&req).unwrap();
        let back: ViewerRequest = serde_json::from_str(&json).unwrap();
        assert!(
            matches!(back, ViewerRequest::Enzymes { ref enzymes } if enzymes == &["EcoRI", "BamHI"])
        );
    }

    // ── dispatch correctness ──────────────────────────────────────────────────

    #[test]
    fn dispatch_open_loads_doc() {
        let mut state = ViewerState::default();
        let resp =
            dispatch(&mut state, &FakeBio::new(), ViewerRequest::Open { path: PathBuf::from("fake.gb") })
                .unwrap();
        assert!(state.open_doc.is_some());
        assert!(matches!(resp, ViewerResponse::Ok));
    }

    #[test]
    fn dispatch_close_clears_doc() {
        let mut state = fixture_state_with_doc();
        state.selection = Some(Selection::range(0, 4));
        let resp = dispatch(&mut state, &FakeBio::new(), ViewerRequest::Close).unwrap();
        assert!(state.open_doc.is_none());
        assert!(state.selection.is_none());
        assert!(matches!(resp, ViewerResponse::Message { .. }));
    }

    #[test]
    fn dispatch_goto_mutates_state() {
        let mut state = fixture_state_with_doc();
        let resp =
            dispatch(&mut state, &FakeBio::new(), ViewerRequest::GoTo { position: 3 }).unwrap();
        assert_eq!(state.scroll_to, Some(2));
        assert!(matches!(state.selection, Some(sel) if sel.anchor == 2 && sel.is_cursor()));
        assert!(matches!(resp, ViewerResponse::Message { .. }));
    }

    #[test]
    fn dispatch_goto_no_doc_returns_error() {
        let mut state = ViewerState::default();
        let err =
            dispatch(&mut state, &FakeBio::new(), ViewerRequest::GoTo { position: 1 }).unwrap_err();
        assert!(matches!(err, DispatchError::NoDocument));
    }

    #[test]
    fn dispatch_find_no_doc_returns_error() {
        let mut state = ViewerState::default();
        let err = dispatch(
            &mut state,
            &FakeBio::new(),
            ViewerRequest::Find { pattern: "ATGC".into(), mismatches: 0 },
        )
        .unwrap_err();
        assert!(matches!(err, DispatchError::NoDocument));
    }

    #[test]
    fn dispatch_enzymes_no_doc_returns_error() {
        let mut state = ViewerState::default();
        let err = dispatch(
            &mut state,
            &FakeBio::new(),
            ViewerRequest::Enzymes { enzymes: vec!["EcoRI".into()] },
        )
        .unwrap_err();
        assert!(matches!(err, DispatchError::NoDocument));
    }

    #[test]
    fn dispatch_find_records_call_args() {
        let mut state = fixture_state_with_doc();
        let bio = FakeBio::new().with_hit(2, 6);
        dispatch(&mut state, &bio, ViewerRequest::Find { pattern: "ATGC".into(), mismatches: 1 })
            .unwrap();
        let calls = bio.find_calls.borrow();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, b"ATGC");
        assert_eq!(calls[0].1, 1);
        assert_eq!(state.scroll_to, Some(2));
        assert_eq!(state.search_hits.len(), 1);
    }

    #[test]
    fn dispatch_enzymes_empty_clears_cut_sites() {
        let mut state = fixture_state_with_doc();
        state.cut_sites.push(crate::CutSite {
            enzyme: "EcoRI".into(),
            recognition_start: 0,
            recognition_end: 6,
            cut_pos: 1,
        });
        let resp =
            dispatch(&mut state, &FakeBio::new(), ViewerRequest::Enzymes { enzymes: vec![] })
                .unwrap();
        assert!(state.cut_sites.is_empty());
        assert!(matches!(resp, ViewerResponse::Message { .. }));
    }

    // ── Terminal CLI parsing ──────────────────────────────────────────────────

    #[test]
    fn parse_goto_from_terminal_args() {
        let cli = ViewerCli::try_parse_from(["seqforge", "goto", "100"]).unwrap();
        assert!(matches!(cli.command, ViewerRequest::GoTo { position: 100 }));
    }

    #[test]
    fn parse_find_from_terminal_args() {
        let cli = ViewerCli::try_parse_from(["seqforge", "find", "ATGC"]).unwrap();
        assert!(matches!(
            cli.command,
            ViewerRequest::Find { ref pattern, mismatches: 0 } if pattern == "ATGC"
        ));
    }

    #[test]
    fn parse_enzymes_from_terminal_args() {
        let cli =
            ViewerCli::try_parse_from(["seqforge", "enzymes", "EcoRI", "BamHI"]).unwrap();
        assert!(
            matches!(cli.command, ViewerRequest::Enzymes { ref enzymes } if enzymes == &["EcoRI", "BamHI"])
        );
    }
}
