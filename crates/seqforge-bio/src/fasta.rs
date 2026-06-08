use seqforge_core::{Buffer, Document, Topology};
use std::fmt::Write as _;
use std::fs;
use std::path::Path;

use crate::BioError;

/// Line width for wrapped FASTA sequence output.
const WRAP: usize = 80;

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

/// Write a `Buffer` to a FASTA file at `path`. Features are not represented
/// in FASTA and are dropped (the caller chooses the format). Header is
/// `buf.name`; sequence is wrapped at [`WRAP`] columns.
pub fn write(buf: &Buffer, path: &Path) -> Result<(), BioError> {
    let mut out = String::with_capacity(buf.text.len() + buf.text.len() / WRAP + 16);
    writeln!(out, ">{}", buf.name).expect("write to String is infallible");
    for chunk in buf.text.chunks(WRAP) {
        out.push_str(&String::from_utf8_lossy(chunk));
        out.push('\n');
    }
    fs::write(path, out)?;
    Ok(())
}
