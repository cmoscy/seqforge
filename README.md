# SeqForge

[![CI](https://github.com/cmoscy/seqforge/actions/workflows/ci.yml/badge.svg)](https://github.com/cmoscy/seqforge/actions/workflows/ci.yml)

A Rust-based sequence viewer for molecular cloning workflows, with an embedded terminal and a unified command layer — every operation is invokable from both the GUI menu and the terminal.

https://github.com/cmoscy/seqforge

> **Status:** v0.2 editor tagged (`v0.2.0`); v0.3 assembly **A1** (workbench + `assemble`/`digest`) and fidelity **F1** (Tatapov/Potapov overhang %) are in-tree. Gibson / overhang design still next. Still pre-1.0. See [ROADMAP.md](ROADMAP.md) for per-track detail.

---

## Install

**Prerequisites:** Rust toolchain via [rustup](https://rustup.rs). The repo pins Rust **1.95.0** in [rust-toolchain.toml](rust-toolchain.toml); rustup installs it automatically on first `cargo` invocation. On Linux you also need the usual GUI build packages (see CI). On Windows, rustup's default MSVC toolchain needs the Visual C++ build tools.

```bash
git clone https://github.com/cmoscy/seqforge.git
cd seqforge
cargo build --release
./target/release/seqforge-app   # Windows: target\release\seqforge-app.exe
```

For a quick debug build (contributors):

```bash
cargo run -p seqforge-app
```

> **Note:** use `cargo build` (not `cargo build -p seqforge-app`) so the `seqforge` CLI binary is built alongside the app. The embedded terminal automatically adds it to `PATH` — no install step needed.

### Making `seqforge` available system-wide (optional)

To also use `seqforge` from any terminal window (not just the embedded one), install it via the GUI or a single flag:

**From the GUI:** `Tools → Install 'seqforge' CLI to PATH`

**Headless / scripted:**
```bash
./target/release/seqforge-app --install-cli
```

Both methods put the bundled CLI on your user `PATH`. On macOS and Linux that is a symlink into `/usr/local/bin` (if writable) or `~/.local/bin`. On Windows it copies `seqforge.exe` into `%LOCALAPPDATA%\SeqForge\bin` and prepends that directory to the user `PATH`. After updating the app, re-run either to refresh.

---

## Usage

### GUI

Open the app, then use the file browser on the left to navigate to a `.gb` or `.fasta` file. Double-click to open it in the viewer.

The viewer shows the dual-strand sequence with ATGC colouring, a position ruler, and stacked annotation bars. The **Inspector** (**Features · Cut sites · Primers**) lists annotations; the **Restriction Sites** panel (`⌘E`) shares the CLI enzyme grammar. Primers show as a map overlay with thermo QC. Map selection highlights the matching Inspector row (and switches tabs unless `inspector.follow_selection = false`).

From **Cut sites**, **Digest** opens a read-only **Fragments** list over the source buffer. For cloning, **File → New Assembly…** builds a recipe of bins (Digest / PCR / AsIs → Ligate or Golden Gate); optional fidelity % appears on the join strip. Products open as normal sequence tabs. Depth: [plans/assembly.md](plans/assembly.md), [plans/fidelity.md](plans/fidelity.md).

### Terminal (embedded)

The bottom pane is an ordinary shell — no special prefix or syntax. It only
differs from any other terminal in that `SEQFORGE_SOCKET` is already exported,
so `seqforge` commands typed there route to the live window automatically (see
[CLI](#cli-standalone-or-from-any-terminal) below).

### CLI (standalone or from any terminal)

The `seqforge` binary works with or without the GUI open. What decides whether a
verb needs a running window is **which document it addresses**, not which verb it
is (ROADMAP decision 27).

Most verbs take a document target:

| target | meaning |
|---|---|
| `--in <path>` | open that file in this process. **No GUI needed.** |
| `--view <n>` | a specific tab in the running SeqForge (ids come from `seqforge buffers`) |
| *omitted* | the running SeqForge's active tab |

The two flags conflict — `--in` with `--view` is rejected by the argument parser,
and by the socket handler for callers that bypass it.

A bare positional path is sugar for `--in`, so the two forms below are the same
request:

```bash
seqforge info plasmid.gb              # sugar
seqforge info --in plasmid.gb         # canonical
seqforge info                         # the running viewer's active tab
seqforge info --view 3                # a specific tab
```

**Reading a file needs nothing running:**

```bash
seqforge info plasmid.gb
seqforge translate plasmid.gb --start 0 --end 30
seqforge orfs plasmid.gb --min-aa 30
seqforge digest plasmid.gb --enzymes EcoRI,BamHI
seqforge primers list plasmid.gb
seqforge primers find plasmid.gb GCGTAC     # sugar for find-primer-sites

seqforge list-features --in plasmid.gb
seqforge list-primers  --in plasmid.gb
seqforge find GAATTC   --in plasmid.gb
seqforge enzymes unique --in plasmid.gb
seqforge find-primer-sites --oligo GCGTAC --in plasmid.gb

seqforge tm GGGACCGCCT                # the exception: addresses no document at all
```

`--in` is **headless by definition**: it resolves in this process against a
workspace that exists for one request. Any view state the verb sets — the enzyme
set, the cursor, the search — is scoped to that request and discarded with it.
So `--in` never touches a running viewer, even if that viewer has the same file
open; use `--view`, or `open` the file first, to act on the session's copy.

Ids in a `--in` result (`FeatureId`, `PrimerId`) are **scoped to that one
invocation** for the same reason. They name entries in a workspace that dies with
the process, so an id read from a file cannot be passed as `--id` to a verb
targeting a running session — mint it from that session instead (decision 12).

### Verbs that name no document

Not drift — these are exempt by design:

| verb | why |
|---|---|
| `tm` | takes an oligo, not a document |
| `open`, `new` | *create* a document rather than naming one |
| `close`, `buffers` | act on the workspace, not a document |
| `focus <handle>` | names a document by a loose handle — index, path, or basename — deliberately fuzzy because a human types it |
| `assemble` | its inputs are a **recipe** (per-bin prepare specs, globs, `buffer:<n>`, combos), richer than a document target |
| `save-as <path>` | the positional path is the *destination*; `--in`/`--view` still names the *source* |

`primers find` seeds on the oligo's 3' end and extends, so a **cloning primer
finds its site**: the reported footprint is the annealed region and the report
carries the 5' tail (restriction site, overhang, homology arm) and the anneal Tm
separately. A tail also survives loading — SeqForge reads the `sequence: …` note
SnapGene and Benchling write, and emits it alongside its own — which is what lets
a tailed primer be used as a PCR fragment source:

```bash
# One fragment, amplified off a plasmid with its restriction sites added by the
# primer tails — the same thing you would do with DpnI at the bench.
seqforge assemble 'template.gb@pcr:6H8-VH-1F..6H8-VH-1R' \
  --method ligate --topology linear --name-template H1 --out build/pcr/
```

**Session verbs** — these are *about* the window, so they require one:

```bash
seqforge open path/to/plasmid.gb
seqforge buffers                      # list open tabs and their view ids
seqforge close
seqforge goto 500
seqforge find ATGC                    # drop --in to search the active tab
seqforge enzymes "EcoRI BamHI"        # quote multi-enzyme queries — it's one argument
seqforge enzymes "golden gate"        # preset: BsaI, BsmBI, BbsI, SapI
seqforge enzymes --op add SpeI        # union into the active set (also: --op remove)
```

The enzyme query is a **single argument**, so any value with a space — an enzyme list
or a two-word preset — must be quoted (`"golden gate"`, `"EcoRI BamHI, SpeI"`).
Within that argument, names may be separated by spaces or commas. Accepted
presets: `unique`, `unique+dual`, `non-cutters`, `type IIs`, `golden gate`,
`moclo`, `all`, `none`. The same grammar is shared by the GUI Restriction Sites
panel (`⌘E`, where no shell quoting applies) and the CLI.

**Write verbs** (require a running window). Every verb that mutates a sequence
takes the same `--in`/`--view` target as the reads, but a file target is not yet
implemented — `--in` on a write reports that clearly and leaves the file
untouched, rather than half-editing it:

```bash
seqforge insert 100 ATGC
seqforge delete 100 110
seqforge undo                         # also: redo
seqforge save                         # also: save-as <path>

seqforge insert 0 ATGC --in plasmid.gb
# Error: `a write verb against a file target (it needs a session)` is not yet implemented
```

`undo`/`redo` are session verbs by nature — history is per-buffer and lives only
as long as the process, so "reverse the previous command" has no referent in a
one-shot invocation.

`goto` is **1-based**; edit ranges are **0-based**. Feature/primer editing and more verbs are wired the same way — run `seqforge --help` for the full list.

**Cloning** (local; no GUI needed for dry-run):

```bash
seqforge digest plasmid.gb --enzymes EcoRI,BamHI
seqforge assemble vector.gb@EcoRI..PstI insert.gb@EcoRI..PstI --method ligate --dry-run
seqforge assemble parts/*.gb@BsaI..BsaI --method golden-gate --dry-run --fidelity-dataset bsai
```

Drop `--dry-run` to build, and `--out` to keep the products:

```bash
# A combinatorial Golden Gate: each bin is a glob, so all-to-all expands to
# one product per source combination — written as GenBank, named from the
# parts that went in.
seqforge assemble \
  'vector.gb@BsaI@401..BsaI@314' 'parts/pos1/*.gb' 'parts/pos2/*.gb' \
  --method golden-gate --enzymes BsaI \
  --out build/ --name-template 'lib-{bin1}-{bin2}'

# Build every combo but one (indices match the --dry-run combo list):
seqforge assemble recipe.json --combos '0-31,!12' --out build/
```

`--name-template` takes brace-delimited tokens — `roles`, `n` (combo index),
`i` (ordinal), and `bin0`…`binN` (the file stem that bin contributed, optionally
truncated as `bin1:6` or reduced to one `_`-separated field as `bin1/1`). It sets
both the product name and the filename. The field selector is what makes
hierarchical assembly compose: a level-2 product named from level-1 inputs takes
the part that varies (`6H8_VH-PVP_pGGa` → `VH-PVP`) rather than nesting whole
stems.

`--origin` rotates each circular product so a named landmark becomes position 1:

```bash
seqforge assemble ... --origin Start        # a feature label (must be unique)
seqforge assemble ... --origin 0            # or a 0-based index
```

Without it a product opens wherever the first bin's restriction cut fell, which
is arbitrary — with it, a whole combinatorial library opens in the same frame.
The same landmark works on an open document: `seqforge set-origin --feature Start`
(the GUI equivalent is **Set Origin at cursor**).

`assemble` is one verb over two document sources, and **routing follows the
document, not the verb**. Inputs that are all paths run in this process — no
viewer needed. Name a live document with `buffer:<n>`, or pass no inputs at all
(meaning "the recipe open in the workbench"), and the same request is forwarded
to the running SeqForge, which opens each product as a tab *and* writes it:

```bash
seqforge assemble parts/*.gb --method golden-gate --enzymes BsaI --emit-recipe r.json
seqforge assemble r.json --combos '!0' --out build/     # local: r.json is a path
seqforge assemble buffer:2@BsaI..BsaI insert.gb --out build/   # forwarded
```

The workbench's **Run → folder…** button is the same code path. In both faces
the tab cap applies to *tabs only* — every product is still written to disk.

When the GUI is running, it sets `SEQFORGE_SOCKET` in the embedded terminal's environment. Any `seqforge` viewer command executed there — or in any shell that has `SEQFORGE_SOCKET` set — routes to the live viewer. If the variable is absent, viewer commands exit with a clear error.

---

## Supported file formats

| Format | Extensions | Notes |
|--------|------------|-------|
| GenBank | `.gb`, `.gbk`, `.genbank` | Fully supported |
| FASTA | `.fasta`, `.fa`, `.fna` | Sequence only; no features |
| SnapGene | `.dna` | Planned (post-MVP) |

---

## Development

```bash
cargo check          # fast type-check
cargo test           # run all tests
cargo clippy         # lint
cargo fmt            # format
cargo build          # build everything (app + CLI)
```

### Testing workflow

The embedded terminal finds the `seqforge` CLI as a sibling of the app binary in `target/`, so both must be built. Build once with `cargo build`, then iterate with `cargo run` — the CLI binary persists between runs:

```bash
cargo build                        # first time: builds app + CLI
cargo run -p seqforge-app          # subsequent runs: rebuilds only what changed
```

The workspace has eight crates:

| Crate | Role |
|-------|------|
| `seqforge-core` | data model, typed command surface, `dispatch` — no GUI deps |
| `seqforge-bio` | I/O, DNA utilities, primers/thermo, assembly engine; wraps restriction + thermo |
| `seqforge-restriction` | REBASE enzyme DB + scanner + presets. See [plans/restriction.md](plans/restriction.md) |
| `seqforge-thermo` | Tm/GC/folding (vendored seqfold); via `seqforge-bio` — [docs/architecture.md](docs/architecture.md) |
| `seqforge-fidelity` | overhang ligation fidelity (Potapov/Pryor via tatapov); assembly dry-run / join strip — [crate README](crates/seqforge-fidelity/README.md) |
| `seqforge-session` | `Workspace` (buffers, views, per-buffer undo), the write path, and the `Host` seam. Headless — no egui |
| `seqforge-app` | `eframe` + `egui_dock` + `egui_term` GUI shell |
| `seqforge-cli` | Standalone `seqforge` binary |

All user-visible actions parse to one `ViewerRequest` and go through `dispatch` in `seqforge-core` (reads) or the `seqforge-session` write path (writes). Menu clicks, CLI invocations, and socket requests are three faces of the same value — they differ only in the document target they carry.

### Contributing

- **Bugs and feedback:** [GitHub Issues](https://github.com/cmoscy/seqforge/issues)
- **Architecture:** [docs/architecture.md](docs/architecture.md), [docs/socket-protocol.md](docs/socket-protocol.md)
- **CI:** pushes and PRs to `main` run `cargo fmt --check`, `cargo clippy -D warnings`, and `cargo test` on Ubuntu (see [`.github/workflows/ci.yml`](.github/workflows/ci.yml))

---

## License

SeqForge original source code is licensed under the MIT License — see [LICENSE](LICENSE).

Bundled data and vendored components retain their own terms — see [NOTICE](NOTICE).
Notably: restriction enzyme data is derived from [REBASE](http://rebase.neb.com) (© Dr. Richard J. Roberts);
thermodynamics code vendors [seqfold](https://github.com/Lattice-Automation/seqfold) (MIT);
ligation-fidelity matrices are from Potapov et al. / Pryor et al. via [tatapov_data](https://github.com/Edinburgh-Genome-Foundry/tatapov_data) (**CC BY-ND 4.0**) — see [crates/seqforge-fidelity/ATTRIBUTION.md](crates/seqforge-fidelity/ATTRIBUTION.md).
