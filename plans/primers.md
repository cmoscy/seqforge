# Primers + Sequence Thermodynamics — Plan & Tracker

> **Status: Phase 1 complete (0.1–1.4 landed); Phase 1.5 (Inspector as unified
> viewer/detail/editor) specced next; Phase 2+ design settled.**
> Architecture, sourcing, and consistency-with-the-implemented-model all worked
> out (see "Decisions locked" and "Consistency with the implemented model"
> below). **Done:** 0.1–0.5 (thermo + `seqforge tm`; live Tm/%GC readout;
> `Primer` model + shift handler; `primer_bind` round-trip; `PrimerTrack`
> arrows). **1.1** — `decompose_primer` (annealed/mismatch/tail, strand-correct)
> + base-level render; the seed-and-extend find pass
> (`find_primer_binding_sites` → `PrimerBinding`), Confirmed/Drifted/Detached
> classification (`classify_attachment`), version-keyed cache, drifted/off-target
> badges. **1.2** — `hairpin_dg`/`self_dimer_dg`/`duplex_tm` (thermo) +
> `primer_qc`/`anneal_tm` (bio). **1.3a** — shared `ListPrimers →
> core::PrimerInfo`/`PrimerState` projection via `BioOps::primer_infos` (bio
> assembles; mirrors `find_cut_sites`). **1.3b** — the right-docked **Inspector**
> pane + Primers tab. **1.3c** — the `InspectorCollection` trait + shared table;
> three tabs (Primers · Cut sites · Features); single-click select/reveal,
> double-click → edit modal (Features → `OpenFeatureForm`), map↔panel selection
> sync, and the Primers header map-toggles (show/hide, arrows-vs-bases). **1.4** —
> CLI `primers list` / `primers find` (same `primer_infos` projection). **Phase 1
> complete; next: Phase 1.5** (Inspector as unified viewer/detail/inline-editor —
> inline edit + enzyme query→pane + primer oligo-copy; ROADMAP decision 15), **then
> Phase 2** (creation/editing — inline primer form + `AddPrimer` / `UpdatePrimer` /
> `RemovePrimer`). Canonical cross-track status:
> [`../ROADMAP.md`](../ROADMAP.md).

## Goal

Display, ingest, evaluate, and (later) design primers, backed by a shared,
sequence-agnostic thermodynamics layer. Every operation has a `seqforge` CLI
equivalent with text output, so agents and scripts get parity with the GUI.

Scope for the current milestone (v0.2 rounding-out): primers become **first-class
objects** — imported, displayed as directional arrows, QC-evaluated (Tm/GC/self-
structure), and manually attached/edited through the editor's single mutation
path. Deferred: generative *design*, PCR/primer-pair logic, and cloning
convergence (Phase 2.2 / Phase 3 and "Out of scope").

## Ecosystem findings (why we build on seqfold, not primer3)

Studied + verified (2026):

- **No Rust primer/thermo library on crates.io.** `noodles` is NGS-only;
  `rust-bio` has no primer concept. We still build.
- **`seqfold` (Lattice Automation) is now a native-Rust MIT engine.** Its Rust
  core (`src/core/{tm,fold,data,energies}.rs`) implements NN Tm (SantaLucia +
  Owczarzy-2008 salt), GC, and Zuker MFE folding (`fold`/`dg`) with
  stack/bulge/internal-loop/hairpin energetics, for **DNA and RNA**. `tm(seq1,
  seq2, pcr)` handles a **two-sequence duplex with internal mismatches** — i.e.
  ungapped primer:template annealing. PyO3 is an optional off-by-default feature;
  deps are `rayon` + `smallvec`. **This is our engine** (vendored, see below).
- **primer3 / `ntthal` is GPL-2 and heavy — dropped as a dependency.** Its only
  capabilities beyond seqfold (gapped heteroduplex alignment, hetero-dimer,
  primer *selection*) are exactly the things we defer. Retained **only as an
  optional offline validation oracle** for the future gapped-bulge routine —
  never linked, never in CI, never copied.
- **What the apps do:** SnapGene does **not** check hairpins/dimers/specificity
  (annotate-a-selection + Tm only). Benchling **does** — hairpin/dimer detection,
  secondary-structure view, ΔG. Our QC lands *ahead of SnapGene, at Benchling
  parity*, while pair/PCR design stays deferred.
- **Tm validation:** seqfold's own `tm_test`/`fold_test` vectors + Biopython
  `Bio.SeqUtils.MeltingTemp` (permissive). primer3-py as an extra offline oracle
  for the deferred bulge work only.

## Architecture

```
seqforge-thermo (NEW)                          seqforge-restriction (exists, zero-dep)
  VENDORED seqfold core (MIT, attributed):       enzyme table, scan, presets
  tm · gc · fold/dg (self-structure ΔG) ·               │
  ungapped heteroduplex tm(seq1,seq2).                  │
  Deps stripped (rayon→serial, smallvec→Vec,            │
  pyo3 dropped) → pure, zero-dep, extractable.          │
        └────────────────┬───────────────────────────────┘
                         ▼
                  seqforge-bio  (exists)
                    + `primer` module: anneal (seed-and-extend, own result type),
                      evaluate (Tm/GC/QC), decompose (annealed/tail/mismatch),
                      staleness pass; primer_bind round-trip
                         │  (BioOps trait)
                         ▼
                  seqforge-core (exists)
                    + Primer type + PrimerId, Annotations.primers (id-API),
                      primer-specific binding-shift handler (never drops)
                   ┌─────┴─────┐
            seqforge-app    seqforge-cli
            PrimerTrack +    primer commands (list/find/add/update/remove, tm);
            staged dialog    apply_add_primer/… in command/edit.rs (write-ops)
```

### Invariants (the anti-conflict rules)

1. **One thermodynamics implementation.** All Tm/structure math is the vendored
   seqfold core in `seqforge-thermo`. No second Tm anywhere.
2. **Primers are persistent, authored annotations in `core`** (like `Feature`),
   so their *mutation* rides the editor's single applier + history — never a
   parallel mutation path. Write-ops are hand-routed in `command/edit.rs`
   (`apply_add_primer`/`_update`/`_remove`), exactly like the feature ops
   (`command/mod.rs:652`), per decision 11. `AddPrimer` is content-given → needs
   **no `bio`** (no `core→bio` edge), same as `apply_add_feature`.
3. **Authored object vs. derived interpretation.** Decision 8 ("pure function of
   `text` → derived, never stored") governs *template projections* (complement,
   Tm-of-a-range, translation, overhangs). It does **not** range over authored
   annotations. A primer is an independent oligo (a reagent) with a *relation* to
   the template; its sequence + attachment are authored, its interpretation is
   derived (see Data model).
4. **No duplicate enzyme data.** Restriction-site tails reuse
   `seqforge-restriction` recognition sequences (Phase 2.2, deferred).
5. **CLI/GUI parity via one dispatch.** Pure ops (`tm`) are doc-free like `info`;
   doc ops mirror the feature request shapes (`AddFeature` → `AddPrimer`, etc.).
6. **Reuse the right rails — mechanism, not lossy types:**
   - jump-to-binding reuses the **reveal mechanism** (`View.scroll_to`), not a
     nonexistent "RevealRange" type;
   - annealing gets its **own result type** (`PrimerBinding { range, strand,
     mismatches, three_prime_match }`) — do **not** overload `core::SearchHit`
     (`document.rs:10`; it carries only `{start,end,strand}` and would lose
     mismatch/anchor data → a second track);
   - hit-testing reuses the **one `Hit` enum + one resolver** (`track.rs:35`) by
     adding `Hit::Primer(PrimerId)` — carrying the **id directly** (see decision
     on ids below), not a positional index;
   - binding position reuses the splice offset math but through a **primer-
     specific handler** (never the verbatim `shift_features`, which *drops*
     collapsed ranges — see #1 below).
7. **seqfold vendored + attributed; primer3 oracle-only.** The `seqforge-thermo`
   crate carries no non-std deps and `publish = false` until extraction — same
   constraint as `seqforge-restriction`.
8. **Primers are within-buffer**, addressed by a stable `PrimerId`. Not an
   app-wide shared library (deliberate divergence from SnapGene — see Deferred).

## Data model (core)

A primer is an **authored object attached relationally** to the template. Split
by what is authored (persisted) vs. derived (never stored, version-cached):

```rust
/// Session-scoped stable handle; addressed by id at rest (see "ids" below).
/// Never persisted (#[serde(skip)]); re-minted on load.
pub struct PrimerId(pub u64);

pub struct Primer {
    #[serde(skip)]
    pub id: PrimerId,
    pub name: String,
    /// Full oligo 5'→3', tail included. AUTHORED — the intrinsic identity of the
    /// reagent; may contain bases that appear nowhere in the template (5' tail).
    /// A reverse primer's bases are the revcomp of the top strand at `binding`.
    pub sequence: String,
    /// Last-known annealing footprint, AUTHORED relational state (like a
    /// Feature.range) — but the load-bearing anchor is the **3' terminus** (where
    /// priming/extension begins), NOT the range length. Rides a primer-specific
    /// shift handler that tracks edits and **never drops** the primer (see #1).
    /// `None` = a detached/floating oligo (no current attachment). Matches
    /// GenBank primer_bind location when present.
    pub binding: Option<Range<usize>>,
    pub strand: Strand,
    /// Preserve extra GenBank notes (flag-qualifiers as None value).
    pub qualifiers: BTreeMap<String, Option<String>>,
}
```

`Annotations` gains `primers: Vec<Primer>` behind an **id-only API** exactly like
features (`add`/`get`/`get_mut`/`remove`/`rename`/ordered `iter`), plus a separate
`next_primer_id` counter (`PrimerId` is a distinct newtype from `FeatureId`). Ids
re-minted on load; GenBank/FASTA stay positional.

### Ids: id-at-rest (decision 12), carry the id in the hit

Decision 12's rule is **"addressed by id at rest; a positional `Vec` index is a
private within-frame render detail, never stored/serialized/returned."** That
dictates `PrimerId` for `View.selected_primer`, undo, the dialog, and the CLI
`--id` flag. For the transient render hit, primers are greenfield, so we **carry
`PrimerId` directly** in `Hit::Primer` — cleaner than the legacy `Hit::Feature`
positional index (`track.rs:36`, whose own comment flags "carry the id directly"
as the intended direction). No `by_position` accessor is needed for primers.

### Derived (never stored; `Cache` keyed on `buffer.version` + the stringency setting)

Computed by aligning the authored `sequence` against the current template,
**anchored on the 3' terminus** (do **not** trust `binding.len()` — the shift
handler can grow/shrink the stored range independently of the fixed oligo length):

- **Decomposition** → `Vec<Segment { Annealed | Tail | Mismatch }>`: align the
  fixed-length oligo at the 3' anchor; leading bases with no template pairing =
  5' tail; disagreements within the annealed span = mismatches. Two ranges exist
  post-edit — *stored/expected* (shift-tracked) vs *derived/actual*; rendering
  picks by state (below). Ungapped for v0.2; the same `Vec<Segment>` interface
  accepts a gapped aligner later (Deferred).
- **QC:** Tm (annealing region), %GC, self-hairpin ΔG, self-dimer ΔG (seqfold).
- **Attachment state** (primary): re-anneal (seed-and-extend) and classify:

  | State | Meaning | Marking |
  |---|---|---|
  | `Confirmed` | derived footprint == stored `binding` | normal arrow |
  | `Drifted` | still anchored + anneals within tolerance, but moved / has mismatches | amber "moved"/mismatch marks |
  | `Detached` | 3' anchor destroyed **or** annealing below the stringency threshold → `binding = None` | panel-only, no arrow (floating oligo) |

- **Additional binding sites** (an *orthogonal* flag, not a state — a `Confirmed`
  primer can also have off-targets): seed on the 3'-terminal k-mer (exact, O(N)
  candidate find), score the few candidates. Change-scoped: an edit can only
  create a new site near the splice → rescan the edited window; re-verify known
  sites in place. Runs on version change and/or an on-demand "check specificity";
  never a heavy always-on full fuzzy scan.

**Tolerances are settings with defaults** (match SnapGene/Benchling): binding
stringency (min 3' match / max mismatch — also gates `Detached`) and Tm params
(Na⁺/Mg²⁺/oligo conc, default = seqfold Owczarzy-2008). Defaulted now, exposed
later.

## Thermo engine — vendoring seqfold

- **Copy (vendor), not submodule.** seqfold's Rust core is `cdylib+rlib`, not on
  crates.io, and pulls pyo3/rayon/smallvec — a git dep would drag that in and
  break the zero-dep/extractable invariant. **Source: `github.com/Lattice-
  Automation/seqfold` @ v0.10.1 (MIT).** Copy `src/core/{tm,fold,data,
  energies}.rs` into `seqforge-thermo`, **strip** pyo3 (drop the feature),
  **rayon** (serial DP — instant at primer/short-window sizes), **smallvec**
  (→`Vec`). Retain seqfold's `LICENSE` + copyright (all MIT requires).
- **Covers, out of the box:** `tm(oligo)` and `gc` (0.1); `dg(oligo)`/`fold`
  self-hairpin & self-dimer ΔG (1.2); `tm(seq1, seq2)` ungapped primer:template
  annealing with mismatches (1.1 — no new heteroduplex code for the in-scope case).
- **Orientation footgun:** `tm(seq1, seq2)` hybridises the two strands
  **antiparallel** — feed the primer (5'→3') and the template strand it binds in
  the correct sense, or the Tm is wrong. One helper owns both this and
  extension-direction = arrow-direction (decision on 5'→3' below).
- **Our thin API:** `tm`, `gc`, `hairpin_dg`, `self_dimer_dg`,
  `anneal_tm(primer, template_region)`. Feature-stable so a later gapped/
  heterodimer impl is a drop-in.
- **Validation:** seqfold vectors + Biopython (primary); primer3-py/`ntthal`
  offline oracle for deferred bulge work only.

## Rendering (PrimerTrack — native to the `Track` trait)

A position-owned track (sibling of CutSites), forward arrows above / reverse
below the strand rows. Aligned to the SnapGene/Benchling idiom:

- **Annealed bases:** on-grid, column-aligned to the footprint; solid half-arrow
  with the **arrowhead at the 3' end** (extension direction).
- **5' tail / overhang:** no template column → **lifts slightly off the grid**
  (small vertical rise + a kink where it peels off), same hue, lighter/hatched so
  the eye reads "not on the template." Long tail → **collapse to a stub + length
  badge**, full tail on hover / in the panel.
- **Mismatch columns:** marked within the annealed region (warning-accent cell) —
  the visual counterpart of `Drifted`.
- **State:** `Confirmed` normal; `Drifted` amber badge + mismatch marks;
  `Detached` **not drawn on the sequence** (no binding) — listed in the panel as a
  floating oligo. Additional-sites → off-target count badge.
- **Track trait:** `block_height` reserves the arrow row (+ a sliver for a
  floating tail); `paint` draws annealed body + tail ribbon + mismatch marks;
  `hit_test` returns `Hit::Primer(id)` across the annealed footprint **and** the
  tail ribbon (co-location invariant: paint rect == hit rect). Theme-driven.
- **Future internal bulge** (deferred) reuses the identical lift-off vocabulary,
  anchored internally — paint layer needs no rethink.

## Panels / Inspector — surfacing (Phase 1.3, track decision 10 below)

**Surface grammar (holds for every UI element in this app — refined by ROADMAP
decision 15 / Phase 1.5):**
- **Transient bars = one-shot view-mutation verbs.** Find, GoTo. Keyboard-invoked,
  one input → mutate view state, dismiss; the *result* lives on the map, nothing to
  return to. Zero permanent layout cost (existing `overlay::show_inline_bar`).
- **The Inspector pane = persistent, inspectable, *editable* collections.** Files,
  Terminal, and the **Inspector** (Primers · Cut sites · Features). The pane is a
  **viewer + detail + inline-editor** (decision 15): browse the list, select for
  detail, edit in place. A populate/filter **query lives in the pane header** where
  a collection needs one (Enzymes). Opt-in / toggleable.
- **Modals (`egui::Window`) = blocking decisions only.** Save / revert / overwrite
  confirmations. **Not** entity editing (that is inline-in-pane) and never a list
  you cross-reference.

The deciding test (decision 15): does the operation manage **persistent state the
user returns to and refines** (→ pane) or a **transient one-shot mutation they
dismiss** (→ bar)? Enzymes *fail* the transient test — `active_enzymes` is a set
the user builds, toggles, and adds to/removes from over a session — so the enzyme
**query re-homes into the Cut-sites tab header** (⌘E re-targets to focus it) and the
standalone enzyme **overlay is retired** (Phase 1.5b). Find/GoTo genuinely are
one-shot → they stay bars.

**The Inspector = one dockable, tabbable pane with sub-tabs: Primers · Cut sites ·
Features.** Each sub-tab is a noun-collection with the same *click-row → reveal on
map* behaviour. Build **Primers first** (this track); **Cut sites** is a cheap
follow-on (it reads the existing `view.cut_sites` — no new backend); **Features**
later.

**Default layout** (extend `rebuild_default_dock`): Files **left**, sequence
view(s) **center**, Inspector **right** (`split_right`, new `layout.inspector_fraction`),
Terminal **bottom**. Role-zoned like an IDE; matches Benchling's right inspector.
egui_dock still lets the operator re-tab/float it (e.g. tab it with Terminal).

### Keeping focus / layout / persistence solid (the "solid pane" checklist)

The Inspector is a **singleton non-view pane — mirror `Tab::FileBrowser` /
`Tab::Terminal` exactly**; do not invent a new mechanism. Invariants to preserve:

1. **Singleton reading the *active view*** (holds **no `ViewId`**, like the status
   bar) → sidesteps the orphan-id bug class (`docs/architecture.md` Workspace/
   Layout/Persistence boundary). No active view → an empty state.
2. **Commands-only.** Every click (reveal, toggle) pushes an `AppCommand` onto
   `pending_commands` — like the browser's file-click — never mutates state
   directly. Preserves the single-applier contract (`command::apply`).
3. **Focus stack.** Add one `FocusScope::Inspector` + a `"Pane:Inspector"`
   `context_tag`; **grab no keys initially** (mouse-driven) so keymap resolution
   is unperturbed. Row-nav keybindings, if ever wanted, land later *additively*
   under that tag (`docs/focus-refactor.md`).
4. **Layout back-compat.** New `Tab` variant must serialize; a persisted
   `LayoutSnapshot` from before the Inspector must still load (fall back to the
   default split). This is the one real regression risk → an explicit round-trip
   test, plus: default-dock-builds-with-Inspector and focus-scope-resolves tests
   (mirror the Browser/Terminal coverage).

### Primers tab — content (aligns with SnapGene expandable rows / Benchling panel)

Essentials as **columns** (scannable); everything else in an **expand / on-select
detail** (keeps it clean — do not inline everything):

| Column | Notes |
|---|---|
| Name | |
| Strand | fwd/rev arrow glyph |
| Binding | `start–end` (1-based) + len; *Unattached* for detached |
| Len | oligo bp |
| Tm | °C, right-aligned (the 0.5 `selection_qc` computation) |
| %GC | right-aligned |
| State | Confirmed / **Drifted** (amber dot) / Detached (grey) — from decomposition |

- **Detail (expand / on-select), not columns:** full oligo 5'→3' with the tail
  marked, mismatch count/positions, and (with 1.2) hairpin / self-dimer ΔG + a
  warning icon.
- **Sort by binding position by default** (list mirrors the map top→bottom).
- **Floating oligos in a separate "Unattached" section** at the bottom (Benchling
  idiom) — QC but no map location.
- **Interactions:**
  - **single click / select** row → `scroll_to` + select footprint (attached);
    panel-only for floating (sets `View.selected_primer`).
  - **double-click / Enter on a selected row → open the edit modal** (the
    *launcher* model, see below). Never inline editing.
  - **Header toggles:** show/hide primers on map, and **arrows-vs-bases**
    (Benchling "Primer bases" — toggles the 1.1 base render).
    `Check specificity` / `Add primer` come later (1.1 find / 2.1).
- **Clean-look rules:** ≤ ~6 columns; compact cues over text (strand arrow, amber
  Drifted dot, grey/italic Detached, warning icon) instead of extra columns;
  right-align numerics.

### Editing model — inline-in-pane (decision 15), one commit path

The Inspector edits **in place**, not via a center modal (this *supersedes* the
earlier launcher→modal design; ROADMAP decision 15). The grammar's load-bearing
invariant — *one `ViewerRequest` = one CLI verb = single applier + history* — is
preserved **exactly**; only the *authoring surface* moves from a floating window
into the pane. Historical note: 1.3c shipped the launcher→`OpenFeatureForm` modal;
Phase 1.5a replaces it with the inline editor below.

- The **row is a viewer by default**: selecting it expands to a read-only field
  view (evolves the existing on-select detail).
- **Edit-on-initiation:** the first edit gesture on a field enters a capture mode —
  the pane pushes a `Pane:Inspector:Editing` focus-capture context-tag (**Enter =
  commit, Esc = cancel/revert**) and holds a small transient **draft**. Until then
  the pane grabs no keys (the `docs/focus-refactor.md` "later, additively" hook).
- **Commit emits exactly one `ViewerRequest`** (`UpdateFeature`, `UpdatePrimer`, …)
  — the same request the CLI verb posts — through the single applier + history. No
  parallel mutation path, no GUI/agent drift. The draft is transient (keyed to the
  selected id in the active view, discarded on any selection/view change), so the
  orphan-id protection (pane holds no `ViewId`) is untouched.

Consequence: **cut-sites are read-only** (derived; managed via the pane's enzyme
query, not row edits) → they opt out of `edit_fields`. Editability stays **opt-in
per collection**. Because the *edit mechanism* (draft + focus tag) is shared while
the *field schema* is per-noun, Features (Phase 1.5a) and Primers (Phase 2.1) reuse
one implementation.

### `InspectorCollection` trait — templatize the *mechanism*, not the schema (the Track analog)

The sub-tabs share one **generic renderer** driven by a per-noun descriptor — the
`Track`-trait move (`plans/render-tracks.md`): one mechanism, many implementations.
Decision 15 extends this from display to **editing + query**, but the split is
deliberate: templatize the **interaction mechanism** (list + selection + the inline
draft/edit-mode + the header query), **not** the field schemas (features have
qualifiers, primers tail+QC — those diverge, so each noun supplies its own field
descriptors and commit verb). Capabilities are **opt-in** — a read-only noun
returns `None` and stays non-editable.

```rust
trait InspectorCollection {
    fn rows(&self) -> Vec<Row>;                              // from PrimerInfo / FeatureInfo / CutSite
    fn on_select(&self, id) -> AppCommand;                   // reveal + select
    // opt-in (decision 15 / Phase 1.5); None = capability absent:
    fn edit_fields(&self, id) -> Option<Vec<Field>>;         // inline detail/editor schema
    fn on_commit(&self, id, draft) -> Option<ViewerRequest>; // one CLI verb
    fn query(&self) -> Option<QueryHeader>;                  // populate/filter (Enzymes)
}
```

Primers seeded the trait (1.3c, display-only); Phase 1.5 adds the edit + query
capabilities (Features first as the proof, enzymes gain the query header), and
Phase 2.1's primer form rides them. Adding a fifth noun later = one descriptor.
Backed by the `List*` projections so the table can't drift from the CLI.

### One projection under it (agent/GUI parity)

Back the pane with a **`ListPrimers` dispatch → `PrimerInfo { id, name, binding,
strand, len, tm, gc, state, mismatches }`** — the *same* shape the Phase 1.4 CLI
`primers list` returns, so the pane and the agent can't drift (mirrors the existing
`ListFeatures → FeatureInfo`). The Cut-sites tab is likewise a view over the data
`Enzymes` dispatch already returns (`ViewerResponse::CutSites`).

## Editing UX (staged dialog — sibling of the feature Edit dialog)

- **Same rails:** `AddPrimer` / `UpdatePrimer` / `RemovePrimer` `ViewerRequest`s,
  **staged** (arm → preview → commit on `Enter`, ROADMAP decision 10), through the
  single applier + history. Siblings of the feature ops — no new mechanism.
- **Detach-on-destroy uses the existing staging, no new modal.** A staged edit
  that would destroy a primer's 3' anchor surfaces **"detaches primer X"** in the
  realized preview; the existing **commit-on-Enter is the confirmation** (ROADMAP
  decision 10). CLI/agent edits have no preview loop → they detach and **report it in
  structured output** ("primer X detached"). Neither path silently corrupts; the
  primer object always survives (binding → `None`), never deleted.
- **Dialog field set** (differs from a feature's): name, **full oligo sequence**,
  **5' tail** (visually distinguished / auto-derived from binding), binding range
  (pre-filled from the current selection, editable), strand, and a **live
  Tm/%GC/self-structure QC panel** (shares the Phase 0.5 computation).
- **Create-from-selection is the primary path:** select region → "Add Primer" →
  dialog pre-filled (binding = selection, oligo = `template[selection]`, **name =
  the auto-generated default** — see decision 9, editable before commit).
- **Naming is never a blocker** (decision 9): the name field is pre-filled with a
  unique `Primer N` default from one shared `suggest_primer_name()`; the CLI
  `--name` is optional and falls back to the same generator. Both call the single
  `core` helper, so GUI/CLI/import share one naming story.
- **Deferred "optimize/design"** button (auto-extend to a target Tm) is a disabled
  affordance pointing at Phase 2.2/3.

## Lossless story (GenBank round-trip)

- **Binding** ↔ GenBank `primer_bind` location (native, authoritative; reverse
  strand = `complement(x..y)`). A `Detached` primer (`binding = None`) has no
  `primer_bind` record — it round-trips via the note alone (an unattached oligo).
- **Full oligo + tail** ↔ a single JSON-valued `/seqforge_primer` qualifier note,
  **mirroring the existing `/seqforge_provenance` pattern**. Schema: full oligo
  5'→3' (+ tail boundary once bulges land). On load, `primer_bind` → `binding`,
  the note → `sequence`; a stale/non-annealing import still round-trips (binding
  preserved, state derived).
- **Diversion is a behavior change** (see Consistency §): `primer_bind` currently
  parses to a `Feature` (`genbank.rs:45`). It now routes to `Primer`; the writer
  must emit it from `primers` **only** (no double-emit from `features`).
- **Within our files:** lossless. **Cross-tool:** binding preserved; tail
  best-effort in `/note`. Full fidelity needs `.dna` (separate, later).

## Consistency with the implemented model (fixes the audit found)

Each item cites the code it must stay consistent with.

1. **Primer binding shift must never `retain_mut`-drop.** `shift_features`
   (`mutations.rs:83`) drops ranges destroyed by an edit (`:105`, `:125`) — right
   for features, **wrong for a reagent**. A primer-specific handler shares the
   offset math but, on 3'-anchor loss, sets `binding = None` (`Detached`) and
   **keeps** the primer. Clamp/compare against the edit point like the straddle
   case (`:111-119`).
2. **Decomposition anchors on the 3' terminus, never `binding.len()`** — the shift
   handler can grow/shrink the stored range (`:114`) independently of the fixed
   oligo. Stored-vs-derived footprint reconciled by state.
3. **`Hit::Primer(PrimerId)`** in the one `Hit` enum (`track.rs:35`), id carried
   directly (id-at-rest, decision 12); no `by_position` for primers.
4. **Own `PrimerBinding` result type**, not `core::SearchHit` (`document.rs:10`);
   reuse only the `View.scroll_to` reveal mechanism.
5. **`apply_add_primer`/`_update`/`_remove` in `command/edit.rs`**, routed like
   `apply_add_feature` (`command/mod.rs:652`), through applier + history
   (decision 11). Content-given → no `bio`.
6. **`primer_bind` diversion:** parser routes it to `primers`; writer emits from
   `primers` only. **Undo** snapshots whole `Annotations` (derives `Clone`, so
   `primers` ride along free); extend the byte-budget *estimate* at
   `history.rs:78` to count primers (benign if missed — estimate only).
7. **`View.selected_primer: Option<PrimerId>`** mirroring `selected_feature`
   (`model.rs:294`); clear it in `clear_selection`.
8. **`tm(seq1, seq2)` antiparallel** orientation owned by one helper (see Thermo).

## Roadmap / tracker

### Phase 0 — Foundation (read-side, minimal mutation)
- [x] 0.1 `seqforge-thermo`: **vendor seqfold core** (deps stripped, MIT
      attribution); expose `tm`, `gc`. Validated vs seqfold + Biopython vectors.
      *(seqfold v0.10.1; `pyo3` dropped, `rayon`→serial, `smallvec`→`Vec`; pure,
      zero-dep, `publish = false`. `bio` re-exports the thin `tm`/`gc` surface.)*
- [x] 0.1 `seqforge tm <oligo>` CLI (pure, no doc) — first shippable slice.
- [x] 0.2 `core`: `Primer` + `PrimerId` + `Annotations.primers` id-API (serde,
      empty default); **primer-specific binding-shift handler** (never drops;
      `Detached` on anchor loss); `View.selected_primer`.
      *(`mutations::shift_primers` detaches on 3'-anchor loss — `binding.end` for
      Forward, `binding.start` for Reverse — else clamps like the feature
      straddle; history byte-budget counts primers; `GoTo`/`clear_selection`
      clear `selected_primer`.)*
- [x] 0.3 `bio`: GenBank `primer_bind` ↔ `Primer` round-trip (lossless via
      `/seqforge_primer` note); route `primer_bind` → `primers` (parser + writer).
      *(parser diverts `primer_bind`→`Primer` — full oligo/tail from the note, or
      best-effort from the template on foreign import; writer emits from `primers`
      only + a `/label` fallback for authored names; `Document.primers` +
      `Annotations::from_parts`; `seqforge info` now reports the primer count.)*
- [x] 0.4 `app`: `PrimerTrack` — directional arrow track (annealed on-grid, tail
      lift-off, `Hit::Primer(id)`, read-only). `seqforge info` reports primer count.
      *(two faithful bands straddling the sequence — forward above / reverse below,
      arrowhead at 3', 5' tail peels off-grid; `stack_primers` per-strand stacking
      into `BlockLayout`; click selects the footprint (lights the 0.5 readout);
      co-location invariant asserted. Mismatch marks / drifted state → 1.1.)*
- [x] 0.5 **Live selection Tm/%GC/length status readout** (no primer object) —
      ships/validates thermo early; shared by the dialog QC panel. *(status bar:
      `Tm … °C · … % GC`; NN Tm capped to oligo lengths ≤ 120 bp via the pure
      `selection_qc` helper. Pulled forward ahead of 0.2/0.3.)*

### Phase 1 — Read-side interaction (no buffer mutation)
- [x] 1.1 `bio` annealing: seed-and-extend binding-site find (own `PrimerBinding`
      type, reuse `scroll_to`). Decomposition (3'-anchored) + attachment-state pass.
      *(**Decomposition + render done**: `decompose_primer` → per-column
      annealed bases / mismatches / 5' tail, strand-correct (the orientation
      footgun is unit-tested); the PrimerTrack now draws the oligo's bases with
      amber mismatch cells + lifted tail letters. Layout reordered so the codon
      band hugs the sequence (translation innermost, then reverse primers, then
      features). **Find + state done**: `find_primer_binding_sites` seeds on the
      3'-terminal k-mer (fwd/rev, circular wrap), scores via `decompose_primer`;
      `classify_attachment` → Confirmed/Drifted/Detached + off-target sites;
      version-keyed `PrimerAnnealCache` in the viewer; track shows drifted
      "moved" badge + off-target `×N` count. Feeds 1.3's panel + 1.4's
      `primers find`.)*
- [x] 1.2 `thermo`: self-hairpin ΔG, self-dimer ΔG (seqfold `fold`/`dg`);
      `anneal_tm` + `primer_qc` in `bio` (orientation-safe heteroduplex Tm).
- [x] 1.3a **`ListPrimers → PrimerInfo` projection** (`core` type + dispatch +
      `BioOps::primer_infos`; bio assembles via `classify_attachment` +
      `primer_qc_with_anneal`, mapping `AttachmentState → PrimerState`). Shared
      with 1.4; unit-tested headless.
- [x] 1.3b **Inspector pane machinery** — right-docked singleton
      (`Tab::Inspector`/`FocusScope::Inspector`, `split_right` @
      `layout.inspector_fraction`), version-keyed `InspectorState` cache, Primers
      tab as a **read-only** list, `View → Inspector` show/hide toggle. Layout
      back-compat + default-dock + focus-scope tested.
- [x] 1.3c **Interactive tabs + generalization** — `InspectorCollection`
      trait + shared table renderer; three tabs (Primers, Cut sites read-only,
      Features); single-click → select/reveal (`RevealPrimer`/`RevealFeature`/
      `RevealRange`), **double-click → edit modal** (`OpenFeatureForm` wired;
      primers await 2.1's modal), on-select detail; map↔panel selection sync
      (`SelectPrimer`); and the header **map-toggles** (`PrimerDisplay { show,
      bases }` on `SequenceView` → `SetPrimerDisplay`; show/hide primers on map +
      arrows-vs-bases). Enter-key row activation deferred as additive (pane grabs
      no keys). Full spec: **"Panels / Inspector"** (decision 10).
- [x] 1.4 CLI: `seqforge primers list` (→ `PrimerInfo`, shared with 1.3 via one
      `seqforge_bio::primer_infos`) + `seqforge primers find <oligo>`
      (`find_primer_binding_sites`). Load → project → JSON; ids minted via
      `Annotations::from_parts` (decision 9). Parity verified against pUC19.

### Phase 1.5 — Inspector as unified viewer/detail/editor (pre-2.1, ROADMAP decision 15)

Graduates the Inspector from read-only noun-lists into the **viewer + detail +
inline-editor** surface of decision 15 (the Figma/Xcode/DevTools/Benchling
Inspector-panel idiom). Lands the shared edit-mode + query-header mechanisms that
Phase 2.1's primer form then rides — so that form is inline from day one, never a
modal. Order within the block is flexible; **1.5c is independent** and can land
first. Find/GoTo stay bars (transient one-shot verbs — decision 15).

- [x] 1.5a **Shared inline edit-mode (Features)** — the selected Features row
      expands to a read-only viewer; an edit gesture (Edit button / double-click)
      drops into an inline field editor backed by a pane-local `FeatureDraft`
      (`inspector.rs`). Commit posts one `UpdateFeature` `ViewerRequest` (= the CLI
      verb, `FeatureDraft::to_request`, unit-tested) through the single applier +
      history; Enter/Save commits, Esc/Cancel reverts; the buffer never mutates
      until commit. New `Pane:Inspector:Editing` context-tag suppresses single-key
      user bindings while typing (keymap `ws_ok` gate), else the pane grabs no
      keys. Draft reconciled each frame (dropped if its feature vanishes). **The
      Inspector no longer opens the center `FeatureForm` modal to edit.** *Scope
      note:* the modal is **retained for create-from-menu** ("New Feature"); the
      inline **create** path + generalizing the draft to a trait method land with
      the 2nd editable noun (primers, 2.1) — extract-on-second-use, not before.
      Tests: commit-mapping + draft seed/validity + `Pane:Inspector:Editing`
      resolution; full app suite (102) + workspace clippy green.
- [x] 1.5b **Enzyme query-in-pane; retire the overlay** — the **Cut sites** tab now
      carries the enzyme query **header** (input + Show/＋Add/Clear, Enter=Show) over
      the grouped enzyme→sites list with per-enzyme ✕ remove + jump + expand
      (`inspector.rs::show_cutsites`, reusing `enzyme_rows` + the existing
      `SubmitEnzymes`/`AddEnzymes`/`RemoveEnzyme`/`RevealRange` commands — no new
      backend). **⌘E re-targets** via `apply_open_enzymes` → `dock_inspector_if_absent`
      (factored from `apply_toggle_inspector`) + `InspectorState::reveal_enzyme_query`
      (Cut-sites tab + one-shot focus) + `FocusScope::Inspector`. **Deleted
      `Overlay::EnzymeBar`, `render_enzyme_bar`, `enzyme_bar_mut`/`has_enzyme_bar`,
      `TAG_ENZYME_BAR`**; `show_inline_bar` slimmed to Find/GoTo (param dropped).
      Find/GoTo bars unaffected. Tests: `reveal_enzyme_query` + full suite (103) +
      workspace clippy green.
- [x] 1.5c **Primer object-copy correctness** — `apply_copy` is now object-aware:
      when the copy range equals a **selected primer's** binding footprint, it
      copies the authored `Primer.sequence` (full oligo 5'→3', tail incl.) instead
      of the template slice (wrong strand for reverse primers; can't represent a 5'
      tail). The `range == binding` gate leaves explicit off-footprint range copies
      (CLI/agent) as literal slices, so parity holds; centralised in `apply_copy`
      so both canvas ⌘C and the menu Copy benefit. Tests: oligo-vs-slice +
      different-range parity + no-selection control (105 total green).
- [x] 1.5d **Editing-surface unification + delete (decision 15 completion).**
      Icon vocabulary via **egui-phosphor** (bundled font tofu'd ✕/＋/arrows/dots) —
      remove/close, strand arrows; primer state dots painter-drawn. **Rows carry no
      delete** (enzyme ✕ = reversible view-filter remove; features/primers are
      authored data). **Feature deletion lives in the editor** with an inline
      **two-step confirm** (`Delete → Confirm delete?`, modal-free; `RemoveFeature`,
      undoable). **Canvas feature gestures route into the Inspector inline editor**
      (`EditFeatureInInspector`), retiring the center `FeatureForm` **for editing**
      (create-from-selection still modal, folds inline at 2.1). **Delete/Backspace
      on a selected feature** → same confirm, via the object-vs-range invariant
      (`apply_set_selection` clears `selected_feature`). Default tab order
      **Features · Enzymes · Primers**. Tests: invariant + edit-routing + copy
      semantics (108 total, clippy + fmt green; toolchain pinned 1.95.0).

### Phase 2 — Creation / editing (uses the editor)
- [ ] 2.1 `AddPrimer`/`UpdatePrimer`/`RemovePrimer` via applier + history; **inline
      primer form** in the Inspector (rides 1.5a's edit-mode — *not* a modal);
      create-from-selection;
      **optional name → `suggest_primer_name()` default** (decision 9);
      detach-on-destroy surfaced in the staged preview / reported by CLI.
- [ ] 2.2 (Deferred within v0.2) Constructive generation: random oligos, barcodes
      (min Hamming), restriction-site tails (reuse `seqforge-restriction`).
- [ ] 2.3 CLI: `seqforge primers add/update/remove …` (`--name` optional, shares
      `suggest_primer_name()` — decision 9), `seqforge oligo random …`.

### Phase 3 — Cloning convergence (Tier 3 territory)
- [ ] 3.1 PCR product simulation; primer-pair / amplicon logic; **hetero-dimer**
      QC (needs the pair/reaction context introduced here — seqfold concat-fold).
- [ ] 3.2 **Gapped heteroduplex** decomposition (indel-mutagenesis bulge) — custom
      NN-param DP over seqfold's tables, behind the same `Vec<Segment>` interface;
      internal-bulge render. primer3 offline oracle here.
- [ ] 3.3 Converge with `seqforge-restriction` Tier 3 into one cloning layer.
- [ ] 3.4 (Optional) Primer3 escape hatch for full primer *selection*.

## Out of scope / deferred directions

- **App-wide primer library** (SnapGene-style shared DB matched across files).
  v0.2 is within-buffer (GenBank-native); a cross-file library is later; `.dna`
  import would feed it.
- **Hetero-dimer** until the pair/PCR context exists (Phase 3.1).
- **Gapped/bulge heteroduplex** rendering + energetics (Phase 3.2).
- **SnapGene `.dna` parsing** (separate; richest primer source).
- **Codon optimization / synthesis design** (a future `thermo` consumer).

## Decisions locked (this track)

1. **Thermo engine = vendored seqfold core** (MIT, deps stripped, attributed);
   primer3 dropped as a dependency, optional offline oracle only.
2. **Primer = authored object attached relationally.** Authoritative: `name`,
   full `sequence` (tail incl.), `binding: Option<Range>` (3'-anchored last-known
   footprint), `strand`, `qualifiers`. Derived (version-cached): decomposition,
   Tm/GC/QC, attachment state, additional sites. Decision 8 governs template
   projections, not authored annotations.
3. **Addressed by `PrimerId` at rest** (decision 12); `Hit::Primer` carries the id
   directly (no positional index, no `by_position`). Within-buffer, not an
   app-wide library.
4. **Edits never delete a primer.** Anchor-destroying / below-threshold edits set
   `binding = None` (`Detached`); GUI surfaces this in the staged preview (commit-
   on-Enter = confirm; no new modal), CLI reports it. Never `retain_mut`-drop.
5. **Ungapped heteroduplex is in scope, covered by seqfold** (`tm(seq1,seq2)`);
   gapped/bulge + hetero-dimer + selection are deferred (Phase 3).
6. **Oligos stored 5'→3'** (universal convention); one helper owns
   extension-direction = arrow-direction **and** `tm(seq1,seq2)` orientation.
7. **Tolerances (binding stringency, Tm salt/conc) are defaulted settings**
   (Owczarzy-2008), modifiable later.
8. **Reuse mechanism, not lossy types:** one `Hit` enum, own `PrimerBinding` type
   (not `SearchHit`), primer-specific shift handler (not `shift_features`),
   `command/edit.rs` routing (like features).
9. **Primer naming: optional-with-default, one shared generator.** `Primer.name`
   is a required non-empty `String`, but *creation never requires the user to
   supply one*: a single `core` helper `Annotations::suggest_primer_name()`
   yields a unique `Primer N` (lowest N not colliding with existing primer
   names). The GUI dialog pre-fills that default (editable before commit); the
   CLI `--name` is optional and falls back to it; the GenBank import fallback
   (currently the literal `"primer"`) is superseded by the same generator so
   nameless imports become unique too. `rename_primer` (0.2) covers relabeling.
   Lands with creation (Phase 2.1/2.3); the import-fallback swap can ride along
   or land earlier. Rationale: matches SnapGene/Benchling (auto-name + rename),
   keeps create-from-selection one-click, and guarantees no two primers share a
   synthetic name. Never reject creation for a missing name.
10. **UI surface grammar: verbs → bars, noun-collections → the Inspector pane,
    blocking decisions → modals.** The primer/cut-site/feature lists live as
    sub-tabs of **one right-docked, tabbable Inspector pane** (build Primers
    first; Cut sites reuses `view.cut_sites`; Features later) — a singleton that
    follows the active view, holds no `ViewId`, and mutates only via
    `pending_commands` (mirrors `Tab::FileBrowser`/`Tab::Terminal`, preserving the
    focus/single-applier/orphan-id invariants). Enzymes keep their query **bar**
    (verb) *and* gain a Cut-sites Inspector tab (noun); the bar feeds the pane.
    Panes are backed by a `List*` projection shared with the CLI (`PrimerInfo` ↔
    `primers list`) so GUI and agent can't drift. Full shape: "Panels / Inspector"
    above. Rationale: matches SnapGene tabs / Benchling inspector; keeps the
    sequence map central; reuses proven pane machinery instead of a new surface.
    **Refinement (implementation): horizontal sub-tabs** (lean on egui_dock, no
    icon rail). One **`InspectorCollection` trait** templatizes the table
    (display + selection + activation dispatch) across the three nouns — the
    `Track` analog — but **not** the edit forms. **Editing = launcher, not inline:**
    a row's double-click/Enter opens the noun's **existing modal**
    (`OpenFeatureForm`/`OpenPrimerForm`), whose Submit is one `ViewerRequest` =
    the CLI verb. This *preserves* the forms→modals grammar (does not bend it),
    keeps the pane keyless beyond an activation gesture, and lowers LoC (no
    inline-edit state machine). Read-only nouns (cut-sites) have no modal —
    editability is opt-in per collection. **Editing model + enzyme placement here
    are superseded by item 11 (ROADMAP decision 15)**; the pane/singleton/`List*`
    invariants stand.
11. **Inspector = unified viewer/detail/inline-editor** (ROADMAP decision 15;
    refines item 10). The pane graduates from read-only lists to the
    Figma/Xcode/DevTools/Benchling **Inspector-panel** idiom: browse → select-detail
    → **edit inline in the pane**, *not* launcher→center-modal. Editing = a transient
    pane draft + a `Pane:Inspector:Editing` focus-capture tag (Enter = commit → the
    one `ViewerRequest`/CLI verb, Esc = cancel); the pane grabs no keys until an
    edit or query begins. Persistent, inspectable, **editable** collections
    (Enzymes · Primers · Features) are pane tabs; only transient one-shot
    view-mutations (Find · GoTo) stay bars. The **enzyme overlay is retired** — its
    query re-homes to the Cut-sites tab header, ⌘E re-targets to focus it. The shared
    move is to templatize the edit **mechanism** (draft + focus tag + query header)
    across nouns — *not* the field schemas (features have qualifiers, primers
    tail+QC), each noun opting in (read-only nouns keep none). Preserves the single
    commit path + orphan-id protection; only *conditionally* relaxes "pane grabs no
    keys" (an additive focus tag the focus-refactor already anticipated). Rationale:
    the dominant professional convention; removes the enzyme-bar special case; net
    LoC cut. Lands as **Phase 1.5**, before Phase 2.1 (so the primer form is inline
    from day one). Full shape: "Panels / Inspector" + "Editing model" above.

## Resolved (previously open) questions

- Crate name: **`seqforge-thermo`** (start narrow).
- Salt correction default: **Owczarzy-2008** (inherited from seqfold).
- `Primer` mutation on the applier + history: **confirmed** — editor is complete;
  Add/Update/RemovePrimer are `ViewerRequest`s routed like the feature ops.
