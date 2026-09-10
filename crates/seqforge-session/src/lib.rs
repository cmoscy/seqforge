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
pub mod project;
pub mod resolver;
pub mod workspace;

pub use bio::Bio;
pub use host::{Host, Level, NullHost};
pub use resolver::WorkspaceResolver;
pub use workspace::{BufferStore, Workspace, display_name, hash_file_bytes};

#[cfg(test)]
mod parity_tests {
    //! `dispatch` ignores the target it is handed — every arm destructures
    //! `target: _`, because resolution happens in the layer above. These tests
    //! pin exactly that: carrying a `Target` through `dispatch` cannot perturb
    //! the answer. It is worth knowing, since it is what makes the target safe
    //! to flatten onto 30 variants.
    //!
    //! It is **not** the parity property. Both runs here open the file
    //! themselves, so nothing resolves a `Target::Path`. The test that a
    //! path target and a view target reach the same document lives in
    //! `seqforge-cli`, which is where `resolve_on_file` — the code that can
    //! actually be wrong — is reachable.

    use seqforge_core::{Target, ViewerRequest, dispatch};

    use crate::{Bio, Workspace};

    fn fixture() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../seqforge-bio/tests/fixtures/pUC19.gbk")
    }

    /// Dispatch `req` twice over equivalent workspaces, differing only in the
    /// inert `Target` value each request carries.
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

        // A second, independent workspace. The request names a path rather
        // than a view, but nothing here resolves that — `dispatch` never reads
        // the field. That is the point being pinned.
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
    fn list_features_is_unperturbed_by_its_target() {
        let (a, b) = both_faces(ViewerRequest::ListFeatures {
            target: Target::active(),
        });
        assert_eq!(a, b);
        assert!(a.contains("\"kind\":\"features\""), "{a}");
    }

    #[test]
    fn find_is_unperturbed_by_its_target() {
        let (a, b) = both_faces(ViewerRequest::Find {
            pattern: "GAATTC".into(),
            mismatches: 0,
            target: Target::active(),
        });
        assert_eq!(a, b);
    }

    /// The enzyme path carries the most state through `dispatch` — it resolves
    /// a query, scans, mutates `view.active_enzymes`, and evaluates
    /// methylation — so it is the strongest place to pin target-inertness.
    #[test]
    fn enzymes_is_unperturbed_by_its_target() {
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
    fn list_primers_is_unperturbed_by_its_target() {
        let (a, b) = both_faces(ViewerRequest::ListPrimers {
            target: Target::active(),
        });
        assert_eq!(a, b);
    }
}
