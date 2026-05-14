use std::path::PathBuf;

use clap::Parser;
use seqforge_core::ViewerRequest;

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
    /// Open a sequence file in the viewer
    Open { path: PathBuf },
    /// Close the current document
    Close,
    /// Navigate to a sequence position (1-based)
    #[command(name = "goto")]
    GoTo { position: usize },
    /// Search for a sequence pattern (IUPAC)
    Find {
        pattern: String,
        #[arg(short, long, default_value = "0")]
        mismatches: u8,
    },
    /// Show restriction sites for given enzymes
    Enzymes { enzymes: Vec<String> },
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
        Cmd::GoTo { position } => {
            seqforge_cli::dispatch_viewer_cmd(ViewerRequest::GoTo { position })
        }
        Cmd::Find { pattern, mismatches } => {
            seqforge_cli::dispatch_viewer_cmd(ViewerRequest::Find { pattern, mismatches })
        }
        Cmd::Enzymes { enzymes } => {
            seqforge_cli::dispatch_viewer_cmd(ViewerRequest::Enzymes { enzymes })
        }
    }
}
