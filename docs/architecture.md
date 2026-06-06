# SeqForge architecture notes

Cross-cutting design notes that don't belong inside a single source
file. Per-feature design rationale lives in `PLAN.md`; this document
is for **contracts that span modules and tiers**.

## Single-applier mutation pattern

See `docs/focus-refactor.md` §2 for the full lifecycle. In short:

- `AppCommand` is a closed enum of every user-, agent-, or code-
  initiated action.
- `AppState::pending_commands: Vec<PendingCommand>` is a per-frame
  queue.
- `command::apply` is the **only** function in the crate that mutates
  the fields a command can touch. Every menu, hotkey, socket request,
  bar submission, focus change, and (future) edit op goes through it.
- The applier drains the queue exactly once per frame. Commands
  enqueued *during* application (chaining) are processed next frame —
  predictable ordering, no infinite-loop risk.

## Workspace / Layout / Persistence boundary (Stage 2.5e)

State is split by **lifetime**, not by struct:

```
Workspace            ← in-session identity & state
├── buffers (Arc<RwLock<Buffer>>)
├── views (HashMap<ViewId, View>)
├── active_view
└── seq_views (per-view render caches)

DockState<Tab>       ← egui_dock owns layout during a session
                       Tab::View(ViewId) refs are session pointers

PersistedSession     ← the only thing that round-trips to disk
├── recent_files
├── layout (LayoutSnapshot — path-keyed)
└── file_state[path] (selection, scroll)
```

`ViewId`/`BufferId` are session-scoped and never persisted. The
save/load boundary speaks `PathBuf`. This makes orphan-id bugs
impossible by construction.

See `crates/seqforge-app/src/persistence.rs` for the types and
`SeqForgeApp::new` / `save` for the wiring.

## Background-task contract (Stage 2.5d)

SeqForge runs UI on the main thread (egui). Long-running biological
computations (alignment, Golden Gate enumeration, PCR primer scoring,
post-MVP) must not block paint. This section documents the contract
that **all** future background tasks must follow. Today no background
tasks exist; documenting the rules now keeps the door open without
forcing a retrofit later.

### Threading rules

1. **Write locks live on the UI thread.** A `Buffer`'s `RwLock` may
   only be `.write()`-locked inside `command::apply` (running on the
   main thread). Background tasks **never** take a write lock.
2. **Background tasks read-lock or snapshot.** Either
   - `buf_arc.read()` — short-lived shared read; cheap but blocks
     UI-thread writes for its duration, so suitable only for
     bounded work; or
   - `BufferSnapshot::from(&buf)` (Tier 4) — a structural-share
     clone of the rope. Detaches from the original entirely; the
     UI is free to edit while the task runs. Becomes available
     when Tier 3b lands the rope-backed `Buffer`.
3. **Results post back as commands.** Background work that produces
   data the UI must react to (search hits, alignment results, primer
   scores) sends `AppCommand::TaskResult { buffer_id, payload }` on
   the existing `pending_commands` channel. The applier routes it to
   the right view(s) and emits the corresponding `AppEvent`.
4. **Cancellation by token.** Each task receives a
   `CancellationToken` (Zed's pattern). The applier signals it when
   the task's premise changes (buffer edited, view closed). Tasks
   poll the token between work units and abort cleanly.

### Why these rules

- No write contention on the UI thread → no jank.
- Edits and background-task results funnel through the same applier
  → consistent event emission, no order-of-arrival bugs.
- Buffer version is the cache key for derived data; tasks tag their
  result with the buffer version they computed against; the applier
  drops stale results.

### What this looks like in code

(Sketch — none of this exists yet. Lands alongside the first concrete
background user, probably alignment or primer scoring.)

```rust
// In command::apply
pub enum AppCommand {
    // ... existing variants ...

    /// A background task returned a payload. The applier looks up the
    /// buffer version and either applies (current) or drops (stale).
    TaskResult {
        buffer_id: BufferId,
        buffer_version: u64,
        payload: TaskResultPayload,
    },
}

pub enum TaskResultPayload {
    Alignment(AlignmentResult),
    PrimerScores(Vec<PrimerScore>),
    // ...
}

// In some future executor module
pub fn spawn_alignment(
    buf: Arc<RwLock<Buffer>>,
    queue: AppCommandQueue,
    cancel: CancellationToken,
) {
    std::thread::spawn(move || {
        let snapshot = buf.read().expect("not poisoned").snapshot();
        let buffer_id = snapshot.buffer_id;
        let version = snapshot.version;
        let result = compute_alignment(&snapshot, &cancel);
        if cancel.is_cancelled() { return; }
        queue.enqueue(AppCommand::TaskResult {
            buffer_id,
            buffer_version: version,
            payload: TaskResultPayload::Alignment(result),
        });
    });
}
```

### Egui specifics

- `egui::Context::request_repaint()` from the background thread wakes
  the UI to pick up the new command. We already do this from the
  socket listener.
- Long tasks should periodically `cancel.check()` and consider yielding
  with `std::thread::yield_now()` to keep the worker pool responsive.
- For multi-stage work, post intermediate `TaskResult`s with
  partial-result payloads; the UI can render progress live.

## ViewKind dispatch (Stage 2.5d)

The viewer renderer in `tabs.rs::Tab::View(_)` matches on
`view.kind: ViewKind` to pick the per-kind renderer. Today only
`ViewKind::TextView` exists, paired with `SequenceView::show`. Adding
a new kind (`LinearView`, `CircularView`, post-MVP) requires:

1. New variant on `ViewKind` (in `seqforge-core::model`).
2. New entry in `ViewKind::context_tag()` (e.g. `"Pane:LinearView"`).
3. New widget module in `seqforge-app` exposing a `show()` with the
   same signature shape as `SequenceView::show`.
4. New match arm in `tabs.rs::Tab::View(_)`'s render dispatch.

The keymap stack picks up the new kind tag automatically via
`FocusState::rebuild_context`, so kind-specific bindings (e.g.
`"r"` for "rotate origin" only in `Pane:CircularView`) work without
keymap-table refactors.

## Restriction backend boundary

Restriction-enzyme data and scanning live in the `seqforge-restriction`
crate, not in `seqforge-bio`. The dependency direction is:

```
seqforge-app / seqforge-cli
        │  (call find_cut_sites / resolve_query)
        ▼
seqforge-bio          ← thin bridge; owns the CutSite/SearchHit shape
        │  (delegates scanning + presets)
        ▼
seqforge-restriction  ← REBASE table, scanner, presets. No deps on the rest
                        of the workspace; designed to extract to crates.io.
```

Contract rules:

- `seqforge-bio` is the **only** crate that depends on
  `seqforge-restriction`. `seqforge-core`, `-app`, and `-cli` never name it
  directly — they speak `core::CutSite`. This keeps the future crates.io
  extraction (RESTRICTION_PLAN.md Tier 4) a one-file change.
- The bridge is `search::site_to_cutsite` (`restriction::Site → core::CutSite`)
  and the grammar mapper in `enzyme_query.rs` (`EnzymePreset → restriction::Preset`).
  `CutSite` is deliberately a lossy projection of `Site` — it currently drops
  `strand` and `enzyme_type`; add them to `CutSite` when a UI feature needs them.
- The enzyme table is `&'static`, generated by `src/bin/codegen.rs` from
  `data/rebase_bairoch.txt`. The generated `enzymes_generated.rs` is committed
  and reviewable; regular builds never run codegen.
- The crate carries **no** non-std dependencies and `publish = false`. Keep it
  that way until Tier 4 — it is the constraint that makes extraction cheap.

## Cache pattern (Stage 2.5e)

`crate::cache::Cache<K, V>` is the canonical pattern for derived data.
Examples in `viewer.rs`: feature stacking keyed by `(BufferId, u64)`,
cut-label stacking keyed by `(Vec<usize>, u32)`. New caches in
Tier 3+ work (alignment overlays, primer scores, mutation tracks)
should use this helper, not roll bespoke invalidation predicates.

Key rules:
- Include the **buffer version** in any cache that depends on sequence
  content. Edits bump the version (Tier 3d); your cache invalidates
  automatically.
- Include any **layout-derived inputs** (line width, char width,
  zoom level) if they affect the computed value. Quantize floats
  before keying so floating-point noise doesn't thrash.
- For caches whose computation cost exceeds the per-paint budget,
  consider moving to a background task and storing the result back
  via `AppCommand::TaskResult`.
