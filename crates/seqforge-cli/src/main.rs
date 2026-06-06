use std::path::PathBuf;

use clap::Parser;
use seqforge_core::{EnzymeOp, ViewId, ViewerRequest};

#[derive(Parser)]
#[command(name = "seqforge", about = "SeqForge sequence tool")]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

/// Top-level subcommands.
///
/// **File commands** (always run locally; no GUI required):
///   info, digest, annotate
///
/// **Viewer commands** (require a running SeqForge instance; use SEQFORGE_SOCKET):
///   open, close, goto, find, enzymes
#[derive(clap::Subcommand)]
enum Cmd {
    // ── File commands ─────────────────────────────────────────────────────────
    /// Print info about a sequence file
    Info { input: PathBuf },
    /// Digest a sequence file with restriction enzymes (post-MVP)
    Digest {
        input: PathBuf,
        #[arg(short, long)]
        enzymes: Vec<String>,
        #[arg(short, long)]
        output: PathBuf,
    },
    /// Annotate a sequence file (post-MVP)
    Annotate {
        input: PathBuf,
        #[arg(short, long)]
        output: PathBuf,
    },

    // ── Viewer commands (forwarded as JSON-RPC to the running GUI) ────────────
    //
    // View-scoped commands (goto/find/enzymes) accept `--view <ID>` for
    // explicit targeting. Omitted, they operate on the active view.
    // Stage 2.5d.
    /// Open a sequence file in the viewer
    Open { path: PathBuf },
    /// Close the current document
    Close,
    /// Navigate to a sequence position (1-based)
    #[command(name = "goto")]
    GoTo {
        position: usize,
        /// Target view id (omit to operate on the active view)
        #[arg(long)]
        view: Option<ViewId>,
    },
    /// Search for a sequence pattern (IUPAC)
    Find {
        pattern: String,
        #[arg(short, long, default_value = "0")]
        mismatches: u8,
        /// Target view id (omit to operate on the active view)
        #[arg(long)]
        view: Option<ViewId>,
    },
    /// Show restriction sites.
    ///
    /// `args` is a free-text query: a preset (`unique`, `unique and dual`,
    /// `non-cutters`), `all`, `none`/`clear` (or empty) to drop sites, or a
    /// whitespace/comma-separated list of enzyme names (e.g. `EcoRI BamHI`).
    Enzymes {
        /// Query tokens; joined with single spaces. Empty clears (with `set`).
        args: Vec<String>,
        /// set (replace, default), add, or remove against the active set.
        #[arg(long, value_enum, default_value_t = EnzymeOp::Set)]
        op: EnzymeOp,
        /// Target view id (omit to operate on the active view)
        #[arg(long)]
        view: Option<ViewId>,
    },
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        // ── File commands (always local) ──────────────────────────────────────
        Cmd::Info { input } => seqforge_cli::run_info(&input),
        Cmd::Digest { .. } | Cmd::Annotate { .. } => {
            anyhow::bail!("not yet implemented (post-MVP)")
        }

        // ── Viewer commands (via JSON-RPC socket) ─────────────────────────────
        Cmd::Open { path } => seqforge_cli::dispatch_viewer_cmd(ViewerRequest::Open { path }),
        Cmd::Close => seqforge_cli::dispatch_viewer_cmd(ViewerRequest::Close),
        Cmd::GoTo { position, view } => {
            seqforge_cli::dispatch_viewer_cmd(ViewerRequest::GoTo { position, view })
        }
        Cmd::Find { pattern, mismatches, view } => seqforge_cli::dispatch_viewer_cmd(
            ViewerRequest::Find { pattern, mismatches, view },
        ),
        Cmd::Enzymes { args, op, view } => {
            let query = args.join(" ");
            seqforge_cli::dispatch_viewer_cmd(ViewerRequest::Enzymes { query, op, view })
        }
    }
}
