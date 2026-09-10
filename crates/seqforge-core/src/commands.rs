use std::path::PathBuf;

use clap::Subcommand;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::span::Span;
use crate::{
    Annotations, Buffer, CutSite, Document, FeatureId, MethylContext, MethylState, Primer,
    PrimerId, SearchHit, Selection, Strand, Topology, View, ViewId, ViewSelection,
};

fn default_methyl_dam() -> bool {
    true
}

fn default_methyl_dcm() -> bool {
    true
}

// ── File commands ─────────────────────────────────────────────────────────────

// ── Document target ───────────────────────────────────────────────────────────

/// Which document a request addresses.
///
/// Every verb operates on exactly one document, and there are three ways to say
/// which: the session's active view (the default, and the only one that existed
/// before), a specific open view, or a file on disk. Routing follows this —
/// a request naming only a path can run in the calling process, one naming
/// session state must reach the session that owns it (ROADMAP decision 27).
///
/// Two `Option`s rather than an enum because `clap` cannot flatten an enum into
/// a subcommand's arguments. The illegal state (both set) is rejected by `clap`
/// via `conflicts_with` on the command line and by [`Target::kind`] on the wire,
/// so it cannot reach a handler either way.
///
/// Flattened in both derives, so `{"method":"goto","view":3}` is unchanged on
/// the wire and `--in` is purely additive.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, clap::Args)]
pub struct Target {
    /// Operate on this open document (a `ViewId` from `buffers`).
    #[arg(long)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub view: Option<ViewId>,
    /// Operate on this file instead of a session document. The file is opened
    /// into the workspace; in a GUI that reuses an already-open buffer.
    #[arg(long = "in", value_name = "PATH", conflicts_with = "view")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
}

/// The resolved form of a [`Target`] — what the two `Option`s actually mean.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetKind<'a> {
    Active,
    View(ViewId),
    Path(&'a std::path::Path),
}

impl Target {
    /// Address the session's active view.
    pub fn active() -> Self {
        Self::default()
    }

    /// Address a specific open view.
    pub fn view(id: ViewId) -> Self {
        Self {
            view: Some(id),
            path: None,
        }
    }

    /// Address a file on disk.
    pub fn path(p: impl Into<PathBuf>) -> Self {
        Self {
            view: None,
            path: Some(p.into()),
        }
    }

    /// What this target means. Errors if both fields are set — `clap` prevents
    /// that on the command line, but a hand-written socket payload could carry
    /// both, and silently preferring one would be a wrong answer.
    pub fn kind(&self) -> Result<TargetKind<'_>, DispatchError> {
        match (self.view, &self.path) {
            (Some(_), Some(_)) => Err(DispatchError::InvalidInput(
                "a request names both --view and --in; they are mutually exclusive".into(),
            )),
            (Some(v), None) => Ok(TargetKind::View(v)),
            (None, Some(p)) => Ok(TargetKind::Path(p)),
            (None, None) => Ok(TargetKind::Active),
        }
    }

    /// Whether this target can be served without a running session.
    pub fn is_path(&self) -> bool {
        self.view.is_none() && self.path.is_some()
    }
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

fn default_frame() -> usize {
    1
}
fn default_min_aa() -> usize {
    30
}

#[derive(Debug, Clone, Serialize, Deserialize, Subcommand)]
#[serde(tag = "method", rename_all = "snake_case")]
pub enum ViewerRequest {
    /// Open a sequence file in the viewer.
    Open { path: PathBuf },
    /// Close the current document.
    Close,
    /// List the open documents (index, path, dirty, active).
    Buffers,
    /// Focus an open document by handle — a 1-based index (from `buffers`) or a
    /// path / file basename. The GUI equivalent is clicking a document tab.
    Focus { target: String },
    /// Navigate to a sequence position (1-based).
    #[serde(rename = "goto")]
    #[command(name = "goto")]
    GoTo {
        position: usize,
        #[command(flatten)]
        #[serde(flatten)]
        target: Target,
    },
    /// Search for a sequence pattern (IUPAC; forward + reverse complement).
    Find {
        pattern: String,
        #[arg(short, long, default_value = "0")]
        #[serde(default)]
        mismatches: u8,
        #[command(flatten)]
        #[serde(flatten)]
        target: Target,
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
        #[command(flatten)]
        #[serde(flatten)]
        target: Target,
        /// Dam methylation active on this molecule (default: on — standard *E. coli* prep).
        #[arg(long, default_value_t = true)]
        #[serde(default = "default_methyl_dam")]
        dam: bool,
        /// Dcm methylation active on this molecule (default: on).
        #[arg(long, default_value_t = true)]
        #[serde(default = "default_methyl_dcm")]
        dcm: bool,
        /// CpG methylation active on this molecule (default: off).
        #[arg(long, default_value_t = false)]
        #[serde(default)]
        cpg: bool,
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
        #[command(flatten)]
        #[serde(flatten)]
        target: Target,
    },
    // The linear edit-command boundary (`plans/span.md` P5d): `Delete` /
    // `Replace` / `ReverseComplement` / `Cut` / `Copy` take `start, end` — these
    // are *transient parameters to a linear splice*, not a stored region identity,
    // so they are a deliberate `Range`-tier survivor per the three-tier rule
    // (`docs/architecture.md`), NOT drift. Cross-origin copy is still expressible
    // in the GUI (a wrapping selection recovered in `extract_region`, which passes
    // a `Span` to `transport::extract`); a headless wrapping cut/copy is a
    // deferrable feature, addable later without disturbing this boundary.
    /// Delete the bases in the half-open range `[start, end)`.
    Delete {
        start: usize,
        end: usize,
        #[command(flatten)]
        #[serde(flatten)]
        target: Target,
    },
    /// Replace the bases in `[start, end)` with new bases.
    Replace {
        start: usize,
        end: usize,
        bases: String,
        #[command(flatten)]
        #[serde(flatten)]
        target: Target,
    },
    /// Reverse-complement the bases in `[start, end)` in place.
    #[command(visible_alias = "rc")]
    ReverseComplement {
        start: usize,
        end: usize,
        #[command(flatten)]
        #[serde(flatten)]
        target: Target,
    },
    /// Cut (copy then delete) the bases in `[start, end)`.
    Cut {
        start: usize,
        end: usize,
        #[command(flatten)]
        #[serde(flatten)]
        target: Target,
    },
    /// Copy the bases in `[start, end)` to the clipboard.
    Copy {
        start: usize,
        end: usize,
        #[command(flatten)]
        #[serde(flatten)]
        target: Target,
    },
    /// Paste the clipboard contents at a position (0-based).
    Paste {
        pos: usize,
        #[command(flatten)]
        #[serde(flatten)]
        target: Target,
    },
    /// Create a new empty in-memory buffer (not backed by a file) and open it.
    New {
        /// Create it circular (default: linear).
        #[arg(long)]
        #[serde(default)]
        circular: bool,
        /// Optional buffer name (defaults to `untitled`).
        #[arg(long)]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    /// Set the origin of a **circular** molecule: rotate so `index` (0-based)
    /// becomes position 0. Topology is unchanged; a feature crossing the new
    /// origin becomes a single wrapping span.
    ///
    /// Give either `index` or `--feature`; a feature label must match exactly
    /// one feature, and its start becomes the new origin. The label form is
    /// what makes the origin reproducible across a batch of related plasmids —
    /// the same landmark rather than the same number.
    SetOrigin {
        #[arg(required_unless_present = "feature", conflicts_with = "feature")]
        index: Option<usize>,
        /// Rotate to the start of the single feature with this label.
        #[arg(long)]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        feature: Option<String>,
        #[command(flatten)]
        #[serde(flatten)]
        target: Target,
    },
    /// Linearize a **circular** molecule, cutting at `at` (default: position 0).
    /// A feature straddling the cut is truncated + fuzzy-marked.
    Linearize {
        #[arg(long)]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        at: Option<usize>,
        #[command(flatten)]
        #[serde(flatten)]
        target: Target,
    },
    /// Circularize a **linear** molecule (join the ends); `origin` optionally
    /// rotates the new circle so that base becomes position 0.
    Circularize {
        #[arg(long)]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        origin: Option<usize>,
        #[command(flatten)]
        #[serde(flatten)]
        target: Target,
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
        #[command(flatten)]
        #[serde(flatten)]
        target: Target,
    },
    /// Summarize the document: name, length, topology, feature/primer counts.
    ///
    /// A projection over `bio`, so it is served by the session layer rather
    /// than [`dispatch`] (decision 9 keeps `core` free of a `bio` dependency).
    Info {
        /// Sugar: a bare positional path, folded into the target by
        /// [`ViewerRequest::fold_positional_target`] before anything reads it.
        /// Never on the wire — the socket only ever sees `path`.
        #[arg(value_name = "PATH")]
        #[serde(skip)]
        input: Option<PathBuf>,
        #[command(flatten)]
        #[serde(flatten)]
        target: Target,
    },
    /// Translate a range to protein. `start`/`end` are 0-based half-open
    /// (default: the whole sequence); `frame` is the GenBank `codon_start`
    /// convention (1, 2, or 3).
    Translate {
        /// 0-based start of the range (default: 0).
        #[arg(long)]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        start: Option<usize>,
        /// 0-based exclusive end of the range (default: sequence length).
        #[arg(long)]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        end: Option<usize>,
        /// Strand: `+` (forward) or `-` (reverse complement).
        #[arg(long, default_value = "+")]
        #[serde(default = "default_strand")]
        strand: String,
        /// Reading frame as GenBank codon_start: 1, 2, or 3.
        #[arg(long, default_value_t = 1)]
        #[serde(default = "default_frame")]
        frame: usize,
        /// Sugar: a bare positional path, folded into the target by
        /// [`ViewerRequest::fold_positional_target`] before anything reads it.
        /// Never on the wire — the socket only ever sees `path`.
        #[arg(value_name = "PATH")]
        #[serde(skip)]
        input: Option<PathBuf>,
        #[command(flatten)]
        #[serde(flatten)]
        target: Target,
    },
    /// Find open reading frames. `min_aa` filters by protein length; forward
    /// and reverse frames are scanned unless `forward_only`.
    Orfs {
        /// Minimum ORF length in amino acids.
        #[arg(long, default_value_t = 30)]
        #[serde(default = "default_min_aa")]
        min_aa: usize,
        /// Report stop-to-stop ORFs instead of Met-to-stop.
        #[arg(long)]
        #[serde(default)]
        stop_to_stop: bool,
        /// Only scan the forward strand.
        #[arg(long)]
        #[serde(default)]
        forward_only: bool,
        /// Sugar: a bare positional path, folded into the target by
        /// [`ViewerRequest::fold_positional_target`] before anything reads it.
        /// Never on the wire — the socket only ever sees `path`.
        #[arg(value_name = "PATH")]
        #[serde(skip)]
        input: Option<PathBuf>,
        #[command(flatten)]
        #[serde(flatten)]
        target: Target,
    },
    /// Find where an ad-hoc oligo anneals (seed-and-extend, both strands,
    /// circular-aware). Unlike `list-primers` this needs no authored primer —
    /// the oligo is supplied inline, which is what makes it a design tool.
    ///
    /// The nested `primers find <PATH> <OLIGO>` form is sugar for this.
    #[command(name = "find-primer-sites")]
    FindPrimerSites {
        /// The oligo sequence, 5'→3'.
        #[arg(long)]
        oligo: String,
        #[command(flatten)]
        #[serde(flatten)]
        target: Target,
    },
    /// List the features on the active buffer (id, kind, label, range, strand).
    /// Ids are session-scoped — use them for `remove-feature`/`rename-feature`.
    ListFeatures {
        #[command(flatten)]
        #[serde(flatten)]
        target: Target,
    },
    /// List the primers on the active buffer with derived attachment state + QC
    /// (Tm/GC/self-structure ΔG). Ids are session-scoped. Backs the Inspector
    /// Primers tab and the CLI `primers list` via one shared projection.
    ListPrimers {
        #[command(flatten)]
        #[serde(flatten)]
        target: Target,
    },
    /// Remove the feature with the given id (from `list-features`).
    RemoveFeature {
        #[arg(long)]
        id: FeatureId,
        #[command(flatten)]
        #[serde(flatten)]
        target: Target,
    },
    /// Rename the feature with the given id (from `list-features`).
    RenameFeature {
        #[arg(long)]
        id: FeatureId,
        #[arg(long)]
        label: String,
        #[command(flatten)]
        #[serde(flatten)]
        target: Target,
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
        #[command(flatten)]
        #[serde(flatten)]
        target: Target,
    },
    /// Add a primer (authored oligo). `sequence` is the full oligo 5'→3' (5' tail
    /// included). `name` is optional — omitted, it falls back to
    /// `Annotations::suggest_primer_name()` (decision 9). `start`/`end` are the
    /// optional annealing footprint (both omitted → a detached/floating oligo);
    /// `strand` is `+`/`-`/`.`. Content-given → needs no `bio` (decision 11).
    AddPrimer {
        #[arg(long)]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        #[arg(long)]
        sequence: String,
        /// 0-based start of the annealing footprint (with `end`).
        #[arg(long)]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        start: Option<usize>,
        /// 0-based exclusive end of the annealing footprint (with `start`).
        #[arg(long)]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        end: Option<usize>,
        /// `+`, `-`, or `.` (unstranded).
        #[arg(long, default_value = "+")]
        #[serde(default = "default_strand")]
        strand: String,
        #[command(flatten)]
        #[serde(flatten)]
        target: Target,
    },
    /// Edit a primer in place: only the fields you pass change. Addressed by id
    /// (from `list-primers`). Passing both `start` and `end` re-sets the binding
    /// footprint; passing one combines it with the current binding.
    UpdatePrimer {
        #[arg(long)]
        id: PrimerId,
        #[arg(long)]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        /// New full oligo 5'→3' (5' tail included).
        #[arg(long)]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sequence: Option<String>,
        /// `+`, `-`, or `.` (unstranded).
        #[arg(long)]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        strand: Option<String>,
        /// New 0-based start of the binding footprint.
        #[arg(long)]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        start: Option<usize>,
        /// New 0-based exclusive end of the binding footprint.
        #[arg(long)]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        end: Option<usize>,
        /// Clear the binding footprint — the primer becomes a floating oligo.
        /// Breaks the `(start, end) = None` "keep current" ambiguity; mutually
        /// exclusive with `start`/`end`.
        #[arg(long)]
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        detach: bool,
        #[command(flatten)]
        #[serde(flatten)]
        target: Target,
    },
    /// Re-anchor a primer to the current template: scan for its best binding site
    /// and write it back (footprint + strand). Turns a Drifted/Detached primer
    /// back into Confirmed without hand-entering coordinates. Errors if the oligo
    /// binds nowhere.
    RescanPrimer {
        #[arg(long)]
        id: PrimerId,
        #[command(flatten)]
        #[serde(flatten)]
        target: Target,
    },
    /// Compose a restriction-enzyme site onto a primer's 5' tail (Phase 2.2a).
    /// Prepends `flank + recognition (+ spacer + overhang for Type IIs)` to the
    /// authored oligo, leaving the binding footprint unchanged (the added bases
    /// are a 5' tail). `overhang` is required for a Type IIs enzyme and must match
    /// its overhang length; omit it for a Type II (palindromic) enzyme.
    AddPrimerSite {
        #[arg(long)]
        id: PrimerId,
        /// Enzyme name (e.g. `EcoRI`, `BsaI`) — from the enzyme catalog.
        #[arg(long)]
        enzyme: String,
        /// User-designed overhang (Type IIs only); its length must equal the
        /// enzyme's overhang.
        #[arg(long)]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        overhang: Option<String>,
        /// 5' flanking bases for efficient cleavage near the end (defaults to a
        /// conservative constant when omitted).
        #[arg(long)]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        flank: Option<String>,
        #[command(flatten)]
        #[serde(flatten)]
        target: Target,
    },
    /// Remove the primer with the given id (from `list-primers`).
    RemovePrimer {
        #[arg(long)]
        id: PrimerId,
        #[command(flatten)]
        #[serde(flatten)]
        target: Target,
    },
    /// Amplify between two attached primers (Primers Phase 3.1). Produces a new
    /// **linear** product buffer inheriting the template's annotations: features
    /// straddling the amplicon edge are truncated + fuzzy-marked; features fully
    /// inside carry over; straddling primers are dropped. Mismatches bake in
    /// (mutagenesis), 5' tails become the product ends (overhangs), and a
    /// circular template gives around-the-horn / whole-plasmid amplification.
    /// `fwd` must be a Forward primer, `rev` a Reverse one; both must be
    /// attached (errors if a primer is detached — attach or rescan it first).
    Pcr {
        /// Forward primer id (from `primers list`).
        #[arg(long)]
        fwd: PrimerId,
        /// Reverse primer id (from `primers list`).
        #[arg(long)]
        rev: PrimerId,
        /// Optional product buffer name (defaults to `<template> amplicon`).
        #[arg(long)]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        /// The **template** view (defaults to the active view).
        #[command(flatten)]
        #[serde(flatten)]
        target: Target,
    },
    /// Digest the active buffer with one or more enzymes (Restriction Tier 2).
    /// Opens a read-only **Fragments** view over the source — a projection of
    /// the *virtual* fragment set (nothing is materialized to a buffer). The
    /// molecule's methylation context (the view's authored Dam/Dcm/CpG state)
    /// filters methylation-blocked cut sites. `query` uses the same enzyme
    /// grammar as `enzymes` (names, `golden gate`, `type IIs`, …).
    ///
    /// One verb for both faces. This used to carry `#[command(skip)]` so a
    /// separate CLI-local `digest <file> --enzymes …` could own the name; they
    /// took their enzymes differently and drifted. The enzyme input is now
    /// `--enzymes` everywhere — the same grammar as the `enzymes` verb — and
    /// the positional slot belongs to the document (`Target`'s path sugar).
    Digest {
        /// Enzyme names or presets (comma- or space-separated; repeatable),
        /// e.g. `--enzymes EcoRI,BamHI` or `--enzymes "golden gate"`.
        #[arg(short, long)]
        #[serde(default, rename = "query")]
        enzymes: Vec<String>,
        /// Treat the molecule as circular (overrides the document's topology).
        #[arg(long)]
        #[serde(default)]
        circular: bool,
        /// The **source** document to digest (defaults to the active view).
        /// Sugar: a bare positional path, folded into the target by
        /// [`ViewerRequest::fold_positional_target`] before anything reads it.
        /// Never on the wire — the socket only ever sees `path`.
        #[arg(value_name = "PATH")]
        #[serde(skip)]
        input: Option<PathBuf>,
        #[command(flatten)]
        #[serde(flatten)]
        target: Target,
    },
    /// Save the active buffer to its source path.
    Save {
        /// Overwrite even if the file changed on disk since it was loaded
        /// (skips the external-change guard). For non-interactive callers.
        #[arg(long)]
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        force: bool,
        #[command(flatten)]
        #[serde(flatten)]
        target: Target,
    },
    /// Save the active buffer to a new path.
    SaveAs {
        path: PathBuf,
        #[command(flatten)]
        #[serde(flatten)]
        target: Target,
    },
    /// Assemble a product from bins → the product(s).
    ///
    /// One verb, two document sources (ROADMAP decision 27). `inputs` is either
    /// a single `recipe.json` or a list of inline bin tokens
    /// `SOURCE[@5′..3′]` — `pUC19.gb@EcoRI..PstI`, `parts/*.gb@BsaI..BsaI`,
    /// `buffer:3@BsaI..BsaI`. Tokens naming only paths run locally in the
    /// calling process; anything naming a `buffer:` handle needs a live session
    /// and is forwarded to it. The GUI's **File → New Assembly… → Run** is the
    /// same request: both call `seqforge_bio::run_indices` and export through
    /// `seqforge_bio::write_products`.
    Assemble {
        /// Path/glob with optional `@5′..3′` (`EcoRI..PstI`, `BsaI..BsaI`,
        /// `pcr:fwd..rev`, `as-is`), `buffer:<n>`, or a single `recipe.json`.
        #[arg(value_name = "TOKEN")]
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        inputs: Vec<String>,
        /// Join method: `ligate` | `golden-gate`.
        ///
        /// Serialized as `join`: the enum's own serde tag is `method` (the
        /// JSON-RPC method name), so the field cannot share it. The CLI flag
        /// stays `--method`.
        #[arg(long, default_value = "ligate")]
        #[serde(rename = "join", default = "default_join_method")]
        method: String,
        /// Intended topology: `circular` | `linear` | `any`.
        #[arg(long, default_value = "circular")]
        #[serde(default = "default_topology_intent")]
        topology: String,
        /// Default digest enzymes when a bin has no `@5′..3′`
        /// (one enzyme → `E..E`; two → `E1..E2`).
        #[arg(long)]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        enzymes: Option<String>,
        /// Combination mode: `all-to-all` (Cartesian product; default) or
        /// `zip` (positional 1:1; bins must share fragment count).
        #[arg(long, default_value = "all-to-all")]
        #[serde(default = "default_expand")]
        expand: String,
        /// Also write the resolved recipe as JSON to this path.
        #[arg(long)]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        emit_recipe: Option<PathBuf>,
        /// Report bins + combo count + join end-compatibility without products.
        #[arg(long)]
        #[serde(default)]
        dry_run: bool,
        /// Score each combo with a fidelity dataset (dry-run overlay only;
        /// never written into recipe.json).
        #[arg(long)]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fidelity_dataset: Option<String>,
        /// Include the RC-expanded subset ligation-frequency matrix on dry-run
        /// JSON. Requires `--fidelity-dataset`.
        #[arg(long)]
        #[serde(default)]
        fidelity_matrix: bool,
        /// Write each product into this directory (created if absent).
        #[arg(long)]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        out: Option<PathBuf>,
        /// Product format for `out`: `genbank` (default) or `fasta`.
        #[arg(long, default_value = "genbank")]
        #[serde(default = "default_product_format")]
        format: String,
        /// Name each product from a template instead of `role+role #n`.
        /// Brace-delimited tokens: `roles`, `n` (combo index), `i` (ordinal),
        /// `bin0`…`binN` (optionally `bin1:6`, or `bin1/2` for one
        /// `_`-separated field) — e.g. `VH-{bin1}-{bin2}`.
        #[arg(long)]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name_template: Option<String>,
        /// Run only these combos: indices, `A-B` ranges, and `!` exclusions
        /// (`0-31,!12`). A bare exclusion means "all but these".
        #[arg(long)]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        combos: Option<String>,
        /// Rotate each circular product so this point becomes position 1: a
        /// feature label (`Start`) or a 0-based index.
        #[arg(long)]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        origin: Option<String>,
    },
    /// Undo the last edit on the active buffer.
    Undo {
        #[command(flatten)]
        #[serde(flatten)]
        target: Target,
    },
    /// Redo the last undone edit on the active buffer.
    Redo {
        #[command(flatten)]
        #[serde(flatten)]
        target: Target,
    },
}

/// Serde default for `AddFeature.strand` (clap supplies it via `default_value`).
fn default_strand() -> String {
    "+".to_string()
}

/// serde defaults for [`ViewerRequest::Assemble`] (clap supplies these via
/// `default_value`; serde needs them for a socket payload that omits the field).
fn default_join_method() -> String {
    "ligate".to_string()
}

fn default_topology_intent() -> String {
    "circular".to_string()
}

fn default_expand() -> String {
    "all-to-all".to_string()
}

impl ViewerRequest {
    /// Fold a sugar positional path into the target.
    ///
    /// `seqforge info x.gb` and `seqforge info --in x.gb` must build the same
    /// value; this is where they converge, once, right after parsing. An
    /// explicit `--in`/`--view` wins, so the two can never disagree silently.
    pub fn fold_positional_target(&mut self) {
        let input = match self {
            ViewerRequest::Info { input, .. }
            | ViewerRequest::Translate { input, .. }
            | ViewerRequest::Orfs { input, .. }
            | ViewerRequest::Digest { input, .. } => input.take(),
            _ => None,
        };
        if let Some(path) = input {
            if let Some(t) = self.target_mut() {
                if t.view.is_none() && t.path.is_none() {
                    *t = Target::path(path);
                }
            }
        }
    }

    /// The request's document [`Target`], if it addresses one.
    ///
    /// `None` for workspace-scoped variants (`Open` / `Close` / `Buffers` /
    /// `New` / `Assemble` / `Focus`), which name no single document. A `Some`
    /// carrying a default `Target` means "the active view" — exactly what an
    /// omitted `view` used to mean.
    pub fn target(&self) -> Option<&Target> {
        match self {
            ViewerRequest::GoTo { target, .. } => Some(target),
            ViewerRequest::Find { target, .. } => Some(target),
            ViewerRequest::Enzymes { target, .. } => Some(target),
            ViewerRequest::Insert { target, .. } => Some(target),
            ViewerRequest::Delete { target, .. } => Some(target),
            ViewerRequest::Replace { target, .. } => Some(target),
            ViewerRequest::ReverseComplement { target, .. } => Some(target),
            ViewerRequest::Cut { target, .. } => Some(target),
            ViewerRequest::Copy { target, .. } => Some(target),
            ViewerRequest::Paste { target, .. } => Some(target),
            ViewerRequest::AddFeature { target, .. } => Some(target),
            ViewerRequest::Info { target, .. } => Some(target),
            ViewerRequest::Translate { target, .. } => Some(target),
            ViewerRequest::Orfs { target, .. } => Some(target),
            ViewerRequest::FindPrimerSites { target, .. } => Some(target),
            ViewerRequest::ListFeatures { target, .. } => Some(target),
            ViewerRequest::ListPrimers { target, .. } => Some(target),
            ViewerRequest::RemoveFeature { target, .. } => Some(target),
            ViewerRequest::RenameFeature { target, .. } => Some(target),
            ViewerRequest::UpdateFeature { target, .. } => Some(target),
            ViewerRequest::AddPrimer { target, .. } => Some(target),
            ViewerRequest::UpdatePrimer { target, .. } => Some(target),
            ViewerRequest::RescanPrimer { target, .. } => Some(target),
            ViewerRequest::AddPrimerSite { target, .. } => Some(target),
            ViewerRequest::RemovePrimer { target, .. } => Some(target),
            ViewerRequest::Pcr { target, .. } => Some(target),
            ViewerRequest::Digest { target, .. } => Some(target),
            ViewerRequest::Save { target, .. } => Some(target),
            ViewerRequest::SaveAs { target, .. } => Some(target),
            ViewerRequest::Undo { target, .. } => Some(target),
            ViewerRequest::Redo { target, .. } => Some(target),
            ViewerRequest::SetOrigin { target, .. } => Some(target),
            ViewerRequest::Linearize { target, .. } => Some(target),
            ViewerRequest::Circularize { target, .. } => Some(target),
            ViewerRequest::Open { .. }
            | ViewerRequest::Close
            | ViewerRequest::Buffers
            | ViewerRequest::New { .. } // creates its own view
            | ViewerRequest::Assemble { .. } // creates its own view(s)
            | ViewerRequest::Focus { .. } => None,
        }
    }

    /// Mutable [`Self::target`], so a shell can collapse a `Path` target into
    /// the view it opened before dispatch — after which nothing downstream can
    /// tell how the document was addressed.
    pub fn target_mut(&mut self) -> Option<&mut Target> {
        match self {
            ViewerRequest::GoTo { target, .. } => Some(target),
            ViewerRequest::Find { target, .. } => Some(target),
            ViewerRequest::Enzymes { target, .. } => Some(target),
            ViewerRequest::Insert { target, .. } => Some(target),
            ViewerRequest::Delete { target, .. } => Some(target),
            ViewerRequest::Replace { target, .. } => Some(target),
            ViewerRequest::ReverseComplement { target, .. } => Some(target),
            ViewerRequest::Cut { target, .. } => Some(target),
            ViewerRequest::Copy { target, .. } => Some(target),
            ViewerRequest::Paste { target, .. } => Some(target),
            ViewerRequest::AddFeature { target, .. } => Some(target),
            ViewerRequest::Info { target, .. } => Some(target),
            ViewerRequest::Translate { target, .. } => Some(target),
            ViewerRequest::Orfs { target, .. } => Some(target),
            ViewerRequest::FindPrimerSites { target, .. } => Some(target),
            ViewerRequest::ListFeatures { target, .. } => Some(target),
            ViewerRequest::ListPrimers { target, .. } => Some(target),
            ViewerRequest::RemoveFeature { target, .. } => Some(target),
            ViewerRequest::RenameFeature { target, .. } => Some(target),
            ViewerRequest::UpdateFeature { target, .. } => Some(target),
            ViewerRequest::AddPrimer { target, .. } => Some(target),
            ViewerRequest::UpdatePrimer { target, .. } => Some(target),
            ViewerRequest::RescanPrimer { target, .. } => Some(target),
            ViewerRequest::AddPrimerSite { target, .. } => Some(target),
            ViewerRequest::RemovePrimer { target, .. } => Some(target),
            ViewerRequest::Pcr { target, .. } => Some(target),
            ViewerRequest::Digest { target, .. } => Some(target),
            ViewerRequest::Save { target, .. } => Some(target),
            ViewerRequest::SaveAs { target, .. } => Some(target),
            ViewerRequest::Undo { target, .. } => Some(target),
            ViewerRequest::Redo { target, .. } => Some(target),
            ViewerRequest::SetOrigin { target, .. } => Some(target),
            ViewerRequest::Linearize { target, .. } => Some(target),
            ViewerRequest::Circularize { target, .. } => Some(target),
            ViewerRequest::Open { .. }
            | ViewerRequest::Close
            | ViewerRequest::Buffers
            | ViewerRequest::New { .. } // creates its own view
            | ViewerRequest::Assemble { .. } // creates its own view(s)
            | ViewerRequest::Focus { .. } => None,
        }
    }
}

/// One open document, as reported by `buffers`. `index` is the stable 1-based
/// handle used to `focus` it (also accepts the path/basename).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocInfo {
    pub index: usize,
    pub name: String,
    pub path: Option<PathBuf>,
    pub dirty: bool,
    pub active: bool,
}

/// Response returned from `dispatch`. Each variant carries the data relevant
/// to that command so callers (CLI, agents) can act on it without parsing text.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ViewerResponse {
    /// Open or Close succeeded.
    Ok,
    /// `buffers` — the open documents in tab order.
    Buffers { count: usize, docs: Vec<DocInfo> },
    /// GoTo — 1-based position the viewer navigated to.
    Navigated { position: usize },
    /// Find — all matching hits (empty when the pattern was cleared).
    SearchResults { count: usize, hits: Vec<SearchHit> },
    /// Enzymes — all cut sites found (empty when the enzyme list was cleared).
    CutSites {
        count: usize,
        sites: Vec<CutSite>,
        methylation: MethylContext,
        /// Parallel to `sites` — derived verdicts under `methylation`.
        methyl_states: Vec<MethylState>,
    },
    /// An editor write-op (insert/delete/replace/RC/cut/paste/undo/redo/feature)
    /// succeeded; `len` is the buffer length after the edit. `changed` is false
    /// for a no-op Undo/Redo (empty history) so callers can report "nothing to
    /// undo" without it being an error.
    Edited { len: usize, changed: bool },
    /// `AddFeature` — the new feature's session-scoped id (use it to
    /// remove/rename), and the buffer length after the add.
    FeatureAdded { id: FeatureId, len: usize },
    /// `AddPrimer` — the new primer's session-scoped id (use it to
    /// update/remove), and the buffer length after the add.
    PrimerAdded { id: PrimerId, len: usize },
    /// `ListFeatures` — every feature on the buffer, in definition order.
    ///
    /// `count` mirrors `SearchResults`/`CutSites`: every list response carries
    /// one, so a caller can read the size without walking the items.
    Features {
        count: usize,
        features: Vec<FeatureInfo>,
    },
    /// `ListPrimers` — every primer on the buffer (definition order) with its
    /// derived attachment state + QC.
    Primers {
        count: usize,
        primers: Vec<PrimerInfo>,
    },
    /// `Digest` — the virtual fragment set over the source, plus any methylation
    /// warnings. A projection (nothing materialized); the Fragments view and
    /// CLI/agent read the same shape.
    Fragments {
        /// The digested document.
        name: String,
        /// The **canonical** resolved enzyme query — what a preset like
        /// `golden gate` expanded to. Worth reporting because the request only
        /// records what the caller typed, and the GUI persists this same string
        /// as the Fragments view's title.
        enzymes: String,
        count: usize,
        fragments: Vec<FragmentInfo>,
        warnings: Vec<String>,
    },
    /// `Info` — the document summary. `path` is `None` for a scratch buffer.
    DocumentInfo {
        name: String,
        length: usize,
        topology: String,
        features: usize,
        primers: usize,
        #[serde(skip_serializing_if = "Option::is_none")]
        path: Option<std::path::PathBuf>,
    },
    /// `Translate` — the protein for one range/strand/frame.
    Translation {
        name: String,
        start: usize,
        end: usize,
        strand: String,
        frame: usize,
        protein: String,
        /// Residue count (the protein's length, not the nucleotide range's).
        length: usize,
    },
    /// `Orfs` — every ORF passing `min_aa`, in position order.
    Orfs {
        name: String,
        count: usize,
        orfs: Vec<OrfInfo>,
    },
    /// `FindPrimerSites` — every place the oligo anneals.
    PrimerSites {
        /// The queried oligo, upper-cased.
        oligo: String,
        count: usize,
        sites: Vec<PrimerSiteInfo>,
    },
    /// `Assemble` — the assembled product(s), in run order.
    Products {
        count: usize,
        products: Vec<ProductInfo>,
        warnings: Vec<String>,
    },
}

/// One open reading frame, projected by value.
///
/// Mirrors `seqforge_bio::Orf`, which `core` cannot name (decision 9 forbids
/// `core ──► bio`). The session layer converts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrfInfo {
    /// 0-based half-open range on the forward strand.
    pub start: usize,
    pub end: usize,
    pub strand: Strand,
    /// Reading frame within the oriented strand: 1, 2, or 3.
    pub frame: usize,
    /// Amino-acid count.
    pub aa_len: usize,
}

/// One assembly product, projected for display / CLI. Unlike a fragment, a
/// product **is** materialized (as a buffer, and optionally a file), so it
/// carries the provenance a caller needs to join it back to its inputs:
/// `combo_index` indexes the same expansion the dry-run reports.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProductInfo {
    pub name: String,
    pub length: usize,
    pub topology: Topology,
    pub combo_index: usize,
    /// The per-bin source names that went into this product, in bin order.
    pub parts: Vec<String>,
    /// Where it was written, when `out` was given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
}

/// serde default for [`ViewerRequest::Assemble::format`].
fn default_product_format() -> String {
    "genbank".to_string()
}

/// One end of a fragment, projected for display / CLI. `kind` is `"blunt"` /
/// `"5'"` / `"3'"`; `cut_by` is the enzyme that made the boundary (`None` = a
/// free molecule terminus), read off the fragment's lineage.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EndInfo {
    pub kind: String,
    /// The single-stranded overhang, 5′→3′ (empty when blunt).
    pub seq: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cut_by: Option<String>,
}

/// A digest fragment summary — a by-value projection (mirrors [`FeatureInfo`] /
/// [`PrimerInfo`]) so the Fragments view and CLI/agent share one shape and
/// cannot drift. Fragments are virtual, so there is no persistent id; the
/// `index` is positional within the digest result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FragmentInfo {
    pub index: usize,
    /// Top-strand length in bp.
    pub length: usize,
    pub topology: Topology,
    pub left: EndInfo,
    pub right: EndInfo,
    /// Source footprint as a [`Span`] (circular-native — wraps the origin
    /// natively). Display layers convert to 1-based.
    pub source_span: Span,
}

/// A feature summary for `ListFeatures` — a by-value projection so CLI/agent
/// callers get id + location without a live handle into editor state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeatureInfo {
    pub id: FeatureId,
    /// Verbatim GenBank feature-type string (e.g. `CDS`, `misc_feature`).
    pub kind: String,
    pub label: String,
    /// The feature's footprint as a [`Span`] (circular-native — an
    /// origin-spanning feature is one wrapping span; a spliced `Join` reports its
    /// bounding span). Serializes as `{start, len}`. Display layers convert to
    /// 1-based and render both arms on wrap.
    pub span: Span,
    pub strand: Strand,
}

/// Derived attachment state of a primer against the current template — the
/// serialized projection vocabulary shared by the Inspector pane and the CLI.
/// Mirrors `seqforge_bio::AttachmentState` (bio's internal computed type); the
/// boundary map lives in `seqforge_bio::primer_infos`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrimerState {
    /// Derived footprint matches the stored binding; clean anneal.
    Confirmed,
    /// Still anchored within tolerance but moved / has mismatches.
    Drifted,
    /// No viable binding (3' anchor lost or below stringency) — floating oligo.
    Detached,
}

/// One place where a primer's oligo anneals on the current template — a
/// serializable per-site projection of `seqforge_bio::PrimerBinding` plus its
/// annealing Tm. A primer may have several (repeats, off-targets); the Inspector
/// lists them and the CLI emits them so mispriming is visible.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrimerSiteInfo {
    /// Annealing footprint on the top strand as a [`Span`] (circular-native, so a
    /// site crossing the origin is one wrapping span). Serializes as `{start,
    /// len}`; display layers convert to 1-based.
    pub span: Span,
    pub strand: Strand,
    /// Mismatches within the footprint (0 = perfect anneal).
    pub mismatches: usize,
    /// primer:template annealing Tm (°C) at this site; `None` on error.
    pub anneal_tm: Option<f64>,
    /// This found site coincides with the primer's authored `binding` (the
    /// currently-attached footprint). At most one site is `attached`.
    pub attached: bool,
    /// Whatever the oligo has 5' of the footprint — a restriction site, an
    /// overhang, a homology arm. For a cloning primer this is the functional
    /// part, and it is invisible from the span alone, so it is reported
    /// explicitly rather than left to be derived.
    ///
    /// Read straight off the oligo (`len - footprint`), never from a
    /// decomposition, which would clamp an origin-crossing span to the sequence
    /// end and over-report the tail.
    pub tail: String,
    pub tail_len: usize,
}

/// A primer summary for `ListPrimers` — a by-value projection (mirrors
/// [`FeatureInfo`]) so the Inspector pane and CLI/agent share one shape and
/// cannot drift. Assembled in `seqforge_bio` (which owns the thermo + anneal
/// code) and returned through [`BioOps::primer_infos`].
///
/// QC fields are `Option` because folding/Tm can fail on a degenerate oligo
/// (surfaces as JSON `null`, like `seqforge tm`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrimerInfo {
    pub id: PrimerId,
    pub name: String,
    /// Full authored oligo 5'→3' (5' tail included).
    pub sequence: String,
    /// Annealing footprint on the top strand as a [`Span`] (wraps the origin
    /// natively); `None` for a detached/floating oligo. Display layers convert to
    /// 1-based.
    pub binding: Option<Span>,
    pub strand: Strand,
    /// Oligo length in bp (full oligo, 5' tail included).
    pub len: usize,
    /// The 5' bases that anneal to nothing — a restriction site, an overhang, a
    /// homology arm. Derived as `sequence` minus the `binding` length, so it is
    /// empty for an ordinary primer and non-empty for a cloning one. Surfaced
    /// because it is invisible from `binding` alone (a `primer_bind` can only
    /// span what anneals) yet it is the functional half of a cloning oligo.
    pub tail: String,
    /// Monomer nearest-neighbour Tm (°C); `None` if the oligo is too short.
    pub tm: Option<f64>,
    /// GC content (percentage, `0.0..=100.0`).
    pub gc: f64,
    /// Self-hairpin ΔG (kcal/mol) at the default fold temp; `None` on fold error.
    pub hairpin_dg: Option<f64>,
    /// Self-dimer ΔG (kcal/mol); `None` on fold error.
    pub self_dimer_dg: Option<f64>,
    /// primer:template annealing Tm (°C) at `binding`; `None` when detached or
    /// on error.
    pub anneal_tm: Option<f64>,
    /// Derived attachment state.
    pub state: PrimerState,
    /// Mismatch count within the stored-binding footprint (0 = clean anneal).
    pub mismatches: usize,
    /// Count of additional (off-target) binding sites — orthogonal to `state`.
    /// Equals the number of `sites` that are not `attached`.
    pub off_targets: usize,
    /// Every place the oligo anneals on the current template (repeats,
    /// off-targets, and — when anchored — the attached site itself). Empty when
    /// the oligo binds nowhere. Drives the Inspector site list + rescan.
    pub sites: Vec<PrimerSiteInfo>,
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
    /// Build the `ListPrimers` projection: for each authored primer, its derived
    /// attachment state + QC (Tm/GC/self-structure ΔG/anneal Tm) against the
    /// current template. This is the seqforge-core seam for the pane/CLI parity
    /// shape — the assembly (classify + qc) lives in `seqforge_bio` (which owns
    /// the thermo + anneal code), mirroring `find_cut_sites -> Vec<CutSite>`.
    fn primer_infos(&self, seq: &[u8], primers: &[&Primer], circular: bool) -> Vec<PrimerInfo>;
    /// Derive methylation verdicts for cut sites under a context (CLI/socket output).
    fn methyl_states_for_sites(
        &self,
        sites: &[CutSite],
        seq: &[u8],
        methylation: &MethylContext,
    ) -> Vec<MethylState>;
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
/// Re-derive `cut_sites` + `methyl_states` from the view's **current**
/// `active_enzymes` + `methylation` against the live buffer bytes, and stamp
/// `results_version`. The single scan implementation: the `Enzymes` command sets
/// the params then calls this; [`rescan_if_stale`] calls it when the stamp lags.
fn scan_cut_sites<B: BioOps>(view: &mut View, buffer: &Buffer, bio: &B) {
    let circular = buffer.is_circular();
    let refs: Vec<&str> = view.active_enzymes.iter().map(String::as_str).collect();
    let sites = bio.find_cut_sites(&buffer.text, &refs, circular);
    let methyl_states = bio.methyl_states_for_sites(&sites, &buffer.text, &view.methylation);
    view.cut_sites = sites;
    view.methyl_states = methyl_states;
    view.results_version = Some(buffer.version);
}

/// Freshen a view's derived results before a consumer reads them: recompute stale
/// cut sites/methyl (their params live on the view), and clear stale search
/// highlights (the query is not retained). No-op when everything is fresh.
///
/// This is the **read-side** freshness guarantee. `commit_edit` only bumps
/// `buffer.version` (the signal); it never recomputes. So every consumer — the GUI
/// paint path and any headless command that reads `cut_sites` (e.g. a future
/// restriction digest) — calls this first, keyed to the view it is about to read.
/// Correctness therefore never depends on GUI focus/active-view events, which CLI
/// callers do not generate.
pub fn rescan_if_stale<B: BioOps>(view: &mut View, buffer: &Buffer, bio: &B) {
    if view.cut_sites_stale(buffer.version) {
        scan_cut_sites(view, buffer, bio);
    }
    if view.search_stale(buffer.version) {
        view.search_hits.clear();
        view.search_version = None;
    }
}

pub fn dispatch<B: BioOps>(
    view: &mut View,
    buffer: &Buffer,
    annotations: &mut Annotations,
    bio: &B,
    req: ViewerRequest,
) -> Result<ViewerResponse, DispatchError> {
    match req {
        ViewerRequest::Open { .. }
        | ViewerRequest::Close
        | ViewerRequest::Buffers
        | ViewerRequest::New { .. }
        | ViewerRequest::Focus { .. } => {
            unreachable!(
                "Open/Close/Buffers/New/Focus are workspace-scoped; the caller must \
                 handle them before invoking dispatch (see command::apply)"
            )
        }

        // Editor write-ops are intercepted in `command::apply`'s `Viewer(req)`
        // arm and routed to `command/edit.rs` (the Phase 11 write path); they
        // never reach `core::dispatch`. Listed explicitly so adding a future
        // write-op forces a compile error here rather than silently falling
        // through.
        // Projections over `bio` — served by `seqforge_session::dispatch`
        // before this function is reached. `core` cannot compute them itself
        // without depending on `bio`, which decision 9 forbids.
        ViewerRequest::Info { .. }
        | ViewerRequest::Translate { .. }
        | ViewerRequest::Orfs { .. }
        | ViewerRequest::FindPrimerSites { .. } => Err(DispatchError::Unimplemented(
            "a bio projection (it needs the session layer)",
        )),

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
        | ViewerRequest::AddPrimer { .. }
        | ViewerRequest::UpdatePrimer { .. }
        | ViewerRequest::RescanPrimer { .. }
        | ViewerRequest::AddPrimerSite { .. }
        | ViewerRequest::RemovePrimer { .. }
        | ViewerRequest::Pcr { .. }
        | ViewerRequest::Digest { .. }
        | ViewerRequest::Save { .. }
        | ViewerRequest::SaveAs { .. }
        | ViewerRequest::Undo { .. }
        | ViewerRequest::Redo { .. }
        | ViewerRequest::SetOrigin { .. }
        | ViewerRequest::Linearize { .. }
        | ViewerRequest::Circularize { .. }
        | ViewerRequest::Assemble { .. } => {
            // Write-ops are workspace-scoped: they mutate buffers, annotations
            // and history, which this function does not own (it read-locks).
            // The app routes them to `seqforge_session::edit` before ever
            // getting here. This used to be `unreachable!` — an invariant of
            // the one caller. Now that a headless shell can reach `dispatch`
            // too, an unroutable request must be an error rather than a panic.
            Err(DispatchError::Unimplemented(
                "a write verb against a file target (it needs a session)",
            ))
        }

        // Note: `view` targeting is handled by the caller before this
        // function is invoked. `dispatch` always operates on whatever
        // (View, Buffer) was passed in.
        ViewerRequest::GoTo {
            position,
            target: _,
        } => {
            let seq_len = buffer.len();
            if position == 0 || position > seq_len {
                return Err(DispatchError::OutOfRange { position, seq_len });
            }
            let idx = position - 1;
            view.scroll_to = Some(idx);
            view.selection = ViewSelection::Text(Selection::cursor(idx));
            Ok(ViewerResponse::Navigated { position })
        }

        ViewerRequest::Find {
            pattern,
            mismatches,
            target: _,
        } => {
            if pattern.is_empty() {
                // Empty pattern is a "clear search" affordance. Drop
                // search hits AND the selection (which was likely
                // pointing at the first hit) so the user lands on a
                // clean state — consistent with `Open` / `Close`.
                // Tier 2 #10.
                view.search_hits.clear();
                view.search_version = None;
                view.selection = ViewSelection::None;
                return Ok(ViewerResponse::SearchResults {
                    count: 0,
                    hits: vec![],
                });
            }
            let circular = buffer.is_circular();
            let hits = bio.find_matches(&buffer.text, pattern.as_bytes(), mismatches, circular);
            let count = hits.len();
            if let Some(first) = hits.first() {
                view.scroll_to = Some(first.span.start);
                view.selection =
                    ViewSelection::Text(Selection::from_span(first.span, buffer.text.len()));
            }
            view.search_hits = hits.clone();
            view.search_version = Some(buffer.version);
            Ok(ViewerResponse::SearchResults { count, hits })
        }

        // Read-op: features are addressed by id, so surface the live id table
        // for CLI/agent callers. Rides `dispatch` (read-only, no history).
        ViewerRequest::ListFeatures { target: _ } => {
            let features: Vec<FeatureInfo> = annotations
                .iter()
                .map(|f| FeatureInfo {
                    id: f.id,
                    kind: f.raw_kind.clone(),
                    label: f.label.clone(),
                    // Exact wrap-aware span for a single-region feature; bounding
                    // span for a spliced `Join` (`selection_span`).
                    span: f.selection_span(buffer.text.len()),
                    strand: f.strand,
                })
                .collect();
            Ok(ViewerResponse::Features {
                count: features.len(),
                features,
            })
        }

        // Read-op: derived primer projection (attachment state + QC), routed
        // through BioOps so core stays independent of seqforge-bio (mirrors
        // Enzymes → find_cut_sites). Shared shape with the CLI `primers list`.
        ViewerRequest::ListPrimers { target: _ } => {
            let circular = buffer.is_circular();
            let primers: Vec<&Primer> = annotations.primers().collect();
            let infos = bio.primer_infos(&buffer.text, &primers, circular);
            Ok(ViewerResponse::Primers {
                count: infos.len(),
                primers: infos,
            })
        }

        ViewerRequest::Enzymes {
            query,
            op,
            target: _,
            dam,
            dcm,
            cpg,
        } => {
            let circular = buffer.is_circular();
            // active_enzymes is the source of truth; the op mutates it and
            // cut_sites is always re-derived through the single scanner.
            let resolved = bio.resolve_enzyme_names(&buffer.text, &query, circular);
            let new_set = match op {
                EnzymeOp::Set => resolved,
                EnzymeOp::Add => union_names(&view.active_enzymes, &resolved),
                EnzymeOp::Remove => difference_names(&view.active_enzymes, &resolved),
            };
            // Install the params, then run the single scan implementation (which
            // also stamps `results_version`). `rescan_if_stale` reuses `scan_cut_sites`
            // so an edit-driven re-derive can never diverge from this command.
            view.active_enzymes = new_set;
            view.methylation = MethylContext { dam, dcm, cpg };
            scan_cut_sites(view, buffer, bio);
            Ok(ViewerResponse::CutSites {
                count: view.cut_sites.len(),
                sites: view.cut_sites.clone(),
                methylation: view.methylation,
                methyl_states: view.methyl_states.clone(),
            })
        }
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
                span: Span::from_range(start..end),
                strand: crate::Strand::Forward,
            });
            self
        }
        fn with_site(mut self, start: usize) -> Self {
            self.sites.push(CutSite {
                enzyme: "EcoRI".into(),
                pattern: "GAATTC".into(),
                recognition: Span::new(start, 6),
                cut_pos: start + 1,
                bottom_cut_pos: start + 5,
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
                primers: vec![],
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
        fn primer_infos(
            &self,
            _seq: &[u8],
            primers: &[&Primer],
            _circular: bool,
        ) -> Vec<PrimerInfo> {
            // Stub: echo one canned projection per primer so the dispatch
            // plumbing (annotations.primers() → response) is testable without
            // pulling in the real thermo/anneal computation (which lives in bio).
            primers
                .iter()
                .map(|p| PrimerInfo {
                    id: p.id,
                    name: p.name.clone(),
                    sequence: p.sequence.clone(),
                    binding: p.binding,
                    strand: p.strand,
                    len: p.sequence.chars().count(),
                    tail: String::new(),
                    tm: None,
                    gc: 0.0,
                    hairpin_dg: None,
                    self_dimer_dg: None,
                    anneal_tm: None,
                    state: PrimerState::Detached,
                    mismatches: 0,
                    off_targets: 0,
                    sites: vec![],
                })
                .collect()
        }
        fn methyl_states_for_sites(
            &self,
            sites: &[CutSite],
            _seq: &[u8],
            methylation: &MethylContext,
        ) -> Vec<MethylState> {
            // Context-sensitive stub: Dam-on blocks every site, so tests can prove
            // the cache reflects the passed context (not just its length).
            let state = if methylation.dam {
                MethylState::Blocked
            } else {
                MethylState::Cuttable
            };
            vec![state; sites.len()]
        }
    }

    // ── ViewerRequest serde round-trips ───────────────────────────────────────

    #[test]
    fn viewer_request_serde_round_trip_goto() {
        let req = ViewerRequest::GoTo {
            position: 100,
            target: Target::active(),
        };
        let json = serde_json::to_string(&req).unwrap();
        assert_eq!(json, r#"{"method":"goto","position":100}"#);
        let back: ViewerRequest = serde_json::from_str(&json).unwrap();
        let ViewerRequest::GoTo { position, target } = back else {
            panic!("wrong variant")
        };
        assert_eq!(position, 100);
        assert_eq!(target, Target::active());
    }

    #[test]
    fn viewer_request_serde_round_trip_find() {
        let req = ViewerRequest::Find {
            pattern: "ATGC".into(),
            mismatches: 2,
            target: Target::active(),
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
            target: Target::active(),
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
            target: Target::view(crate::ViewId(17)),
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"view\":17"));
        let back: ViewerRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.target().and_then(|t| t.view), Some(crate::ViewId(17)));
    }

    #[test]
    fn target_extracts_explicit_view_id() {
        let r = ViewerRequest::Find {
            pattern: "AT".into(),
            mismatches: 0,
            target: Target::view(crate::ViewId(42)),
        };
        assert_eq!(r.target().and_then(|t| t.view), Some(crate::ViewId(42)));
    }

    #[test]
    fn target_workspace_scoped_variants_return_none() {
        let close = ViewerRequest::Close;
        assert_eq!(close.target().and_then(|t| t.view), None);
        let open = ViewerRequest::Open {
            path: std::path::PathBuf::from("/x"),
        };
        assert_eq!(open.target().and_then(|t| t.view), None);
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
            target: Target::active(),
            dam: true,
            dcm: true,
            cpg: false,
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
            target: Target::active(),
        };
        let json = serde_json::to_string(&req).unwrap();
        assert_eq!(json, r#"{"method":"insert","pos":10,"bases":"ATG"}"#);
        let back: ViewerRequest = serde_json::from_str(&json).unwrap();
        assert!(
            matches!(back, ViewerRequest::Insert { pos: 10, ref bases, ref target } if bases == "ATG" && target == &Target::active())
        );
    }

    #[test]
    fn viewer_request_serde_round_trip_delete() {
        let req = ViewerRequest::Delete {
            start: 5,
            end: 9,
            target: Target::active(),
        };
        let json = serde_json::to_string(&req).unwrap();
        assert_eq!(json, r#"{"method":"delete","start":5,"end":9}"#);
        let back: ViewerRequest = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            back,
            ViewerRequest::Delete {
                start: 5,
                end: 9,
                ref target
            } if target == &Target::active()
        ));
    }

    #[test]
    fn viewer_request_serde_round_trip_reverse_complement() {
        let req = ViewerRequest::ReverseComplement {
            start: 0,
            end: 4,
            target: Target::active(),
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
            (
                ViewerRequest::Undo {
                    target: Target::active(),
                },
                "undo",
            ),
            (
                ViewerRequest::Save {
                    force: false,
                    target: Target::active(),
                },
                "save",
            ),
        ] {
            let json = serde_json::to_string(&req).unwrap();
            assert_eq!(json, format!(r#"{{"method":"{tag}"}}"#));
            let back: ViewerRequest = serde_json::from_str(&json).unwrap();
            assert_eq!(back.target().and_then(|t| t.view), None);
        }
    }

    #[test]
    fn viewer_request_editor_view_target_round_trips() {
        let req = ViewerRequest::Insert {
            pos: 3,
            bases: "C".into(),
            target: Target::view(crate::ViewId(7)),
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"view\":7"));
        let back: ViewerRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.target().and_then(|t| t.view), Some(crate::ViewId(7)));
    }

    #[test]
    fn viewer_request_serde_round_trip_enzymes_preset() {
        let req = ViewerRequest::Enzymes {
            query: "unique".into(),
            op: EnzymeOp::Set,
            target: Target::active(),
            dam: true,
            dcm: true,
            cpg: false,
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
                target: Target::active(),
            },
        )
        .unwrap();
        assert_eq!(view.scroll_to, Some(2));
        assert!(
            matches!(view.selection.text_range(), Some(sel) if sel.anchor == 2 && sel.is_cursor())
        );
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
                target: Target::active(),
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
            location: crate::Location::simple(1..4),
            raw_kind: "CDS".into(),
            label: "gene".into(),
            strand: crate::Strand::Forward,
            qualifiers: Default::default(),
            lineage: None,
        }]);
        let minted = ann.iter().next().unwrap().id;
        let resp = dispatch(
            &mut view,
            &buf,
            &mut ann,
            &FakeBio::new(),
            ViewerRequest::ListFeatures {
                target: Target::active(),
            },
        )
        .unwrap();
        match resp {
            ViewerResponse::Features { features, .. } => {
                assert_eq!(features.len(), 1);
                assert_eq!(features[0].id, minted);
                assert_eq!(features[0].kind, "CDS");
                assert_eq!(features[0].span, Span::from_range(1..4));
            }
            other => panic!("expected Features, got {other:?}"),
        }
    }

    #[test]
    fn dto_span_serializes_as_start_len() {
        // P5b: FeatureInfo/PrimerSiteInfo carry a `span` that serializes natively
        // as `{start, len}` (the pre-release wire shape — no adapter). A wrapping
        // span round-trips exactly, unlike the old `{start, end}` which couldn't
        // represent an origin-crossing footprint.
        let fi = FeatureInfo {
            id: FeatureId(3),
            kind: "CDS".into(),
            label: "ori".into(),
            span: Span::new(16, 8), // wraps on L=20
            strand: Strand::Forward,
        };
        let json = serde_json::to_string(&fi).unwrap();
        assert!(
            json.contains("\"span\":{\"start\":16,\"len\":8}"),
            "span serializes as {{start,len}}: {json}"
        );
        let back: FeatureInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(back.span, Span::new(16, 8));
        assert_eq!(
            back.span.linear_pieces(20),
            crate::span::Pieces::Two(16..20, 0..4)
        );
    }

    #[test]
    fn dispatch_list_primers_returns_projection() {
        let (mut view, buf, _) = fixture();
        let mut ann = Annotations::default();
        let minted = ann.add_primer(Primer {
            id: Default::default(),
            name: "p1".into(),
            sequence: "ATGC".into(),
            binding: Some(Span::from_range(0..4)),
            strand: crate::Strand::Forward,
            qualifiers: Default::default(),
        });
        let resp = dispatch(
            &mut view,
            &buf,
            &mut ann,
            &FakeBio::new(),
            ViewerRequest::ListPrimers {
                target: Target::active(),
            },
        )
        .unwrap();
        match resp {
            ViewerResponse::Primers { primers, .. } => {
                assert_eq!(primers.len(), 1);
                assert_eq!(primers[0].id, minted);
                assert_eq!(primers[0].name, "p1");
                assert_eq!(primers[0].binding, Some(Span::from_range(0..4)));
                assert_eq!(primers[0].len, 4);
            }
            other => panic!("expected Primers, got {other:?}"),
        }
    }

    #[test]
    fn viewer_request_serde_round_trip_add_primer() {
        // strand omitted → defaults to "+"; name/start/end omitted stay None
        // (floating oligo). Method tag is snake_case.
        let json = r#"{"method":"add_primer","sequence":"ATGC"}"#;
        let req: ViewerRequest = serde_json::from_str(json).unwrap();
        assert!(matches!(
            req,
            ViewerRequest::AddPrimer { ref sequence, ref strand, name: None, start: None, end: None, .. }
            if sequence == "ATGC" && strand == "+"
        ));
        // Round-trips with a footprint.
        let full = ViewerRequest::AddPrimer {
            name: Some("p".into()),
            sequence: "ATGC".into(),
            start: Some(0),
            end: Some(4),
            strand: "-".into(),
            target: Target::active(),
        };
        let back: ViewerRequest =
            serde_json::from_str(&serde_json::to_string(&full).unwrap()).unwrap();
        assert!(matches!(
            back,
            ViewerRequest::AddPrimer {
                start: Some(0),
                end: Some(4),
                ..
            }
        ));
    }

    #[test]
    fn viewer_request_serde_round_trip_update_remove_primer() {
        let upd = ViewerRequest::UpdatePrimer {
            id: PrimerId(3),
            name: None,
            sequence: Some("ATG".into()),
            strand: None,
            start: None,
            end: Some(9),
            detach: false,
            target: Target::active(),
        };
        let json = serde_json::to_string(&upd).unwrap();
        assert!(json.contains(r#""method":"update_primer""#), "got {json}");
        // `detach: false` is the omitted default — it must not bloat the wire.
        assert!(!json.contains("detach"), "default detach elided: {json}");
        let back: ViewerRequest = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            back,
            ViewerRequest::UpdatePrimer {
                id: PrimerId(3),
                end: Some(9),
                detach: false,
                ..
            }
        ));

        // A detach request round-trips and carries the flag.
        let detach = ViewerRequest::UpdatePrimer {
            id: PrimerId(3),
            name: None,
            sequence: None,
            strand: None,
            start: None,
            end: None,
            detach: true,
            target: Target::active(),
        };
        let json = serde_json::to_string(&detach).unwrap();
        assert!(json.contains(r#""detach":true"#), "got {json}");
        let back: ViewerRequest =
            serde_json::from_str(&serde_json::to_string(&detach).unwrap()).unwrap();
        assert!(matches!(
            back,
            ViewerRequest::UpdatePrimer { detach: true, .. }
        ));

        // RescanPrimer round-trips.
        let rescan = ViewerRequest::RescanPrimer {
            id: PrimerId(5),
            target: Target::active(),
        };
        let json = serde_json::to_string(&rescan).unwrap();
        assert!(json.contains(r#""method":"rescan_primer""#), "got {json}");
        let back: ViewerRequest = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            back,
            ViewerRequest::RescanPrimer {
                id: PrimerId(5),
                ..
            }
        ));

        // AddPrimerSite round-trips; optional overhang/flank elide when None.
        let site = ViewerRequest::AddPrimerSite {
            id: PrimerId(3),
            enzyme: "BsaI".into(),
            overhang: Some("AATG".into()),
            flank: None,
            target: Target::active(),
        };
        let json = serde_json::to_string(&site).unwrap();
        assert!(json.contains(r#""method":"add_primer_site""#), "got {json}");
        assert!(json.contains(r#""overhang":"AATG""#), "got {json}");
        assert!(!json.contains("flank"), "None flank elided: {json}");
        let back: ViewerRequest = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            back,
            ViewerRequest::AddPrimerSite {
                id: PrimerId(3),
                ..
            }
        ));

        let rm = ViewerRequest::RemovePrimer {
            id: PrimerId(7),
            target: Target::active(),
        };
        let back: ViewerRequest =
            serde_json::from_str(&serde_json::to_string(&rm).unwrap()).unwrap();
        assert!(matches!(
            back,
            ViewerRequest::RemovePrimer {
                id: PrimerId(7),
                ..
            }
        ));
    }

    #[test]
    fn remove_feature_request_serde_round_trips_id() {
        let req = ViewerRequest::RemoveFeature {
            id: FeatureId(42),
            target: Target::active(),
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
                target: Target::active(),
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
                target: Target::active(),
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
            pattern: "GAATTC".into(),
            recognition: Span::new(0, 6),
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
                target: Target::active(),
                dam: true,
                dcm: true,
                cpg: false,
            },
        )
        .unwrap();
        assert!(view.cut_sites.is_empty());
        assert!(view.active_enzymes.is_empty());
        assert!(view.methyl_states.is_empty());
        assert!(matches!(resp, ViewerResponse::CutSites { count: 0, .. }));
    }

    #[test]
    fn dispatch_enzymes_caches_methyl_states_parallel_to_sites() {
        let (mut view, buf, mut ann) = fixture();
        let bio = FakeBio::new().with_site(0).with_site(10);
        let enzymes = |view: &mut View, ann: &mut Annotations, dam: bool| {
            dispatch(
                view,
                &buf,
                ann,
                &bio,
                ViewerRequest::Enzymes {
                    query: "EcoRI".into(),
                    op: EnzymeOp::Set,
                    target: Target::active(),
                    dam,
                    dcm: false,
                    cpg: false,
                },
            )
            .unwrap()
        };
        // Dam on: cache is populated, parallel to sites, and reflects the context.
        enzymes(&mut view, &mut ann, true);
        assert_eq!(view.methyl_states.len(), view.cut_sites.len());
        assert!(
            view.methyl_states
                .iter()
                .all(|s| *s == MethylState::Blocked)
        );
        // Re-run with Dam off: cache refreshes to the new context.
        enzymes(&mut view, &mut ann, false);
        assert!(
            view.methyl_states
                .iter()
                .all(|s| *s == MethylState::Cuttable)
        );
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
                target: Target::active(),
                dam: true,
                dcm: true,
                cpg: false,
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
                target: Target::active(),
            },
        )
        .unwrap();
        assert!(matches!(
            resp,
            ViewerResponse::SearchResults { count: 1, .. }
        ));
        if let ViewerResponse::SearchResults { hits, .. } = resp {
            assert_eq!(hits[0].span.start, 2);
        }
    }

    // ── Freshen-at-read: version-fingerprinted derived results ────────────────

    /// Convenience: run the `Enzymes` scan (dam/dcm off so methyl is Cuttable).
    fn scan(view: &mut View, buf: &Buffer, ann: &mut Annotations, bio: &FakeBio, query: &str) {
        dispatch(
            view,
            buf,
            ann,
            bio,
            ViewerRequest::Enzymes {
                query: query.into(),
                op: EnzymeOp::Set,
                target: Target::active(),
                dam: false,
                dcm: false,
                cpg: false,
            },
        )
        .unwrap();
    }

    #[test]
    fn scan_stamps_results_version_and_rescan_refreshes_after_edit() {
        let (mut view, mut buf, mut ann) = fixture();
        let bio = FakeBio::new().with_site(0);
        scan(&mut view, &buf, &mut ann, &bio, "EcoRI");
        assert_eq!(view.cut_sites.len(), 1);
        assert_eq!(view.results_version, Some(buf.version));
        assert!(
            !view.cut_sites_stale(buf.version),
            "fresh right after a scan"
        );

        // An edit bumps the buffer version; the overlay is now stale.
        buf.version += 1;
        assert!(view.cut_sites_stale(buf.version));

        // Freshen-at-read re-derives against the bumped version and re-stamps.
        // `rescan_if_stale` takes the view explicitly — no "active"/focus input —
        // which is exactly why a CLI-targeted, non-focused view stays correct.
        rescan_if_stale(&mut view, &buf, &bio);
        assert_eq!(view.results_version, Some(buf.version));
        assert!(!view.cut_sites_stale(buf.version));
        assert_eq!(view.cut_sites.len(), 1);
    }

    #[test]
    fn rescan_rescans_even_when_prior_scan_found_no_sites() {
        // Staleness keys on `active_enzymes` (the config), not `cut_sites` (the
        // output): an empty prior result is NOT proof of freshness — an edit can
        // introduce a site where the last scan found none.
        let (mut view, mut buf, mut ann) = fixture();
        let empty = FakeBio::new(); // finds nothing
        scan(&mut view, &buf, &mut ann, &empty, "EcoRI");
        assert!(view.cut_sites.is_empty());
        assert!(!view.active_enzymes.is_empty());

        buf.version += 1; // edit
        assert!(
            view.cut_sites_stale(buf.version),
            "empty prior result with active enzymes must read as stale"
        );

        let now_has_site = FakeBio::new().with_site(0);
        rescan_if_stale(&mut view, &buf, &now_has_site);
        assert_eq!(view.cut_sites.len(), 1, "rescan discovers the new site");
        assert_eq!(view.results_version, Some(buf.version));
    }

    #[test]
    fn rescan_clears_stale_search_hits_without_recomputing() {
        let (mut view, mut buf, mut ann) = fixture();
        let bio = FakeBio::new().with_hit(2, 6);
        dispatch(
            &mut view,
            &buf,
            &mut ann,
            &bio,
            ViewerRequest::Find {
                pattern: "ATGC".into(),
                mismatches: 0,
                target: Target::active(),
            },
        )
        .unwrap();
        assert_eq!(view.search_hits.len(), 1);
        assert_eq!(view.search_version, Some(buf.version));

        buf.version += 1; // edit invalidates the search
        assert!(view.search_stale(buf.version));
        rescan_if_stale(&mut view, &buf, &bio);
        assert!(
            view.search_hits.is_empty(),
            "stale search is cleared (query not retained), not re-run"
        );
        assert_eq!(view.search_version, None);
    }

    #[test]
    fn rescan_is_noop_with_no_active_enzymes_or_search() {
        // A view that never scanned/searched must not acquire results on freshen.
        let (mut view, mut buf, _ann) = fixture();
        let bio = FakeBio::new().with_site(0);
        buf.version += 5;
        rescan_if_stale(&mut view, &buf, &bio);
        assert!(view.cut_sites.is_empty());
        assert!(view.search_hits.is_empty());
        assert_eq!(view.results_version, None);
    }

    #[test]
    fn dispatch_find_empty_pattern_clears() {
        let (mut view, buf, mut ann) = fixture();
        view.search_hits.push(SearchHit {
            span: Span::from_range(0..4),
            strand: crate::Strand::Forward,
        });
        // Pre-populate a selection so we can verify clear-on-empty behavior.
        view.selection = ViewSelection::Text(Selection::range(0, 4));
        let resp = dispatch(
            &mut view,
            &buf,
            &mut ann,
            &FakeBio::new(),
            ViewerRequest::Find {
                pattern: "".into(),
                mismatches: 0,
                target: Target::active(),
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

#[cfg(test)]
mod target_tests {
    use super::*;

    /// The wire shape is unchanged for the pre-existing form: `Target` is
    /// flattened, so `view` still sits at the top level of the request object
    /// and an omitted target still means "the active view".
    #[test]
    fn view_targeting_is_wire_compatible() {
        let req: ViewerRequest =
            serde_json::from_str(r#"{"method":"goto","position":42,"view":3}"#).unwrap();
        match &req {
            ViewerRequest::GoTo { position, target } => {
                assert_eq!(*position, 42);
                assert_eq!(target.kind().unwrap(), TargetKind::View(ViewId(3)));
            }
            other => panic!("wrong variant: {other:?}"),
        }
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains(r#""view":3"#), "round trip: {json}");
        assert!(
            !json.contains("path"),
            "an unset path must not appear: {json}"
        );
    }

    #[test]
    fn an_omitted_target_is_the_active_view() {
        let req: ViewerRequest = serde_json::from_str(r#"{"method":"goto","position":1}"#).unwrap();
        let ViewerRequest::GoTo { target, .. } = &req else {
            panic!("wrong variant")
        };
        assert_eq!(target.kind().unwrap(), TargetKind::Active);
        // and it serializes back to the same minimal object
        assert_eq!(
            serde_json::to_string(&req).unwrap(),
            r#"{"method":"goto","position":1}"#
        );
    }

    /// The new arm: a file, addressable over the socket as well as the CLI.
    #[test]
    fn a_path_target_round_trips() {
        let req = ViewerRequest::GoTo {
            position: 7,
            target: Target::path("/tmp/p.gb"),
        };
        let json = serde_json::to_string(&req).unwrap();
        let back: ViewerRequest = serde_json::from_str(&json).unwrap();
        let ViewerRequest::GoTo { target, .. } = &back else {
            panic!("wrong variant")
        };
        assert_eq!(
            target.kind().unwrap(),
            TargetKind::Path(std::path::Path::new("/tmp/p.gb"))
        );
        assert!(target.is_path(), "runnable without a session");
    }

    /// `clap` rejects this on the command line, but a hand-written socket
    /// payload can carry both. Preferring one silently would be a wrong answer.
    #[test]
    fn naming_both_a_view_and_a_path_is_an_error() {
        let t = Target {
            view: Some(ViewId(1)),
            path: Some("/tmp/p.gb".into()),
        };
        let err = t.kind().unwrap_err();
        assert!(err.to_string().contains("mutually exclusive"), "{err}");
        assert!(!t.is_path(), "ambiguous targets are not locally runnable");
    }
}
