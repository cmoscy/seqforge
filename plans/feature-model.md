# Feature Location Model + Annotation Transport — Plan & Tracker

> Canonical cross-track status: [`../ROADMAP.md`](../ROADMAP.md) (v0.3 milestone,
> decision 23). Foundational for the cloning track: PCR
> ([`primers.md`](primers.md) Phase 3) and the assembly workbench
> ([`assembly.md`](assembly.md)) both **consume** the transport layer defined here.
> Boundary contract: [`../docs/architecture.md`](../docs/architecture.md)
> ("Derived sequence data", decision 8).

> **Status — NOT STARTED (design of record).** This track was pulled *ahead* of PCR
> once we realized copy/paste, PCR, and ligation are the **same** operation —
> "extract an annotated subsequence and re-home its features into a destination
> coordinate frame" — and that building it three times invites three divergent
> partial-feature bugs. It should have ridden in with the original copy/paste work;
> it lands now, mature, before any upstream consumer.

## Why (the problem)

Three observations forced this:

1. **The clipboard is bytes-only.** `state.clipboard: Option<Vec<u8>>`
   (`command/edit.rs`); paste splices raw bytes; even
   `apply_reverse_complement` swaps bytes without re-homing features. Nothing in
   the app transports an annotation across a boundary today.
2. **We already discard GenBank's location richness on import.** `gb_io` parses the
   full location grammar (`Range` · `Complement` · `Before`/`After` fuzzy ·
   `Join`), but `genbank.rs` flattens every location to a single bounding `Range`
   via `f.location.find_bounds()` (`genbank.rs:80`, `:139`). **Multi-segment and
   partial features are silently collapsed today** — this is current data loss, not
   a hypothetical.
3. **Copy/paste, PCR, and ligation are one operation.** Each is
   *extract → place*; ligation adds *merge* (rejoin split pieces) + *ends*. A shared
   layer decides the partial-feature policy **once**.

## The GenBank-native representation

Feature geometry becomes a recursive **`Location`** (core-owned mirror of
`gb_io::seq::Location`; core can't depend on the parser crate):

```rust
enum Location {
    Simple { range: Range<usize>, before: bool, after: bool }, // before/after = < / > (partial/truncated)
    Join(Vec<Location>),                                        // compound = SnapGene "segments"
    Complement(Box<Location>),                                  // strand; a Join may be mixed-strand
}
```

- `Feature.range: Range<usize>` → **`Feature.location: Location`**, with a derived
  **`Location::span() -> Range`** bounding accessor (computes the hull; **never
  stored** — decision 8: the span is a pure function of the location, so
  storing it would denormalize with a sync invariant, the same reason the
  complement strand was dropped from `Buffer`). `.span()` is `gb_io`'s
  `find_bounds()` and Biopython's bounding-extent, done our way.
- **Blast radius is confined to an accessor swap.** The many range-only consumers
  (selection, `shift_features`, most rendering, methylation, primers) call
  `feature.span()` instead of `feature.range`; only genuinely segment-aware code
  (the features-track multi-segment draw, GenBank I/O, extract/place/merge) matches
  on the variants. Segment-awareness is **opt-in**.
- **GenBank round-trip stops flattening.** `genbank.rs` maps
  `gb_io::Location ↔ core::Location` losslessly (`join(...)` ↔ `Join`, `<`/`>` ↔
  `before`/`after`, `complement(...)` ↔ `Complement`) — a strict improvement over
  the current hull-only import.
- **Mixed-strand `Join`** (trans-splicing) is the one rare case we may stub
  (reject or single-strand it) without cornering the model.

## The transport layer (the shared pipeline)

The carrier is an **annotated subsequence** — the blunt, linear *degenerate case*
of the assembly track's `Fragment` (decision 21: `Product = Fragment`; decision 7:
blunt-whole persistence). It carries no `End`s; `Fragment` = carrier + ends
composes them later. It is a **lean `SeqSlice`, not `Document`** — `Document`
(`document.rs:234`) has the same field core but drags `name`/`topology`/
`source_path` (meaningless for a region); a purpose-built slice keeps the surface
honest. `SeqSlice::into_document(name, topology)` promotes it only when a *new*
buffer is materialized (paste-as-new, PCR product).

```rust
struct SeqSlice {
    bytes:    Vec<u8>,
    features: Vec<Feature>,   // local 0-based coords
    primers:  Vec<Primer>,   // carried by the same rule; see "What transfers"
}

// (B) extract — the partial-feature policy lives here, decided ONCE.
Annotations::extract(range, circular, policy: PartialPolicy) -> SeqSlice

// (C) place — shift + flip + re-mint + merge into a destination frame.
Annotations::place(slice, at_offset, orient: Orient, merge: bool) -> Vec<FeatureId>
```

### What transfers — features + primers; derived data recomputed

Only **authored, positionally-bound** annotations ride the carrier; everything
derived is recomputed on the destination (decision 8). The primitive is the
Biopython **slice + concat algebra** (`record[a:b]` + `record1 + record2`) —
robust because it is two total functions, not a pile of special cases.

- **Features** — positional sub-ranges, clean slice semantics (the `PartialPolicy`
  trisection below). `place` reuses **`shift_features`** (`mutations.rs:84`) for the
  coordinate math.
- **Primers** — carried by the **same containment predicate**, keyed on the
  **stored `binding` range** (authored, edit-tracked, GenBank-`primer_bind`-backed),
  **not** the derived `decompose_primer` annealed set (which can drift with template
  state — decision 8). The 5' tail has no template coordinates, so it is never in
  the test — it rides along in the authored `sequence` verbatim. `place` reuses
  **`shift_primers`** (`mutations.rs:140`), which *already* implements
  "shift-or-detach, never drop the reagent." Primers **never merge** (a reagent is
  atomic — the `Join`/segment machinery does not apply), so they are strictly
  simpler than features here.

  | `primer.binding` vs the extracted range | Outcome |
  |---|---|
  | fully inside (`binding ⊆ range`) | **carry** — `sequence`/`strand` verbatim, `binding` shifted, fresh `PrimerId` |
  | straddles the boundary | **detach** (reagent survives, `binding=None`) — or drop, one flag |
  | fully outside / already detached | not carried |

- **Derived / view / reagent-nonpositional data is NOT carried** — cut sites, ORFs,
  translation, QC (recomputed); `ViewSelection`, scroll (view state); `methylation`
  is buffer-level, so the destination keeps its own.

Orientation note: `Orient::Rev` mirrors *both* features (flip strand + mirror
coords) and primer bindings (mirror `binding` + flip strand; `sequence` stays — the
physical oligo is unchanged). Copy/paste + PCR are `Identity`, so the primer-RC
path is exercised first at ligation, alongside the RC feature path.

- **`PartialPolicy`** = `DropPartials` (default; the Biopython/pydna `record[a:b]`
  behavior) | `TruncatePartials` (clamp to the range, mark the cut boundary with a
  fuzzy `before`/`after` = GenBank `<`/`>`; renders as a torn edge). This is the
  "disable for strict logic" toggle, for free.
- **`Orient`** = `Identity` (copy/paste, PCR) | `Rev` (ligation places a fragment
  reverse-complemented → flip strands + mirror coords of its features). Designed in
  from day one even though the first two callers pass `Identity`.
- **Id re-minting is mandatory** (decision 12): placed features get **fresh**
  `FeatureId`s via `Annotations::add` — two buffers can't share id identity, and
  pasting twice must create distinct features. `place` owns this.
- **Merge is source-identity-keyed, not name-keyed.** Two pieces coalesce **iff
  they share a source identity** (`Lineage::same_source` — same `source_doc` +
  `source_range`, `document.rs`; the `op` label is metadata, not part of the key)
  **and** are adjacent/joinable in the product:
  - contiguous + same-lineage → collapse to one `Simple` (seamless);
  - gapped same-lineage (insert between halves, or a circular origin span) → one
    feature with a `Join` location (SnapGene "segments"; pydna's origin-span join);
  - different lineage → stay separate **even if names match** (name-only merge is a
    footgun — two distinct "promoter"/"his-tag" features would false-merge; SnapGene
    only gets away with "add to the largest overlapping feature" because it tracks
    identity through the op). An optional *conservative* name fallback may exist but
    is off by default. `merge = false` disables merging entirely (strict mode).
- **Lineage stamping** falls out: `extract` stamps each carried feature (and the
  slice) with `Lineage{source_doc, source_range, op: LineageOp}` — the existing
  round-trippable field, one segment of the coordinate-lineage map — which is
  SnapGene's history-tree / pydna's lineage, and answers PCR's product-provenance
  uniformly. See `docs/architecture.md` "Lineage".

## Consumers (the DRY payoff)

| Consumer | extract (B) | place (C) |
|---|---|---|
| **Copy → Paste** | `extract(selection)` into a `SeqSlice` clipboard | `place(clip, paste_pos, Identity, merge)` |
| **PCR** (Ph3) | `extract(amplicon)` | `place(amplicon, Δ=tail_f_len, Identity, merge=false)` + novel tail bytes at ends |
| **Ligation / GG** | `extract` per digest fragment | `place(frag, running_offset, orient, merge=true)` + `End`/overhang join |

`B` and `C` are literally the same functions in all three — the modularity payoff.
The clipboard grows an OS-interop wrinkle: `state.clipboard: Option<SeqSlice>`
**plus** a `.bytes()` projection (a biologist still pastes plain letters into an
email), so "clipboard = Fragment" is ~90% true, not literal.

## Phasing

- **F0 — Feature `Location` model.** `range: Range` → `location: Location` +
  derived `.span()`; swap range-only call sites to `.span()`; `genbank.rs` maps
  `gb_io::Location ↔ core::Location` (stop flattening); fuzzy `before`/`after`;
  `Join`; multi-segment + torn-edge render in the features track. **Sub-decision at
  kickoff:** full `Join`+fuzzy now, or fuzzy-only now with `Join` deferred to the
  ligation phase (merge would produce only contiguous `Simple` ranges until then).
- **F1 — Transport layer.** `SeqSlice` + `extract`/`place` (policy + orientation +
  id re-mint + provenance) + provenance-keyed `merge`; **features + primers**
  carried (primers by `binding ⊆ range`, reusing `shift_primers`); **proven through
  copy/paste-with-features-and-primers** (the upgrade that should have ridden in
  originally) — the lowest-risk first caller and the test bed.
- **→ PCR** ([`primers.md`](primers.md) Phase 3) consumes F1.
- **→ Ligation / assembly** ([`assembly.md`](assembly.md)) composes F1 with `End`s +
  `Orient::Rev`.

## Testing

- **Location model:** GenBank round-trip of `join(...)`, `<a..b`, `a..>b`,
  `complement(join(...))` (lossless, no flattening); `.span()` = hull of any
  variant; a range-only consumer still reads correct bounds via `.span()`.
- **extract:** contained feature kept + localized; straddler dropped
  (`DropPartials`) vs truncated + fuzzy-marked (`TruncatePartials`); circular
  range wrapping the origin.
- **place:** offset shift; `Orient::Rev` flips strand + mirrors coords; fresh ids
  minted; paste-into-new-buffer carries features.
- **primer transfer:** `binding ⊆ range` → carried (`sequence`/`strand` verbatim,
  `binding` shifted, fresh `PrimerId`); straddler → detached (reagent survives);
  fully-outside / already-detached → not carried; a 5' tail rides along untouched.
- **merge:** same-lineage contiguous → one `Simple`; same-lineage gapped → `Join`;
  different-lineage same-name → **not** merged; `merge=false` → strict/separate.
- **Property:** copy region → paste elsewhere → features localize+shift correctly;
  extract→place round-trips a whole buffer; `merge` is idempotent.
- **Parity:** GUI copy/paste and the CLI `copy`/`paste` verbs produce identical
  feature layouts.

## Out of scope (this track)

- `End`/overhang typing, digest, the join verbs — assembly track
  ([`assembly.md`](assembly.md)).
- Mixed-strand `Join` (trans-splicing) beyond a stub.
- App-wide feature/part library; `.dna` primary sticky-ended import — deferred.
