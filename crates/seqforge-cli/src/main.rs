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
/// **Viewer / editor commands** (require a running SeqForge instance via
/// `SEQFORGE_SOCKET`): open, close, goto, find, enzymes, and all v0.2 editor
/// ops (insert, delete, replace, reverse-complement / rc, cut, copy, paste,
/// add-feature, remove-feature, rename-feature, save, save-as, undo, redo).
///
/// The viewer/editor surface is **flattened directly from
/// [`ViewerRequest`]** — its `clap::Subcommand` derive is the single source of
/// truth shared with the socket wire format (serde). Adding a `ViewerRequest`
/// variant gives it CLI + embedded-terminal reach with no second edit here.
#[derive(clap::Subcommand)]
enum Cmd {
    // ── File commands (always run locally; no GUI required) ───────────────────
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
    /// Translate a (sub)range of a sequence file to protein (no GUI needed).
    Translate {
        input: PathBuf,
        /// 0-based start of the range (default: 0).
        #[arg(long)]
        start: Option<usize>,
        /// 0-based exclusive end of the range (default: sequence length).
        #[arg(long)]
        end: Option<usize>,
        /// Strand: `+` (forward) or `-` (reverse complement).
        #[arg(long, default_value = "+")]
        strand: String,
        /// Reading frame as GenBank codon_start: 1, 2, or 3.
        #[arg(long, default_value_t = 1)]
        frame: usize,
    },
    /// Find open reading frames in a sequence file (no GUI needed).
    Orfs {
        input: PathBuf,
        /// Minimum ORF length in amino acids.
        #[arg(long, default_value_t = 30)]
        min_aa: usize,
        /// Report stop-to-stop ORFs instead of Met-to-stop.
        #[arg(long)]
        stop_to_stop: bool,
        /// Only scan the forward strand.
        #[arg(long)]
        forward_only: bool,
    },

    // ── Viewer / editor commands (forwarded as JSON-RPC to the running GUI) ───
    //
    // Flattened from `ViewerRequest`: each variant becomes a top-level
    // subcommand. View-scoped variants accept `--view <ID>` for explicit
    // targeting; omitted, they operate on the active view (Stage 2.5d).
    #[command(flatten)]
    Viewer(ViewerRequest),
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        // ── File commands (always local) ──────────────────────────────────────
        Cmd::Info { input } => seqforge_cli::run_info(&input),
        Cmd::Translate {
            input,
            start,
            end,
            strand,
            frame,
        } => seqforge_cli::run_translate(&input, start, end, &strand, frame),
        Cmd::Orfs {
            input,
            min_aa,
            stop_to_stop,
            forward_only,
        } => seqforge_cli::run_orfs(&input, min_aa, stop_to_stop, forward_only),
        Cmd::Digest { .. } | Cmd::Annotate { .. } => {
            anyhow::bail!("not yet implemented (post-MVP)")
        }

        // ── Viewer / editor commands (via JSON-RPC socket) ────────────────────
        // One arm for the whole forwarded surface — no per-variant mapping.
        Cmd::Viewer(req) => seqforge_cli::dispatch_viewer_cmd(req),
    }
}
