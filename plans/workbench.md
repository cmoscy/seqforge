# Workbench Shell Refactor

> Canonical cross-track status: [`../ROADMAP.md`](../ROADMAP.md). Opening
> infrastructure of the **generative/assembly track**, landed right after the
> `v0.2.0` viewer+editor foundation freeze.

> **Document area = egui_dock, natively.** Side-by-side of *different* documents
> is egui_dock's built-in tab-drag (drag a document tab to an edge); the
> hand-rolled `SplitPane` (which cloned the active buffer into a second view —
> the only path that put one buffer in two panes, and the sole trigger of the
> egui ID-collision) is deleted. For **GUI–CLI parity**, document management is a
> shared `ViewerRequest` vocabulary: `buffers` (list open docs — index, path,
> dirty, active) and `focus <handle>` (activate by 1-based index / path /
> basename; the GUI equivalent is clicking a tab). Documents are addressed by a
> **stable human handle (path or index)**, never the opaque per-process `ViewId`.
> Deferred: per-op `--doc` targeting (ops default to the active doc; `focus` then
> run) and a one-way `active_view` derive.

## Why

Everything currently lives in one homogeneous `egui_dock` tree — `Tab =
FileBrowser | Terminal | Welcome | View | Inspector` — so there is **no privileged
center**. Any leaf can collapse and a neighbor can flow into its space. The
concrete bug: close the last viewer tab → the center leaf collapses → the empty-
case fallback (`ensure_welcome_invariant`'s `push_to_focused_leaf`) strands
`Welcome` in the Files column → the next file opens *inside the Files pane*, and
the degenerate layout persists across launches. This is the 2nd layout-invariant
papercut (the fallback itself was the 1st).

The fix is the **workbench-shell model** every serious viewer/editor uses (VS Code,
JetBrains, Xcode, Figma, Benchling): shell regions are **structural native egui
panels** that cannot collapse or displace each other; the editor is a privileged,
non-collapsible center. This makes the whole bug class *unrepresentable* (same move
as decisions 12/17), collapses the persistence layer, and — decisively — provides
the **named, addressable regions** that the assembly workbench + plugin/agent
contribution model will target (decisions 11/17). A homogeneous dock tree has no
stable regions to contribute UI into; this refactor is the substrate.

## Decision of record

**Structural native panels replace the homogeneous dock for the shell; `egui_dock`
is demoted to the center editor tab strip only.** Files/Terminal/Inspector become
`SidePanel`/`TopBottomPanel` regions (toggleable, resizable, never collapsing); the
`CentralPanel` always exists and hosts either the center View dock (split preserved)
or a Welcome empty-state. Named regions are the plugin/agent contribution substrate.
Tear-off floating windows / arbitrary drag-anywhere docking are intentionally
dropped — the shell is fixed by design.

## Region scope model (the payoff)

The native regions express a clean **scope** split, which is what makes the layout
predictable and let us delete the old fraction config:

- **Left = workspace** — the file browser (navigate *between* documents). Toggle:
  `ToggleFiles`.
- **Right = active document** — the Inspector (details) with the **minimap
  overview pinned as a sized strip at its bottom** (both read the active view).
  The Inspector is the greedy `CentralPanel` (gets the majority — suits long
  primer/enzyme lists); the minimap is the resizable `TopBottomPanel::bottom`
  with its own divider (drag up to grow the overview, down to reclaim it for the
  lists). `ToggleInspector` / `ToggleMinimap` show each **independently** (either
  can fill the column alone). The circular map is still a square by panel width
  (capped at half the strip height), matching the navigator/thumbnail convention
  (SnapGene map / Photoshop Navigator), not a code minimap.
- **Bottom = terminal** (commands). **Center = editor.**

Panel sizes are owned by egui (persisted in its memory); the old `[layout]` split
fractions are deleted.

## Target architecture

```
TopBottomPanel(menu bar)        ← unchanged (already native)
TopBottomPanel(status bar)      ← unchanged (already native)
TopBottomPanel::bottom("terminal")  [show_terminal]  ← TerminalPane
SidePanel::left("files")            [show_files]     ← BrowserState
SidePanel::right("inspector")   [show_inspector||show_minimap] ← InspectorState (fill) + MiniMap (bottom strip)
CentralPanel                        ← ALWAYS present; never collapses
  └ workspace has views? egui_dock DockState<Tab::View> (split supported)
    else:                 Welcome empty-state rendered directly
```

Panel *draw order* fixes the geometry (Central must be last). Terminal drawn before
the side panels → full-width bottom (matches the prior `split_below(root)` look).

## Phasing (each step keeps the app runnable + testable)

- **(a) Inspector → `SidePanel::right`** gated on `show_inspector`. Move the
  `TabViewer::ui` Inspector arm to a panel render; stop docking `Tab::Inspector`;
  `ToggleInspector` becomes a bool flip.
- **(b) Terminal → `TopBottomPanel::bottom`** gated on `show_terminal`.
- **(c) Files → `SidePanel::left`** gated on `show_files` (browser only; the
  minimap re-homed to the right Inspector column — see the as-shipped note).
- **(d) Center + persistence + cleanup.** `Tab` shrinks to
  `Welcome | View(ViewId)`; swap `LayoutSnapshot` for a flat `WorkbenchLayout`;
  simplify `ResetLayout`/close paths.

> **As-shipped deviation (c):** the minimap did **not** stay under the browser on
> the left. It re-homed to the **right** column as a sized `TopBottomPanel::bottom`
> under the Inspector (which is the greedy `CentralPanel`), gated on its own
> `show_minimap` bool with a `ToggleMinimap` command — so the overview reads the
> active document (right = active-document scope) and either region can fill the
> column alone. egui's canonical "one sized panel + `CentralPanel`" idiom (per its
> `panels.rs` demo) gives a single divider whose range is bounded only by each
> panel's `height_range`, never by content-min; the Inspector-as-`CentralPanel`
> role means content-heavy primer/enzyme lists get the majority by default.
>
> **As-shipped deviation (d):** `Tab` kept **`Welcome | View`**, not `View`-only,
> and the center `DockState` always holds ≥1 tab (a `Welcome` placeholder when no
> file is open) rather than the `CentralPanel` rendering an empty-state directly.
> This avoids empty-`DockState` edge cases in egui_dock at zero risk, and is
> visually identical (the dock lives inside the `CentralPanel`). Consequently
> `ensure_welcome_invariant` is **kept** (now trivial — Welcome↔View over the one
> center dock, and its `push_to_focused_leaf` fallback is safe because the dock no
> longer contains any shell leaves). The three shell `Tab` variants
> (`FileBrowser`/`Terminal`/`Inspector`) and the whole tree-mirroring
> `LayoutSnapshot` **are** deleted, as planned.

## State + persistence

Add to `AppState`: `show_files/show_terminal/show_inspector/show_minimap: bool`.
Panel *sizes* are **not** persisted here — egui owns them in its own per-panel
memory (which is why the old `[layout]` split fractions could be deleted). Replace
`persistence::{LayoutSnapshot, LeafSnapshot, capture_layout, rebuild_dock}` with a
flat, visibility-only layout:

```
WorkbenchLayout { show_files, show_terminal, show_inspector, show_minimap,
                  open_paths: Vec<PathBuf>, active: usize }
```

Old sessions (`layout: None` / deserialize-default) → default workbench (all
regions visible; `#[serde(default)]` on every field). Reuse the existing
`restore_session` open-replay and per-file `FileState` (selection/scroll)
unchanged.

## Preserved-behavior contract (the verification bar)

A UI-spine refactor: every existing pane/focus/keybinding behavior must survive.

- **Acceptance test (the motivating bug):** open files → 3-pane (Files L / viewer
  center / Terminal bottom). Close ALL viewer tabs → center shows Welcome,
  Files/Terminal stay put, no takeover. Reopen a file → lands in center. Relaunch →
  same (no degenerate persisted state).
- **Editor:** in-canvas edit gating (View focused + no overlay), Find/GoTo bar
  anchors to active view, staged-edit commit, undo/redo.
- **Terminal:** PTY renders, focus gating, keystrokes.
- **Inspector:** `⌘E` focuses it; toggle hides/shows; enzyme/primer/cut-site tabs +
  methylation toggles.
- **Tabs:** `⌘W` close, `⌘1–9` focus-by-index, `⌘{`/`⌘}` cycle, `SplitPane`
  side-by-side, drag-reorder within center.
- **Keymap contexts** (`Pane:Viewer/Terminal/Browser/Inspector`) resolve per panel.
- **Persistence:** open files + active + panel sizes/visibility survive relaunch;
  per-file selection/scroll restored.
- **Minimap** renders in the right column as a sized strip under the Inspector;
  its divider resizes it, and `ToggleMinimap` shows/hides it independently.

## Testing

1. Unit — `WorkbenchLayout` capture/restore round-trip; `place_view_tab` appends to
   center; `ToggleInspector` flips the bool. Update the layout-adjacent command
   tests that assert dock structure; keep the suite green.
2. `cargo fmt --check` + `clippy -D warnings` + `cargo test --workspace` (CI gate).
3. Manual GUI walk of the contract above — the real net for a UI refactor —
   explicitly the close-all-tabs regression + a relaunch.

## Out of scope

- Removing `egui_dock` (kept, center-scoped — preserves split).
- Tear-off floating windows / drag-anywhere docking.
- The plugin contribution API itself (this provides only the region substrate).

## Files touched

`crates/seqforge-app/src/`: `app.rs` (panel render + AppState fields),
`tabs.rs` (`Tab` shrinks; render bodies move out), `persistence.rs` (flat layout),
`command/layout.rs` + `command/mod.rs` (command simplification), `focus.rs`
(reused as-is; per-panel focus set), `command/file.rs` (close-path simplification).
