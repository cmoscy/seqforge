// seqforge-bio: thin wrappers over gb-io + bio + seqforge-restriction; ported workflows

mod dna;
mod enzyme_query;
mod fasta;
mod genbank;
mod primer;
mod search;
mod translate;

pub use dna::{complement, reverse_complement};
// Thermodynamics live in the pure, zero-dep `seqforge-thermo` crate (vendored
// seqfold). `bio → thermo` is the only cross-crate edge the primer/thermo work
// adds; `core` never depends on thermo (Tm/GC are derived, never stored). The
// thin `tm`/`gc` surface is re-exported here so `-cli`/`-app` reach it through
// `bio` without naming `thermo` directly — the same boundary shape as
// `restriction` (see docs/architecture.md).
pub use enzyme_query::{
    EnzymePreset, EnzymeQuery, parse_enzyme_query, resolve_query, resolve_query_names,
};
pub use primer::{
    AnnealSettings, AnnealedBase, AttachmentState, DesignError, EnzymeSpec, PrimerAttachment,
    PrimerBinding, PrimerDecomposition, PrimerQc, PrimerQcPlusAnneal, anneal_tm,
    classify_attachment, decompose_primer, enzyme_catalog, enzyme_cuts, find_primer_binding_sites,
    primer_infos, primer_qc, primer_qc_with_anneal, restriction_tail,
};
pub use search::{find_cut_sites, find_iupac_matches, methyl_states_for_sites};
pub use seqforge_thermo::{
    DEFAULT_FOLD_TEMP_C, FoldError, TmError, duplex_tm, gc, hairpin_dg, self_dimer_dg, tm,
};
pub use translate::{Orf, find_orfs, translate};

use seqforge_core::{Annotations, Buffer, Document};
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
    #[error("write error: {0}")]
    Write(String),
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

/// Write a buffer + annotations to `path`, dispatching on extension.
/// GenBank preserves features (raw_kind, `Option` qualifiers, provenance);
/// FASTA writes the sequence only.
pub fn save(buf: &Buffer, ann: &Annotations, path: &Path) -> Result<(), BioError> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();

    match ext.as_str() {
        "gb" | "gbk" | "genbank" => genbank::write(buf, ann, path),
        "fasta" | "fa" | "fna" | "ffn" | "faa" | "frn" => fasta::write(buf, path),
        other => Err(BioError::UnsupportedFormat(other.to_owned())),
    }
}
