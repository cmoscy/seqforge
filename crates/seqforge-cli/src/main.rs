use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "seqforge", about = "SeqForge sequence tool CLI")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Print basic info about a sequence file
    Info {
        /// Path to a .gb or .fasta file
        path: PathBuf,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Info { path } => {
            let doc = seqforge_bio::load(&path)
                .with_context(|| format!("Failed to load {}", path.display()))?;

            println!("Name:     {}", doc.name);
            println!("Length:   {} bp", doc.len());
            println!("Topology: {:?}", doc.topology);
            println!("Features: {}", doc.features.len());
        }
    }

    Ok(())
}
