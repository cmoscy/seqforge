use anyhow::Context;
use clap::Subcommand;
use std::path::{Path, PathBuf};

#[derive(Subcommand)]
pub enum Commands {
    /// Print basic info about a sequence file
    Info {
        /// Path to a .gb or .fasta file
        path: PathBuf,
    },
}

pub fn run_info(path: &Path) -> anyhow::Result<()> {
    let doc = seqforge_bio::load(path)
        .with_context(|| format!("Failed to load {}", path.display()))?;
    println!("Name:     {}", doc.name);
    println!("Length:   {} bp", doc.len());
    println!("Topology: {:?}", doc.topology);
    println!("Features: {}", doc.features.len());
    Ok(())
}
