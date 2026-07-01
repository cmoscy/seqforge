# SeqForge extensibility — trajectory (NOT committed work)

> **Status: direction only.** Nothing here is a task or a contract. This
> records the *shape* we intend to grow toward so the few near-term choices
> that touch the data model (chiefly a runtime feature handle) are made with it
> in view. Status + sequencing live in [`../ROADMAP.md`](../ROADMAP.md)
> (decision 11 + "Deferred — direction recorded"); stable contracts live in
> [`architecture.md`](architecture.md). When any of this becomes real, it
> graduates into a `plans/*.md` track and this file shrinks to the parts still
> speculative.

Desktop is the primary target. Other deploy targets (VSCode webview, web) are
**viewer + native runtime tools**, not full plugin hosts — that constraint is
what keeps the surface below small.

## Value vocabulary

Plugins and ops interoperate by passing a small set of **serde** value types
through the host — the lingua franca:

- `Sequence { bases, topology }` — a literal value (by-content).
- `Ref { buffer, range?, feature? }` — a handle into live editor state
  (by-handle).
- `Feature { id, kind, range, strand, label }` — needs the runtime `FeatureId`
  below to be a stable handle.
- `Bin { name, items: Vec<Ref | Sequence> }` — a labeled tray of inputs.
- `Product { seq, provenance, name }` — an op output; name rendered from a
  host-level template against provenance.

**Interop rule:** everything is either *by-content* (`Sequence`, portable and
pure — travels to WASM/headless unchanged) or *by-handle* (`Ref`, couples to
live state). A pure op takes `Sequence` in / `Product` out; a state-mutating op
takes a `Ref`. Same protocol, two coupling levels.

## bin → op → product

The one model that generalizes cloning (PCR, digest, ligate, Golden Gate) *and*
feature ops:

```
Bins (inputs)  ──►  Op (inputs + params → products)  ──►  Products
                                                          new Buffers → Tab::View
```

Ops are pure functions living in the engine crates
(`seqforge-restriction` Tier 2/3 for digest/ligate/GG; a PCR module). Products
re-enter the **existing** `Buffer` → `Tab::View` machinery unchanged. The pane
is thin glue: draw bins + params + Run; it does not know what any op is.

## Two plugin tiers — one protocol

| | In-process (Rust) | Out-of-process (Python, etc.) |
|---|---|---|
| Mechanism | `impl Op`, linked in | speaks JSON-RPC over `SEQFORGE_SOCKET` |
| Coupling | direct `Ref` into live buffers | `Sequence` values across the wire |
| Isolation | none (can panic the app) | full (separate process) |
| Polyglot | no | **yes** — terminal is the host |
| Status | needs the registry (below) | socket bus exists; protocol needs data-exchange verbs |

Both tiers speak the same vocabulary, so a Rust op and a Python subprocess are
interchangeable from the pane's view.

### Two accuracy caveats (verified against the code)

1. **The `ViewerRequest` seam is single-source on the wire, split on
   dispatch.** clap + serde reach CLI + JSON-RPC for *any* variant. But
   **read-ops** flow through `seqforge-core::dispatch`, while the editor
   **write-ops** are deliberately excluded from `dispatch` (it `unreachable!()`s
   on them) and hand-routed through `seqforge-app`'s `command/edit.rs` — forced
   by the core⊘bio boundary (decision 9: mutating ops need history + `bio`-derived
   bytes, which `core` must not depend on). So a plugin op picks a path: a pure
   read/query op can ride `dispatch`; a mutating op wires through the app write
   path. Do **not** describe this as "add a variant, get everything for free" —
   that holds only for the wire/CLI surface.
2. **`SEQFORGE_SOCKET` reaches the terminal child via process-global
   `std::env::set_var`** (`terminal.rs`), not explicit child-env injection into
   the PTY `BackendSettings`. It works today, but the mechanism is global and
   (edition 2024) `unsafe`. **Hardening direction:** inject the var explicitly
   into the spawned shell's environment when the plugin bus becomes
   load-bearing. This is the one spot where today's implementation is not the
   better long-term shape.

The protocol is also currently **control-shaped** (act on the active/target
view) — there is no `get_sequence`/`create_buffer` that hands a `Sequence` out
to an external tool and takes one back. Adding those data-exchange verbs is the
additive step that makes the out-of-process tier first-class.

## Registry after two ops — not before

`Tab` and `ViewerRequest` are closed enums today, dispatched by exhaustive
`match`. That is correct for built-ins. The **open registry** (a `Box<dyn Op>`
table + a single `Tab::Workflow` / `ViewerRequest::Tool { … }` escape hatch) is
extracted **only after two real cloning ops exist** — designing the trait before
two implementations is the way to get it wrong. Build one hardcoded cloning pane
end-to-end first; extract the abstraction from the shared structure it reveals.

## Dependency order

1. `seqforge-restriction` Tier 2/3 engine (digest → `Fragment` → ligate → GG).
   Pure, headless, testable. **The value + correctness risk lives here.**
2. One **hardcoded** cloning pane end-to-end (bins → op → products open as
   views). Proves the producer→view flow.
3. Extract the `Op` trait + registry from the two ops that now exist.
4. `cargo generate` plugin template (one `impl Op` + manifest + fixture test).

## Act-now vs deferred

- **Act-now (editor Phase 14): structural `FeatureId`** (ROADMAP decision 12).
  The only choice with a retrofit cost. Features are a bare `Vec<Feature>`
  addressed by `usize` index today (`model.rs`, `document.rs`); the ops that
  consume that index (`apply_add/remove/rename_feature`,
  `ViewerRequest::{Add,Remove,Rename}Feature`) already exist and are tested —
  only the UI is missing. The refinement is **not** id-sprinkled-over-a-`Vec`
  ("boundary discipline") but a **structural** guarantee: the `Annotations`
  feature API becomes id-only (`get`/`get_mut`/`remove`/`rename`/ordered
  `iter`), so no public positional index exists to store or misuse — the
  stale-index bug class is *unrepresentable*, not reviewer-guarded. Index
  survives only as a private within-frame render detail. Ids are session-scoped
  (`#[serde(skip)]`, re-minted on load; persistence stays positional), so undo
  (annotation snapshot) carries them for free. Doing this before the consuming
  UI and before `v0.2.0` freezes the `--id` wire is cheapest now; the `Vec` can
  later become an `IndexMap<FeatureId, Feature>` behind the same API with zero
  outside churn.
- **Deferred (no work now):** the vocabulary types, data-exchange verbs, the
  registry, the cloning pane, the Python tier, and the explicit terminal
  child-env refactor. All additive later over seams that already exist.
