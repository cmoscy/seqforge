//! Projections that need `seqforge-bio`.
//!
//! `info`, `translate`, `orfs`, `find-primer-sites` and `digest` are read-only
//! derivations, so they belong with the other read verbs in
//! [`seqforge_core::dispatch`] — except that they need
//! `seqforge_bio::{translate, find_orfs, digest_projection, primer_sites}`, and
//! decision 9 forbids `core ──► bio`.
//!
//! Widening `BioOps` to reach them would push bio-shaped concerns into `core`'s
//! trait surface for the sake of five verbs. Instead they live here, above both
//! crates, in the shape `digest_projection` already established: take
//! `(&View, &Buffer, &Annotations)` plus the request's own parameters, return a
//! `ViewerResponse`. Both shells call [`dispatch`], which tries these first and
//! falls through to `core::dispatch` for everything else — the same
//! extend-then-delegate shape the GUI's command layer already uses.

use seqforge_core::{
    Annotations, Buffer, DispatchError, OrfInfo, Strand, Topology, View, ViewerRequest,
    ViewerResponse,
};

/// Run `req` against one document: bio projections here, everything else in
/// `core::dispatch`.
///
/// This is the single read entry point for both shells, so a verb cannot be
/// reachable from one face and not the other.
pub fn dispatch<B: seqforge_core::BioOps>(
    view: &mut View,
    buffer: &Buffer,
    annotations: &mut Annotations,
    bio: &B,
    req: ViewerRequest,
) -> Result<ViewerResponse, DispatchError> {
    match req {
        ViewerRequest::Info { .. } => Ok(info(buffer, annotations)),
        ViewerRequest::Translate {
            start,
            end,
            strand,
            frame,
            ..
        } => translate(buffer, start, end, &strand, frame),
        ViewerRequest::Orfs {
            min_aa,
            stop_to_stop,
            forward_only,
            ..
        } => Ok(orfs(buffer, min_aa, stop_to_stop, forward_only)),
        ViewerRequest::FindPrimerSites { oligo, .. } => Ok(primer_sites(buffer, &oligo)),
        ViewerRequest::Digest {
            ref enzymes,
            circular,
            ..
        } => Ok(digest(view, buffer, annotations, enzymes, circular)),
        other => seqforge_core::dispatch(view, buffer, annotations, bio, other),
    }
}

fn topology_str(t: Topology) -> String {
    format!("{t:?}").to_lowercase()
}

fn info(buffer: &Buffer, annotations: &Annotations) -> ViewerResponse {
    ViewerResponse::DocumentInfo {
        name: buffer.name.clone(),
        length: buffer.text.len(),
        topology: topology_str(buffer.topology),
        features: annotations.len(),
        primers: annotations.primers_len(),
        path: buffer.source_path.clone(),
    }
}

fn translate(
    buffer: &Buffer,
    start: Option<usize>,
    end: Option<usize>,
    strand: &str,
    frame: usize,
) -> Result<ViewerResponse, DispatchError> {
    let len = buffer.text.len();
    let start = start.unwrap_or(0);
    let end = end.unwrap_or(len);
    if start >= end || end > len {
        return Err(DispatchError::InvalidInput(format!(
            "range {start}..{end} is invalid for a sequence of length {len}"
        )));
    }
    let strand = match strand.trim() {
        "-" | "reverse" | "Reverse" => Strand::Reverse,
        _ => Strand::Forward,
    };
    let protein = seqforge_bio::translate(&buffer.text[start..end], strand, frame);
    Ok(ViewerResponse::Translation {
        name: buffer.name.clone(),
        start,
        end,
        strand: format!("{strand:?}").to_lowercase(),
        frame,
        length: protein.chars().count(),
        protein,
    })
}

fn orfs(buffer: &Buffer, min_aa: usize, stop_to_stop: bool, forward_only: bool) -> ViewerResponse {
    let found = seqforge_bio::find_orfs(&buffer.text, min_aa, !stop_to_stop, !forward_only);
    let orfs: Vec<OrfInfo> = found
        .iter()
        .map(|o| OrfInfo {
            start: o.start,
            end: o.end,
            strand: o.strand,
            frame: o.frame,
            aa_len: o.aa_len,
        })
        .collect();
    ViewerResponse::Orfs {
        name: buffer.name.clone(),
        count: orfs.len(),
        orfs,
    }
}

/// Where an ad-hoc oligo anneals.
///
/// Builds the same [`PrimerSiteInfo`] the Inspector and `list-primers` use
/// (`seqforge_bio::primer_sites`), so an ad-hoc oligo and an authored primer
/// are scored by one implementation — including the wrap-aware `anneal_tm_span`
/// that a hand-rolled copy here got wrong for origin-crossing sites.
///
/// [`PrimerSiteInfo`]: seqforge_core::PrimerSiteInfo
fn primer_sites(buffer: &Buffer, oligo: &str) -> ViewerResponse {
    let circular = matches!(buffer.topology, Topology::Circular);
    let sites = seqforge_bio::primer_sites(oligo, &buffer.text, circular);
    ViewerResponse::PrimerSites {
        oligo: oligo.to_uppercase(),
        count: sites.len(),
        sites,
    }
}

/// The virtual fragment set.
///
/// Methylation comes from the view (the molecule's authored Dam/Dcm/CpG state),
/// and `circular` may override the document's topology — the two axes on which
/// three separate `digest` implementations used to disagree.
fn digest(
    view: &View,
    buffer: &Buffer,
    annotations: &Annotations,
    enzymes: &[String],
    circular: bool,
) -> ViewerResponse {
    // `--enzymes` is repeatable, so join the occurrences; the query grammar
    // handles separators within one argument.
    let query = enzymes.join(" ");
    let circular = circular || matches!(buffer.topology, Topology::Circular);
    let (fragments, warnings, canonical) = seqforge_bio::digest_projection(
        &buffer.text,
        &buffer.name,
        circular,
        annotations,
        &query,
        &view.methylation,
    );
    ViewerResponse::Fragments {
        name: buffer.name.clone(),
        enzymes: canonical,
        count: fragments.len(),
        fragments,
        warnings,
    }
}

#[cfg(test)]
mod tests {
    //! Value tests, not parity tests.
    //!
    //! Both shells now call [`dispatch`], so a bug *inside* it is identical on
    //! every face and the cross-layer parity tests in `seqforge-cli` cannot see
    //! it — they pin addressing, which is a different property. Each parameter
    //! therefore needs an assertion that it actually changes the answer.

    use super::*;
    use crate::{Bio, Workspace};
    use seqforge_core::Target;

    fn fixture(name: &str) -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../seqforge-bio/tests/fixtures")
            .join(name)
    }

    fn run(file: &str, req: ViewerRequest) -> ViewerResponse {
        let bio = Bio;
        let mut ws = Workspace::default();
        let vid = ws.open_path(&fixture(file), &bio).expect("fixture opens");
        ws.with_buffer(vid, |v, b, a| dispatch(v, b, a, &bio, req))
            .expect("view resolves")
            .expect("projection succeeds")
    }

    fn orf_count(min_aa: usize, stop_to_stop: bool, forward_only: bool) -> usize {
        match run(
            "pUC19.gbk",
            ViewerRequest::Orfs {
                min_aa,
                stop_to_stop,
                forward_only,
                input: None,
                target: Target::active(),
            },
        ) {
            ViewerResponse::Orfs { count, orfs, .. } => {
                assert_eq!(count, orfs.len(), "envelope count must match items");
                count
            }
            other => panic!("expected Orfs, got {other:?}"),
        }
    }

    #[test]
    fn every_orfs_parameter_changes_the_answer() {
        let all = orf_count(30, false, false);
        assert_eq!(all, 13);
        assert_eq!(orf_count(30, false, true), 5, "--forward-only must filter");
        assert!(
            orf_count(100, false, false) < all,
            "--min-aa must filter: got {} vs {all}",
            orf_count(100, false, false)
        );
        assert_ne!(
            orf_count(30, true, false),
            all,
            "--stop-to-stop is a different scan"
        );
    }

    #[test]
    fn translate_honours_strand_and_range() {
        let go = |start, end, strand: &str| match run(
            "pUC19.gbk",
            ViewerRequest::Translate {
                start: Some(start),
                end: Some(end),
                strand: strand.into(),
                frame: 1,
                input: None,
                target: Target::active(),
            },
        ) {
            ViewerResponse::Translation {
                protein, length, ..
            } => {
                assert_eq!(length, protein.chars().count());
                protein
            }
            other => panic!("expected Translation, got {other:?}"),
        };
        assert_eq!(go(0, 30, "+"), "EIPTA*AMRK");
        assert_eq!(go(0, 30, "-"), "LSHSSRCRYL", "the minus strand differs");
        assert_ne!(go(3, 33, "+"), go(0, 30, "+"), "the range must matter");
    }

    #[test]
    fn translate_rejects_an_out_of_range_request() {
        let bio = Bio;
        let mut ws = Workspace::default();
        let vid = ws.open_path(&fixture("pUC19.gbk"), &bio).unwrap();
        let err = ws
            .with_buffer(vid, |v, b, a| {
                dispatch(
                    v,
                    b,
                    a,
                    &bio,
                    ViewerRequest::Translate {
                        start: Some(10),
                        end: Some(5),
                        strand: "+".into(),
                        frame: 1,
                        input: None,
                        target: Target::active(),
                    },
                )
            })
            .unwrap()
            .expect_err("start >= end is invalid");
        assert!(err.to_string().contains("invalid"), "{err}");
    }

    /// `--circular` was CLI-only before, so the socket could not ask the
    /// question. It has to actually override the document's topology.
    #[test]
    fn the_circular_override_changes_the_fragment_set() {
        let go = |circular| match run(
            "small_linear.fasta",
            ViewerRequest::Digest {
                enzymes: vec!["NheI".into()],
                circular,
                input: None,
                target: Target::active(),
            },
        ) {
            ViewerResponse::Fragments {
                count, fragments, ..
            } => {
                assert_eq!(count, fragments.len());
                count
            }
            other => panic!("expected Fragments, got {other:?}"),
        };
        // Joining the ends of a linear molecule fuses the two terminal
        // fragments into one.
        assert_eq!(go(false), 32, "linear");
        assert_eq!(go(true), 31, "circularized");
    }

    /// `--enzymes` is repeatable and each occurrence may itself hold a list;
    /// they have to join into one query rather than the last one winning.
    #[test]
    fn repeated_enzymes_arguments_all_contribute() {
        let go = |enzymes: Vec<String>| match run(
            "pUC19.gbk",
            ViewerRequest::Digest {
                enzymes,
                circular: false,
                input: None,
                target: Target::active(),
            },
        ) {
            ViewerResponse::Fragments { count, .. } => count,
            other => panic!("expected Fragments, got {other:?}"),
        };
        let one = go(vec!["EcoRI".into()]);
        let split = go(vec!["EcoRI".into(), "BamHI".into()]);
        let joined = go(vec!["EcoRI,BamHI".into()]);
        assert_eq!(split, joined, "repeated flags == one comma-separated flag");
        assert!(
            split > one,
            "adding an enzyme must add a cut: {split} vs {one}"
        );
    }

    #[test]
    fn info_reports_the_document_not_the_process() {
        match run(
            "pUC19.gbk",
            ViewerRequest::Info {
                input: None,
                target: Target::active(),
            },
        ) {
            ViewerResponse::DocumentInfo {
                length,
                topology,
                features,
                path,
                ..
            } => {
                assert_eq!(length, 2686);
                assert_eq!(topology, "circular");
                assert_eq!(features, 9);
                assert!(path.is_some());
            }
            other => panic!("expected DocumentInfo, got {other:?}"),
        }
    }
}
