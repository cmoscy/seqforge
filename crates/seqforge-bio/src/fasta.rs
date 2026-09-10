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

    // Reject multi-record files rather than silently concatenating them.
    // Before this guard, every line after the first header — including the
    // `>` header lines themselves — was folded into one "sequence", so a
    // 5-record file loaded as a single corrupt molecule with header text as
    // bases. Multi-record support proper (`path#RecordName` addressing) is
    // tracked in plans/assembly.md.
    let mut sequence: Vec<u8> = Vec::new();
    for line in lines {
        if let Some(next) = line.strip_prefix('>') {
            let next = next.split_whitespace().next().unwrap_or(next);
            return Err(BioError::Fasta(format!(
                "{} holds more than one record ({name:?}, {next:?}, …); \
                 SeqForge reads one sequence per file — split it first",
                path.display()
            )));
        }
        sequence.extend(
            line.bytes()
                .filter(|b| !b.is_ascii_whitespace())
                .map(|b| b.to_ascii_uppercase()),
        );
    }

    if sequence.is_empty() {
        return Err(BioError::EmptyFile);
    }

    Ok(Document {
        name,
        sequence,
        topology: Topology::Linear,
        features: Vec::new(),
        primers: Vec::new(),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn write_tmp(name: &str, body: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("seqforge-fasta-{name}.fa"));
        fs::write(&path, body).unwrap();
        path
    }

    #[test]
    fn single_record_loads() {
        let p = write_tmp("single", ">frag1 desc\nacgt\nAC GT\n");
        let doc = load(&p).unwrap();
        assert_eq!(doc.name, "frag1");
        assert_eq!(doc.sequence, b"ACGTACGT");
        fs::remove_file(p).ok();
    }

    #[test]
    fn multi_record_errors_instead_of_concatenating() {
        let p = write_tmp("multi", ">a\nACGT\n>b\nTTTT\n");
        let err = load(&p).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("more than one record"), "unexpected: {msg}");
        assert!(
            msg.contains("\"a\"") && msg.contains("\"b\""),
            "unexpected: {msg}"
        );
        fs::remove_file(p).ok();
    }
}
