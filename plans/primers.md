# Primers + Sequence Thermodynamics — Plan & Tracker

> **Status: NOT STARTED.** Architecture agreed; begins after current cleanup.
> First concrete step is Phase 0.1 (`seqforge-thermo` crate + `seqforge tm`).
> Canonical cross-track status: [`../ROADMAP.md`](../ROADMAP.md).

## Goal

Display, ingest, evaluate, and (later) design primers, backed by a shared,
sequence-agnostic thermodynamics layer. Every operation has a `seqforge` CLI
equivalent with text output, so agents and scripts get parity with the GUI.

## Ecosystem findings (why we build, not adopt)

Studied before proposing (2026):

- **No Rust library** covers molecular-cloning formats + primers. `noodles` is
  NGS/genomics only (BAM/VCF/GFF/FASTA…); `rust-bio` has no primer concept.
- **PlasCAD** (`plascad`, by `na_seq`'s author) is the only Rust project in our
  domain — but it's a **binary GUI app (egui), MIT**, not a reusable library.
  Useful as **port/reference source**, not a dependency.
- **No maintained Rust Primer3 binding / primer-design crate.** Primer3 is C
  (GPL) with Python bindings. Reserved only as an optional escape hatch for
  full primer *selection* (Phase 3), never a core dependency.
- **Tm reference of record:** SantaLucia 1998 (NN params) + a salt correction
  (SantaLucia & Hicks 2004 / Owczarzy). Validate against **Biopython
  `Bio.SeqUtils.MeltingTemp`** (permissive, authoritative — generate test
  vectors from it). Port code from **PlasCAD (Rust, MIT)** / **tg-oss (JS,
  MIT)**. **Never copy Primer3 source** (GPL — verify before reuse).

## Architecture

```
seqforge-thermo (NEW, pure, zero-dep)        seqforge-restriction (exists, zero-dep)
  NN energetics → tm, gc, hairpin,             enzyme table, scan, presets
  dimer, end_stability (sequence-agnostic)            │
        └────────────────┬───────────────────────────┘
                         ▼
                  seqforge-bio  (exists)
                    + `primer` module: anneal, evaluate, generate;
                      primer_bind round-trip
                         │  (BioOps trait)
                         ▼
                  seqforge-core (exists)
                    + Primer type, Annotations.primers, dispatch
                   ┌─────┴─────┐
            seqforge-app    seqforge-cli
            overlay/track    primer commands
```

### Invariants (the anti-conflict rules)

1. **One thermodynamics implementation.** All Tm/structure math lives in
   `seqforge-thermo`; primer evaluation and future sequence-design both consume
   it. No second Tm anywhere.
2. **Primers are persistent annotations in `core`** (like `Feature`), so their
   *mutation* rides the editor's single applier + history (Phase 2) — never a
   parallel mutation path.
3. **Tm/GC are derived, never stored.** `core::Primer` carries no Tm field, so
   `core` needs no `thermo` dependency.
4. **No duplicate enzyme data.** Restriction-site tails reuse
   `seqforge-restriction` recognition sequences.
5. **CLI/GUI parity via one dispatch.** Pure ops (`tm`) are doc-free like
   `info`; doc ops mirror the `Enzymes` request shape.
6. **Reuse `RevealRange`** for jump-to-binding (already built for enzymes).

### Data model (core)

```rust
pub struct Primer {
    pub name: String,
    pub sequence: String,        // full oligo 5'→3', tail included
    pub binding: Range<usize>,   // annealing region on the template
    pub strand: Strand,
    pub qualifiers: BTreeMap<String, String>, // preserve extra GenBank notes
}
pub struct Annotations { pub features: Vec<Feature>, pub primers: Vec<Primer> }
```

### Lossless story

- **Within our files:** lossless — `primers` ↔ GenBank `primer_bind` + `/note`
  for the full oligo/tail (tg-oss convention).
- **Cross-tool:** binding site preserved; tails are best-effort in `/note`
  (a universal GenBank limitation). Full fidelity needs `.dna` (separate, later).

## Roadmap / tracker

### Phase 0 — Foundation (pre-editor, zero mutation)
- [ ] 0.1 `seqforge-thermo` crate: `tm(seq, params)` (SantaLucia 1998 + salt),
      `gc(seq)`. Zero deps. Tests validated against Biopython vectors.
- [ ] 0.1 `seqforge tm <oligo>` CLI (pure, no doc) — first shippable slice.
- [ ] 0.2 `core`: `Primer` type + `Annotations.primers` (serde, empty default).
- [ ] 0.3 `bio`: GenBank `primer_bind` ↔ `Primer` round-trip (lossless via notes).
      Recognise `primer_bind` (currently collapses to `FeatureKind::Other`).
- [ ] 0.4 `app`: render primers as a distinct directional arrow track (read-only).
- [ ] 0.4 `seqforge info` reports primer count.

### Phase 1 — Read-side interaction (no buffer mutation)
- [ ] 1.1 `bio` annealing: find an oligo's binding sites (3′-exact + mismatch).
- [ ] 1.2 `seqforge-thermo`: `hairpin`, `dimer`, `end_stability` (simple
      complementarity scan — not Primer3 `thal`).
- [ ] 1.3 Primer overlay/panel: list (name/binding/Tm/strand + QC), jump-to-
      binding (reuse `RevealRange`), toggle visibility.
- [ ] 1.4 CLI: `seqforge primers list`, `seqforge primers find <oligo>` (transient).

### Phase 2 — Creation / editing (requires the editor)
- [ ] 2.1 `primers add` / `primers remove` / design-from-selection — through the
      editor's applier + history (single mutation path).
- [ ] 2.2 Constructive generation (`bio::primer`): random oligos w/ sane
      defaults, barcodes (min Hamming distance), restriction-site tails (reuse
      `seqforge-restriction` recognition data).
- [ ] 2.3 CLI: `seqforge oligo random …`, `seqforge primers add …`, etc.

### Phase 3 — Cloning convergence (Tier 3 territory)
- [ ] 3.1 PCR product simulation; primer-pair / amplicon logic.
- [ ] 3.2 Converge primer generation with `seqforge-restriction` Tier 3
      (ligation / Gibson / Golden Gate) into one cloning layer.
- [ ] 3.3 (Optional) Primer3 escape hatch for full primer *selection*
      (feature-gated FFI or subprocess) — only if heuristics prove insufficient.

## Out of scope (for now)

- SnapGene `.dna` parsing (separate; richest primer source — port from
  PlasCAD/tg-oss when `.dna` lands).
- Primer3-grade `thal` secondary-structure DP (simple heuristics suffice).
- Codon optimization / synthesis design (a future `thermo` consumer; the
  shared layer is built to support it without rework).

## Open questions

- New crate name: `seqforge-thermo` (narrow, extractable) vs broadening to
  `seqforge-analysis` later if it accretes ORF/translation/codon work. Start
  narrow.
- Which salt correction to default to (SantaLucia 2004 vs Owczarzy). Pick one,
  match NEB/common-tool expectations, expose as a setting later.
- Editor-era: confirm `Primer` mutation slots onto the same `AppCommand` +
  history machinery the editor introduces (see [`editor.md`](editor.md)).
