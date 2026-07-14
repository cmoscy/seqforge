# SeqForge MVP Plan (v0.1 viewer track)

> **Status: COMPLETE (archival record).** Phases 0–9.5 are done; this document
> is the history of the read-only viewer milestone. Canonical status lives in
> [`../ROADMAP.md`](../ROADMAP.md); durable architecture contracts live in
> [`../docs/architecture.md`](../docs/architecture.md). Details below describe
> the codebase as of v0.1 — where a later refactor changed something (e.g. the
> `Document` → `Buffer`/`Annotations` split, the `na_seq` → `seqforge-restriction`
> swap), the superseding source is linked inline.

## Context

Build a Rust-based GUI sequence viewer/editor with an embedded terminal, targeted at molecular cloning workflows (restriction digest, PCR, Golden Gate, etc.). The defining architectural goal is a **single typed command layer** so that every operation is invokable from both the GUI menu and the embedded terminal — keeping execution flow uniform and minimizing code duplication as features land.

PlasCAD (`David-OConnor/plascad`, MIT, Rust + egui) already covers ~80% of this concept as a viewer/editor, but lacks an embedded terminal and a unified command-dispatch layer. After review, the chosen path is to **build fresh**, reusing the same upstream Rust bio crates PlasCAD depends on, and referencing PlasCAD's source for proven patterns. This gives full control over the command-dispatch architecture from day one and avoids inheriting design decisions that don't fit the terminal-first vision.

**MVP scope (locked):** read-only viewer + embedded terminal + file browser. No editing/saving in v1. Restriction-site detection and sequence search are the only sequence ops. Dual-strand text viewer (no graphical linear/circular views yet).

---

## Architecture

### GUI toolkit

**egui (via `eframe`) + `egui_dock`** for VSCode-style panel layout. Rationale: monospace text rendering is the optimal case for immediate-mode + galley caching; embedded terminal works via `egui_term`; "BYO state" pairs naturally with a `Command` enum dispatcher.

**Alternative on the table:** `iced` + `iced_term` v0.8.0 (March 2026, actively maintained by Harzu). Defer unless egui's state model becomes painful.

### Layout (egui_dock)

```
┌──────────┬─────────────────────────┐
│ File     │   Sequence Viewer       │
│ Browser  │   (dual-strand text +   │
│          │    annotations + sites) │
│          ├─────────────────────────┤
│          │   Embedded Terminal     │
└──────────┴─────────────────────────┘
```

### Command dispatch (the core pattern)

Commands fall into two distinct categories that determine how `seqforge-cli` executes them:

**File commands** — operate on sequence files on disk. No running GUI required. These are the primary standalone CLI use case: digest, annotate, align, cloning workflows. Always execute locally in the CLI process.

**Viewer commands** — mutate the state of a running GUI instance (scroll to position, open a file in the viewer, highlight a range, run a search and show results). Meaningless without a GUI; fail with a clear message if none is running.

```rust
// seqforge-core — no GUI dep, runs anywhere
enum FileCommand {
    Info { input: PathBuf },
    Digest { input: PathBuf, enzymes: Vec<String>, output: PathBuf },
    Annotate { input: PathBuf, output: PathBuf },
}

// seqforge-core — requires a running GUI instance; JSON-RPC wire encoding
#[serde(tag = "method", rename_all = "snake_case")]
enum ViewerRequest {
    Open { path: PathBuf },
    Close,
    GoTo { position: usize },
    Find { pattern: String, mismatches: u8 },
    Enzymes { enzymes: Vec<String> },
}

#[serde(tag = "kind", rename_all = "snake_case")]
enum ViewerResponse {
    Ok,
    Navigated { position: usize },
    SearchResults { count: usize, hits: Vec<SearchHit> },
    CutSites { count: usize, sites: Vec<CutSite> },
}

fn dispatch_file(cmd: FileCommand) -> Result<(), DispatchError>;
fn dispatch<B: BioOps>(state: &mut ViewerState, bio: &B, req: ViewerRequest)
    -> Result<ViewerResponse, DispatchError>;
```

Both menu clicks and terminal input parse to the appropriate `Command` type and call the right dispatch function. Terminal uses a thin parser (`clap` derive) so commands have help text and validation for free. **Reference: Helix's `helix-term/src/commands.rs` for the typed-action shape.**

### CLI as a first-class standalone tool

`seqforge-cli` is a complete tool independent of the GUI — modelled after `git`, not a GUI remote control. File commands work identically whether the GUI is open or not:

```bash
seqforge digest plasmid.gb --enzymes EcoRI BamHI -o fragments.gb
seqforge annotate input.gb --add-feature "CDS:100-500:+:lacZ" -o output.gb
seqforge align query.fa reference.gb -o alignment.gb
seqforge golden-gate parts/*.gb --enzyme BsaI -o assembly.gb
```

Viewer commands additionally try the GUI socket (see below) and error gracefully if no instance is running.

### GUI session IPC (viewer commands only)

Both human users and agents invoke viewer commands via `seqforge <subcommand>` in the embedded terminal. The CLI detects `SEQFORGE_SOCKET` and routes them to the GUI:

- On launch, SeqForge opens a Unix domain socket at a temp path and sets `SEQFORGE_SOCKET=/tmp/seqforge-$PID.sock` in the PTY environment.
- `seqforge-cli`, when executing a `ViewerCommand` and `SEQFORGE_SOCKET` is set, serializes the command and sends it over the socket. The GUI receives it and calls `dispatch_viewer`.
- If the socket is absent and the command is a `ViewerCommand`, the CLI exits with a clear error; `FileCommand`s are unaffected.
- `seqforge --help` gives any agent the full command schema with no extra documentation.

This is ~50 lines (socket listener in the app, socket-client in the CLI) and lands in Phase 6.

### Sandboxing (post-MVP hook, not in scope for v0.1)

The socket is a natural containment boundary — all viewer mutations flow through typed `ViewerCommand` values. The hooks to enable sandboxing are small and do not require changing the dispatch layer:

1. **PTY spawn** accepts a configurable wrapper command (macOS sandbox profile, Linux `bwrap`). Add the config field and stub; leave wrapper empty for now.
2. **Socket listener** validates incoming commands against a session policy before calling dispatch. Add the policy field, default to `AllowAll`.

Defer until the basic app is stable and sequence editing works smoothly.

### State model

**Two-layer split** — keeps GUI types out of `seqforge-core` so dispatch and socket IPC have no egui deps:

```rust
// seqforge-core — pure data, no GUI types
struct ViewerState {
    open_doc: Option<Document>,
    selection: Option<Selection>,      // cursor or range
    selected_feature: Option<usize>,
    scroll_to: Option<usize>,          // one-shot; consumed by viewer each frame
    search_hits: Vec<SearchHit>,
    cut_sites: Vec<CutSite>,
    active_enzymes: Vec<String>,
}

// seqforge-app — GUI shell
struct AppState {
    viewer: ViewerState,               // passed to dispatch()
    dock_state: DockState<Tab>,        // egui_dock — GUI only
    browser: BrowserState,
    pending_commands: Vec<PendingCommand>, // (AppCommand, Option<oneshot_tx>); consumed each frame
    overlays: OverlayStack,
    focus: FocusState,
    events: EventSink,
    // …
}
```

- `Document` is the doc model (sequence, features, computed cut-sites cache) — independent of egui types.
- Persist `AppState` (minus transient fields) via `eframe::App::save`/`load` with serde.
- For MVP, `open_doc` is `Option<Document>` (one file at a time). Multi-doc (`Vec<Document>`) deferred to post-MVP.
- **Reference: Rerun's `re_viewer` crate** for store-vs-UI-state separation in egui.

Keyboard focus, command dispatch, and the event bus are fully covered in [Focus & Command Architecture](#focus--command-architecture) below — that section is the binding reference for adding hotkeys, panes, overlays, and agent actions.

---

## Bio core (dependencies)

| Need | Crate | Status |
|---|---|---|
| GenBank parse/write | `gb-io` 0.9 | Active, used by PlasCAD |
| Restriction enzymes (recognition, cut offsets, Type IIs, presets) | `seqforge-restriction` (in-workspace) | **Replaced `na_seq` (see [`restriction.md`](restriction.md)). `na_seq` dependency dropped.** |
| FASTA + DNA primitives (complement, translation, GC%, MW) | hand-rolled in `seqforge-bio` | IUPAC complement table + parsers live in `seqforge-bio` |
| Pattern matching (IUPAC, mismatches), alphabets, alignment (later) | `bio` (rust-bio) 2.3 | Active |
| SnapGene `.dna` (deferred to post-MVP) | None — port from `tg-oss/packages/bio-parsers/src/snapgeneToJson.js` when needed | n/a |

**Targeted ports from `examples/tg-oss` (only as features land beyond MVP):**

- Digest fragment enumeration + overhang classification — `packages/sequence-utils/src/getDigestFragmentsForRestrictionEnzymes.js` (~150 LOC)
- Golden Gate part assembly (post-MVP) — `packages/sequence-utils/src/getPossiblePartsFromSequenceAndEnzymes.js`

**Already ported:**

- Annotation row-stacking — `stackElements` from `examples/seqviz/src/elementsToRows.ts` (~30 LOC Rust). Landed in Phase 4.

Skip porting: complement, translation (hand-rolled in `seqforge-bio`), restriction-site finding (now covered by the in-workspace `seqforge-restriction` crate, which replaced `na_seq` — see [`restriction.md`](restriction.md)). Digest fragment enumeration is `seqforge-restriction` Tier 2.

---

## Embedded terminal

- `egui_term` 0.1.0 (Apr 2025) — wraps `alacritty_terminal` and `portable-pty`. Renders into an egui `Ui`.
- Terminal widget owns its PTY + grid state.
- **Single command path:** `seqforge-cli` is the sole way to issue viewer commands from the terminal — `seqforge goto 100`, `seqforge find ATGC`, etc. The CLI detects `SEQFORGE_SOCKET` and routes over the socket. No keystroke intercept; TUI tools (vim, nvim, less) work normally.
- **Agent / script path:** same `seqforge <subcommand>` CLI calls. `dispatch_file` runs in-process; viewer commands go over the session socket and return structured JSON-RPC responses an agent can parse.
- For commands that need rich output (e.g., a digest fragment table), `ViewerResponse` carries the data; the app layer can open a result tab from the response kind.

---

## File browser

- Left pane: `egui_extras::TableBuilder` rows backed by `walkdir` for the project tree.
- File-open via `egui-file-dialog` (modal) and drag-and-drop via `egui::Context::input` drop events.
- Double-click on `.gb` / `.fasta` / `.fa` opens a viewer tab.

---

## Sequence viewer (dual-strand text)

Monospace rendering using `egui::Painter` + `Galley` (via `LayoutJob` for per-base ATGC coloring).

**Layout per block (standard convention: cut labels → ruler → strands → annotations):**

```
[cut label row 0: EcoRI  BamHI ]   ← stacked above ruler; omitted when no sites
[cut label row 1: HindIII      ]
[position ruler: 1    10   20 …]
[top strand 5'→3': A T G C …  ]   ← ATGC colored; cut staple passes through
[bottom strand 3'→5': T A C G …]  ← complement, dimmed; staple ends here
[annotation row 0              ]   ← stacked below strands
[annotation row 1              ]
…
[gap]
```

**Key design decisions made during implementation:**

- **Dynamic line width:** computed each frame from available pane width (`floor((width - margins) / char_width)`), not a fixed 60 bp. Blocks reflow on pane resize.
- **char_width source:** measured from an actual laid-out galley (`layout_no_wrap("A" × 64).width / 64`) rather than `glyph_width()`, which can differ due to subpixel rounding. This ensures annotation bar edges align exactly with character cell boundaries.
- **Annotation stacking:** port of seqviz `stackElements` — sort by start, greedily pack into the first non-overlapping row. `O(n log n)`, computed once per document load.
- **Feature selectability:** clicking an annotation bar sets `selection = (feature.range.start, feature.range.end)` and highlights the bar with a white border. Dragging on the strand initiates a sequence-range selection. Both expose `(start, end)` on `AppState` for command context.
- **Annotations render below strands** (standard convention: SnapGene, Geneious).

**Performance:**

- Each line rendered as a single `LayoutJob` galley (not per-character `painter.text` calls). Galley cache in egui makes repeat frames cheap.
- Painter clip-rect culling: blocks outside the visible scroll viewport are skipped before any layout work.

**Selection:** click+drag to select a range, exposes `(start, end)` to the dispatcher for context-aware terminal commands.

---

## Repository layout

```
seqforge/
├── Cargo.toml             # workspace
├── crates/
│   ├── seqforge-core/        # Buffer/Annotations/View, ViewerRequest/Response, BioOps, dispatch — no GUI deps
│   ├── seqforge-bio/         # gb-io + rust-bio wrappers; ported workflows; impl BioOps; bridges to seqforge-restriction
│   ├── seqforge-restriction/ # REBASE enzyme table + scanner + presets; zero-dep, extractable
│   ├── seqforge-cli/         # standalone tool: FileCommand runs locally always; ViewerRequest sent via JSON-RPC socket when SEQFORGE_SOCKET set
│   └── seqforge-app/         # eframe binary: egui + egui_dock + egui_term wiring; AppBio impl
└── examples/                 # existing reference repos (seqviz, tg-oss) — read-only
```

> **Updated since v0.1:** the original MVP layout was 4 crates over `na_seq`.
> `seqforge-restriction` replaced `na_seq` (see [`restriction.md`](restriction.md)),
> and `seqforge-thermo` is planned for the primer track (see [`primers.md`](primers.md)).
> The current crate dependency graph is in [`../docs/architecture.md`](../docs/architecture.md).

The split keeps GUI out of `core` so the same dispatcher can later back a headless CLI, test harness, or WASM WebView worker.

---

## Critical files to read before coding

- `examples/tg-oss/packages/sequence-utils/src/cutSequenceByRestrictionEnzyme.js` — restriction site logic reference
- `examples/tg-oss/packages/sequence-utils/src/getDigestFragmentsForRestrictionEnzymes.js` — fragment enumeration (port target post-MVP)
- `examples/seqviz/src/elementsToRows.ts` — annotation row-stacking algorithm (already ported)
- `examples/seqviz/src/digest.ts` — concise reference for cut-site dedup + circular handling
- PlasCAD `src/` (clone separately, MIT) — egui + bio crate wiring patterns
- Helix `helix-term/src/commands.rs` — typed-command dispatcher shape
- Rerun `re_viewer` (open source) — store/UI-state separation

---

## Verification (MVP done = all of these pass)

1. `cargo run` opens the app with the three-pane dock layout.
2. File browser shows `examples/` and lets you double-click a `.gb` file (use any GenBank file; if none in repo, drop one in).
3. Viewer pane renders top + bottom strands with index ruler, fills pane width dynamically, shows annotations stacked below with correct colors; clicking an annotation selects its range.
4. Embedded terminal accepts: `seqforge find ATGCGT`, `seqforge enzymes EcoRI BamHI`, `seqforge goto 1234`, `seqforge --help` — each invokes `dispatch` and updates the viewer.
5. Same operations work from the menu (`Edit → Find...`, `Tools → Restriction Sites...`, `Navigate → Go to position...`).
6. `seqforge goto 100` (no `:` prefix, plain shell command) in the embedded terminal also works — CLI detects `SEQFORGE_SOCKET` and routes to `dispatch_viewer`. `seqforge digest plasmid.gb --enzymes EcoRI -o out.gb` works in any terminal, GUI open or not.
7. App quits cleanly; on relaunch, recent files and dock layout are restored (`eframe` persistence).
8. Open at least one large plasmid (~10 kb) and one small fragment (~500 bp) without rendering hitches.
9. Smoke test on macOS (primary). Linux/Windows builds via CI; manual test deferred.

---

## Out of scope for MVP (explicit non-goals)

- Linear/circular graphical viewers (tg-oss/seqviz LinearView/CircularView)
- Editing, undo/redo, save
- SnapGene `.dna` support
- Cloning workflows (digest fragments, PCR, Golden Gate, Gibson)
- Primer design / Tm calc
- Alignment views
- WASM build
- Agent sandboxing / PTY namespace isolation (hooks designed in, implementation deferred)

---

## Implementation Phases

Each phase is independently testable. Don't start phase N+1 until phase N's "done" check passes.

### Phase 0 — Workspace skeleton ✅ DONE

**Goal:** Cargo workspace compiles, CI green, zero functionality.

- [x] `cargo new --bin seqforge-app` inside a workspace `Cargo.toml`
- [x] Add empty `seqforge-core`, `seqforge-bio`, `seqforge-cli` library crates
- [x] Add `eframe = "0.31"`, `egui_dock`, `egui_extras`, `egui-file-dialog` to `seqforge-app`
- [x] Add `gb-io = "0.9"`, `na_seq = "0.3"`, `bio = "2.3"` to `seqforge-bio`
- [x] Add `clap = { version = "4", features = ["derive"] }` to `seqforge-cli`
- [x] `rustfmt.toml` + `clippy.toml` (deny warnings in CI)
- [x] GitHub Actions: `cargo check`, `cargo test`, `cargo clippy -- -D warnings`, `cargo fmt --check`

**Done when:** `cargo run -p seqforge-app` opens an empty eframe window with "Hello" text and CI passes. ✅

---

### Phase 1 — Bio core: parse + model ✅ DONE

**Goal:** Load a GenBank file from disk into a GUI-free `Document` struct, exercise it via a headless CLI.

- [x] Define `Document { name, sequence: Vec<u8>, topology: Linear|Circular, features: Vec<Feature>, source_path }` in `seqforge-core`
- [x] `Feature { range: Range<usize>, kind: FeatureKind, label: String, strand: Strand, qualifiers: BTreeMap<String,String> }`
- [x] `seqforge-bio::load(path) -> Result<Document>` — dispatches on extension to `gb-io` (GenBank) or hand-rolled FASTA parser
- [x] `seqforge-bio::reverse_complement(&[u8]) -> Vec<u8>` + `complement(&[u8]) -> Vec<u8>` — IUPAC lookup table
- [x] Snapshot tests: round-trip 3 reference files (small linear, circular plasmid, multi-feature)

**Notes:**

- `na_seq` uses its own `Nucleotide` enum, not `&[u8]`. `reverse_complement` and `complement` are implemented directly with an IUPAC byte table.
- `gb_io::reader::GbParserError` is the public path (not `gb_io::errors::`).

**Done when:** `cargo run -p seqforge-cli -- info path/to/plasmid.gb` prints name, length, topology, feature count. ✅

---

### Phase 2 — egui dock shell ✅ DONE

**Goal:** Three-pane layout renders, no real content.

- [x] `egui_dock` skeleton with three tabs: `FileBrowser`, `Viewer`, `Terminal`
- [x] `AppState` struct held by the eframe `App` impl; tabs render placeholder text
- [x] Persist `DockState` via `eframe::App::save` → serde blob in eframe storage
- [x] Menu bar stub: `File`, `Edit`, `View`, `Tools`, `Navigate`, `Help` (items disabled)

**Notes:**

- `egui_dock` requires `features = ["serde"]` in `Cargo.toml` for `DockState: Serialize`.
- `TabViewer` holds a `'a` lifetime reference to mutable sub-state (browser) so tabs can mutate app state during rendering.
- Layout: FileBrowser 20% left; Viewer top-right 70%; Terminal bottom-right 30%.

**Done when:** three labelled empty panes; drag-rearrange works; layout survives restart. ✅

---

### Phase 3 — File browser pane ✅ DONE

**Goal:** Open a directory, click a `.gb` file, emit an `OpenFile` intent (no handler yet).

- [x] `BrowserState { root, expanded: HashSet<PathBuf>, selected: Option<PathBuf> }`
- [x] Render via recursive `walkdir` tree (depth=1 per node, sorted by name)
- [x] `egui-file-dialog` for "Open Folder…" modal (`dialog.pick_directory()` + `dialog.update(ctx)`)
- [x] Drag-and-drop folder onto window sets root (`ctx.input(|i| i.raw.dropped_files)`)
- [x] Double-click on `.gb` / `.gbk` / `.fasta` / `.fa` / `.fna` logs `OpenFile(path)` to stdout

**Notes:**

- `egui-file-dialog 0.9` API: `dialog.state()` returns `DialogState` enum; there is no `is_open()` method.
- `BrowserState` is `#[serde(skip)]` on `file_dialog` since `FileDialog` is not serializable.

**Done when:** folder tree visible, expandable, double-click logs the path. ✅

---

### Phase 4 — Viewer widget (dual-strand text) ✅ DONE

**Goal:** Render an open `Document` as dual-strand text with ruler, stacked annotations, and sequence selection.

- [x] `SequenceView` widget using `egui::Painter` + `LayoutJob` galleys
- [x] Top strand 5'→3' with ATGC base coloring; index ruler every 10 bp above
- [x] Bottom strand: complement (not reverse complement), dimmed, 3'→5' label
- [x] Dynamic line width — fills available pane width, reflows on resize
- [x] `char_width` derived from actual galley measurement (not `glyph_width`) to keep annotation bars aligned with character cells
- [x] Annotation stacking: port of seqviz `stackElements` — greedy interval packing, `O(n log n)`
- [x] Annotations render **below** both strands (standard convention)
- [x] Click annotation bar → selects feature range; drag on strand → sequence range selection; both expose `(start, end)` on `AppState`
- [x] Clip-rect culling: only visible blocks are processed each frame
- [x] `SequenceView::reset()` clears selection + selected feature on new doc load

**Implementation notes:**

- `cached_seq_len` guard: complement + stacking are computed once when `seq.len()` changes, cached in `SequenceView`. Not recomputed per frame.
- `pending_open: Option<PathBuf>` side-channel in `AppState`: `TabViewer` sets it during `DockArea` rendering; `update()` consumes it afterward. Phase 5 generalizes this to `pending_requests: Vec<PendingReq>`.
- Feature labels: rendered on any segment (including continuations) where `bar.width() >= label.chars().count() * char_width`. Omitted on narrow segments, consistent with SnapGene/Geneious behavior.

**Key files:**

- `crates/seqforge-app/src/viewer.rs` — `SequenceView`, `stack_features`, `annot_bar_rect`, `build_strand_galley`
- `crates/seqforge-bio/src/dna.rs` — added `complement()`

**Done when:** open `examples/…some.gb`, see paired strands + ruler + stacked annotations below, can scroll and select both features and sequence ranges. ✅

---

### Phase 5 — Command dispatch ✅ DONE

**Goal:** `FileCommand` and `ViewerCommand` enums with their dispatch functions wired to menu and file browser. The architectural keystone.

**Architecture note:** `dispatch_viewer` takes `&mut ViewerState` (pure data, in `seqforge-core`) not `&mut AppState`. `AppState` in `seqforge-app` holds `ViewerState` and passes a `&mut` reference. This keeps `seqforge-core` free of egui deps and makes Phase 6 socket IPC straightforward (the socket thread only needs `ViewerState`).

- [x] Extract `ViewerState` from `SequenceView` / `AppState` into `seqforge-core`; add clap dep to seqforge-core
- [x] Define `ViewerRequest` enum (`Open`, `Close`, `GoTo`, `Find`, `Enzymes`) + `ViewerResponse` enum; both derive clap + serde; wire encoding is `{"method":"..."}` JSON-RPC 2.0
- [x] Define `FileCommand` enum; stub variants: `Info`, `Digest`, `Annotate`
- [x] `BioOps` trait in `seqforge-core` — `load`, `find_matches`, `find_cut_sites`; `AppBio` in `seqforge-app` implements it; breaks core/bio dep cycle
- [x] `dispatch<B: BioOps>(state, bio, req) -> Result<ViewerResponse, DispatchError>` — single dispatch function; no `SideEffect` indirection
- [x] `dispatch_file(cmd: FileCommand) -> Result<(), DispatchError>` in `seqforge-core`
- [x] `AppState`: `viewer: ViewerState` + `seq_view: SequenceView` + `pending_requests: Vec<PendingReq>` (request + optional oneshot response channel)
- [x] `SequenceView` reads from `&mut ViewerState` rather than holding document data itself
- [x] Wire `File → Open…` and `File → Close` menu items; `Edit → Find…`, `Navigate → Go to…`, `Tools → Restriction Sites…` stubs
- [x] File-browser double-click emits `ViewerRequest::Open` through dispatch
- [x] `Selection { anchor, focus }` — cursor when equal, range when not; single click places cursor, drag builds range, annotation/hit/site click sets range

**Notes:**

- `Selection` replaces raw `Option<(usize, usize)>` — cursor = zero-length selection (seqviz/SnapGene pattern)
- `BioOps` trait bridges the core/bio crate boundary: dispatch calls `bio.load` / `bio.find_matches` / `bio.find_cut_sites` directly — no `SideEffect` round-trip through the app layer
- `GoTo` dispatch validates `position` in `[1, seq_len]`; out-of-range returns `DispatchError::OutOfRange { position, seq_len }`
- `scroll_to: Option<usize>` on `ViewerState` is a one-shot field: set by `GoTo`/`Find` dispatch, consumed by the viewer to center in viewport

**Done when:** opening files works via menu *and* file-browser double-click, both go through `dispatch_viewer`. Both dispatch functions are unit-tested. ✅

---

### Phase 6 — Embedded terminal + session IPC ✅ DONE

**Goal:** Terminal pane runs a real shell; `:viewer-commands` route to `dispatch_viewer`; plain shell commands and `seqforge file-commands` run normally; `seqforge viewer-commands` route to `dispatch_viewer` via session socket.

**Terminal:**

- [x] `egui_term 0.1.0` widget in the Terminal tab, spawning `$SHELL`
- [x] No keystroke intercept — viewer commands issued via `seqforge <subcommand>` CLI directly in the shell; TUI tools (nvim, vim, less, htop) work unaffected
- [x] Embedded terminal history isolated to `~/.local/share/seqforge/terminal_history` via `HISTFILE` set before PTY spawn

**Session socket (viewer commands from CLI/agents):**

- [x] On app start, open Unix socket at `/tmp/seqforge-{pid}.sock`; set `SEQFORGE_SOCKET` in env before PTY spawn (child shell inherits it)
- [x] Socket listener thread receives newline-delimited JSON-RPC 2.0 requests, parses into `ViewerRequest` via serde tagged enum, dispatches, returns a JSON-RPC response on the same connection; pushes `(ViewerRequest, oneshot_tx)` to `socket_rx` mpsc channel; main `update()` drains it into `pending_requests` each frame
- [x] `seqforge-cli`: viewer subcommands (`open`, `close`, `goto`, `find`, `enzymes`) read `SEQFORGE_SOCKET` and send JSON over socket; error if unset
- [x] File subcommands (`info`, `digest`, `annotate`) always run in-process; `FileCommand` never touches socket

**Sandboxing stubs (design only — implement post-MVP):**

- [x] PTY spawn: comment stub in `TerminalPane::new` — `sandbox_wrapper: Option<Vec<String>>` hook location documented
- [x] Socket listener: comment stub in `handle_connection` — `CommandPolicy` validation hook location documented

**Implementation notes:**

- `TerminalPane` and `socket_rx` live in `AppState` as `#[serde(skip)]` fields — avoids split-borrow issues and keeps initialization in `SeqForgeApp::new`
- `TerminalView::new(ui, ...)` assigned to a local before `ui.add(...)` to satisfy borrow checker (both borrow `ui`)
- `std::env::set_var`/`remove_var` are `unsafe` in Rust 2024 edition; wrapped with safety comments

**CLI PATH scoping (added post-Phase 6):**

- `seqforge` CLI is embedded-terminal-only by default (VS Code "Install command in PATH" pattern)
- `sibling_seqforge_dir()` in `terminal.rs` finds the `seqforge` binary next to the running app binary and prepends it to the PTY's PATH — `cargo build` (not `cargo install`) is sufficient for embedded terminal use
- `cli_install.rs` in `seqforge-app`: `install_cli_to_path()` symlinks the bundled CLI to `/usr/local/bin/seqforge` or `~/.local/bin/seqforge`; `is_installed()` checks for an existing symlink
- `Tools → Install 'seqforge' CLI to PATH` menu item (or `Reinstall…` if already linked); result shown in a centered modal window via `cli_status: Option<String>` in `AppState`
- `seqforge-app --install-cli` flag for headless/scripted installs (prints result and exits)
- `README.md` written with install instructions, `seqforge <subcommand>` CLI usage, opt-in PATH install, supported formats, and dev workflow

**Tests (18 total across workspace):**

- `socket::tests` — JSON command round-trip via `UnixStream::pair()`; `FileCommand` serialization check
- `seqforge_cli::tests` — viewer cmd fails cleanly without `SEQFORGE_SOCKET`
- `seqforge_core::commands::tests` — dispatch coverage for GoTo bounds, Find, Enzymes, error cases

**Done when:** `seqforge open path/to/file.gb`, `seqforge find ATGCGT`, `seqforge goto 1234` work from the terminal via socket. `seqforge digest plasmid.gb --enzymes EcoRI -o out.gb` works whether or not the GUI is open. ✅

---

### Phase 7 — Restriction sites + search ✅ DONE

**Goal:** The two real sequence operations for MVP.

> **Superseded (post-MVP):** the `na_seq`-based cut-site backend described
> below was replaced by the in-workspace `seqforge-restriction` crate
> (REBASE-derived table, Type IIs support, presets). The `seqforge-bio`
> public API (`find_cut_sites`, `resolve_query`) is unchanged — the swap was
> invisible to callers. See [`restriction.md`](restriction.md) for the new design. The
> checkboxes below record the original Phase 7 implementation.

- [x] Use `na_seq`'s restriction enzyme module (`re_lib::load_re_library` + `find_re_matches`) to find cut sites
- [x] `find_iupac_matches` — own O(n·m) IUPAC scanner with Hamming-distance mismatch allowance; circular extension handled by appending first `pat_len-1` bases before scanning
- [x] Both forward + reverse-complement search; palindromic patterns deduplicated
- [x] `SearchHit { start, end, strand }` and `CutSite { enzyme, recognition_start, recognition_end, cut_pos, bottom_cut_pos }` types in `seqforge-core`
- [x] `bottom_cut_pos` for palindromic enzymes derived from palindrome symmetry: `recognition_end - cut_after - 1`; equals `cut_pos` for blunt cutters
- [x] `ViewerState` gains `search_hits`, `cut_sites`, `active_enzymes` (all `#[serde(skip)]`, cleared on new doc load)
- [x] `BioOps` trait bridges core/bio boundary — `dispatch` calls `bio.find_matches` / `bio.find_cut_sites` directly and populates `ViewerState`; no `SideEffect` indirection
- [x] Render search hits as amber (forward) / cyan (reverse) semi-transparent highlights behind strand text; clicking a hit selects its range
- [x] Render cut sites as **staple shapes** through the strand rows — vertical top line from stacked label through top strand, horizontal bridge to `bottom_cut_pos`, vertical bottom line through bottom strand; blunt cutters use a single straight line
- [x] Cut site labels stacked above the ruler using the same greedy interval algorithm as feature stacking; `block_h` grows by `n_label_rows × CUT_LABEL_ROW_H` (14 px/row)
- [x] Cut label stacking cached in `SequenceView` (`cached_cut_site_key`, `cached_char_width`); invalidation key is a sorted `Vec<usize>` of cut positions — catches same-count enzyme swaps that a bare count check would miss
- [x] Clicking a cut site label selects the recognition site range; staple line area remains clickable for cursor placement (not enzyme selection)
- [x] Empty `seqforge find` clears hits; empty `seqforge enzymes` clears cut sites; both require an open document

**Implementation notes:**

- `na_seq::restriction_enzyme::find_re_matches` only searches forward — correct for palindromic enzymes (all entries in na_seq's library); circular handled by extending input sequence
- na_seq's `find_re_matches` skips the last `re_seq_len + 1` positions (off-by-one in upstream code); circular extension compensates
- `find_iupac_matches` and `find_cut_sites` live in `seqforge-bio/src/search.rs`
- `stack_features` and `stack_cut_labels` are thin wrappers over a shared `greedy_stack(ranges: &[(usize, usize)]) -> (Vec<usize>, usize)`; algorithm lives in one place
- Label width approximated by `LABEL_CHAR_W` const (`(FONT_SIZE - 3.0) * 0.55`) — used by both `stack_cut_labels` and Pass 1 click-rect computation
- `open_doc(state) -> Result<&Document, DispatchError>` helper replaces the `require_document` + `.unwrap()` two-step that appeared in three dispatch arms
- `Close` dispatch calls `clear_results()` — fixes stale `search_hits`/`cut_sites` that persisted after document close
- `Find` dispatch sets `selection` to the first hit's range alongside `scroll_to`, so the viewer lands on the first result with it visually selected
- Cut site x positions are inter-base: `seq_x0 + col * char_width` places lines between character cells, matching the cursor line convention

**Tests (14 in `seqforge-bio::search`, 4 new in `seqforge-core::commands`):**

- `exact_forward_hit`, `palindrome_not_double_counted`, `reverse_complement_hit`, `iupac_n_wildcard`, `mismatch_allowance`, `circular_wrap_around`
- `find_ecori_cut_sites`, `unknown_enzyme_returns_empty`, `enzyme_name_case_insensitive`, `multiple_enzymes`
- `find_returns_search_side_effect`, `enzymes_returns_show_enzymes_side_effect`, `find_without_doc_returns_error`, `enzymes_empty_clears_cut_sites`

**Done when:** `seqforge enzymes EcoRI BamHI` shows staple-shaped cut sites with stacked labels above the ruler at known positions; `seqforge find ATGCNNNNGCAT` highlights IUPAC matches on both strands; clicking a search hit or enzyme label selects the corresponding range. ✅

---

### Phase 8 — Persistence + polish ✅ DONE

**Goal:** App feels finished for MVP scope.

- [x] Recent files list persisted in eframe storage; `File → Recent` submenu (max 10, deduped)
- [x] Dock layout persistence (already in Phase 2 — verified)
- [x] Keyboard shortcuts: `Cmd/Ctrl+O`, `Cmd/Ctrl+F`, `Cmd/Ctrl+G`, `Cmd/Ctrl+W`
- [x] Shift+click range selection: extends `selection.focus` while holding `selection.anchor` fixed
- [x] Status bar at bottom: cursor position, selection length, doc length, topology
- [x] Error toasts via `egui-notify` for failed file loads / bad commands
- [x] `Edit → Find…` and `Navigate → Go to Position…` wired to inline viewer bar

**Find / GoTo UX — inline bar (not floating dialogs):**

Both Find and GoTo use an inline bar rendered at the top of the Viewer tab pane, not floating `Window` dialogs. This follows the VSCode / SnapGene convention: the document stays live and interactive while search or navigation is active.

```
┌─────────────────────────────────────────────────┐
│ Find: [ATGCNNNN______] Mismatches: [0] [Find] [Clear] [✕] │  ← inline bar
├─────────────────────────────────────────────────┤
│  sequence viewer content …                      │
└─────────────────────────────────────────────────┘
```

Find / GoTo bars are `Overlay::FindBar` / `Overlay::GoToBar` variants on the shared `OverlayStack`; submission produces an `AppCommand::SubmitFind` / `AppCommand::SubmitGoTo` that `apply()` translates into a `ViewerRequest`. Escape pops the overlay; clicking the bar's text field is the only way it captures input. See [Focus & Command Architecture](#focus--command-architecture) for the overlay model and "How to add X" for adding a new overlay.

**Key files:**

- `crates/seqforge-app/src/overlay.rs` — `Overlay`, `OverlayStack`, `FindBar`, `GoToBar` (absorbs the old `bar.rs`)
- `crates/seqforge-app/src/keymap.rs` — `⌘F` / `⌘G` bindings with `when_context = ["Pane:Viewer"]`
- `crates/seqforge-app/src/command.rs` — `AppCommand::{OpenFind, OpenGoTo, SubmitFind, SubmitGoTo, DismissOverlay}` and `apply()`

**Done when:** the MVP verification checklist (top of this plan) all passes. ✅

---

### Phase 9 — Verification + release prep

- [ ] Walk the MVP verification checklist on macOS
- [ ] `README.md` screenshots (README prose written in Phase 6; add screenshots here)
- [ ] Socket hardening: prefer `$XDG_RUNTIME_DIR/seqforge-{pid}.sock` over `/tmp`; `chmod 0600` immediately after `bind`; update `SEQFORGE_SOCKET` propagation to PTY; set env vars **before** spawning the socket thread (fixes Rust-2024 `set_var`-after-thread-spawn UB)
- [ ] Write `docs/socket-protocol.md` — one-page JSON-RPC 2.0 wire format reference (method names, params shape, response variants, standard error codes); state the threat model (single-user workstation; per-user runtime dir is the boundary; no auth token)
- [ ] Apply the [Pre-editor Refactor Punch List](#pre-editor-refactor-punch-list) below
- [ ] Tag `v0.1.0`

> **Update:** CI *does* exist — `.github/workflows/ci.yml` runs
> `cargo fmt --all --check`, `cargo clippy --all-targets --all-features -- -D warnings`,
> and `cargo test --workspace` (cheapest-first ordering, recently focused). The
> original "CI not in scope for v0.1" note is superseded.

---

### Phase 9.5 — Sequence Minimap Sidebar Panel ✅ DONE

**Goal:** Compact, read-only sequence overview panel below the file browser. Topology-aware: circular sequences render as a plasmid ring with feature arcs; linear sequences render as a proportional horizontal bar with feature rectangles. Click-to-navigate via the existing `GoTo` path. Non-focusable; never mutates state directly.

**Features landed (`fb7d2d6`):**

- [x] `MiniMap` widget in `crates/seqforge-app/src/minimap.rs` — retained state, geometry cache, painter
- [x] **Topology dispatch:** `buf.is_circular()` → circular ring (`paint_circular`) or linear bar (`paint_linear`); adding a new topology is `+1 branch`
- [x] **Circular ring:** backbone ring via `painter.circle_stroke`; feature arcs as polyline approximations (~1 segment per 3°); LOD filter drops arcs < 2.5° span
- [x] **Linear bar:** backbone rect; feature bars using greedy stacking (reuses `viewer::greedy_stack`); LOD filter drops bars < 2 px wide; feature colors reuse `viewer::feature_color`
- [x] **Geometry cache:** keyed by `(BufferId, buffer.version, quantised_panel_size)` — identical invalidation contract to `SequenceView::feature_cache`; rebuild is free once the editor lands and starts bumping `version`
- [x] **Click-to-navigate:** angular hit-test (circular) or proportional x hit-test (linear) → `AppCommand::Viewer(GoTo{position})`
- [x] **Cursor indicator:** 2 px white tick radially through backbone (circular) or 1.5 px white vline on spine (linear)
- [x] **Selection highlight:** blue semi-transparent arc (circular) or rect (linear) over the selected range
- [x] **Selected feature highlight:** white stroke border over the selected feature's arc/bar
- [x] **Strand arrowheads:** small filled triangles at arc/bar termini for `Strand::Forward` / `Strand::Reverse` features
- [x] **Viewport indicator:** `View::visible_range` written each frame by `SequenceView::show`; minimap renders a white semi-transparent arc (circular) or rect (linear) showing the currently visible portion of the sequence
- [x] **Header label:** construct name (truncated with `…`) + bp count + topology tag rendered above the panel
- [x] **Dynamic sizing:** panel fills available pane space; circular is `min(w, h)` square; linear uses full width; panel size is part of the cache key (quantised to 0.5 px steps)
- [x] **Resizable split:** drag handle between browser tree and minimap adjusts `browser_fraction`; persisted in `MiniMap` across tab switches; clamped to `[0.15, 0.85]`
- [x] **Centering:** circular ring horizontally and vertically centered in the available panel area
- [x] Background inherits egui theme (no explicit fill = transparent over egui's default)

**Key files:**

- `crates/seqforge-app/src/minimap.rs` — new; `MiniMap`, geometry builders, painters, arrowhead helpers
- `crates/seqforge-app/src/tabs.rs` — `TabViewer::minimap` field; `Tab::FileBrowser` arm wired with drag handle and `minimap.show()`
- `crates/seqforge-app/src/viewer.rs` — `visible_range` computed and written to `view.visible_range` at end of scroll closure; `greedy_stack` + `feature_color` + `StackLayout` made `pub(crate)`
- `crates/seqforge-core/src/model.rs` — `View::visible_range: Option<(usize, usize)>` added (`#[serde(skip)]`)
- `crates/seqforge-app/src/app.rs` — `AppState::minimap: MiniMap` + destructuring in `update()`

**Done when:** open `pUC19.gbk` → ring with colored arcs + arrowheads + white viewport arc; scroll text viewer → arc moves; click ring → viewer navigates. Open linear `.fasta` → bar with feature rects + viewport rect. ✅

---

## Dependency-of-phases graph

```
0 → 1 → 2 → 3 → 5 → 6
               ↓    ↑
               4 ───┘
                    ↓
                    7 → 8 → 9 → 9.5
```

Phase 4 (viewer) and Phase 5 (dispatch) can be developed in parallel after Phase 3. Phase 6 needs both. Phase 7 needs viewer + dispatch.

---

## Conventions summary (apply across all phases)

- **Errors:** `thiserror` in libs, `anyhow` at app boundary. No `unwrap()` in non-test code.
- **State:** `AppState` is the single source of truth; widgets receive `&mut` references, never own data.
- **Commands:** every user-visible action goes through `dispatch`. No menu handler does work directly.
- **Bio types:** `Vec<u8>` for sequences (ASCII bytes), not `String`. Half-open `Range<usize>` for ranges. *(See the [Pre-editor Refactor Punch List](#pre-editor-refactor-punch-list) — `Vec<u8>` is slated for replacement by a rope before editing lands.)*
- **Files:** sequence files via `seqforge-bio::load`; never have GUI code touch `gb-io` or `seqforge-restriction` directly (the latter is reachable only through `seqforge-bio` — see [`../docs/architecture.md`](../docs/architecture.md) "Restriction backend boundary").
- **Tests:** every pure function in `seqforge-bio` and `seqforge-core` gets unit tests; widgets get manual smoke tests documented in the phase's "done when" line.
- **Fixtures:** check in 3 small reference files under `crates/seqforge-bio/tests/fixtures/` (small linear, plasmid, multi-feature). Avoid >100 kb test files.

---

## Reference repos (clone separately, do not vendor)

| Repo | Used for |
|---|---|
| `David-OConnor/plascad` | egui + bio crate wiring, sequence rendering reference |
| `helix-editor/helix` | typed Command enum + dispatcher pattern |
| `rerun-io/rerun` | egui store/UI-state separation, dock viewport |
| `Harzu/egui_term` | terminal widget integration examples |
| `dlesl/gb-io` | GenBank parsing examples in `examples/` |
| `rust-bio/rust-bio` | pattern matching, alphabets |
| `zed-industries/zed` | rope + anchors + transactional edits + action/keymap model (reference for the editor transition) |

---

## Post-v0.1: enzyme overlay + set-as-source-of-truth (commit `cca1812`)

Shipped after the MVP; recorded here so the command surface is documented.

- **`view.active_enzymes` is canonical; `view.cut_sites` is always re-derived** via the single `find_cut_sites` scanner. No second source of truth.
- **`ViewerRequest::Enzymes` carries `op: EnzymeOp { Set, Add, Remove }`** (serde `default` = `Set`); dispatch does set-math (`union`/`difference` over canonical names, case-insensitive, idempotent) then one re-scan. `BioOps::resolve_enzyme_names` returns canonical names (presets expanded, unknowns dropped). CLI exposes `--op`.
- **`CutSite.recognition: String`** (IUPAC pattern, display-only) populated by `site_to_cutsite`.
- **Overlay UI** (`⌘E`, persistent; Esc closes, re-open rehydrated): scrollable list of displayed enzymes, ＋Add, per-row ✕ remove, click name → reveal (single) / expand sites (multi), per-site jump via `AppCommand::RevealRange` (selection + scroll). Isoschizomers stay as distinct rows (decision 4 in [`../ROADMAP.md`](../ROADMAP.md)).

## Post-v0.1: cut-site hover + label decluttering *(Phase 16 GUI-walk findings)*

Two cut-site presentation refinements surfaced while walking the editor. Both are
**render/interaction only** — no change to `cut_sites`/`active_enzymes` (decision 4
holds: enzymes stay distinct entities, individually hover/click-able; this only
changes how their labels are *drawn* and how hover *feedback* reads).

- [ ] **Hover highlights the recognition site.** Hovering an enzyme label (already
  the `hovered_cut_site` trigger for the staple reveal) also washes its
  `recognition_start..recognition_end` on **both** strands in the neutral
  `ui.hover_wash` grey — the enzyme half of the shared `BlockCtx::hover_footprint`
  path (the primer half is single-stranded; see [`primers.md`](primers.md)
  "Rendering"). Directly disambiguates a crowded MCS: point at one name, its exact
  site lights up. Ephemeral, paint-time only.
- [ ] **Co-located labels group under one leader.** Today `build_block_layouts`
  greedy-stacks *each* cut site independently, so isoschizomers sharing a `cut_pos`
  (e.g. AvaI/XmaI/BsoBI/TspMI) each take a row **and** redraw a tick at the same x —
  a pile of overlapping ticks with no "one site" signal. Fix: bucket co-located
  sites into a **group** (keyed on `cut_pos`), stack the *groups*, and render each
  group's names as a tight vertical stack over a **single** leader tick. Per-name
  hit rects preserved (each enzyme stays individually addressable). Matches the
  SnapGene idiom. *(Angled leader-line routing for near-but-distinct labels is a
  larger layout task — deferred.)*

