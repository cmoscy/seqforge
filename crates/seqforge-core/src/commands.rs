use std::path::PathBuf;

use clap::Subcommand;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{CutSite, Document, SearchHit};

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

// ── Command enums ─────────────────────────────────────────────────────────────

/// Commands that mutate the state of a running GUI instance.
#[derive(Debug, Clone, Subcommand, Serialize, Deserialize)]
pub enum ViewerCommand {
    /// Open a sequence file in the viewer
    Open { path: PathBuf },
    /// Close the current document
    Close,
    /// Navigate to a sequence position (1-based)
    #[command(name = "goto")]
    GoTo { position: usize },
    /// Search for a sequence pattern (IUPAC; forward + reverse complement)
    Find {
        pattern: String,
        #[arg(short, long, default_value = "0")]
        mismatches: u8,
    },
    /// Show restriction cut sites for given enzymes
    Enzymes { enzymes: Vec<String> },
}

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

// ── Command output ────────────────────────────────────────────────────────────

/// Output produced by a dispatch call.
#[derive(Debug, Default)]
pub struct CommandOutput {
    pub messages: Vec<String>,
    pub side_effects: Vec<SideEffect>,
}

impl CommandOutput {
    pub fn message(msg: impl Into<String>) -> Self {
        Self {
            messages: vec![msg.into()],
            side_effects: vec![],
        }
    }

    pub fn effect(effect: SideEffect) -> Self {
        Self {
            messages: vec![],
            side_effects: vec![effect],
        }
    }
}

/// Side effects that the app layer must handle (seqforge-core cannot perform them directly,
/// because seqforge-core must not depend on seqforge-bio).
#[derive(Debug)]
pub enum SideEffect {
    /// App should load this file and set `ViewerState.open_doc`.
    LoadDocument(PathBuf),
    /// App should run an IUPAC pattern search and populate `ViewerState.search_hits`.
    SearchPattern { pattern: String, mismatches: u8 },
    /// App should find restriction sites and populate `ViewerState.cut_sites`.
    ShowEnzymes(Vec<String>),
    /// Viewer should scroll to and highlight this range.
    FocusRange(usize, usize),
    /// A result panel should be opened with this title.
    OpenTab(String),
}

// ── Errors ────────────────────────────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum DispatchError {
    #[error("no document is open")]
    NoDocument,
    #[error("`{0}` is not yet implemented")]
    Unimplemented(&'static str),
}

// ── Dispatch ──────────────────────────────────────────────────────────────────

/// Dispatch a viewer command against mutable viewer state.
///
/// `SideEffect::LoadDocument` must be handled by the caller (seqforge-app),
/// since seqforge-core cannot call seqforge-bio (that would be circular).
pub fn dispatch_viewer(
    state: &mut ViewerState,
    cmd: ViewerCommand,
) -> Result<CommandOutput, DispatchError> {
    match cmd {
        ViewerCommand::Open { path } => {
            Ok(CommandOutput::effect(SideEffect::LoadDocument(path)))
        }

        ViewerCommand::Close => {
            state.open_doc = None;
            state.clear_selection();
            state.scroll_to = None;
            Ok(CommandOutput::message("Document closed."))
        }

        ViewerCommand::GoTo { position } => {
            if state.open_doc.is_none() {
                return Err(DispatchError::NoDocument);
            }
            let idx = position.saturating_sub(1); // 1-based → 0-based
            state.scroll_to = Some(idx);
            state.selection = Some(Selection::cursor(idx));
            state.selected_feature = None;
            Ok(CommandOutput::message(format!("Navigated to position {position}.")))
        }

        ViewerCommand::Find { pattern, mismatches } => {
            if state.open_doc.is_none() {
                return Err(DispatchError::NoDocument);
            }
            if pattern.is_empty() {
                state.search_hits.clear();
                return Ok(CommandOutput::message("Cleared search results."));
            }
            Ok(CommandOutput::effect(SideEffect::SearchPattern { pattern, mismatches }))
        }

        ViewerCommand::Enzymes { enzymes } => {
            if state.open_doc.is_none() {
                return Err(DispatchError::NoDocument);
            }
            if enzymes.is_empty() {
                state.cut_sites.clear();
                state.active_enzymes.clear();
                return Ok(CommandOutput::message("Cleared restriction sites."));
            }
            Ok(CommandOutput::effect(SideEffect::ShowEnzymes(enzymes)))
        }
    }
}

/// Dispatch a file command. Runs entirely in the calling process; no GUI needed.
pub fn dispatch_file(cmd: FileCommand) -> Result<CommandOutput, DispatchError> {
    match cmd {
        // Info is handled directly by seqforge-cli; routing here is a no-op.
        FileCommand::Info { .. } => Ok(CommandOutput::default()),
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
    pub command: ViewerCommand,
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

    #[test]
    fn close_clears_state() {
        let mut state = fixture_state_with_doc();
        state.selection = Some(Selection::range(0, 4));
        let out = dispatch_viewer(&mut state, ViewerCommand::Close).unwrap();
        assert!(state.open_doc.is_none());
        assert!(state.selection.is_none());
        assert!(!out.messages.is_empty());
    }

    #[test]
    fn open_returns_load_side_effect() {
        let mut state = ViewerState::default();
        let path = PathBuf::from("plasmid.gb");
        let out = dispatch_viewer(&mut state, ViewerCommand::Open { path: path.clone() }).unwrap();
        assert!(matches!(
            out.side_effects.first(),
            Some(SideEffect::LoadDocument(p)) if p == &path
        ));
    }

    #[test]
    fn goto_sets_scroll_and_cursor() {
        let mut state = fixture_state_with_doc();
        dispatch_viewer(&mut state, ViewerCommand::GoTo { position: 3 }).unwrap();
        assert_eq!(state.scroll_to, Some(2)); // 0-based
        let sel = state.selection.unwrap();
        assert!(sel.is_cursor());
        assert_eq!(sel.anchor, 2);
    }

    #[test]
    fn goto_fails_without_open_doc() {
        let mut state = ViewerState::default();
        let err = dispatch_viewer(&mut state, ViewerCommand::GoTo { position: 1 }).unwrap_err();
        assert!(matches!(err, DispatchError::NoDocument));
    }

    #[test]
    fn parse_goto_from_terminal_args() {
        let cli = ViewerCli::try_parse_from(["seqforge", "goto", "100"]).unwrap();
        assert!(matches!(cli.command, ViewerCommand::GoTo { position: 100 }));
    }

    #[test]
    fn parse_find_from_terminal_args() {
        let cli = ViewerCli::try_parse_from(["seqforge", "find", "ATGC"]).unwrap();
        assert!(matches!(
            cli.command,
            ViewerCommand::Find { pattern, mismatches: 0 } if pattern == "ATGC"
        ));
    }

    #[test]
    fn parse_enzymes_from_terminal_args() {
        let cli =
            ViewerCli::try_parse_from(["seqforge", "enzymes", "EcoRI", "BamHI"]).unwrap();
        assert!(
            matches!(cli.command, ViewerCommand::Enzymes { ref enzymes } if enzymes == &["EcoRI", "BamHI"])
        );
    }

    #[test]
    fn find_returns_search_side_effect() {
        let mut state = fixture_state_with_doc();
        let out =
            dispatch_viewer(&mut state, ViewerCommand::Find { pattern: "ATGC".into(), mismatches: 0 })
                .unwrap();
        assert!(matches!(
            out.side_effects.first(),
            Some(SideEffect::SearchPattern { pattern, mismatches: 0 }) if pattern == "ATGC"
        ));
    }

    #[test]
    fn enzymes_returns_show_enzymes_side_effect() {
        let mut state = fixture_state_with_doc();
        let out = dispatch_viewer(
            &mut state,
            ViewerCommand::Enzymes { enzymes: vec!["EcoRI".into()] },
        )
        .unwrap();
        assert!(matches!(
            out.side_effects.first(),
            Some(SideEffect::ShowEnzymes(e)) if e == &["EcoRI"]
        ));
    }

    #[test]
    fn find_without_doc_returns_error() {
        let mut state = ViewerState::default();
        let err = dispatch_viewer(
            &mut state,
            ViewerCommand::Find { pattern: "ATGC".into(), mismatches: 0 },
        )
        .unwrap_err();
        assert!(matches!(err, DispatchError::NoDocument));
    }

    #[test]
    fn enzymes_empty_clears_cut_sites() {
        let mut state = fixture_state_with_doc();
        state.cut_sites.push(crate::CutSite {
            enzyme: "EcoRI".into(),
            recognition_start: 0,
            recognition_end: 6,
            cut_pos: 1,
        });
        let out =
            dispatch_viewer(&mut state, ViewerCommand::Enzymes { enzymes: vec![] }).unwrap();
        assert!(state.cut_sites.is_empty());
        assert!(!out.messages.is_empty());
    }
}
