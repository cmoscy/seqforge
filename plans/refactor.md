# Pre-editor Refactor Punch List (model-split track)

> **Status: Tiers 1, 2 (light), and 2.5 COMPLETE.** This is the record of the
> model-object split (`Document` → `Buffer`/`Annotations`/`View`/`Workspace`)
> and the bug/security hardening that preceded editor work. Canonical status is
> in [`../ROADMAP.md`](../ROADMAP.md).
>
> ⚠️ **Tier 3 is superseded.** The "rope → anchors → transactional edits, before
> any edit-feature code" sequence below reflects the *original* editor strategy.
> It has been **replaced** by the snapshot-undo-on-`Vec<u8>` decision recorded in
> [`editor.md`](editor.md) (Decision: undo model). Rope is no longer a planned
> prerequisite. See the marker at the Tier 3 heading.

The MVP architecture is well-shaped for the editor transition — single `apply` site, typed commands, event bus, overlay stack, core/app split. But the **data model** (`Vec<u8>` sequences, absolute-offset features and selections, no version counter, no undo log) was the open question once edits start landing.

This section was the gate between read-only MVP and the editor transition.

## Current Status (auto-updated as commits land)

Legend: ✅ done · 🟡 partial · ⏳ next · 📋 queued

| Stage | Status | Commits |
|---|---|---|
| Tier 1 #2 — `set_var` ordering | ✅ | `cadd087` |
| Tier 1 #3 — Selection events from clicks | ✅ | `395eb2d` |
| **Tier 1 #1 — Socket hardening (`$XDG_RUNTIME_DIR` + chmod 0600)** | ✅ | (uncommitted) |
| **Tier 1 #4 — Stale socket cleanup (SocketGuard Drop)** | ✅ | (uncommitted) |
| **Tier 1 #5 — Windows build (`#[cfg(unix)]` gating)** | ✅ | (uncommitted) |
| Tier 2 #6 — `is_circular()` audit | ✅ | no replacements needed; already clean |
| **Tier 2 #9 — `screen_to_seq` end-of-doc cursor** | ✅ | (uncommitted) |
| **Tier 2 #10 — `Find` empty pattern clears selection** | ✅ | (uncommitted) |
| Tier 2 #7, #8 — viewer.rs structural refactors | 📋 | deferred (collides with editor rendering) |
| **Stage 2.5a — Model split (Buffer / View / Workspace)** | ✅ | `3a6fd38`, `a0332bf`, `f40bd4d` |
| **Stage 2.5b — Multi-tab within a pane** | ✅ | `5316cae` |
| **Stage 2.5c — Multi-pane split-view via egui_dock** | ✅ | `19c2a77` |
| **Stage 2.5c follow-up — Flatten to `Tab::View(ViewId)`** | ✅ | `19c2a77` |
| **Focus / overlay UX polish** | ✅ | `19c2a77` |
| **Stage 2.5e — PersistedSession + Cache helper + command split** | ✅ | `3e5deb9` |
| **Stage 2.5d — `ViewKind` plumbing + socket protocol view-targeting + docs** | ✅ | (uncommitted) |
| Tier 3a — Buffer version counter (cache invalidation key) | 🟡 | landed as side effect of 2.5a; bump-on-edit waits for 3d |
| Tier 3b — Rope-backed Buffer | 📋 | — |
| Tier 3c — Anchors | 📋 | — |
| Tier 3d — Transactional edits + undo | 📋 | — |
| Tier 4 — Nice-to-haves | 📋 | — |

**At a glance:**
- All structural prerequisites for editor work are landed. The dock owns layout during a session; the workspace is a flat `views: HashMap<ViewId, View>` + `BufferStore`; persistence is path-keyed via `PersistedSession`.
- `Buffer::version` exists and the viewer's per-view caches key on it via the generic `Cache<K, V>` helper; once `Buffer` actually mutates (Tier 3d), invalidation Just Works.
- Command pipeline split into `command/{mod, file, nav, layout}.rs` — each file ≤253 LOC, room to grow as edit/multi-cursor/plugin variants land.
- Socket protocol accepts optional `view: ViewId` targeting; `docs/socket-protocol.md` documents the wire format, errors, and threat model; `docs/architecture.md` captures the background-task contract.
- 66 tests pass; clippy clean; full build green; macOS smoke-tested through Stage 2.5d.

---

## Tier 1 — Bug fixes and security hardening

These are real defects in the current code, independent of editor work. Land them as standalone PRs.

1. ✅ **Socket hardening.** `socket_path()` now prefers `$XDG_RUNTIME_DIR/seqforge-<pid>.sock` (per-user, mode-0700 directory) before falling back to `/tmp/seqforge-<pid>.sock`. After `bind`, the socket file is explicitly `chmod 0600`'d so only the owning user can connect. Threat model documented in `docs/socket-protocol.md` (Stage 2.5d).
2. ✅ **Fix `set_var`-after-thread-spawn ordering** (`cadd087`). All `unsafe { set_var(...) }` calls moved to `terminal::install_pty_env`, which `SeqForgeApp::new` invokes **before** `start_socket_listener` spawns the listener thread.
3. ✅ **Selection events from clicks** (`395eb2d`). `AppCommand::SetSelection` / `SelectFeature` route clicks through `command::apply` so `AppEvent::SelectionChanged` fires from a single path.
4. ✅ **Stale socket file cleanup.** `socket::SocketGuard` is a `Drop` wrapper held in `AppState`. Window close (normal exit) drops it and unlinks the file; the listener thread's existing on-error cleanup covers abnormal exit. Per-pid socket paths prevent collisions when both cleanups miss.
5. ✅ **Windows build.** `mod socket;` and every consumer in `app.rs` are `#[cfg(unix)]`-gated. The `seqforge-cli` viewer-IPC half (`dispatch_viewer_cmd`) is `#[cfg(unix)]` too, with a `#[cfg(not(unix))]` stub that returns a clear error. File commands (`info` / `digest` / `annotate`) work everywhere. Adopting `interprocess` for cross-platform sockets is deferred until Windows becomes a real target.

## Tier 2 — Code consolidation (cleanup pass)

Small, low-risk refactors that pay back as the editor lands.

6. ✅ **`Buffer::is_circular()` helper.** Audit complete — no `matches!(_, Topology::Circular)` predicate sites remained to replace. The only `Topology::Circular` references in the workspace are construction/parsing of the variant (genbank parser, raw constructors), which is correct.
7. 📋 **`InteractiveLayer` enum for viewer hit-testing.** Three parallel hit-test passes (annot / search / cut, `viewer.rs:255-312`) are structurally identical. One enum + one generic collector trims ~50 lines and keeps z-order explicit. Becomes essential when the linear/circular graphical views land (they'll share interaction logic). **Deferred** — collides with editor rendering changes (cursor / paste indicator); easier to land after Tier 3d.
8. 📋 **`BlockLayout` value type.** Block geometry (`block_h`, `n_blocks`, `block_y`, `seq_x0`, `line_width`) is recomputed in three places in `viewer.rs` (pass 1, pass 2, `screen_to_seq`). Compute once into a `BlockLayout`, pass everywhere. Eliminates a class of off-by-one bugs and is a pre-req for #7. **Deferred alongside #7.**
9. ✅ **`screen_to_seq` end-of-doc cursor.** Changed `p >= seq_len` to `p > seq_len`. The valid cursor range is now the closed `0..=seq_len`, with the upper bound being the "insert-at-end" position. Editor table stakes; lands here so Tier 3d edits have the affordance immediately.
10. ✅ **`Find` clears `selection` on empty pattern.** Empty `pattern` previously cleared `search_hits` only, leaving a stale selection (typically pointing at the first hit). Now also clears `selection` for consistency with the rest of the "drop derived data" surface (`Open` / `Close`). Test extended to assert both cleared.

## Tier 2.5 — Model object split + Workspace/Pane/View hierarchy

This is **the** architectural change that gates everything else. Each downstream tier (version counter, rope, anchors, transactions, undo) becomes a local PR inside a single type instead of a sweep across the codebase. Tabs and split-view become incremental rather than a rewrite. Linear/circular graphical views (post-MVP) land as `+1 enum variant`.

**Why now:** the current `AppState::viewer: ViewerState { open_doc, selection, scroll_to, search_hits, … }` shape conflates **buffer data** (bytes, features), **per-view UI state** (selection, scroll, search results), and **app-wide state** (which doc is active) into one struct. Multi-tab, multi-view, split-view, and shared buffer ownership are all blocked by this. Doing them piecemeal means three painful retrofits; doing them in one model-object split is one structural PR with no behavior change.

### 2.5.0 Locked-down decisions

These choices are cheap to commit to today and expensive to reverse later. Lock them in:

1. **Pane is the dock-tab unit.** `egui_dock::Tab::Pane(PaneId)` replaces `Tab::Viewer`. Free horizontal/vertical splits + tab drag-rearrange from egui_dock. The file browser and terminal stay as their own dock-tab kinds — they aren't split-able panes, they're sidebar/utility panes.
2. **Same buffer may appear in multiple panes (and in multiple views within one pane).** `Buffer` is owned via `Arc<RwLock<Buffer>>` and stored in a `BufferStore` keyed by `BufferId`. View state is independent per-view.
3. **`View` is the unit of selection + scroll + search results.** Not `Tab`, not `Pane`, not `Buffer`. A view references a buffer via `Arc<RwLock<Buffer>>` and holds everything that's specific to *this rendering* of *this buffer*.
4. **Active pane + active view = the "current" target for `AppCommand`s.** Most commands operate on `workspace.active_view_mut()`. Optional `pane`/`view` params on socket protocol for explicit targeting; default is active. Documented in `docs/socket-protocol.md`.
5. **`FocusScope::Pane(PaneId)`** replaces `FocusScope::Viewer`. KeyContext gets `Pane:<ViewKind>` tags so keymaps can target view kinds (`Pane:TextView`, `Pane:LinearView` later) without naming specific panes.
6. **Find bar is app-level, operates on active view, contents reflect `active_view.find_query`.** Tab switch swaps the bar contents. (Per-pane find bars are a v0.3+ concern.)
7. **`ViewKind` enum exists from day one with `TextView` as the only variant.** Adding linear/circular views post-MVP becomes a new variant + a new render impl, no dispatch refactor.
8. **Background-task contract:** background work read-locks `Buffer` or takes a cheap `BufferSnapshot` (rope clone). Writes happen **only** inside `apply()` on the UI thread, under a brief write lock. Background results post back as `AppCommand::TaskResult(...)`. Documented as an architectural invariant from day one even though no background tasks exist yet.
9. **Complement cache moves to `Buffer`** (pure function of the sequence, view-independent). All other per-view caches stay on `View` because they depend on view width / params (feature stacking, cut label stacking, etc.).
10. **Cache invalidation keys on `buffer.version()`.** Per-view caches store `cached_version: u64` and recompute when it diverges. Sets up Tier 3a directly.

### 2.5.1 Target type structure

```rust
// ── seqforge-core ────────────────────────────────────────────────────────────

pub type BufferId = u64;

/// The editable data. Shareable via Arc<RwLock<Buffer>>.
/// In Tier 2.5 this is essentially today's Document fields minus identity.
/// Tier 3 turns `text` into a rope, adds anchors + history + transactions.
pub struct Buffer {
    pub text: Vec<u8>,              // → Rope in Tier 3b
    pub complement: Vec<u8>,        // cached; recomputed on edit
    pub topology: Topology,
    pub version: u64,               // Tier 3a wires this in for cache invalidation
    // Tier 3c adds: anchors: AnchorMap
    // Tier 3d adds: history: History
}

/// Features and any view-independent derived data. Lives alongside a Buffer.
pub struct Annotations {
    pub features: Vec<Feature>,     // Tier 3c: ranges become anchors
}

/// Per-render state. Each open view in the UI gets one of these.
/// Selection, scroll, search results, find query state.
pub struct View {
    pub id: ViewId,
    pub buffer_id: BufferId,
    pub kind: ViewKind,
    pub selection: Option<Selection>,
    pub selected_feature: Option<usize>,
    pub scroll_to: Option<usize>,           // one-shot
    pub scroll_pos: Option<f32>,            // remembered on tab switch
    pub search_hits: Vec<SearchHit>,
    pub cut_sites: Vec<CutSite>,
    pub active_enzymes: Vec<String>,
    pub find_query: Option<FindQuery>,
}

pub type ViewId = u64;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ViewKind {
    TextView,
    // Future: LinearView, CircularView, FeatureTableView, …
}

// ── seqforge-app ─────────────────────────────────────────────────────────────

pub type PaneId = u64;

/// A pane in the dock area. Holds a tab strip of Views; one is active.
pub struct Pane {
    pub id: PaneId,
    pub views: Vec<View>,
    pub active: usize,              // index into `views`
    pub seq_view: SequenceView,     // rendering cache; tied to active view
                                    // (moved to View in 2.5.5 if cache-thrash on switch is felt)
}

/// Buffer-handle store, keyed by id. Dedupes when the same file opens twice.
pub struct BufferStore {
    buffers: HashMap<BufferId, Arc<RwLock<Buffer>>>,
    annotations: HashMap<BufferId, Annotations>,
    by_path: HashMap<PathBuf, BufferId>,
    next_id: BufferId,
}

pub struct Workspace {
    pub panes: HashMap<PaneId, Pane>,
    pub pane_order: Vec<PaneId>,    // for tab-cycle hotkeys
    pub active_pane: Option<PaneId>,
    pub buffers: BufferStore,
    next_view_id: ViewId,
    next_pane_id: PaneId,
}

impl Workspace {
    pub fn active_view(&self) -> Option<&View>;
    pub fn active_view_mut(&mut self) -> Option<&mut View>;
    pub fn active_pane(&self) -> Option<&Pane>;
    pub fn active_pane_mut(&mut self) -> Option<&mut Pane>;
    pub fn buffer(&self, id: BufferId) -> Option<Arc<RwLock<Buffer>>>;
    pub fn open_path(&mut self, path: &Path, bio: &dyn BioOps) -> Result<ViewId, …>;
    pub fn close_view(&mut self, pane: PaneId, view: ViewId);
    pub fn switch_to(&mut self, pane: PaneId, view: ViewId);
}

pub struct AppState {
    pub workspace: Workspace,       // replaces `viewer: ViewerState`
    pub dock_state: DockState<DockTab>,
    pub browser: BrowserState,
    pub recent_files: Vec<PathBuf>,
    pub overlays: OverlayStack,
    pub focus: FocusState,
    // events, terminal, socket_rx, toasts as today
}

pub enum DockTab {
    Pane(PaneId),                   // replaces `Viewer`
    FileBrowser,
    Terminal,
}
```

### 2.5.2 Dispatch reshape

`seqforge_core::dispatch` operates on a single view + its buffer:

```rust
pub fn dispatch<B: BioOps>(
    view: &mut View,
    buffer: &mut Buffer,            // write-locked by the caller (apply)
    annotations: &mut Annotations,
    bio: &B,
    req: ViewerRequest,
) -> Result<ViewerResponse, DispatchError>;
```

Active-view resolution happens once in `apply()`:

```rust
// command::apply (seqforge-app)
fn dispatch_to_active<B: BioOps>(state: &mut AppState, bio: &B, req: ViewerRequest)
    -> Result<ViewerResponse, DispatchError>
{
    let view = state.workspace.active_view_mut().ok_or(DispatchError::NoView)?;
    let buf_arc = state.workspace.buffers.get(view.buffer_id)?;
    let mut buf = buf_arc.write().unwrap();
    let ann = state.workspace.buffers.annotations_mut(view.buffer_id)?;
    dispatch(view, &mut buf, ann, bio, req)
}
```

`Open` is the special case — it goes through `Workspace::open_path` (which creates a buffer if new, finds the active pane, opens a new view in it) and does not flow through the per-view dispatch path.

### 2.5.3 Events become id-tagged

Every event that's view- or pane-specific carries the ids:

```rust
pub enum AppEvent {
    DocOpened { pane: PaneId, view: ViewId, buffer: BufferId, name: String, len: usize },
    DocClosed { pane: PaneId, view: ViewId, buffer: BufferId },
    SelectionChanged { pane: PaneId, view: ViewId, selection: Option<Selection> },
    SearchCompleted { pane: PaneId, view: ViewId, hits: usize },
    BufferEdited { buffer: BufferId, version: u64 },   // Tier 3d
    FocusChanged(FocusScope),
    PaneActivated(PaneId),
    TabSwitched { pane: PaneId, view: ViewId },
    OverlayPushed(&'static str),
    OverlayPopped(&'static str),
}
```

Status bar reads from active view; future panels can filter on `pane`/`view`/`buffer` ids.

### 2.5.4 New `AppCommand` variants

```rust
pub enum AppCommand {
    // existing variants…
    SwitchTab { pane: PaneId, view: ViewId },
    CloseTab { pane: PaneId, view: ViewId },
    NextTab,                        // active pane, next view
    PrevTab,
    SplitPane { direction: SplitDirection },    // post-MVP-friendly stub
    FocusPane(PaneId),              // FocusPane(FocusScope) → FocusPane(PaneId)
    // Tier 3d:
    // Undo, Redo,
}
```

### 2.5.5 Tab strip widget

Each `Pane` renders a tab strip at the top of its dock area before delegating to `SequenceView`. Clicks emit `SwitchTab`. Middle-click / × button emits `CloseTab`. Drag-reorder within a pane is a stretch goal; cross-pane drag (move a tab to another pane) is post-MVP.

### 2.5.6 Socket protocol additions

Add optional `pane` and `view` params to viewer methods (default: active). Document in `docs/socket-protocol.md` from day one so the schema is forward-compatible. Example wire:

```json
{"jsonrpc":"2.0","id":1,"method":"goto","params":{"position":100,"view":17}}
```

Agents can target any open view; humans get the default-to-active behavior via the embedded terminal.

### 2.5.7 Staged rollout (four PRs)

Each stage compiles and runs; `main` stays shippable.

- ✅ **Stage 2.5a — Types + single-pane/single-view migration** (`3a6fd38`, `a0332bf`, `f40bd4d`).
  Landed in three sub-commits:
  1. Introduced `Buffer`, `Annotations`, `View`, `ViewKind`, `Pane`, `BufferStore`, `Workspace` types — unused, with 14 new tests.
  2. Migrated `AppState::viewer: ViewerState` → `AppState::workspace: Workspace`. `dispatch` reshaped to `(view, buffer, annotations, bio, req)`. `Open` / `Close` moved out of dispatch into `Workspace::open_path` / `close_active_view`. Six closure helpers (`with_buffer{,_mut}`, `with_active_buffer{,_mut}`, `view{,_mut}`) hide lock acquisition and disjoint-borrow ceremony. Viewer caches re-key on `(buffer_id, buffer.version)`. New `DispatchError` variants: `NoActiveView`, `ViewNotFound(ViewId)`, `PoisonedLock`.
  3. Cleanup — deleted legacy `ViewerState` and `DispatchError::NoDocument` alias.

- ✅ **Stage 2.5b — Multi-tab support within a pane** (`5316cae`).
  `SwitchTab { pane, view }`, `CloseTab { pane, view }`, `NextTab`, `PrevTab` commands. Tab strip widget (`tabs.rs::render_tab_strip`) above the viewer area: selectable labels + × close buttons. `Cmd+W` closes active tab; closing the last view of a buffer also drops the buffer (emits `DocClosed`). `Cmd+Shift+]` / `[` cycle tabs. Open-of-already-open dedupes via `find_open_view_for` — switches to the existing tab instead of duplicating. New events: `TabSwitched`, `TabClosed`. `AppState::workspace` marked `#[serde(skip)]` because `BufferStore` holds `Arc<RwLock<Buffer>>` that can't round-trip; `recent_files` restores the working set across restarts.

- ✅ **Stage 2.5c — Multi-pane support (split-view via egui_dock).**
  Landed in two passes. The first introduced a `Pane` workspace concept paired with `Tab::Pane(PaneId)` in the dock; the follow-up flattened it (see next bullet). Net features:
  - `Cmd+\` splits the dock leaf hosting the active view; the split clones the active view's buffer into a new `View` in the new leaf so users get side-by-side comparison in one keystroke (Zed convention).
  - View menu offers Split Right / Split Below.
  - `Cmd+1`..`Cmd+9` focuses the Nth view tab in dock traversal order.
  - egui_dock provides drag-to-rearrange and drag-to-split-edge natively.
  - Closing the last view in a leaf no longer leaves an empty hole — a `Tab::Welcome` placeholder fills the central area whenever no `Tab::View(_)` exists.

- ✅ **Stage 2.5c follow-up — Flatten to `Tab::View(ViewId)`.**
  After 2.5c landed, the dock-level `Pane` tab and the in-pane custom tab strip were doing the same job (two tab strips per leaf). The follow-up dissolves `Pane` as a first-class workspace concept and addresses every viewer tab by `ViewId` directly. Concretely:
  - `Pane` / `PaneId` / `pane_order` / `active_pane` removed from `Workspace`. `Workspace::views: HashMap<ViewId, View>` is now the flat source of truth for view identity.
  - `SequenceView` render cache moved from `Pane` onto a `Workspace::seq_views: HashMap<ViewId, SequenceView>` keyed by view. Each view has an independent cache.
  - `Tab::Pane(PaneId)` → `Tab::View(ViewId)`. `FocusScope::Pane(PaneId)` → `FocusScope::View(ViewId)`. Events lose their `pane:` field.
  - egui_dock now owns *all* layout: which view is in which leaf, the tab order, the per-leaf active tab, drag-rearrange, split-via-drag. We render exactly one tab strip per leaf (the dock's native one).
  - End-of-frame reconciler: `dock_state.find_active_focused()` syncs into `workspace.active_view` via a `SwitchTab` command so dock-internal tab clicks flow through the single-applier path.
  - Dock × button routes through `TabViewer::on_close` → `AppCommand::CloseTab`, identical to ⌘W.
  - `apply_open_file::place_view_tab` targets the active view's leaf first, falls back to any leaf with View/Welcome, then last-resort focused leaf — new opens never land in Browser/Terminal.
  - **Persistence sanitizer**: on startup `dock_state` is loaded from disk but `workspace` is `#[serde(skip)]`, so persisted `Tab::View(_)` ids reference views that don't exist. `app.rs::sanitize_dock_after_restore` strips orphan view tabs and re-establishes the Welcome invariant before the first frame.

- ✅ **Focus / overlay UX polish** (bundled with the flatten).
  - **Find/GoTo bar anchored to the active view**: `show_inline_bar` is gated by `workspace.active_view == Some(this_view_id)` so the bar visually appears in the pane that will receive the search, regardless of which pane is rendering. `OpenFind`/`OpenGoTo` also pull focus into the active viewer before pushing the overlay.
  - **Focused-pane outline**: a 2px accent stroke is painted around the focused view's content rect each frame. Unambiguous in split-view layouts.
  - **Last-focused preservation**: `AppState::focus_before_overlay` snapshots `focus.scope` on empty→non-empty overlay push, restored on the corresponding pop. Wired through every overlay command. Dialog-accept (OpenFile) clears the snapshot so the new view's focus isn't overridden on completion.

- ✅ **Stage 2.5e — PersistedSession (path-keyed) + `Cache<K, V>` helper + command split.**
  Architectural deep-clean prompted by the NodeIndex-shift panic and the dual-source-of-truth bug class that produced it. Aligns the persistence model with Zed / VSCode: layout is owned by egui_dock during a session, but the save/load boundary speaks paths, not ids.

  **Persistence model (final shape for the project).** `AppState` is no longer `Serialize` — runtime state is purely transient. Persistence is a separate `PersistedSession` blob:
  ```rust
  // In-session: never persisted.
  struct Workspace {
      buffers: BufferStore,
      views: HashMap<ViewId, View>,        // ViewIds are session-scoped
      active_view: Option<ViewId>,
      seq_views: HashMap<ViewId, SequenceView>,
  }
  DockState<Tab>  // egui_dock owns layout during the session

  // The only thing that round-trips to disk.
  struct PersistedSession {
      recent_files: Vec<PathBuf>,
      layout: Option<LayoutSnapshot>,         // path-keyed tree of splits + leaves
      file_state: HashMap<PathBuf, FileState>, // selection + scroll per file
  }

  enum LayoutSnapshot {
      Leaf(LeafSnapshot),
      HSplit { ratio: f32, a: Box<_>, b: Box<_> },
      VSplit { ratio: f32, a: Box<_>, b: Box<_> },
  }
  enum LeafSnapshot {
      Browser, Terminal,
      Viewer { paths: Vec<PathBuf>, active: usize },
  }
  ```
  **Save flow** (in `eframe::App::save`): walk `dock_state` → emit `LayoutSnapshot` (resolving `Tab::View(vid)` → buffer's `source_path`); snapshot per-view state into `file_state`. **Load flow** (in `SeqForgeApp::new`): build `dock_state` skeleton from snapshot (placeholder leaves), replay `OpenFile` for each persisted path targeting the correct leaf, restore `selection`/`scroll_pos` from `file_state`. **Orphan view tabs are now impossible by construction** — `ViewId` and `BufferId` are never persisted.

  **Side effects of the persistence move:**
  - `sanitize_dock_after_restore` deleted (the bug it fixed is gone by construction).
  - The end-of-frame reconciler's defensive `views.contains_key` guard retained as belt-and-braces.
  - `apply_close_view` now stashes per-file state into `pending_file_state` so close + reopen restores selection/scroll inside a single session, not just across restarts.

  **`Cache<K, V>` helper** (`cache.rs`). Generic single-entry version-keyed cache; `get_or_compute(key, || ...)` runs the producer iff the key differs. Refactored `SequenceView`'s ad-hoc caches (`cached_feat_row`, `cached_cut_label_row`, etc.) into two `Cache` instances — feature stacking keyed by `(BufferId, version)`, cut-label stacking keyed by `(sorted_cut_positions, quantized_char_width)`. Pattern for every derived-data cache that lands in Tier 3+/4.

  **Command pipeline split** (`command/` directory). `command.rs` (was 643 lines, growing as edits land) replaced by four cohesive modules:
  - `command/mod.rs` (253 LOC) — `AppCommand` enum, `SplitDirection`, public `apply` dispatcher + `is_enabled`, shared helpers (`active_selection`, `emit_selection_diff`, `dispatch_active`, `snapshot/restore_focus_for_overlay`, `view_tab_order`, `count_view_tabs`).
  - `command/file.rs` (188 LOC) — Open / Close / recents / CLI install.
  - `command/nav.rs` (112 LOC) — Find / GoTo / Selection / Feature highlight.
  - `command/layout.rs` (218 LOC) — Split / Focus / tab cycling / dock-tree invariants (`ensure_welcome_invariant`, `place_view_tab`, `dock_activate_view`).

  Shared helpers are `pub(super)` on `mod.rs`; submodules import via `use super::...`. Adding a new command domain (e.g. `command/edit.rs` for Tier 3d) is now a localized change.

- ✅ **Stage 2.5d — `ViewKind` plumbing + socket protocol view-targeting + docs.**
  Closes out the structural Tier 2.5 work; remaining items are editor-track (Tier 3).
  - **`ViewKind` consumers wired in.** `view.kind` matched in `tabs.rs::Tab::View(_)` render path; `SequenceView::show` is the `TextView` arm. Adding `LinearView` / `CircularView` is now `+1 enum variant + +1 widget module + +1 match arm` — no dispatch refactor.
  - **`FocusState::rebuild_context` pushes `ViewKind::context_tag()`** (`Pane:TextView`) onto the keymap context stack when focus is on a viewer pane. Bindings can target a view kind without naming a pane id. `Pane:Viewer` stays as the generic workspace-level tag for cmd-chord scoping; the kind tag layers on top.
  - **Socket protocol view targeting.** `ViewerRequest::{GoTo, Find, Enzymes}` gain an optional `view: Option<ViewId>` field, serialized with `#[serde(skip_serializing_if = "Option::is_none")]` so default behaviour (omitted) operates on active view and the wire format stays clean for the common case. `ViewerRequest::target_view()` extracts the explicit id; `dispatch_active` routes via `with_buffer(vid, ...)` when set, `with_active_buffer(...)` when not. `ViewNotFound` if the view was closed mid-conversation.
  - **`seqforge` CLI gains `--view <ID>`** on `goto` / `find` / `enzymes`. Backwards compatible (flag is optional).
  - **`docs/socket-protocol.md`** written: transport, wire format, methods, view-targeting semantics, error codes (parse / invalid-request / method / params / dispatch — including ViewNotFound), 5-second dispatch timeout, threat model (local control plane, no auth, DoS surface noted).
  - **`docs/architecture.md`** written: background-task contract (write locks only on UI thread, background tasks read-lock or use future `BufferSnapshot`, results post back as `AppCommand::TaskResult`, cancellation via tokens); ViewKind dispatch checklist; Cache pattern checklist; Workspace/Layout/Persistence boundary summary.
  - **`ViewId: FromStr`** added in `seqforge-core::model` so clap can auto-parse `--view 5`.
  - **`BioOps::load` widening** is deferred — the adapter (`workspace.rs::pure_complement`) is small (~5 LOC of duplicated work per Open) and the rewrite touches three crates. Will land alongside Tier 3b (rope-backed Buffer) when both `Buffer` and `Annotations` need to flow through `BioOps` anyway.
  - ~~Session restore~~ — done as part of 2.5e via `LayoutSnapshot` + `file_state`.

Each stage is independently mergeable. Stage 2.5a was the load-bearing structural change; b/c/e each added a user-visible capability; 2.5d adds the remaining agent-protocol polish and locks in the async contract before Tier 4.

---

## Tier 3 — Editor-transition prerequisites (the big rocks) — ⚠️ SUPERSEDED

> **This whole tier is superseded by the editor undo decision** (see
> [`editor.md`](editor.md) → "Decision: undo model"). The editor ships on
> `Vec<u8>` + **snapshot-based undo**, not rope + anchors + inverse-op
> transactions. The mapping:
> - **3a (version counter)** — ✅ kept; already wired into the cache layer. The
>   first edit op (in `editor.md`) bumps it.
> - **3b (rope)** — ❌ removed from the roadmap. `ropey` is the known fix *if*
>   profiling later shows large-sequence edit lag; it would also make snapshot
>   undo O(log n)-cheap at genome scale. Not planned — adopt on evidence.
> - **3c (anchors)** — ⏸️ demoted to on-demand. Not tied to rope; useful only if
>   the manual feature-shift policy proliferates or a live multi-view selection
>   bug appears. Not a prerequisite.
> - **3d (inverse-op transactions)** — ❌ out of scope unless genome-scale
>   high-frequency editing becomes a goal.
>
> The text below is retained for rationale/history only; do not treat it as the
> plan of record.

With Tier 2.5 in place, each item below is a **local PR inside `Buffer`** (or `Annotations` for 3c features). Order matters: each builds on the previous one. Do them as four separate PRs, in this order, **before** writing any edit-feature code.

### 3a. 🟡 Buffer version counter — wired into cache, awaits edits to bump

The cache-invalidation half of 3a landed as a side effect of Stage 2.5a:

- `Buffer::version: u64` exists.
- `SequenceView` caches re-key on `(cached_buffer_id, cached_version)` instead of the old `cached_seq_len`. Mismatch on either ⇒ teardown.
- The complement cache moved onto `Buffer` itself (no separate viewer-side cache to invalidate).

What's left for 3a: nothing today, because no code mutates `Buffer::version` yet. The first edit operation (Tier 3d) will bump it, and the caching infrastructure picks up automatically. Audit + close-out when 3d lands.

### 3b. Rope-backed Buffer

Replace `Buffer::text: Vec<u8>` with a rope. `ropey` is the obvious pick (~2k LOC, no deps, byte-indexed). Behind the scenes:

- `Buffer::text: Rope` with `len()`, `slice(range)`, `byte_at(i)` accessors.
- `seqforge-bio` operations that scan the sequence iterate chunks instead of taking `&[u8]`. The search code in `seqforge-bio/src/search.rs` is the main consumer — switch to chunk-aware scanning (or, for the MVP read-only feature set, `rope.bytes().collect::<Vec<_>>()` and call existing code; convert later).
- The viewer's `build_strand_galley` already takes a slice; pass `rope.slice(block_start..block_end).bytes()` or materialize per-block.
- `Buffer::clone()` becomes cheap (rope clone is O(log n) structural share) — enables `BufferSnapshot` for background tasks (Tier 4).

Why now and not later: every editor mutation is O(n) on `Vec<u8>`. Even at 10 kb plasmids you'll feel it on a held-down delete key; at BAC scale (~150 kb) or whole-genome (Mb) it's unusable.

### 3c. Anchors

`Feature.range: Range<usize>` and `Selection { anchor: usize, focus: usize }` both store **absolute byte offsets**. The moment you insert 100 bp at position 50, every feature after that has stale indices and every view's selection (potentially across multiple views of the same buffer) points at the wrong base.

Introduce an `Anchor` type whose offset is resolved against the current `Buffer`. References:

- Zed's `text::Anchor` — sum-tree-backed, auto-shifts through edits. Overkill for SeqForge but the canonical model.
- Helix's `Range` + `Selection` — simpler, offset-based with explicit `map_through_changes(transaction)` step. Probably the right level for us.

Features (in `Annotations`) and selections (in `View`) stop storing raw offsets; they store anchors resolved at read time. The viewer queries `feature.range.resolve(&buffer)` each frame. **Do this before #3d** — undo/redo without anchors means undo-an-insert leaves selection in the wrong place, and split-view-of-same-buffer makes that bug doubly visible.

### 3d. Transactional edits + undo stack

Once anchors are in, add an operation model on `Buffer`:

```rust
pub enum EditOp {
    Insert { pos: Anchor, bytes: Vec<u8> },
    Delete { range: AnchorRange },
}

pub struct Transaction {
    ops: Vec<EditOp>,
    inverse: Vec<EditOp>,
    selection_before: Option<Selection>,
    selection_after: Option<Selection>,
}

impl Buffer {
    pub fn apply_transaction(&mut self, tx: Transaction);
    pub fn undo(&mut self) -> Option<Transaction>;
    pub fn redo(&mut self) -> Option<Transaction>;
}
```

`Buffer::apply_transaction` bumps `version`, applies the ops to the rope, and records the inverse on the buffer's history stack. **History lives on `Buffer`, not on `View`** — undo is per-buffer (which means two views of the same buffer share an undo stack, the correct behavior). `AppCommand::Undo` / `Redo` resolve the active view's buffer and call the corresponding method.

Hook this into the existing `command::apply` site — the focus refactor already made it single-threaded and centralized. Emit `AppEvent::BufferEdited { buffer, version }` so caches and any future panels invalidate without polling.

## Tier 4 — Nice-to-haves (after editor lands)

Defer until you actually feel the pain.

11. **Feature interval tree.** Pass 1 iterates all features per block (`viewer.rs:272, :513`). Fine at hundreds of features, painful at thousands. `rust-bio::data_structures::interval_tree::IntervalTree` keyed by anchor — O(log n + k visible).
12. **Background executor.** Restriction site / search are fast enough today; alignment, Golden Gate enumeration, PCR primer scoring will block the UI thread. Zed's pattern: a task executor with cancellation tokens, results posted back into `pending_commands` as `AppCommand::ViewerResult(...)`.
13. **Per-subscriber event channels.** Today `EventLog` is a global ring buffer everyone polls. Once there's >1 subscriber, give each its own `mpsc::Receiver` filtered to event kinds it cares about (Zed's `cx.subscribe` pattern).
14. **Multi-cursor selections.** Generalize `Selection` to `Vec<Selection>`. Helix's selection API is the reference; one-line struct change unlocks a major editing UX.
15. **`BufferSnapshot` / `Buffer` split.** Immutable snapshot type for rendering / search / annotations vs. mutable buffer for edits. Lets the viewer borrow a snapshot for a frame while a background task edits the live buffer. Pairs naturally with the rope.

---

## Sequencing summary

```
Phase 9 verification (in progress)
  │
  ├─→ Phase 9.5 minimap sidebar      ✅ fb7d2d6
  │
  │   Tier 1 — bug fixes
  ├─→ #2 set_var ordering         ✅ cadd087
  ├─→ #3 selection events         ✅ 395eb2d
  ├─→ #1 socket hardening         ✅ (uncommitted)
  ├─→ #4 stale socket cleanup     ✅ (uncommitted)
  └─→ #5 Windows build            ✅ (uncommitted)

  Tier 2 — consolidation
  ├─→ #6 is_circular audit        ✅ (already clean)
  ├─→ #9 end-of-doc cursor        ✅ (uncommitted)
  ├─→ #10 empty Find clear        ✅ (uncommitted)
  └─→ #7, #8 viewer.rs refactor   📋 (deferred post-Tier 3d)

  Tier 2.5 — model object split
  ├─→ 2.5a (1/3) types unused      ✅ 3a6fd38
  ├─→ 2.5a (2/3) migration         ✅ a0332bf
  ├─→ 2.5a (3/3) cleanup           ✅ f40bd4d
  ├─→ 2.5b multi-tab               ✅ 5316cae
  ├─→ 2.5c multi-pane split-view   ✅ 19c2a77
  ├─→ 2.5c flatten Tab::View       ✅ 19c2a77
  ├─→ focus/overlay polish + fix   ✅ 19c2a77
  ├─→ 2.5e PersistedSession etc.   ✅ 3e5deb9
  └─→ 2.5d ViewKind + socket proto ✅ (uncommitted)

  Tier 3 — editor prerequisites
  ├─→ 3a version-keyed caches      🟡 (caches done; edit-bump in 3d)
  ├─→ 3b rope-backed Buffer        📋
  ├─→ 3c anchors                   📋
  └─→ 3d transactions + undo       📋
       │
       ▼
  [Editor transition plan — separate document]
       │
       ▼
  Tier 4 — nice-to-haves (interval tree, background executor, etc.)
```

Tier 1 and Tier 2 land in parallel with Stage 2.5; none of them blocks the others. Stage 2.5 ended up as five landings — a/b/c were the original staged plan; **2.5e** was a follow-up architectural deep-clean prompted by realizing the dock_state-as-`#[serde]` pattern was the structural cause of every layout-sync bug we'd hit; **2.5d** is the remaining agent-protocol polish ahead of Tier 3. Tier 3 is strict-sequential and each item is now a local PR inside `Buffer`.

**Snapshot:** main is on `c053058` (Stage 2.5d docs). Uncommitted working tree carries the **Tier 1 hardening + Tier 2 editor-prep** bundle: socket path prefers `$XDG_RUNTIME_DIR`; bind is followed by `chmod 0600`; `SocketGuard` (Drop) cleans the socket file on normal exit; all socket/IPC code is `#[cfg(unix)]`-gated so Windows builds; `screen_to_seq` accepts the closed range `0..=seq_len` (insert-at-end cursor); empty `Find` now clears `selection` as well as `search_hits`. **Tier 2.5 + Tier 1 + the lighter Tier 2 items are all done.** The remaining Tier 2 work (`InteractiveLayer` enum, `BlockLayout` value type) is intentionally deferred because it will collide with editor rendering changes. Next concrete work is **Tier 3b — rope-backed `Buffer`**, which begins the editor track.
