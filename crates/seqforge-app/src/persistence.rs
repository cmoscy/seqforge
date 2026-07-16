//! Session persistence — Stage 2.5e.
//!
//! ## Why this module exists
//!
//! Before 2.5e, `AppState::dock_state` was `#[serde]`-persisted while
//! `AppState::workspace` was `#[serde(skip)]`. On restart the dock
//! carried `Tab::View(ViewId)` entries pointing at views the freshly-
//! defaulted workspace didn't know about. The end-of-frame reconciler
//! would spam `ViewNotFound` toasts; a tree-restructure bug in
//! `egui_dock::Tree::remove_tab` made the workaround panic on multi-
//! orphan startup. Two sources of truth, asymmetric persistence,
//! whole class of bugs.
//!
//! ## The pattern
//!
//! - `ViewId` / `BufferId` are **session-scoped** pointers. Never
//!   persisted; rebuilt fresh each launch.
//! - `PathBuf` is the **only identity** stable across process
//!   restarts. The save/load boundary uses paths.
//! - `dock_state` is `#[serde(skip)]`. egui_dock owns layout during a
//!   session; we snapshot it at save time into a path-keyed
//!   [`LayoutSnapshot`] and replay it on load.
//! - Per-view state (selection, scroll, search bar) saves into
//!   [`FileState`] keyed by source path, so closing and reopening a
//!   file restores it.
//!
//! This matches the patterns used by Zed (`Workspace` + `Pane` +
//! per-file SQLite memento) and VSCode (editor-group snapshot +
//! per-editor memento), adapted to our stack.

use std::collections::HashMap;
use std::path::PathBuf;

use egui_dock::{DockState, Node, NodeIndex};
use seqforge_core::Selection;
use serde::{Deserialize, Serialize};

use crate::tabs::Tab;
use crate::workspace::Workspace;

// ── Persisted session ────────────────────────────────────────────────────────

/// The complete on-disk session shape. Anything the user expects to
/// survive a restart lives here; everything else is rebuilt.
///
/// Forward compatibility: every nested type derives `Default` so a
/// missing field in older saved data deserializes to a sensible
/// no-op. Adding new fields is non-breaking; renaming or removing
/// requires a migration.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct PersistedSession {
    /// Recent-files menu, most-recent first.
    #[serde(default)]
    pub recent_files: Vec<PathBuf>,
    /// Flat workbench layout — panel visibility + open document paths
    /// (decision 19). Replaces the old dock-tree snapshot. Missing (older
    /// sessions) → default (all regions visible, no files).
    #[serde(default)]
    pub workbench: WorkbenchLayout,
    /// Per-file UI state keyed by source path. Restored when the
    /// file's View is created during load.
    #[serde(default)]
    pub file_state: HashMap<PathBuf, FileState>,
}

/// UI state that survives close-and-reopen of a single file.
///
/// Deliberately minimal: only what the user would notice missing on
/// reopen. Computed/derived data (search results, cut sites, render
/// caches) stays transient.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct FileState {
    #[serde(default)]
    pub selection: Option<Selection>,
    #[serde(default)]
    pub scroll_pos: Option<f32>,
}

// ── Workbench layout (flat — decision 19) ─────────────────────────────────────

/// Flat, path-keyed session layout: which shell regions are visible and which
/// documents are open in the center. Replaces the old dock-tree mirror — the
/// shell regions are native panels now, so there is no tree to serialize. Split
/// arrangements in the center are *not* persisted (a transient view choice);
/// restored files reopen as tabs in one center leaf. ViewIds never appear here
/// (session-scoped pointers); paths are the only stable identity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkbenchLayout {
    #[serde(default = "yes")]
    pub show_files: bool,
    #[serde(default = "yes")]
    pub show_terminal: bool,
    #[serde(default = "yes")]
    pub show_inspector: bool,
    #[serde(default = "yes")]
    pub show_minimap: bool,
    /// Open document paths, in center-tab order.
    #[serde(default)]
    pub open_paths: Vec<PathBuf>,
    /// Index into `open_paths` of the active tab.
    #[serde(default)]
    pub active: usize,
}

fn yes() -> bool {
    true
}

impl Default for WorkbenchLayout {
    /// All regions visible, no files — matches a fresh `AppState`.
    fn default() -> Self {
        Self {
            show_files: true,
            show_terminal: true,
            show_inspector: true,
            show_minimap: true,
            open_paths: Vec::new(),
            active: 0,
        }
    }
}

// ── Save: dock_state + panel visibility → WorkbenchLayout ─────────────────────

/// Capture the flat workbench layout: the panel-visibility bools plus the open
/// document paths (in center-tab order) and the active index. `View` tabs are
/// resolved to their buffer's `source_path`; pathless (scratch) buffers are
/// skipped — they wouldn't be reopenable.
pub fn capture_workbench(
    dock: &DockState<Tab>,
    workspace: &Workspace,
    show_files: bool,
    show_terminal: bool,
    show_inspector: bool,
    show_minimap: bool,
) -> WorkbenchLayout {
    let tree = dock.main_surface();
    let active_vid = workspace.active_view;

    let mut open_paths = Vec::new();
    let mut active = 0usize;
    for i in 0..tree.len() {
        if let Node::Leaf { tabs, .. } = &tree[NodeIndex(i)] {
            for tab in tabs {
                if let Tab::View(vid) = tab {
                    let path = workspace
                        .view(*vid)
                        .and_then(|v| workspace.buffers.get(v.buffer_id))
                        .and_then(|arc| arc.read().ok().and_then(|b| b.source_path.clone()));
                    if let Some(p) = path {
                        if Some(*vid) == active_vid {
                            active = open_paths.len();
                        }
                        open_paths.push(p);
                    }
                }
            }
        }
    }

    WorkbenchLayout {
        show_files,
        show_terminal,
        show_inspector,
        show_minimap,
        open_paths,
        active,
    }
}

// ── Save: per-view state → FileState map ─────────────────────────────────────

/// Snapshot each open view's per-file state into a path-keyed map.
/// Views whose buffer has no `source_path` (scratch buffers) are
/// skipped — they wouldn't survive restart anyway.
pub fn capture_file_state(workspace: &Workspace) -> HashMap<PathBuf, FileState> {
    let mut out = HashMap::new();
    for view in workspace.views.values() {
        let Some(arc) = workspace.buffers.get(view.buffer_id) else {
            continue;
        };
        let Ok(buf) = arc.read() else { continue };
        let Some(path) = buf.source_path.clone() else {
            continue;
        };
        out.entry(path).or_insert(FileState {
            selection: view.selection.text_range(),
            scroll_pos: view.scroll_pos,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workbench_layout_round_trips_through_json() {
        let wb = WorkbenchLayout {
            show_files: false,
            show_terminal: true,
            show_inspector: false,
            show_minimap: false,
            open_paths: vec![PathBuf::from("/tmp/a.gb"), PathBuf::from("/tmp/b.fasta")],
            active: 1,
        };
        let json = serde_json::to_string(&wb).unwrap();
        let back: WorkbenchLayout = serde_json::from_str(&json).unwrap();
        assert!(!back.show_files);
        assert!(back.show_terminal);
        assert!(!back.show_inspector);
        assert!(!back.show_minimap);
        assert_eq!(back.open_paths.len(), 2);
        assert_eq!(back.active, 1);
    }

    /// Older sessions have no `workbench` key (or missing region bools) — they
    /// must default to *visible*, not hidden, so a panel never silently vanishes.
    #[test]
    fn missing_workbench_defaults_to_all_regions_visible() {
        let session: PersistedSession = serde_json::from_str("{}").unwrap();
        assert!(session.workbench.show_files);
        assert!(session.workbench.show_terminal);
        assert!(session.workbench.show_inspector);
        assert!(session.workbench.show_minimap);
        assert!(session.workbench.open_paths.is_empty());
    }
}
