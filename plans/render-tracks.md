# SeqForge Render-Track Plan — sequence-viewer rendering abstraction

> **Status: design of record; migration not started.** Canonical cross-track
> status lives in [`../ROADMAP.md`](../ROADMAP.md). This plan owns the design +
> phase checkboxes for turning `viewer.rs`'s monolithic render into a block-aware
> **Track** abstraction. It changes **rendering/interaction only** — the domain
> model (sequence-centric `Buffer` + id-addressed `Annotations` + derived-on-demand
> translation/cut-sites/ORFs) is untouched (decisions 8, 12, 13).

## Context

`viewer.rs::show()` is a ~1170-line method with **two hand-synchronised passes** over
block-wrapped sub-lanes:

1. a **hit-collection** pass building five parallel vecs — `annot_hits`,
   `cut_site_rects`, `orf_hits`, `aa_hits`, `search_hit_rects`;
2. a **paint** pass that re-derives the same per-block geometry independently.

Plus `build_block_layouts`, which runs **every frame** for **all** blocks and is
**O(blocks × features)** (it rescans every feature per block). Three pressures make one
abstraction the right fix:

- **CDS-translation ownership.** Global reading-frame translation is *position-owned*
  (belongs hugging the sequence); a CDS's protein is *feature-owned* (belongs with the
  feature — the SnapGene/Benchling idiom). Rendering both as pooled "lanes in a band"
  conflates the two and reads ambiguously on feature-dense plasmids.
- **Hit/paint divergence.** Two passes independently computing the same geometry must
  stay pixel-identical by hand — a standing bug class.
- **More tracks coming.** The roadmap adds primers + Tm/GC; threaded through the
  monolith each is expensive, as an `impl Track` each is small.

## What this is — and is NOT

**Rendering + interaction abstraction only.** No data-model change: `Buffer` owns bytes;
features stay position-ranged annotations addressed by `FeatureId`; translation,
cut-sites, and ORFs stay derived-on-demand and never stored. `apply_splice`, history,
and save are untouched. This **strengthens** decision 13 — translation becomes a
literal, toggleable render track rather than special-cased inline drawing.

The architecture remains **sequence/position-centric**. Tracks are **mostly
position-owned**; exactly one (**Features**) is composite/feature-owned, drawing each
bar plus its CDS translation sub-lane. "Feature-centric" describes that one track's
internal composition, not the app.

## The model

- **Block-aware, not full-height.** SeqForge wraps the sequence into blocks
  (text-editor style); a track is a **per-block sub-lane** that stacks vertically within
  each block — unlike IGV/JBrowse full-width bands.
- **`Track` trait:**
  - `block_height(&self, ctx: &BlockCtx) -> f32`
  - `paint(&self, ctx: &BlockCtx, geom: &BlockGeom, painter: &Painter)`
  - `hit_test(&self, ctx: &BlockCtx, geom: &BlockGeom, pos: Pos2) -> Option<Hit>`
- **`BlockCtx`** — read-only per-block inputs: `block_start/end`, seq slice,
  `&Annotations`, `&[CutSite]`, `&[SearchHit]`, `TranslationDisplay`, `&Theme`,
  `staging`, `&TranslationCache` (memoized).
- **`BlockGeom`** — per-block geometry: full block `Rect`, `seq_x0`,
  `char_width`/`char_height`, this track's assigned `y0`. The **full** block rect lets
  connector tracks (cut-site staples) paint across neighbouring bands.
- **`Hit` enum** — one payload vocabulary: `Feature(FeatureId)` · `CutSite(usize)` ·
  `SearchHit(usize)` · `Codon(Range<usize>)` · `Orf(OrfPromote)` · `SeqPos(usize)`.
  One resolver maps `Hit` (+ shift/double-click state) to `SetSelection` /
  `SelectFeature` / context actions in the existing priority order
  (feature → search → cut → codon → seqpos).
- **Stacked tracks vs decorations.** Selection, cursor, search-hit wash, and the staged
  preview diff are **column-aligned decorations owned by the Sequence track** (they
  paint within the strand rows, not a band of their own). Cut-site staples are
  connectors the CutSites track paints across bands.
- **`TrackStack`** — owns the ordered `Vec<Box<dyn Track>>` and runs **one** virtualized
  block loop: per visible block, sum `block_height`s, then dispatch `paint` / `hit_test`
  at each track's computed `y0`.

**Track order (top→bottom):** CutLabels · Ruler · Sequence (strands + decorations) ·
Translation (global frame lanes) · Features (bars + per-CDS AA sub-row).

## Phases

Each phase independently shippable; `build`/`test`/`clippy`/`fmt` green before the next.

- [x] **T0 — Docs (this doc + ROADMAP row + architecture note).** *(Done, `8e7db77`.)*
- [x] **T1 — Geometry + `Hit` unification (no trait).** *(Done, `294755d`.)* Collapsed the
  five parallel hit vecs (`annot`/`search`/`cut`/`orf`/`aa`) in `viewer.rs::show()` into a
  single `Vec<(Rect, Hit)>` + a `find_hit(hits, pos, extract)` resolver queried by variant
  in priority order (feature → search → cut → codon → seqpos). `Hit::Feature` still carries
  the within-frame positional index (→ `FeatureId` at click time; a later phase may carry
  the id directly). Behaviour identical; test `find_hit_resolves_by_variant_then_order`.
- [ ] **T2 — `Track` trait + `TrackStack`.** Split `viewer.rs` → `viewer/` submodules.
  Migrate position-owned tracks: Ruler, CutSites (labels + staples), Translation (frame
  lanes; reuse `build_translation_cache` + the codon-aligned painter). Sequence +
  Features stay legacy.
- [ ] **T3 — Features track (composite) + C2.** Greedy-stack bars (reuse `greedy_stack`)
  and paint a per-CDS AA sub-row directly under each bar (variable row heights owned in
  the track). Remove `feature_lanes` from the band; frames stay in Translation. Reuse
  `cds_glyphs`. **Lands the deferred editor 14e C2.**
- [ ] **T4 — Sequence track + decorations + retire monolith + perf.** Strands (reuse
  `build_strand_galley`) + selection/cursor/search-wash/preview-diff as Sequence-track
  paint; delete the dual passes + monolithic `build_block_layouts`; **memoize/virtualize
  per-block layout** on a fingerprint (`version`, `line_width`, `display`, cut-site set)
  to kill the per-frame O(blocks×features) rescan. Keep `preview`/`translation_cache`
  memoization intact.
- [ ] **T5 — (optional) minimap.** Evaluate reuse of track geometry; likely stays
  separate (different projection). Note-only unless trivial.

## Verification

- Every phase green on build/test/clippy/fmt; existing viewer unit tests (translation
  cache, preview, `stage_input`, `move_focus`, `staged_summary`) stay green.
- New tests — the anti-divergence guarantees:
  - each track's `hit_test` returns the right `Hit` inside its painted extent, `None`
    outside;
  - **co-location invariant:** a track's painted rect equals the rect its `hit_test`
    matches (one geometry);
  - Features track places a CDS's AA glyphs in the sub-row under its bar;
  - `TrackStack` block height == Σ track `block_height`; visible-only dispatch.
- Manual GUI walk: frame toggles under the sequence; CDS protein under its own bar
  (multi-feature disambiguation); click/shift-click/double-click/context menu resolve as
  before; staples/search/selection/cursor/preview unaffected.
- Perf: measure per-frame layout on a large fixture (`examples/tg-oss/.../pj5_00002.gb`
  or a ~50 kb construct) before T1 and after T4.

## Out of scope

- No domain-model changes (decisions 8/12/13 hold).
- Primers/thermo tracks are built **after** this lands, native to the trait.
- The `show()` → `viewer/` submodule split happens incidentally in T2–T4.
