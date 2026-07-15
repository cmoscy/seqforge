# `seqforge-restriction`: REBASE-Backed Enzyme + Cloning Crate

> Canonical cross-track status: [`../ROADMAP.md`](../ROADMAP.md). The crate
> boundary contract is in [`../docs/architecture.md`](../docs/architecture.md)
> ("Restriction backend boundary").

> **Status — Tier 1 COMPLETE.** The crate is a workspace member with the REBASE
> table (`enzymes_generated.rs`), scanner (`find_sites`/`find_all_sites`),
> presets (`Unique`, `UniqueOrDual`, `NonCutters`, `TypeIIs`, `GoldenGate`,
> `MoClo`), and Layer 1 + Layer 2 tests. `seqforge-bio` is migrated off
> `na_seq` (dependency dropped); the GUI/CLI grammar accepts the new preset
> keywords. Full workspace suite (108 tests) green.
>
> **Deviations from the plan as written:** codegen ships as `src/bin/codegen.rs`
> (a bin target in the crate), not a separate `seqforge-restriction-xtask`
> crate. The `Enzyme` struct does **not** carry an `isoschizomers` field yet
> (isoschizomers currently surface as separate co-located labels), and — contrary
> to the "Data Model" sketch below — carries **no methylation field**: the
> `bairoch` snapshot has no per-restriction-enzyme sensitivity data (its `MS`
> lines are all on methyltransferase records), so that data must be sourced
> separately. See [`methylation.md`](methylation.md). `OverhangKind`
> is computed from offsets; overhang *sequences* arrive with Tier 2 `Fragment`.
>
> **Next:** Tier 2 (digest + fragments) — separate plan, not yet started.

## Context

SeqForge's restriction-site path currently leans on `na_seq::load_re_library()`, whose enzyme set is limited to ~200 Type II palindromic 6-cutters. It has no Type IIs enzymes (BsaI, BsmBI, BbsI, SapI), which blocks Golden Gate and most modern cloning workflows. The underlying `RestrictionEnzyme` model also can't represent asymmetric cleavage outside the recognition site, and the finder has known quirks (skips matches in the last few bases of a sequence; strips N bases from input, distorting positions).

The Rust ecosystem has nothing equivalent to Biopython's `Bio.Restriction` — there is no crate that combines a REBASE-derived enzyme database with scanning, digestion, and ligation primitives. This plan establishes that crate inside the SeqForge workspace. It is shipped unpublished initially; once SeqForge has pressure-tested the API through real cloning workflows (Golden Gate, Gibson, digest+ligate), we extract to crates.io.

Source of truth: **REBASE** directly ([rebase.neb.com](http://rebase.neb.com)), bairoch format. No Biopython runtime or test dependency.

## Crate Layout

New workspace member: `crates/seqforge-restriction/`

```
crates/seqforge-restriction/
├── Cargo.toml
├── README.md
├── data/
│   └── rebase_bairoch.txt        # Checked-in REBASE snapshot
├── xtask/
│   └── codegen.rs                # Standalone bin: parses snapshot → enzymes.rs
├── src/
│   ├── lib.rs                    # Public surface
│   ├── enzyme.rs                 # Enzyme / EnzymeType / Overhang types
│   ├── enzymes_generated.rs      # `include!`d const table (codegen output)
│   ├── scan.rs                   # Site finder (single + multi-enzyme)
│   ├── digest.rs                 # Linearize + fragment list
│   ├── ligate.rs                 # Sticky/blunt end compatibility + joins
│   └── preset.rs                 # Named filters (Unique, GoldenGate, etc.)
└── tests/
    ├── rebase_canonical.rs       # Spec-anchored: EcoRI, BsaI, etc.
    ├── digest_invariants.rs      # Property tests
    └── fixtures/                 # Plasmid .gb files for integration tests
```

`Cargo.toml` workspace members list gets the new entry. The crate is **not published** until the API has been used by SeqForge through at least one real cloning workflow.

## Data Pipeline

**Snapshot, not runtime download.** REBASE updates monthly but our needs don't — we refresh the snapshot quarterly or on user demand. Steps:

1. `data/rebase_bairoch.txt` — checked-in REBASE bairoch-format snapshot. ~5000 enzymes raw; we filter to commercial subset (~300) at codegen time. Header preserves attribution as required by REBASE license.

2. `xtask/codegen.rs` — standalone bin invoked manually via `cargo run -p seqforge-restriction-xtask`. Parses the bairoch file, applies filters, emits `src/enzymes_generated.rs` containing a `const ENZYMES: &[Enzyme] = &[...]`. **Not a `build.rs`** — keeps regular builds fast and the generated file deterministic / reviewable in PRs.

3. Filters at codegen time:
   - Commercially available (marked in REBASE as NEB / Thermo / Promega / etc.)
   - Exclude homing endonucleases (rarely used, complex recognition)
   - Exclude prototype-only enzymes
   - Result: ~300 enzymes covering >99% of real-world workflows including all Golden Gate Type IIs

4. The generated file is committed. Contributors don't run codegen unless updating the snapshot. CI runs the codegen check (`cargo run -p ... -- check`) to confirm `data/` and `enzymes_generated.rs` are in sync.

## Data Model

```rust
pub struct Enzyme {
    pub name: &'static str,
    pub recognition: &'static [IupacByte],  // IUPAC-coded recognition seq
    pub top_cut: i16,        // signed offset from 5' end of recognition
    pub bottom_cut: i16,     // signed offset on bottom strand (relative to top)
    pub enzyme_type: EnzymeType,
    pub methylation_sensitive: bool,   // ⚠ NOT SHIPPED — see note below + methylation.md
    pub isoschizomers: &'static [&'static str],
}

pub enum EnzymeType { TypeII, TypeIIs, TypeIII, Other }

pub enum Overhang {
    Blunt,
    FivePrime(&'static [u8]),   // sticky end sequence on the 5' overhang
    ThreePrime(&'static [u8]),
}
```

Key design choices:
- `top_cut` / `bottom_cut` are **signed offsets** so Type IIs cuts outside the recognition site (BsaI: top=+1, bottom=+5 past the 3′ end) and Type II cuts inside the site (EcoRI: top=+1, bottom=+5 from 5′ end of GAATTC) use the same arithmetic.
- `IupacByte` is a `#[repr(u8)]` enum mirroring our existing IUPAC table. Recognition seqs in REBASE like `GGTCTC` or `GACNNNNNGTC` parse straight into `&[IupacByte]`.
- `isoschizomers` lets the GUI show "BsmBI ≡ Esp3I" when relevant — critical for cloning workflows.
- `methylation_sensitive` as sketched here (a bare `bool`) was **never shipped and was the wrong shape** — this Tier-1 sketch wrongly assumed the sensitivity data rode in `bairoch`. It does not (see the deviations note above). The realized design is a per-system `MethylSensitivity { dam, dcm, cpg }` record joined from a *second* REBASE snapshot through the same codegen pipeline; full plan in [`methylation.md`](methylation.md) (ROADMAP decision 18).
- The whole table is `&'static` — zero allocation at startup, all lookups are pointer arithmetic.

## Public API Surface

Kept deliberately concrete. No traits, no extension points, no `Box<dyn Anything>`.

```rust
// Lookup
pub fn enzyme_by_name(name: &str) -> Option<&'static Enzyme>;
pub fn all_enzymes() -> &'static [Enzyme];

// Scanning
pub fn find_sites(seq: &[u8], enzyme: &'static Enzyme, circular: bool) -> Vec<Site>;
pub fn find_all_sites(seq: &[u8], enzymes: &[&'static Enzyme], circular: bool) -> Vec<Site>;

// Site has: enzyme name, strand, recognition_start..end, top_cut, bottom_cut, overhang.

// Digestion
pub fn digest(seq: &[u8], enzymes: &[&'static Enzyme], topology: Topology) -> Vec<Fragment>;
// Fragment: bytes + 5' overhang + 3' overhang + source enzyme(s).

// Ligation
pub fn ends_compatible(a: &Fragment, b: &Fragment) -> bool;
pub fn ligate(fragments: &[Fragment]) -> Vec<LigationProduct>;

// Presets (named filters over scan results)
pub fn resolve_preset(preset: Preset, seq: &[u8], circular: bool) -> PresetResult;
```

`Preset` enum: `Unique`, `UniqueOrDual`, `NonCutters`, `TypeIIs`, `GoldenGate` (BsaI/BsmBI/BbsI/SapI), `MoClo` (subset of GoldenGate). Easy to add more.

## SeqForge Integration

Replace `na_seq` end-to-end. Migration is two steps, both small:

1. **`seqforge-bio` becomes a thin wrapper.** Its `find_cut_sites`, `parse_enzyme_query`, `resolve_query` delegate to `seqforge-restriction`. Public API of `seqforge-bio` stays unchanged — callers in `seqforge-core` and `seqforge-app` don't notice. Drop the `na_seq` dependency.

2. **New presets surface in the GUI query grammar.** `parse_enzyme_query` accepts `type IIs`, `golden gate`, `moclo` as keywords mapping to the new presets. CLI gets them for free (same grammar). Overlay's hint text updates to mention the new keywords.

After step 2, the existing tests in `seqforge-bio` and `seqforge-core` should pass unchanged — the migration is a pure backend swap.

## Testing Strategy

Three layers, applied per-feature:

**Layer 1 — spec-anchored unit tests** (`tests/rebase_canonical.rs`). For canonical enzymes, assert against REBASE documentation directly:
- EcoRI cuts `GAATTC` at position 1 (5′ overhang `AATT`)
- BamHI cuts `GGATCC` at position 1 (5′ overhang `GATC`)
- BsaI (Type IIs) recognizes `GGTCTC`, cuts top at +1 / bottom at +5
- BsmBI recognizes `CGTCTC`, cuts top at +1 / bottom at +5
- SapI recognizes `GCTCTTC`, cuts top at +1 / bottom at +4
- Blunt cutter SmaI: `CCCGGG`, both strands at +3
- 3′ overhang: PstI cuts `CTGCAG` at position 5 (3′ overhang `TGCA`)

These tests catch regressions in the codegen output and the scanner's cut-position math, independent of any external oracle.

**Layer 2 — invariant property tests** (`tests/digest_invariants.rs`):
- Digest then ligate with compatible ends recovers the original sequence
- Site-finding commutes with reverse-complement (sites on forward strand of `seq` ↔ sites on reverse strand of `revcomp(seq)`)
- Circular wrap-around: a recognition site spanning the origin is found iff `circular = true`
- Sticky-end self-compatibility: any 5′ overhang ligates to its reverse complement
- Filtering enzymes to a subset doesn't change the sites found for that subset

**Layer 3 — fixture-based integration tests.** A small corpus of well-known plasmids (pUC19, pBR322, pET28a, pSB1C3) under `tests/fixtures/`. For each, expected outputs are computed once (manually verified against REBASE / SnapGene / NEBcutter) and committed as JSON. Integration tests reload and compare. Biopython enters here as an **optional** cross-check — a `tests/oracle/generate.py` exists for users who want to regenerate fixtures from Biopython, but Rust tests don't require Python to run.

## Roadmap Within the Crate

The crate ships in tiers. Each tier delivers a useful chunk on its own.

**Tier 1 — Enzyme table + scanner** *(unblocks SeqForge today)*
- REBASE snapshot + codegen
- `Enzyme` type, `find_sites` / `find_all_sites`
- All presets including Type IIs / Golden Gate
- Layer 1 + Layer 2 tests
- `seqforge-bio` migrated off `na_seq`

**Tier 2 — Digestion + fragments**
- `digest()` returning `Fragment` with overhang metadata
- Linear and circular topology handling
- Visualization-ready: each fragment carries its source enzyme(s) for highlighting
- Adds fragments tab / view in SeqForge GUI

**Tier 3 — Ligation + cloning ops**
- `ends_compatible`, `ligate`
- Golden Gate one-pot simulation (Type IIs digest + ligate in one call)
- Gibson assembly simulation (overlap detection + join)
- Primer design helpers (Tm calc, primer/template alignment) — *only if needed for cloning workflows; otherwise defer*

**Tier 4 — Extraction**
- Polish README, doc-tests, examples
- API review pass
- First crates.io publish as `seqforge-restriction` or rename to a non-namespaced crate (e.g. `restriction-rs`, `genelib`)
- Announce on r/rust, r/bioinformatics, Rust bio Discord

Tiers 2–4 are scoped here for context but **executed as separate plans** when their time comes. This document is the Tier 1 plan.

## Files Touched (Tier 1)

New:
- `crates/seqforge-restriction/Cargo.toml`
- `crates/seqforge-restriction/README.md`
- `crates/seqforge-restriction/data/rebase_bairoch.txt`
- `crates/seqforge-restriction/xtask/codegen.rs` (separate `cargo run` target)
- `crates/seqforge-restriction/src/{lib,enzyme,scan,preset}.rs`
- `crates/seqforge-restriction/src/enzymes_generated.rs` (codegen output, committed)
- `crates/seqforge-restriction/tests/{rebase_canonical,scan_invariants}.rs`

Modified:
- `Cargo.toml` (workspace members + workspace deps if any)
- `crates/seqforge-bio/Cargo.toml` (depend on seqforge-restriction, drop na_seq)
- `crates/seqforge-bio/src/search.rs` (delegate `find_cut_sites` to new crate)
- `crates/seqforge-bio/src/enzyme_query.rs` (delegate `resolve_query`, add Type IIs / Golden Gate keywords)
- `crates/seqforge-bio/src/lib.rs` (re-exports if surface changes)

Removed:
- `na_seq` from workspace dependencies after migration verified

## Verification

1. `cargo run -p seqforge-restriction-xtask -- generate` regenerates `enzymes_generated.rs` from `rebase_bairoch.txt`; output is byte-identical to committed version on a clean snapshot.
2. `cargo test -p seqforge-restriction` — all Layer 1 + Layer 2 tests pass.
3. `cargo test --workspace` — existing 80 tests in `seqforge-bio` / `seqforge-core` / `seqforge-app` pass unchanged (proves the API migration is invisible to callers).
4. `cargo run -p seqforge-app`, open a fixture plasmid, ⌘E → `golden gate` shows BsaI/BsmBI/BbsI/SapI sites; `type IIs` shows the broader Type IIs set; hover-reveal staples render with correct wedge geometry on both strands.
5. CLI: `seqforge enzymes golden gate` produces the same site count as the GUI.
6. Smoke: open pUC19 fixture, run `enzymes unique`, count matches a SnapGene / NEBcutter screenshot manually.

## Out of Scope (Tier 1)

- Tiers 2–4 (digestion / ligation / extraction) — separate plans
- Primer design, Tm calculation — deferred to Tier 3 if needed
- Methylation-aware site filtering — **now its own pre-`v0.2` plan** ([`methylation.md`](methylation.md)); the Tier-1 assumption that the data was already in the table was wrong (it is not in `bairoch`), so it is a real sourcing effort, not a UI wire-up
- Star activity warnings — table tracks where applicable; UI surfaces later
- Sequence editor support — orthogonal track; merges in when buffer mutations land
- `bio-seq` migration of the buffer representation — separate work, Phase 10+
- Publishing to crates.io — explicit Tier 4 gate

## Notes / Open Questions

- **REBASE attribution.** License requires attribution on use of REBASE data. The crate README and the codegen output header both cite REBASE / Roberts et al. per their requirements.
- **Snapshot refresh policy.** Quarterly cadence by default; out-of-band if a user reports a missing commercial enzyme. The codegen check in CI ensures the committed table matches `data/rebase_bairoch.txt` so drift is caught.
- **Crate name.** `seqforge-restriction` for now keeps the namespace clear inside the workspace. Final name picked at Tier 4 extraction; candidates include `restriction-rs`, `genelib`, `re-rs`. Avoid `restriction` alone (taken on crates.io; checking before extract).
- **The `iirs` crate** (inverted-repeats finder) overlaps slightly with palindrome detection inside enzymes. Not a dependency — we have enough scanning machinery — but worth a footnote for users who land here looking for repeat-finding tools.
