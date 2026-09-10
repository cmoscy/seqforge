use std::path::PathBuf;

use clap::Parser;
use seqforge_core::{Target, ViewerRequest};

#[derive(Parser)]
#[command(name = "seqforge", about = "SeqForge sequence tool")]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

/// Top-level subcommands.
///
/// One question decides where a verb runs: **which document does it name?**
/// `--in <path>` resolves here, in this process, against a workspace that lives
/// for one request. `--view <n>` or no target at all names a document in the
/// running SeqForge and is forwarded over `SEQFORGE_SOCKET`. Verbs that are
/// *about* the session (`open`, `close`, `buffers`, `focus`, `new`) and every
/// write verb need that session; reads do not.
///
/// `tm` is the documented exception: it takes an oligo, not a document.
///
/// The viewer/editor surface is **flattened directly from
/// [`ViewerRequest`]** — its `clap::Subcommand` derive is the single source of
/// truth shared with the socket wire format (serde). Adding a `ViewerRequest`
/// variant gives it CLI + embedded-terminal reach with no second edit here.
// `Cmd::Viewer` carries the whole flattened `ViewerRequest`, whose `Assemble`
// variant is much larger than `Info { input }`. Boxing it is not an option:
// `#[command(flatten)]` needs the enum inline to project its variants into
// subcommands, and that projection is the single-source property decision 11
// asks for. One short-lived value is parsed per process.
#[allow(clippy::large_enum_variant)]
#[derive(clap::Subcommand)]
enum Cmd {
    // ── Sugar ─────────────────────────────────────────────────────────────────
    //
    // These name a document the old way — a bare positional path — and fold
    // into the same `ViewerRequest` the flattened surface below produces. They
    // exist so `seqforge info plasmid.gb` keeps working; the canonical forms
    // (`--in <path>` / `--view <n>` / the active view) come from `Viewer`.
    //
    // `info`, `translate`, `orfs` and `digest` take one positional, so the path
    // is simply optional and everything folds into one subcommand. `primers`
    // takes two, which would be ambiguous with an optional leading path, so it
    // stays a nested group and `find-primer-sites` is its canonical form.
    /// Inspect primers in a sequence file. Sugar for `list-primers --in` /
    /// `find-primer-sites --in`.
    Primers {
        #[command(subcommand)]
        cmd: PrimersCmd,
    },
    /// Melting temperature + GC of an oligo. Addresses no document.
    Tm {
        /// The oligo sequence, 5'→3'.
        oligo: String,
    },

    #[command(flatten)]
    Viewer(ViewerRequest),
}

#[derive(clap::Subcommand)]
enum PrimersCmd {
    /// List primers with derived attachment state + QC (Tm/GC/ΔG).
    List { input: PathBuf },
    /// Find binding sites for an oligo on the sequence (seed-and-extend).
    Find {
        input: PathBuf,
        /// The oligo sequence, 5'→3'.
        oligo: String,
    },
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        // Sugar: build the request the flattened surface would have built.
        Cmd::Tm { oligo } => seqforge_cli::run_tm(&oligo),
        Cmd::Primers { cmd } => seqforge_cli::dispatch_cmd(match cmd {
            PrimersCmd::List { input } => ViewerRequest::ListPrimers {
                target: Target::path(input),
            },
            PrimersCmd::Find { input, oligo } => ViewerRequest::FindPrimerSites {
                oligo,
                target: Target::path(input),
            },
        }),

        Cmd::Viewer(mut req) => {
            // Converge the sugar and the canonical form on one value before
            // anything routes or serializes it.
            req.fold_positional_target();
            seqforge_cli::dispatch_cmd(req)
        }
    }
}
