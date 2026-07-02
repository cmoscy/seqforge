# SeqForge Render-Track Plan — sequence-viewer rendering abstraction

> **Status: design of record; T0–T3 landed, T4 next.** Canonical cross-track
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
- [x] **T2 — `Track` trait + `TrackStack`.** *(Done.)* Split `viewer.rs` (~2940 lines)
  into `viewer/{mod,track,translation}.rs` + `viewer/tracks/{ruler,cut_sites,translation,
  sequence,features}.rs`. `track.rs` holds the `Track` trait (`block_height` / `paint` /
  `hit_rects`), `BlockCtx` (per-block read-only inputs), `BlockGeom` (per-track y0 + strand
  offsets so connector tracks reach the sequence rows), `Style` (shared sizing/fonts/colours),
  and `TrackStack` — one virtualized block loop that sums each track's `block_height` into
  its `y0`, then dispatches paint (z-order defers CutSites so its hover staple lands on top
  of the strands) / `hit_rects`. Position tracks Ruler, CutSites (labels + staples),
  Translation (whole band, reusing `build_translation_cache` + the codon painter) are
  migrated; Sequence (strands + decorations) and Features (bars) are legacy-core `Track`
  impls delegating to the extracted paint. Interaction (`find_hit` resolver, click/drag/
  context menu) stays in `show()`, unchanged. New tests: co-location invariant
  (`features_hit_rect_equals_painted_bar_rect` — hit rect == `annot_bar_rect` paint uses)
  and `stack_block_height_equals_build_block_layouts` (Σ track heights + gap == layout
  height). Behaviour identical; 85 app tests + clippy + fmt green.
  *Deviation from the sketch:* the Translation track keeps the **whole** band
  (frame + feature lanes) in T2; T3 moves `feature_lanes` out to the Features track.
- [x] **T3 — Features track (composite) + C2.** *(Done.)* The Features track is now
  composite/feature-owned: greedy-stacked bars (via `build_block_layouts`) each with the
  feature's own CDS translation painted directly under its bar. `TranslationCache` swaps
  the packed `feature_lanes` band for per-feature `feature_glyphs` (`FeatureAa { id, glyphs }`,
  reusing `cds_glyphs`); `feature_lanes` are gone from the band — the Translation track now
  paints global frame lanes only (`frame_band_rows`). Feature stack rows have **variable
  height**: `build_block_layouts` grows a row by one AA row when it holds a translated
  feature (`feat_row_offsets` / `feat_band_h`, keyed on the memoized cache). Shared
  `paint_aa_lane` / `aa_codon_hits` helpers keep the codon outline + residue glyph + codon
  hit-rect identical between the two tracks (co-location preserved). Codon selection under a
  feature moves with its sub-row. Behaviour otherwise identical; 82 app tests + clippy + fmt
  green. **Lands editor 14e C2.**
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
