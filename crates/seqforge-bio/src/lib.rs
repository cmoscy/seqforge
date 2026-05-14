// seqforge-bio: thin wrappers over na_seq + gb-io + bio; ported workflows

mod dna;
mod fasta;
mod genbank;
mod search;

pub use dna::{complement, reverse_complement};
pub use search::{find_cut_sites, find_iupac_matches};

use seqforge_core::Document;
use std::path::Path;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum BioError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("GenBank parse error: {0}")]
    GenBank(#[from] gb_io::reader::GbParserError),
    #[error("FASTA parse error: {0}")]
    Fasta(String),
    #[error("Unsupported file format: {0}")]
    UnsupportedFormat(String),
    #[error("File is empty or contains no sequences")]
    EmptyFile,
}

pub fn load(path: &Path) -> Result<Document, BioError> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();

    match ext.as_str() {
        "gb" | "gbk" | "genbank" => genbank::load(path),
        "fasta" | "fa" | "fna" | "ffn" | "faa" | "frn" => fasta::load(path),
        other => Err(BioError::UnsupportedFormat(other.to_owned())),
    }
}
