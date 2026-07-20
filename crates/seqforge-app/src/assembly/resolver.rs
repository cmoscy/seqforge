//! Resolve a recipe's [`SourceRef`]s against the live workspace — open buffers
//! by handle, files at rest by path (loaded read-only, **not** opened as tabs).
//! Display names match tab titles: buffer → [`display_name`], path → file basename.

use seqforge_bio::{ResolvedSource, SourceResolver};
use seqforge_core::{Annotations, SourceRef};

use crate::workspace::{Workspace, display_name};

/// A [`SourceResolver`] over the app's [`Workspace`]. Borrows immutably, so build
/// it, run the assembly, and drop it *before* mutating the workspace to
/// materialize products.
pub(crate) struct WorkspaceResolver<'a> {
    pub ws: &'a Workspace,
}

impl SourceResolver for WorkspaceResolver<'_> {
    fn resolve(&self, r: &SourceRef) -> Result<ResolvedSource, String> {
        match r {
            SourceRef::Buffer(bid) => {
                let arc = self
                    .ws
                    .buffers
                    .get(*bid)
                    .ok_or_else(|| format!("buffer {bid} is no longer open"))?;
                let buf = arc.read().map_err(|_| "buffer lock poisoned".to_string())?;
                let ann = self
                    .ws
                    .buffers
                    .annotations(*bid)
                    .cloned()
                    .unwrap_or_default();
                Ok(ResolvedSource {
                    name: display_name(&buf),
                    bytes: buf.text.clone(),
                    topology: buf.topology,
                    ann,
                })
            }
            SourceRef::Path(p) => {
                let doc = seqforge_bio::load(p).map_err(|e| format!("{}: {e}", p.display()))?;
                let name = p
                    .file_name()
                    .and_then(|n| n.to_str())
                    .map(str::to_owned)
                    .unwrap_or(doc.name);
                Ok(ResolvedSource {
                    name,
                    bytes: doc.sequence,
                    topology: doc.topology,
                    ann: Annotations::from_parts(doc.features, doc.primers),
                })
            }
        }
    }
}
