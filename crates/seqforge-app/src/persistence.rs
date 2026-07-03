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

use egui_dock::{DockState, Node, NodeIndex, Split, SurfaceIndex};
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
    /// Dock layout snapshot — splits + which paths are in each leaf.
    /// `None` means "use the default layout" (fresh install / cleared
    /// session).
    #[serde(default)]
    pub layout: Option<LayoutSnapshot>,
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

// ── Layout snapshot ──────────────────────────────────────────────────────────

/// Path-keyed mirror of the egui_dock tree. ViewIds intentionally do
/// not appear here — they're session-scoped pointers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum LayoutSnapshot {
    /// A leaf node holding tabs of one of the well-known kinds.
    Leaf(LeafSnapshot),
    /// Vertical split (top = `a`, bottom = `b`).
    VSplit {
        ratio: f32,
        a: Box<LayoutSnapshot>,
        b: Box<LayoutSnapshot>,
    },
    /// Horizontal split (left = `a`, right = `b`).
    HSplit {
        ratio: f32,
        a: Box<LayoutSnapshot>,
        b: Box<LayoutSnapshot>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum LeafSnapshot {
    /// The Files browser pane.
    Browser,
    /// The terminal pane.
    Terminal,
    /// The Inspector pane (right dock).
    Inspector,
    /// A viewer leaf — zero or more files (by path) plus the index
    /// of the active tab. Empty `paths` round-trips to a single
    /// `Tab::Welcome` placeholder.
    Viewer { paths: Vec<PathBuf>, active: usize },
}

impl LayoutSnapshot {
    /// The hard-coded default layout — Browser left, viewer in the
    /// central area (no files), Terminal at the bottom of the central
    /// area. Mirrors what `AppState::Default::default()` builds.
    #[allow(dead_code)]
    pub fn default_layout() -> Self {
        LayoutSnapshot::HSplit {
            ratio: 0.20,
            a: Box::new(LayoutSnapshot::Leaf(LeafSnapshot::Browser)),
            b: Box::new(LayoutSnapshot::VSplit {
                ratio: 0.70,
                a: Box::new(LayoutSnapshot::Leaf(LeafSnapshot::Viewer {
                    paths: Vec::new(),
                    active: 0,
                })),
                b: Box::new(LayoutSnapshot::Leaf(LeafSnapshot::Terminal)),
            }),
        }
    }
}

// ── Save: dock_state → LayoutSnapshot ────────────────────────────────────────

/// Walk the main surface of `dock_state` and emit a path-keyed
/// `LayoutSnapshot`. `Tab::View(vid)` entries are resolved to their
/// buffer's `source_path` via the workspace; tabs whose buffer has no
/// path (scratch buffers, post-MVP) are omitted from the saved layout
/// — they wouldn't be reopenable anyway.
pub fn capture_layout(dock: &DockState<Tab>, workspace: &Workspace) -> Option<LayoutSnapshot> {
    let main = dock.main_surface();
    // The main surface is a Tree<Tab> with nodes stored in a heap-
    // indexed Vec. NodeIndex(0) is the root.
    capture_node(main, NodeIndex::root(), workspace)
}

fn capture_node(
    tree: &egui_dock::Tree<Tab>,
    node: NodeIndex,
    workspace: &Workspace,
) -> Option<LayoutSnapshot> {
    if node.0 >= tree.len() {
        return None;
    }
    match &tree[node] {
        egui_dock::Node::Empty => None,
        egui_dock::Node::Leaf { tabs, active, .. } => {
            Some(LayoutSnapshot::Leaf(capture_leaf(tabs, *active, workspace)))
        }
        egui_dock::Node::Vertical { fraction, .. } => {
            let a = capture_node(tree, node.left(), workspace)?;
            let b = capture_node(tree, node.right(), workspace)?;
            Some(LayoutSnapshot::VSplit {
                ratio: *fraction,
                a: Box::new(a),
                b: Box::new(b),
            })
        }
        egui_dock::Node::Horizontal { fraction, .. } => {
            let a = capture_node(tree, node.left(), workspace)?;
            let b = capture_node(tree, node.right(), workspace)?;
            Some(LayoutSnapshot::HSplit {
                ratio: *fraction,
                a: Box::new(a),
                b: Box::new(b),
            })
        }
    }
}

fn capture_leaf(tabs: &[Tab], active: egui_dock::TabIndex, workspace: &Workspace) -> LeafSnapshot {
    // A leaf can hold any mix of tabs but in practice only one kind
    // — Browser / Terminal / Welcome / View(_) — appears per leaf.
    // For mixed leaves (unlikely), the *first* tab decides the
    // category; this is good enough until a future kind violates
    // the invariant.
    for tab in tabs {
        match tab {
            Tab::FileBrowser => return LeafSnapshot::Browser,
            Tab::Terminal => return LeafSnapshot::Terminal,
            Tab::Inspector => return LeafSnapshot::Inspector,
            _ => {}
        }
    }
    // Otherwise it's a viewer leaf — collect paths of View tabs.
    let mut paths = Vec::new();
    let mut active_idx = 0usize;
    for (idx, tab) in tabs.iter().enumerate() {
        if let Tab::View(vid) = tab {
            let path = workspace
                .view(*vid)
                .and_then(|v| workspace.buffers.get(v.buffer_id))
                .and_then(|arc| arc.read().ok().and_then(|b| b.source_path.clone()));
            if let Some(p) = path {
                if idx == active.0 {
                    active_idx = paths.len();
                }
                paths.push(p);
            }
        }
    }
    LeafSnapshot::Viewer {
        paths,
        active: active_idx,
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
            selection: view.selection,
            scroll_pos: view.scroll_pos,
        });
    }
    out
}

// ── Load: LayoutSnapshot → empty dock skeleton ───────────────────────────────

/// Rebuild a `DockState<Tab>` from a snapshot. Viewer leaves come
/// back as `Tab::Welcome` placeholders; the caller is expected to
/// `OpenFile` each persisted path afterwards, which replaces the
/// placeholders with `Tab::View(_)` tabs (placement is driven by the
/// returned [`PendingOpens`] map).
///
/// On any structural problem with the snapshot, falls back to the
/// default layout — never panics.
pub fn rebuild_dock(snapshot: &LayoutSnapshot) -> (DockState<Tab>, PendingOpens) {
    let mut pending = PendingOpens::default();

    // Seed with a placeholder so we have a single root leaf to split
    // against. The recursive walk overwrites it.
    let mut dock = DockState::new(vec![Tab::Welcome]);
    install_node(
        &mut dock,
        snapshot,
        SurfaceIndex::main(),
        NodeIndex::root(),
        &mut pending,
    );
    (dock, pending)
}

/// Map from "Tab::Welcome placeholder location" to the list of file
/// paths that should populate that leaf, plus the desired active
/// index. The load path replays `OpenFile` for each path, targeting
/// the placeholder leaf.
#[derive(Default, Debug)]
pub struct PendingOpens {
    /// Each entry: (surface, node, paths, active_index_within_paths).
    pub leaves: Vec<(SurfaceIndex, NodeIndex, Vec<PathBuf>, usize)>,
}

fn install_node(
    dock: &mut DockState<Tab>,
    snapshot: &LayoutSnapshot,
    surface: SurfaceIndex,
    node: NodeIndex,
    pending: &mut PendingOpens,
) {
    match snapshot {
        LayoutSnapshot::Leaf(leaf) => {
            install_leaf(dock, leaf, surface, node, pending);
        }
        LayoutSnapshot::HSplit { ratio, a, b } => {
            // Existing placeholder becomes 'a'; 'b' is the freshly
            // allocated sibling. We need to know which "kind" of
            // placeholder to put on each side so the recursive call
            // can swap it for the real content.
            let placeholder = Tab::Welcome;
            let [a_idx, b_idx] = dock.split(
                (surface, node),
                Split::Right,
                ratio.clamp(0.05, 0.95),
                Node::leaf(placeholder),
            );
            install_node(dock, a, surface, a_idx, pending);
            install_node(dock, b, surface, b_idx, pending);
        }
        LayoutSnapshot::VSplit { ratio, a, b } => {
            let [a_idx, b_idx] = dock.split(
                (surface, node),
                Split::Below,
                ratio.clamp(0.05, 0.95),
                Node::leaf(Tab::Welcome),
            );
            install_node(dock, a, surface, a_idx, pending);
            install_node(dock, b, surface, b_idx, pending);
        }
    }
}

fn install_leaf(
    dock: &mut DockState<Tab>,
    leaf: &LeafSnapshot,
    surface: SurfaceIndex,
    node: NodeIndex,
    pending: &mut PendingOpens,
) {
    // Replace the placeholder's tab list with the right content.
    let tabs = match leaf {
        LeafSnapshot::Browser => vec![Tab::FileBrowser],
        LeafSnapshot::Terminal => vec![Tab::Terminal],
        LeafSnapshot::Inspector => vec![Tab::Inspector],
        LeafSnapshot::Viewer { paths, active } => {
            if paths.is_empty() {
                vec![Tab::Welcome]
            } else {
                // Queue the opens; install a Welcome placeholder
                // that the OpenFile replay will replace.
                pending.leaves.push((surface, node, paths.clone(), *active));
                vec![Tab::Welcome]
            }
        }
    };
    if let egui_dock::Node::Leaf {
        tabs: existing,
        active,
        ..
    } = &mut dock[surface][node]
    {
        *existing = tabs;
        *active = egui_dock::TabIndex(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Collect every tab across the main surface's leaves.
    fn collect_tabs(dock: &DockState<Tab>) -> Vec<Tab> {
        let tree = dock.main_surface();
        let mut out = Vec::new();
        for i in 0..tree.len() {
            if let Node::Leaf { tabs, .. } = &tree[NodeIndex(i)] {
                out.extend(tabs.iter().cloned());
            }
        }
        out
    }

    /// A snapshot written *before* the Inspector variant existed must still
    /// deserialize and rebuild — the one real back-compat risk of adding a new
    /// `Tab`/`LeafSnapshot` variant. The Inspector is simply absent (graceful
    /// fallback until a ResetLayout); nothing panics.
    #[test]
    fn legacy_layout_without_inspector_loads() {
        let legacy = LayoutSnapshot::HSplit {
            ratio: 0.2,
            a: Box::new(LayoutSnapshot::Leaf(LeafSnapshot::Browser)),
            b: Box::new(LayoutSnapshot::VSplit {
                ratio: 0.7,
                a: Box::new(LayoutSnapshot::Leaf(LeafSnapshot::Viewer {
                    paths: vec![],
                    active: 0,
                })),
                b: Box::new(LayoutSnapshot::Leaf(LeafSnapshot::Terminal)),
            }),
        };
        // Round-trip through JSON exactly as a persisted session would.
        let json = serde_json::to_string(&legacy).unwrap();
        let back: LayoutSnapshot = serde_json::from_str(&json).unwrap();
        let (dock, _pending) = rebuild_dock(&back);
        let tabs = collect_tabs(&dock);
        assert!(tabs.contains(&Tab::FileBrowser));
        assert!(tabs.contains(&Tab::Terminal));
        assert!(
            !tabs.iter().any(|t| matches!(t, Tab::Inspector)),
            "legacy layout must not conjure an Inspector"
        );
    }

    /// A snapshot that carries the Inspector round-trips and rebuilds it.
    #[test]
    fn layout_with_inspector_round_trips() {
        let snap = LayoutSnapshot::HSplit {
            ratio: 0.8,
            a: Box::new(LayoutSnapshot::Leaf(LeafSnapshot::Viewer {
                paths: vec![],
                active: 0,
            })),
            b: Box::new(LayoutSnapshot::Leaf(LeafSnapshot::Inspector)),
        };
        let json = serde_json::to_string(&snap).unwrap();
        let back: LayoutSnapshot = serde_json::from_str(&json).unwrap();
        let (dock, _pending) = rebuild_dock(&back);
        assert!(
            collect_tabs(&dock)
                .iter()
                .any(|t| matches!(t, Tab::Inspector)),
            "a persisted Inspector leaf must rebuild"
        );
    }
}
