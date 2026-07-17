# Buffer lifecycle & topology ops

> Mini-phase between the feature-model/transport foundation and Primers Phase 3.
> Purpose: make the `extract`/`place`/`merge` + `Span` transport foundation
> **drivable** end-to-end (copy → New → paste), and add the molecule-topology
> verbs a cloning workflow needs. Best-practice modeled on SnapGene/Benchling.

## The verbs (each = one contract)

Command-surface rule (settled): **an arg extends a verb; a new verb marks a
contract change.** So RC is *extended* (not duplicated) and topology in-place
cases fold into optional args (no separate `SetTopology` toggle).

| verb | contract |
|------|----------|
| `New { circular, name? }` | create an empty in-memory scratch buffer + view + dock tab |
| `SetOrigin { index }` | rotate a **circular** molecule so `index` → 0; topology-invariant; a feature crossing the new origin becomes one wrapping `Span` |
| `Linearize { at? }` | **circular → linear**, cut at `at` (default 0); a feature straddling the cut is truncated + fuzzy-marked |
| `Circularize { origin? }` | **linear → circular**, join ends (optional rotate) |
| `ReverseComplement { start, end }` | **extended**: whole-molecule range (`0..len`) now also mirrors features (coords + strand); sub-range stays byte-only (feature-windowed inversion = follow-up) |

## Where the logic lives (reuse over new code)

- **`core::topology`** (new) — `rotate_origin(text, ann, n)` (byte rotate +
  wrap-aware feature/primer re-home mod L) and `reverse_complement_circular(ann,
  l)` (whole-molecule annotation mirror + strand flip; `rem_euclid`, correct for
  wrapping features). Pure geometry, no `bio` dep.
- **Linearize reuses `transport::extract`** — extracting the whole circle rooted
  at `at` with `TruncatePartials` *is* a linearization; no bespoke split code.
- **`core::history`** gains `topology_before` + `stamp_topology` so Linearize /
  Circularize are undoable *including* the topology flag.
- **`Workspace::replace_whole`** (new, `workspace.rs`) — whole text + annotations
  (+ optional topology) as one undo unit; the shared engine behind Set-Origin /
  Linearize / Circularize. RC's whole-molecule annotation mirror rides the
  existing byte-RC edit's undo unit.
- **`Workspace::new_buffer`** — `BufferStore::new_scratch` (promoted from the old
  `#[cfg(test)] insert_raw`) + `add_view`.
- Appliers: `file::apply_new` (mirrors `apply_open_file`'s tab flow),
  `edit::apply_{set_origin,linearize,circularize}`, extended
  `edit::apply_reverse_complement`. Dispatch in `command/mod.rs`; GUI in the File
  (New) and Edit → Topology menus (`app.rs`).

## Status — ✅ COMPLETE (447 tests green, clippy -D warnings clean)

CLI/agent parity via the dispatch table; GUI menus wired. Verified end-to-end by
integration tests: copy an annotated region → `New{circular}` → paste carries the
feature across buffers; `Circularize → SetOrigin → Linearize → Undo` restores
topology + bytes; plus `core::topology` unit tests (rotate round-trip, seam wrap,
RC self-inverse, wrapping-arc preserved).

## Deferred (noted, not blocking)

- **Sub-range** RC feature mirroring (whole-molecule done; windowed inversion +
  straddle policy is the follow-up).
- Across-origin primer **thermo** (representation done in the Span track; the
  stored-binding decompose still truncates a wrapping binding).
