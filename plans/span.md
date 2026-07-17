# Span — a circular-native geometry abstraction

> Design of record. Foundational architecture track, follow-on to the
> feature-model / transport track (`plans/feature-model.md`, complete). Boundary
> contract: `docs/architecture.md` ("Derived sequence data", decision 8).

## Context — why this track

SeqForge is a **plasmid/cloning tool**, so circular molecules are the primary
case, not an edge case. But its geometry is linear-biased, and every place that
**flattens a circular region to a linear hull** leaks the same class of bug. Two
concrete symptoms observed on pUC19:

- The `ori` is a GenBank `join(2315..2686,1..217)` — one ~589 bp region that
  **crosses the origin**. Its linear hull is `0..2686` (the whole plasmid). The
  main viewer renders/hits segment-native, but the **minimap still flattens to
  the hull** (`minimap.rs` `build_circular_geom`), so it draws the ori as a
  near-full ring — two renderers, two independent geometry paths.
- **Selection can't cross the origin.** `Selection` is a single `start..end`
  (`start ≤ end`), so shift-selecting from near the end, through position 0, to
  the start is unrepresentable — a real correctness/UX gap for a plasmid editor.

A wrapping selection and an origin-spanning feature are the **same shape** — "a
contiguous region on a circular molecule that may cross the origin" — modeled
today by two different linear-biased things, neither of which models the wrap.

**Outcome:** one `Span` type centralizing wrap-awareness once; selection that
crosses the origin; a single geometry-derivation path shared by main viewer +
minimap; a consolidation sweep across feature/primer/enzyme/search/selection.

## The `Span` type (core, new `span.rs`)

```rust
/// Contiguous region on a molecule of length L, possibly wrapping the origin.
/// start ∈ 0..L, len ∈ 0..=L; covers start, start+1, … start+len-1 (mod L).
/// Pure geometric value — does NOT store L (decision 8: derived-not-stored;
/// the owning Feature/Selection/Primer always has the Buffer len in hand).
pub struct Span { pub start: usize, pub len: usize }
```

**Representation = `start + len`.** Rejected: `start..end` with `start > end` =
wraps (the current `transport.rs` `PosMap` convention) — **can't distinguish
empty from full-circle**; `enum Linear|Wrapping` — re-introduces
two-shapes-for-one-concept (the wrap split is a *rendering projection*,
`linear_pieces`, not the identity).

`L` is a method param; topology enforced at construction (a linear-molecule Span
has `start+len ≤ L`). Surface: `from_range`, `between(a,b,L)` (directed, backs
wrapping selection), `contains`, `wraps(L)`, `end(L)`,
**`linear_pieces(L)`** (THE render/highlight/copy primitive — 1 run
non-wrapping, 2 if it crosses the origin; returned as an alloc-free `Pieces`
enum, `smallvec` not being a dependency), `hull(L)` (explicit, lossy-on-wrap,
bounds-only), `shift`, `intersection`, `overlaps`.

`shift_range` (`mutations.rs`) + `PosMap` (`transport.rs`) stay the splice/clamp
*policy* engine — complementary to `Span::shift` (pure translate).

## Reconciliation with the `Location` model

- **`Location::Simple { range → span }`** — a non-wrapping Span == the old range,
  and can now wrap. An origin-spanning contiguous feature becomes **one
  `Simple`, not a `Join`** — retiring the GenBank `join(...)` overload.
- **`Location::Join`** then means only genuinely spliced/multi-segment features.
- **Retire `Location::span()` / `Feature::span()` hull.** ~40 call sites split
  into: "does P fall in it?" → `contains(p, len)`; "linear runs to paint/copy" →
  `pieces(len)`; "one bounding box for stacking/LOD" → `hull(len)` (explicit).
- **Ingestion normalization** (`genbank.rs`): thread `len`+topology in; an
  origin-adjacent `join` (`seg.end==L` then next `seg.start==0`) on a circular
  molecule → wrapping `Simple`. Export inverts for byte round-trip.

## Selection becomes wrap-capable

`Selection { anchor, focus, wrap: bool }` (`#[serde(default)]` on `wrap`).
`wrap=true` = the arc from `anchor` **through the origin** to `focus`.
`to_span(L)` replaces `.ordered()`; highlight/copy consume
`to_span(L).linear_pieces(L)`. Circular `move_focus` wraps mod L instead of
clamping and toggles `wrap` on origin crossing — via a **pure
`move_focus_circular(anchor, focus, wrap, delta, L)`** with truth-table unit
tests before UI wiring. **Drag stays non-wrapping** (never sets `wrap`).

## Single source of truth for geometry

Add **`Feature::pieces(len)`** (via `Location`) in **core** — every linear run a
feature occupies, origin-split. Main viewer `feature_segment_rects`
(`features.rs`) and minimap `build_circular_geom`/`build_linear_geom`
(`minimap.rs`) both iterate it — one `PaintArc`/`PaintBar` per piece; delete the
minimap wrap hack. They **cannot drift** — wrap handling is one function.
Stacking keeps `hull(len)` (honest bounds-only). Minimap also gains the
`FeatureVisibility` filter (closes the source-still-shows divergence).

## Cross-model consolidation sweep

**Adopt Span:** `Selection`; `Location::Simple.span`; `extract` signature
(`Span+len`, deletes the second `start>end` wrap encoding); `Primer.binding:
Option<Span>` (anneal across origin — phase last); `SearchHit`. **Partial:**
`CutSite.recognition` → Span, but keep `cut_pos`/`bottom_cut_pos` as bare `usize`
(points, not regions); `CutSiteKey` stays `usize`. **Do NOT** (over-abstraction):
`Provenance.source_range` (opaque lineage key), cut points, translation frames,
`visible_range` viewport hint.

## Phasing (each ships green: fmt + clippy -D warnings + test --workspace)

- **P0 — `Span` type** + exhaustive unit tests. No consumers.
- **P1 — `Location` holds `Span`; retire `.span()`.** Add `pieces/contains/hull`;
  migrate ~40 sites behind a deprecated `.span()` shim; update
  `shift_location`/`offset`/`mirror`/`combine_locations`. *(largest; prereq.)*
- **P2 — ingestion normalization** (bio only): origin-join → wrapping `Simple` +
  export inverse + fixed-point round-trip test.
- **P3 — wrap-capable `Selection` + shared geometry:** `wrap` bit, `to_span`,
  circular `move_focus`, shift-extend/click; sequence highlight + both minimap
  paths consume `pieces`/`linear_pieces`.
- **P4 — consolidation sweep (deferrable, independent):** `extract`(Span), then
  `Primer.binding` / `SearchHit` / `CutSite.recognition` → `Span`.

P2 and P3 are independent after P1; P4 is cuttable without regressing the three
requirements.

## Blast radius + LoC estimate

~40 real `.span()` calls / 14 files; 104 `anchor/focus/ordered/is_cursor`
accesses / 20 files. **Net ≈ +330 across ~28 files** (~1,670 gross touched);
**+310 / ~24 files** excluding P4. Largest groups: the selection-consumer sweep,
the `.span()` migration, and genbank ingest/export.

**Riskiest:** (1) circular `move_focus`/`wrap` toggle — pure-fn + truth-table
tests; (2) `extract`/`PosMap` dual-encoding during P4 — adapter first, delete old
convention last; (3) genbank round-trip fidelity — fixed-point property test on
pUC19 `ori`; (4) the 40-site migration — compiler-driven, grep-to-zero.

## Decisions (settled at kickoff)

1. **Retire the `Join`-for-origin-wrap overload** (Simple-holds-wrapping-Span) —
   **yes**.
2. **Shift-click arc on a circle:** preserve current sweep direction if a
   selection exists, else non-wrapping.
3. **`pieces`/`contains` on `Location`** (geometry), `Feature` thin delegators —
   so primers (no `Location`) reuse the `Span` methods directly.
4. **`Primer.binding` timing:** P4.

Revisit any of these only if implementation surfaces a concrete reason.

## Verification

- Per-phase: `cargo fmt --all` + `cargo clippy --workspace --all-targets -D
  warnings` + `cargo test --workspace` green.
- `Span` unit tests (empty/full/wrap/contains/linear_pieces/intersection);
  `move_focus_circular` truth-table tests.
- GenBank **fixed-point** round-trip on a circular fixture with an origin feature
  (pUC19 `ori`): load→save→load stable, `join(...)` bytes preserved.
- Manual (GUI): pUC19 `ori` renders as two arms meeting at the origin in **both**
  the main viewer and the minimap; shift-select from near the end through
  position 0 selects the wrapping region.
