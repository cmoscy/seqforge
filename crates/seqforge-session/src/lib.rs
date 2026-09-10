//! Headless session layer: the editor's document state and the dispatch that
//! mutates it, with no renderer in scope.
//!
//! ## Why this crate exists
//!
//! ROADMAP decision 9 forbids `core ──► bio`, so bio-derived write operations
//! have to live *above* `bio`. `docs/architecture.md` puts it as "it belongs
//! where both crates are already in scope" — and for a long time the only crate
//! satisfying that was `seqforge-app`, so the session model landed inside the
//! GUI. That was a dependency-graph accident, not a design intent: it made every
//! verb that touched a workspace GUI-only, and parity had to be restored by
//! duplicating commands (see ROADMAP decision 27).
//!
//! This crate is that missing address. It sits above `core` *and* `bio` and
//! below both shells, so `seqforge-cli` and `seqforge-app` drive one
//! implementation.
//!
//! ## The rule
//!
//! **Nothing here may depend on egui.** `cargo tree -p seqforge-session` is
//! expected to be renderer-free, and that is what keeps the CLI able to run a
//! real session without linking a GUI. State that is genuinely visual — the
//! per-view render cache, dock layout, overlays — stays in `seqforge-app`.

pub mod bases;
pub mod bio;
pub mod edit;
pub mod host;
pub mod resolver;
pub mod workspace;

pub use bio::Bio;
pub use host::{Host, Level, NullHost};
pub use resolver::WorkspaceResolver;
pub use workspace::{BufferStore, Workspace, display_name, hash_file_bytes};

#[cfg(test)]
mod parity_tests {
    //! The property the document target buys: one verb, two ways of naming the
    //! document, one answer.
    //!
    //! Before `Target`, this test could not be written — a read verb reached its
    //! document only through a GUI's active view, so "the same request against a
    //! file" had no expression. It is the assertion that would have caught the
    //! `digest` methylation drift (ROADMAP decision 27).

    use seqforge_core::{Target, ViewerRequest, dispatch};

    use crate::{Bio, Workspace};

    fn fixture() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../seqforge-bio/tests/fixtures/pUC19.gbk")
    }

    /// Run `req` against a workspace where the file is already open — the
    /// "session" face — and against one that opens it from the path — the
    /// headless face. The two must agree.
    fn both_faces(req: ViewerRequest) -> (String, String) {
        let path = fixture();
        let bio = Bio;

        let mut ws = Workspace::default();
        let vid = ws.open_path(&path, &bio).expect("fixture opens");
        let by_view = ws
            .with_buffer(vid, |v, buf, ann| {
                dispatch(v, buf, ann, &bio, with_target(&req, Target::view(vid)))
            })
            .unwrap()
            .unwrap();

        // A second, independent workspace, addressed by path — what the CLI's
        // local runner does.
        let mut ws2 = Workspace::default();
        let vid2 = ws2.open_path(&path, &bio).expect("fixture opens");
        let by_path = ws2
            .with_buffer(vid2, |v, buf, ann| {
                dispatch(v, buf, ann, &bio, with_target(&req, Target::path(&path)))
            })
            .unwrap()
            .unwrap();

        (
            serde_json::to_string(&by_view).unwrap(),
            serde_json::to_string(&by_path).unwrap(),
        )
    }

    /// Rebuild `req` with a different target. Cheap and explicit: the point is
    /// that only the addressing differs between the two runs.
    fn with_target(req: &ViewerRequest, target: Target) -> ViewerRequest {
        let mut r = req.clone();
        if let Some(t) = r.target_mut() {
            *t = target;
        }
        r
    }

    #[test]
    fn list_features_agrees_across_targets() {
        let (a, b) = both_faces(ViewerRequest::ListFeatures {
            target: Target::active(),
        });
        assert_eq!(a, b);
        assert!(a.contains("\"kind\":\"features\""), "{a}");
    }

    #[test]
    fn find_agrees_across_targets() {
        let (a, b) = both_faces(ViewerRequest::Find {
            pattern: "GAATTC".into(),
            mismatches: 0,
            target: Target::active(),
        });
        assert_eq!(a, b);
    }

    /// The enzyme path — where the drift actually was. `Enzymes` resolves a
    /// query, scans, and evaluates methylation, so agreement here covers the
    /// whole chain that `seqforge digest` and the viewer used to duplicate.
    #[test]
    fn enzymes_agrees_across_targets() {
        let (a, b) = both_faces(ViewerRequest::Enzymes {
            query: "unique".into(),
            op: Default::default(),
            dam: true,
            dcm: true,
            cpg: false,
            target: Target::active(),
        });
        assert_eq!(a, b);
        assert!(a.contains("\"kind\":\"cut_sites\""), "{a}");
    }

    #[test]
    fn list_primers_agrees_across_targets() {
        let (a, b) = both_faces(ViewerRequest::ListPrimers {
            target: Target::active(),
        });
        assert_eq!(a, b);
    }
}
