//! The single document-verb interpreter.
//!
//! Loaders (CLI file path, GUI live workspace) only load. This module only
//! computes — routing each request to **edit**, **query**, or **produce**.
//! The shell that called it prints or docks. See ROADMAP decision 27 and
//! `docs/architecture.md`.

use std::path::Path;

use seqforge_core::{
    BioOps, DispatchError, TargetKind, Topology, ViewId, ViewerRequest, ViewerResponse,
};

use crate::bio::Bio;
use crate::edit::{self, resolve_target};
use crate::host::Host;
use crate::produce::{self, AssembleOpts, ComboSelection};
use crate::query;
use crate::workspace::Workspace;

/// Cap on product tabs opened into a live workspace. Every product is still
/// written when `--out` is set; only the dock/tab count is capped.
pub const MAX_MATERIALIZED: usize = 24;

/// Outcome of [`execute`]: the typed response plus any views the verb created
/// that a GUI shell should dock.
#[derive(Debug)]
pub struct Executed {
    pub response: ViewerResponse,
    pub opened: Vec<ViewId>,
}

fn done(response: ViewerResponse) -> Result<Executed, DispatchError> {
    Ok(Executed {
        response,
        opened: Vec::new(),
    })
}

fn done_opened(response: ViewerResponse, opened: Vec<ViewId>) -> Result<Executed, DispatchError> {
    Ok(Executed { response, opened })
}

fn from_edit(r: Result<Option<ViewerResponse>, DispatchError>) -> Result<Executed, DispatchError> {
    done(r?.unwrap_or(ViewerResponse::Ok))
}

/// Resolve the document a request names inside an already-loaded workspace.
///
/// A `Target::Path` means the loader already opened that file (CLI one-shot);
/// the active view is that document.
fn resolve_view(ws: &Workspace, target: &seqforge_core::Target) -> Result<ViewId, DispatchError> {
    match target.kind()? {
        TargetKind::Active | TargetKind::Path(_) => resolve_target(ws, None),
        TargetKind::View(v) => resolve_target(ws, Some(v)),
    }
}

/// Run a document verb against an already-loaded workspace.
///
/// `bio` is the production [`Bio`] in both shells; the parameter exists so
/// tests can inject a stub where needed. Clipboard and notifications go
/// through `host`.
pub fn execute(
    ws: &mut Workspace,
    host: &mut dyn Host,
    bio: &dyn BioOps,
    req: ViewerRequest,
) -> Result<Executed, DispatchError> {
    match req {
        // ── Shell/lifecycle verbs the GUI still presents specially ─────────
        // Open / Close / Buffers / Focus / Save / SaveAs need dock order,
        // recent-files, or conflict dialogs. They are not interpreted here.
        ViewerRequest::Open { .. }
        | ViewerRequest::Close
        | ViewerRequest::Buffers
        | ViewerRequest::Focus { .. }
        | ViewerRequest::Save { .. }
        | ViewerRequest::SaveAs { .. } => Err(DispatchError::Unimplemented(
            "a shell lifecycle verb (open/close/buffers/focus/save) — the GUI presents it",
        )),

        // ── Produce: mint buffers or panes ─────────────────────────────────
        ViewerRequest::New { circular, name } => {
            let topology = if circular {
                Topology::Circular
            } else {
                Topology::Linear
            };
            let name = name.unwrap_or_else(|| "untitled".to_string());
            let vid = ws.new_buffer(name, Vec::new(), topology);
            done_opened(ViewerResponse::Ok, vec![vid])
        }
        ViewerRequest::Digest {
            enzymes,
            circular,
            target,
            ..
        } => {
            let vid = resolve_view(ws, &target)?;
            produce::digest(ws, host, vid, &enzymes, circular)
        }
        ViewerRequest::Pcr {
            fwd,
            rev,
            name,
            target,
        } => produce::pcr(ws, host, target.view, fwd, rev, name),
        ViewerRequest::Assemble {
            inputs,
            method,
            topology,
            enzymes,
            expand,
            emit_recipe,
            dry_run,
            fidelity_dataset,
            fidelity_matrix,
            out,
            format,
            name_template,
            combos,
            origin,
        } => {
            let mut recipe = produce::recipe_from_flags(
                &inputs,
                &method,
                &topology,
                enzymes.as_deref(),
                &expand,
            )?;
            if let Some(t) = name_template {
                recipe.name_template = Some(t);
            }
            produce::assemble(
                ws,
                host,
                AssembleOpts {
                    recipe,
                    dry_run,
                    fidelity_dataset,
                    fidelity_matrix,
                    emit_recipe,
                    out,
                    format,
                    combos: ComboSelection::Spec(combos),
                    origin,
                },
            )
        }

        // ── Edit: mutate this buffer ───────────────────────────────────────
        ViewerRequest::Insert { pos, bases, target } => {
            from_edit(edit::apply_insert(ws, target.view, pos, bases))
        }
        ViewerRequest::Delete { start, end, target } => {
            from_edit(edit::apply_delete(ws, target.view, start, end))
        }
        ViewerRequest::Replace {
            start,
            end,
            bases,
            target,
        } => from_edit(edit::apply_replace(ws, target.view, start, end, bases)),
        ViewerRequest::ReverseComplement { start, end, target } => {
            from_edit(edit::apply_reverse_complement(ws, target.view, start, end))
        }
        ViewerRequest::Cut { start, end, target } => {
            from_edit(edit::apply_cut(ws, host, target.view, start, end))
        }
        ViewerRequest::Copy { start, end, target } => {
            from_edit(edit::apply_copy(ws, host, target.view, start, end))
        }
        ViewerRequest::Paste { pos, target } => {
            from_edit(edit::apply_paste(ws, host, target.view, pos))
        }
        ViewerRequest::Undo { target } => from_edit(edit::apply_undo(ws, target.view)),
        ViewerRequest::Redo { target } => from_edit(edit::apply_redo(ws, target.view)),
        ViewerRequest::SetOrigin {
            index,
            feature,
            target,
        } => from_edit(edit::apply_set_origin(ws, target.view, index, feature)),
        ViewerRequest::Linearize { at, target } => {
            from_edit(edit::apply_linearize(ws, target.view, at))
        }
        ViewerRequest::Circularize { origin, target } => {
            from_edit(edit::apply_circularize(ws, target.view, origin))
        }
        ViewerRequest::AddFeature {
            start,
            end,
            kind,
            label,
            strand,
            target,
        } => from_edit(edit::apply_add_feature(
            ws,
            target.view,
            start,
            end,
            kind,
            label,
            strand,
        )),
        ViewerRequest::RemoveFeature { id, target } => {
            from_edit(edit::apply_remove_feature(ws, target.view, id))
        }
        ViewerRequest::RenameFeature { id, label, target } => {
            from_edit(edit::apply_rename_feature(ws, target.view, id, label))
        }
        ViewerRequest::UpdateFeature {
            id,
            kind,
            label,
            strand,
            start,
            end,
            target,
        } => from_edit(edit::apply_update_feature(
            ws,
            target.view,
            id,
            kind,
            label,
            strand,
            start,
            end,
        )),
        ViewerRequest::AddPrimer {
            name,
            sequence,
            start,
            end,
            strand,
            target,
        } => from_edit(edit::apply_add_primer(
            ws,
            target.view,
            name,
            sequence,
            start,
            end,
            strand,
        )),
        ViewerRequest::UpdatePrimer {
            id,
            name,
            sequence,
            strand,
            start,
            end,
            detach,
            target,
        } => from_edit(edit::apply_update_primer(
            ws,
            target.view,
            id,
            name,
            sequence,
            strand,
            start,
            end,
            detach,
        )),
        ViewerRequest::RescanPrimer { id, target } => {
            from_edit(edit::apply_rescan_primer(ws, target.view, id))
        }
        ViewerRequest::AddPrimerSite {
            id,
            enzyme,
            overhang,
            flank,
            target,
        } => from_edit(edit::apply_add_primer_site(
            ws,
            target.view,
            id,
            enzyme,
            overhang,
            flank,
        )),
        ViewerRequest::RemovePrimer { id, target } => {
            from_edit(edit::apply_remove_primer(ws, target.view, id))
        }

        // ── Query: answer about this buffer ────────────────────────────────
        other => {
            let target = other.target().ok_or_else(|| {
                DispatchError::InvalidInput("internal: view-scoped verb without a target".into())
            })?;
            let vid = resolve_view(ws, target)?;
            let resp = ws
                .with_buffer(vid, |view, buf, ann| {
                    query::dispatch(view, buf, ann, bio, other)
                })
                .and_then(|inner| inner)?;
            done(resp)
        }
    }
}

/// Convenience: run [`execute`] with the production [`Bio`].
pub fn execute_bio(
    ws: &mut Workspace,
    host: &mut dyn Host,
    req: ViewerRequest,
) -> Result<Executed, DispatchError> {
    execute(ws, host, &Bio, req)
}

/// Whether a file-addressed request's result is observable without a live
/// viewer (the CLI loader's gate before calling [`execute`]).
///
/// Framed as the three kinds under `execute`:
/// - **query** — always ok (JSON is the result).
/// - **produce** — ok when the result is observable without a live session
///   (digest list, assemble print / `--dry-run` / `--out`). PCR is refused:
///   the product is only an in-memory buffer.
/// - **edit** — refused on a file.
pub fn file_address_observable(req: &ViewerRequest) -> bool {
    match req {
        // query
        ViewerRequest::Info { .. }
        | ViewerRequest::Translate { .. }
        | ViewerRequest::Orfs { .. }
        | ViewerRequest::FindPrimerSites { .. }
        | ViewerRequest::ListFeatures { .. }
        | ViewerRequest::ListPrimers { .. }
        | ViewerRequest::GoTo { .. }
        | ViewerRequest::Find { .. }
        | ViewerRequest::Enzymes { .. } => true,

        // produce — observable without a live session
        ViewerRequest::Digest { .. } => true,
        ViewerRequest::Assemble { .. } => true,
        ViewerRequest::Pcr { .. } | ViewerRequest::New { .. } => false,

        // edit
        ViewerRequest::Insert { .. }
        | ViewerRequest::Delete { .. }
        | ViewerRequest::Replace { .. }
        | ViewerRequest::ReverseComplement { .. }
        | ViewerRequest::Cut { .. }
        | ViewerRequest::Copy { .. }
        | ViewerRequest::Paste { .. }
        | ViewerRequest::AddFeature { .. }
        | ViewerRequest::RemoveFeature { .. }
        | ViewerRequest::RenameFeature { .. }
        | ViewerRequest::UpdateFeature { .. }
        | ViewerRequest::AddPrimer { .. }
        | ViewerRequest::UpdatePrimer { .. }
        | ViewerRequest::RescanPrimer { .. }
        | ViewerRequest::AddPrimerSite { .. }
        | ViewerRequest::RemovePrimer { .. }
        | ViewerRequest::Undo { .. }
        | ViewerRequest::Redo { .. }
        | ViewerRequest::SetOrigin { .. }
        | ViewerRequest::Linearize { .. }
        | ViewerRequest::Circularize { .. } => false,

        // lifecycle (shell presents)
        ViewerRequest::Open { .. }
        | ViewerRequest::Close
        | ViewerRequest::Buffers
        | ViewerRequest::Focus { .. }
        | ViewerRequest::Save { .. }
        | ViewerRequest::SaveAs { .. } => false,
    }
}

/// Error message when a file-addressed write is refused by the CLI loader.
pub fn file_write_refused_msg(req: &ViewerRequest) -> String {
    let verb = match req {
        ViewerRequest::Pcr { .. } => "pcr",
        ViewerRequest::Insert { .. } => "insert",
        ViewerRequest::Save { .. } | ViewerRequest::SaveAs { .. } => "save",
        _ => "this write",
    };
    format!(
        "{verb} against a file needs a live session (or a future persist step); \
         the result would be an in-memory buffer this process is about to drop. \
         Open the file in SeqForge and use --view, or pass --in only for reads / digest / assemble"
    )
}

/// Open `path` into a throwaway workspace and [`execute`] `req`.
pub fn execute_on_file(
    path: &Path,
    mut req: ViewerRequest,
) -> Result<ViewerResponse, DispatchError> {
    if let Some(t) = req.target_mut() {
        // Ensure the request still carries the path for documentation; resolve
        // uses the active view after open.
        if t.path.is_none() {
            t.path = Some(path.to_path_buf());
        }
    }
    if !file_address_observable(&req) {
        return Err(DispatchError::Unimplemented(
            "a write verb against a file target (it needs a session)",
        ));
    }
    let bio = Bio;
    let mut ws = Workspace::default();
    ws.open_path(path, &bio).map_err(DispatchError::BioError)?;
    let mut host = crate::NullHost::default();
    let Executed { response, .. } = execute(&mut ws, &mut host, &bio, req)?;
    Ok(response)
}
