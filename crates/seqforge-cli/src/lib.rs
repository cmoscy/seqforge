use anyhow::Context;
use interprocess::local_socket::{GenericFilePath, Stream, ToFsName, prelude::*};
use seqforge_core::{ViewerRequest, ViewerResponse};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;

// ── Local verbs ───────────────────────────────────────────────────────────────

/// Melting temperature + GC of an oligo — a pure, local derivation (no running
/// GUI, no file). Reaches the vendored seqfold engine through `seqforge-bio`'s
/// thin `tm`/`gc` surface (`bio → thermo`; `core` never sees thermo). Tm is the
/// nearest-neighbour model (SantaLucia NN + Owczarzy-2008 salt), in °C.
pub fn run_tm(oligo: &str) -> anyhow::Result<()> {
    let tm = seqforge_bio::tm(oligo)
        .map_err(|e| anyhow::anyhow!("cannot compute Tm for {oligo:?}: {}", e.0))?;
    let hairpin = seqforge_bio::hairpin_dg(oligo, seqforge_bio::DEFAULT_FOLD_TEMP_C);
    let dimer = seqforge_bio::self_dimer_dg(oligo, seqforge_bio::DEFAULT_FOLD_TEMP_C);
    let out = serde_json::json!({
        "kind": "oligo_tm",
        "oligo": oligo.to_uppercase(),
        "length": oligo.len(),
        "tm": tm,
        "gc": seqforge_bio::gc(oligo),
        "hairpin_dg": hairpin.ok(),
        "self_dimer_dg": dimer.ok(),
    });
    println!("{}", serde_json::to_string_pretty(&out)?);
    Ok(())
}

// ── Routing: where does this command's document come from? ────────────────────

/// Where a command's document comes from, and therefore who can run it
/// (ROADMAP decision 27).
///
/// This is what replaced the old two-tier command split. "Needs a GUI" is not a
/// property of the *verb* — every verb is `open a document, do something, maybe
/// write a document` — it is a property of where the document lives. A request
/// naming only file paths can run in this process; one naming a session buffer
/// or the active view has to reach the session that owns it.
#[derive(Debug, PartialEq, Eq)]
pub enum DocSource {
    /// Every input is a path on disk — runnable here, no socket required.
    Paths,
    /// At least one input names live session state (`buffer:<n>`, or an
    /// implicit "the active view"). Must be forwarded.
    Session,
}

impl DocSource {
    /// Classify a request. Only `Assemble` can currently go either way; every
    /// other `ViewerRequest` variant is view-scoped and therefore `Session`.
    pub fn of(req: &ViewerRequest) -> Self {
        match req {
            ViewerRequest::Assemble { inputs, .. } => {
                // No inputs at all means "the recipe open in the workbench".
                if inputs.is_empty() || inputs.iter().any(|t| t.starts_with("buffer:")) {
                    DocSource::Session
                } else {
                    DocSource::Paths
                }
            }
            // Every other verb addresses one document, so its `Target` decides.
            // `--in` names a file we can open here; `--view` and the default
            // (the active view) name session state we cannot see.
            other => match other.target() {
                Some(t) if t.is_path() => DocSource::Paths,
                _ => DocSource::Session,
            },
        }
    }
}

/// Resolve a file-targeted request in this process, against a throwaway
/// workspace, and return the response.
///
/// The whole path is three steps because nothing here is new: `open_path` mints
/// the buffer and a view, `core::dispatch` is already session-free (it takes
/// `(&mut View, &Buffer, &mut Annotations, &B, req)`), and the response
/// serializes to the same JSON the socket returns. The workspace — and with it
/// the buffer's undo history — is dropped when the process exits, which is why
/// `undo`/`redo` stay session verbs: there is no previous command to reverse.
///
/// Kept separate from printing so tests can assert that this path — the one
/// that actually resolves a `Target::Path` — agrees with a session addressing
/// the same document by `ViewId`. That is the parity property; comparing two
/// `dispatch` calls cannot express it, because no `dispatch` arm reads its
/// target.
pub fn resolve_on_file(req: ViewerRequest) -> anyhow::Result<ViewerResponse> {
    if !seqforge_session::file_address_observable(&req) {
        anyhow::bail!("{}", seqforge_session::file_write_refused_msg(&req));
    }
    let path = req
        .target()
        .and_then(|t| t.path.clone())
        .ok_or_else(|| anyhow::anyhow!("internal: routed a request with no file target"))?;
    seqforge_session::execute_on_file(&path, req).map_err(|e| anyhow::anyhow!("{e}"))
}

/// Run a file-targeted request in this process and print the response.
fn run_on_file(req: ViewerRequest) -> anyhow::Result<()> {
    let resp = resolve_on_file(req)?;
    println!("{}", serde_json::to_string_pretty(&resp)?);
    Ok(())
}

/// Run a request here if its document is on disk; otherwise forward it to the
/// running SeqForge over the socket.
///
/// One entry point for the whole verb surface — the CLI no longer decides
/// local-vs-remote by which enum a command was declared in.
pub fn dispatch_cmd(req: ViewerRequest) -> anyhow::Result<()> {
    match DocSource::of(&req) {
        DocSource::Paths => match req {
            // Path-only assemble needs no pre-opened document — bins are files.
            req @ ViewerRequest::Assemble { .. } => {
                let mut ws = seqforge_session::Workspace::default();
                let mut host = seqforge_session::NullHost::default();
                let seqforge_session::Executed { response, .. } =
                    seqforge_session::execute_bio(&mut ws, &mut host, req)
                        .map_err(|e| anyhow::anyhow!("{e}"))?;
                println!("{}", serde_json::to_string_pretty(&response)?);
                Ok(())
            }
            req => run_on_file(req),
        },
        DocSource::Session => dispatch_viewer_cmd(req),
    }
}

// ── Viewer command socket dispatch ────────────────────────────────────────────

/// Send a `ViewerRequest` to a running SeqForge GUI via the local socket
/// using the JSON-RPC 2.0 wire format.
///
/// Reads `SEQFORGE_SOCKET` from the environment. That value is a filesystem
/// Unix-domain socket path on macOS/Linux, or a named-pipe path
/// (`\\.\pipe\seqforge-<pid>`) on Windows. If unset, the command cannot be
/// delivered and an error is returned.
pub fn dispatch_viewer_cmd(req: ViewerRequest) -> anyhow::Result<()> {
    let socket_path = std::env::var("SEQFORGE_SOCKET").map_err(|_| {
        anyhow::anyhow!("no SeqForge instance running (SEQFORGE_SOCKET is not set)")
    })?;

    let name = Path::new(&socket_path)
        .as_os_str()
        .to_fs_name::<GenericFilePath>()
        .with_context(|| format!("invalid SEQFORGE_SOCKET value {socket_path}"))?;
    let mut stream = Stream::connect(name)
        .with_context(|| format!("could not connect to SeqForge socket at {socket_path}"))?;

    // Serialize the ViewerRequest as a JSON-RPC 2.0 request.
    // ViewerRequest uses serde tag="method", so we merge method into params.
    let req_value = serde_json::to_value(&req)?;
    let method = req_value
        .get("method")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_owned();
    let mut params = match req_value {
        serde_json::Value::Object(mut m) => {
            m.remove("method");
            serde_json::Value::Object(m)
        }
        _ => serde_json::Value::Null,
    };
    if matches!(params, serde_json::Value::Object(ref m) if m.is_empty()) {
        params = serde_json::Value::Null;
    }

    let rpc = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": method,
        "params": params,
    });
    let json = serde_json::to_string(&rpc)?;
    stream.write_all(format!("{json}\n").as_bytes())?;
    stream.flush()?;

    // Read and parse the JSON-RPC response.
    let mut response_line = String::new();
    BufReader::new(&stream).read_line(&mut response_line)?;
    let response: serde_json::Value = serde_json::from_str(response_line.trim())
        .context("invalid JSON-RPC response from SeqForge")?;

    if let Some(err) = response.get("error") {
        let msg = err
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown error");
        anyhow::bail!("SeqForge rejected command: {msg}");
    }

    // Pretty-print the result so agents can consume it.
    if let Some(result) = response.get("result") {
        println!("{}", serde_json::to_string_pretty(result)?);
    }

    Ok(())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use seqforge_core::ViewerRequest;

    #[test]
    fn viewer_cmd_fails_without_socket_env() {
        // Safety: test binary is single-threaded by default; no concurrent env reads.
        unsafe { std::env::remove_var("SEQFORGE_SOCKET") };
        let err = super::dispatch_viewer_cmd(ViewerRequest::Close).unwrap_err();
        assert!(err.to_string().contains("SEQFORGE_SOCKET"));
    }
}

#[cfg(test)]
mod primer_tests {
    use std::path::PathBuf;

    fn puc19() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../seqforge-bio/tests/fixtures/pUC19.gbk")
    }

    use seqforge_core::{Target, ViewerRequest, ViewerResponse};

    #[test]
    fn primers_list_runs_on_fixture() {
        let resp = super::resolve_on_file(ViewerRequest::ListPrimers {
            target: Target::path(puc19()),
        })
        .expect("fixture projects");
        let ViewerResponse::Primers { count, primers } = resp else {
            panic!("expected Primers, got {resp:?}")
        };
        assert_eq!(count, primers.len(), "envelope count must match the items");
        assert!(count > 0, "the fixture has primers");
    }

    /// A cloning primer must surface its 5' tail. The footprint alone does not
    /// say what the oligo adds, and for a tailed primer the tail *is* the point
    /// — it is what makes the oligo usable as a PCR fragment source.
    #[test]
    fn find_primer_sites_reports_the_tail_separately() {
        // An M13 anneal with an EcoRI site hung off the 5' end.
        let resp = super::resolve_on_file(ViewerRequest::FindPrimerSites {
            oligo: "GAATTCGTAAAACGACGGCCAGT".into(),
            target: Target::path(puc19()),
        })
        .expect("fixture projects");
        let ViewerResponse::PrimerSites {
            oligo,
            count,
            sites,
        } = resp
        else {
            panic!("expected PrimerSites, got {resp:?}")
        };
        assert_eq!(oligo, "GAATTCGTAAAACGACGGCCAGT");
        assert_eq!(count, sites.len());
        let site = sites.first().expect("the M13 site is found");
        assert_eq!(
            site.tail, "GAATTC",
            "the tail is the added restriction site"
        );
        assert_eq!(site.tail_len, 6);
        assert_eq!(
            site.span.len,
            "GTAAAACGACGGCCAGT".len(),
            "the footprint is the annealed region, not the whole oligo"
        );
        assert!(site.anneal_tm.is_some(), "a clean anneal has a Tm");
    }

    /// `info` against a file reports the document, not the process.
    #[test]
    fn info_projects_the_document() {
        let resp = super::resolve_on_file(ViewerRequest::Info {
            input: None,
            target: Target::path(puc19()),
        })
        .expect("fixture projects");
        let ViewerResponse::DocumentInfo {
            length,
            topology,
            path,
            ..
        } = resp
        else {
            panic!("expected DocumentInfo, got {resp:?}")
        };
        assert_eq!(length, 2686, "pUC19");
        assert_eq!(topology, "circular");
        assert!(path.is_some(), "a file-targeted read knows its path");
    }
}

#[cfg(test)]
mod routing_tests {
    use super::DocSource;
    use seqforge_core::{Target, ViewerRequest};

    fn assemble(inputs: &[&str]) -> ViewerRequest {
        ViewerRequest::Assemble {
            inputs: inputs.iter().map(|s| s.to_string()).collect(),
            method: "ligate".into(),
            topology: "circular".into(),
            enzymes: None,
            expand: "all-to-all".into(),
            emit_recipe: None,
            dry_run: false,
            fidelity_dataset: None,
            fidelity_matrix: false,
            out: None,
            format: "genbank".into(),
            name_template: None,
            combos: None,
            origin: None,
        }
    }

    /// The rule that replaced the two-tier command split: routing follows the
    /// document, not the verb (ROADMAP decision 27).
    #[test]
    fn path_inputs_run_locally() {
        assert_eq!(
            DocSource::of(&assemble(&["a.gb", "parts/*.gb@BsaI..BsaI"])),
            DocSource::Paths,
            "nothing here needs a session, so no socket is required"
        );
    }

    #[test]
    fn a_buffer_input_needs_the_session_that_owns_it() {
        assert_eq!(
            DocSource::of(&assemble(&["a.gb", "buffer:3@BsaI..BsaI"])),
            DocSource::Session,
            "one live source is enough to make the whole request session-bound"
        );
    }

    #[test]
    fn no_inputs_means_the_open_workbench_recipe() {
        assert_eq!(DocSource::of(&assemble(&[])), DocSource::Session);
    }

    #[test]
    fn view_scoped_verbs_are_always_session_bound() {
        assert_eq!(
            DocSource::of(&ViewerRequest::GoTo {
                position: 10,
                target: Target::active()
            }),
            DocSource::Session
        );
    }
}

/// The parity property the document target actually buys: **the two resolution
/// layers reach the same document**.
///
/// `resolve_on_file` opens a throwaway workspace from a `Target::Path`; a
/// session already holds the document and addresses it by `ViewId`. Those are
/// different code paths — `DocSource::of` routing, `open_path`, and the GUI's
/// `resolve_path_target` all sit between a request and its buffer — and they
/// are where a drift like the `digest` methylation bug would reappear.
///
/// This cannot be written inside `seqforge-session`: `dispatch` ignores the
/// target it is given, so comparing two `dispatch` calls compares nothing.
#[cfg(test)]
mod target_parity_tests {
    use seqforge_core::{Target, ViewerRequest, ViewerResponse};

    fn fixture() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../seqforge-bio/tests/fixtures/pUC19.gbk")
    }

    /// Resolve `req` both ways and return the two responses as JSON.
    ///
    /// `by_path` goes through the real CLI local runner. `by_view` stands in for
    /// a running session: the document is already open, and the request names it
    /// by id.
    fn both_layers(mut req: ViewerRequest) -> (String, String) {
        let path = fixture();

        *req.target_mut().expect("verb carries a target") = Target::path(&path);
        let by_path = super::resolve_on_file(req.clone()).expect("file target resolves");

        let bio = seqforge_session::Bio;
        let mut ws = seqforge_session::Workspace::default();
        let vid = ws.open_path(&path, &bio).expect("fixture opens");
        *req.target_mut().unwrap() = Target::view(vid);
        let mut host = seqforge_session::NullHost::default();
        let seqforge_session::Executed {
            response: by_view, ..
        } = seqforge_session::execute(&mut ws, &mut host, &bio, req)
            .expect("view-targeted execute succeeds");

        (json(&by_path), json(&by_view))
    }

    fn json(r: &ViewerResponse) -> String {
        serde_json::to_string(r).unwrap()
    }

    #[test]
    fn list_features_agrees_across_resolution_layers() {
        let (a, b) = both_layers(ViewerRequest::ListFeatures {
            target: Target::active(),
        });
        assert_eq!(a, b);
        assert!(a.contains("\"kind\":\"features\""), "{a}");
    }

    #[test]
    fn find_agrees_across_resolution_layers() {
        let (a, b) = both_layers(ViewerRequest::Find {
            pattern: "GAATTC".into(),
            mismatches: 0,
            target: Target::active(),
        });
        assert_eq!(a, b);
        // A fixture with no hits would make this vacuous.
        assert!(!a.contains("\"count\":0"), "fixture must have a hit: {a}");
    }

    /// The enzyme chain is where the methylation drift lived: query resolution,
    /// scan, and methylation verdicts all have to survive both layers.
    #[test]
    fn enzymes_agrees_across_resolution_layers() {
        let (a, b) = both_layers(ViewerRequest::Enzymes {
            query: "unique".into(),
            op: Default::default(),
            dam: true,
            dcm: true,
            cpg: false,
            target: Target::active(),
        });
        assert_eq!(a, b);
        assert!(!a.contains("\"count\":0"), "fixture must cut: {a}");
    }

    /// Methylation is a *parameter* on both layers now, not a hardcoded default
    /// on one of them. Flipping it must move both answers, together — the
    /// regression test for the drift decision 27 describes.
    #[test]
    fn methylation_is_honoured_by_both_resolution_layers() {
        let on = |dam| ViewerRequest::Enzymes {
            query: "all".into(),
            op: Default::default(),
            dam,
            dcm: true,
            cpg: false,
            target: Target::active(),
        };
        let (dam_on_path, dam_on_view) = both_layers(on(true));
        let (dam_off_path, dam_off_view) = both_layers(on(false));

        assert_eq!(dam_on_path, dam_on_view);
        assert_eq!(dam_off_path, dam_off_view);
        assert_ne!(
            dam_on_path, dam_off_path,
            "Dam must change the verdicts, or this test proves nothing"
        );
    }

    /// The verbs folded in from the CLI-local tier. Before, these could only
    /// name a file, so "the same request against the active document" had no
    /// expression and this test could not be written.
    #[test]
    fn info_agrees_across_resolution_layers() {
        let (a, b) = both_layers(ViewerRequest::Info {
            input: None,
            target: Target::active(),
        });
        assert_eq!(a, b);
        assert!(a.contains("\"kind\":\"document_info\""), "{a}");
    }

    #[test]
    fn translate_agrees_across_resolution_layers() {
        let (a, b) = both_layers(ViewerRequest::Translate {
            start: Some(0),
            end: Some(30),
            strand: "+".into(),
            frame: 1,
            input: None,
            target: Target::active(),
        });
        assert_eq!(a, b);
        assert!(a.contains("\"protein\""), "{a}");
    }

    #[test]
    fn orfs_agrees_across_resolution_layers() {
        let (a, b) = both_layers(ViewerRequest::Orfs {
            min_aa: 30,
            stop_to_stop: false,
            forward_only: false,
            input: None,
            target: Target::active(),
        });
        assert_eq!(a, b);
        assert!(!a.contains("\"count\":0"), "the fixture has ORFs: {a}");
    }

    #[test]
    fn find_primer_sites_agrees_across_resolution_layers() {
        let (a, b) = both_layers(ViewerRequest::FindPrimerSites {
            oligo: "GAATTCGTAAAACGACGGCCAGT".into(),
            target: Target::active(),
        });
        assert_eq!(a, b);
        assert!(a.contains("\"tail\":\"GAATTC\""), "{a}");
    }

    /// `digest` was the one verb declared twice — a CLI-local command and a
    /// session command sharing a name via `#[command(skip)]`, taking their
    /// enzymes differently. One verb now, so the two faces can be compared.
    #[test]
    fn digest_agrees_across_resolution_layers() {
        let (a, b) = both_layers(ViewerRequest::Digest {
            enzymes: vec!["EcoRI".into(), "BamHI".into()],
            circular: false,
            input: None,
            target: Target::active(),
        });
        assert_eq!(a, b);
        assert!(a.contains("\"kind\":\"fragments\""), "{a}");
    }

    /// `--circular` is expressible on both faces now. It used to be CLI-only,
    /// so the socket could not ask the question at all.
    #[test]
    fn the_circular_override_is_honoured_by_both_layers() {
        let cut = |circular| ViewerRequest::Digest {
            enzymes: vec!["EcoRI".into()],
            circular,
            input: None,
            target: Target::active(),
        };
        let (lin_path, lin_view) = both_layers(cut(false));
        let (circ_path, circ_view) = both_layers(cut(true));
        assert_eq!(lin_path, lin_view);
        assert_eq!(circ_path, circ_view);
    }

    #[test]
    fn list_primers_agrees_across_resolution_layers() {
        let (a, b) = both_layers(ViewerRequest::ListPrimers {
            target: Target::active(),
        });
        assert_eq!(a, b);
    }

    /// A write verb routed to a file must fail cleanly rather than half-apply.
    #[test]
    fn a_write_verb_against_a_file_is_refused() {
        let err = super::resolve_on_file(ViewerRequest::Insert {
            pos: 0,
            bases: "ATGC".into(),
            target: Target::path(fixture()),
        })
        .expect_err("writing through a file target is not implemented");
        assert!(
            err.to_string().contains("session") || err.to_string().contains("live"),
            "{err}"
        );
    }

    #[test]
    fn pcr_against_a_file_is_refused() {
        let err = super::resolve_on_file(ViewerRequest::Pcr {
            fwd: seqforge_core::PrimerId(1),
            rev: seqforge_core::PrimerId(2),
            name: None,
            target: Target::path(fixture()),
        })
        .expect_err("pcr against a file needs a session");
        assert!(
            err.to_string().contains("pcr") || err.to_string().contains("session"),
            "{err}"
        );
    }

    #[test]
    fn assemble_dry_run_returns_typed_response() {
        let path = fixture();
        let token = format!("{}@EcoRI..EcoRI", path.display());
        let mut ws = seqforge_session::Workspace::default();
        let mut host = seqforge_session::NullHost::default();
        let seqforge_session::Executed { response, opened } = seqforge_session::execute_bio(
            &mut ws,
            &mut host,
            ViewerRequest::Assemble {
                inputs: vec![token],
                method: "ligate".into(),
                topology: "circular".into(),
                enzymes: None,
                expand: "all-to-all".into(),
                emit_recipe: None,
                dry_run: true,
                fidelity_dataset: None,
                fidelity_matrix: false,
                out: None,
                format: "genbank".into(),
                name_template: None,
                combos: None,
                origin: None,
            },
        )
        .expect("dry-run runs");
        assert!(opened.is_empty(), "dry-run must not open product views");
        assert!(
            matches!(response, ViewerResponse::AssemblyDryRun { .. }),
            "{response:?}"
        );
    }
}
