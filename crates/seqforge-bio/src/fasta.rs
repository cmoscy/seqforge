use seqforge_core::{Document, Topology};
use std::fs;
use std::path::Path;

use crate::BioError;

pub fn load(path: &Path) -> Result<Document, BioError> {
    let raw = fs::read_to_string(path)?;
    let mut lines = raw.lines();

    let header = lines
        .next()
        .and_then(|l| l.strip_prefix('>'))
        .ok_or_else(|| BioError::Fasta("Missing FASTA header".to_owned()))?;

    let name = header
        .split_whitespace()
        .next()
        .unwrap_or(header)
        .to_owned();

    let sequence: Vec<u8> = lines
        .flat_map(|l| l.bytes())
        .filter(|b| !b.is_ascii_whitespace())
        .map(|b| b.to_ascii_uppercase())
        .collect();

    if sequence.is_empty() {
        return Err(BioError::EmptyFile);
    }

    Ok(Document {
        name,
        sequence,
        topology: Topology::Linear,
        features: Vec::new(),
        source_path: Some(path.to_owned()),
    })
}
