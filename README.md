# SeqForge

A Rust-based sequence viewer for molecular cloning workflows, with an embedded terminal and a unified command layer — every operation is invokable from both the GUI menu and the terminal.

> **Status:** v0.1 read-only viewer shipped (file browser, embedded terminal, restriction-site detection, search); editor (v0.2) is in progress — insert/delete/replace, undo, and save are working. See [ROADMAP.md](ROADMAP.md) for status across all tracks.

---

## Install

**Prerequisites:** Rust toolchain via [rustup](https://rustup.rs).

```bash
git clone <repo-url>
cd seqforge
cargo build --release
./target/release/seqforge-app
```

> **Note:** use `cargo build` (not `cargo build -p seqforge-app`) so the `seqforge` CLI binary is built alongside the app. The embedded terminal automatically adds it to `PATH` — no install step needed.

### Making `seqforge` available system-wide (optional)

To also use `seqforge` from any terminal window (not just the embedded one), install it via the GUI or a single flag:

**From the GUI:** `Tools → Install 'seqforge' CLI to PATH`

**Headless / scripted:**
```bash
./target/release/seqforge-app --install-cli
```

Both methods symlink the bundled binary into `/usr/local/bin` (if writable) or `~/.local/bin`. After updating the app, re-run either to refresh the symlink.

---

## Usage

### GUI

Open the app, then use the file browser on the left to navigate to a `.gb` or `.fasta` file. Double-click to open it in the viewer.

The viewer shows the dual-strand sequence with ATGC colouring, a position ruler, and stacked annotation bars. Click an annotation to select its range; click and drag on the strand to select a custom range.

### Terminal (embedded)

The bottom pane is an ordinary shell — no special prefix or syntax. It only
differs from any other terminal in that `SEQFORGE_SOCKET` is already exported,
so `seqforge` commands typed there route to the live window automatically (see
[CLI](#cli-standalone-or-from-any-terminal) below).

### CLI (standalone or from any terminal)

The `seqforge` binary works with or without the GUI open.

**File commands** (always local — no GUI needed):

```bash
seqforge info plasmid.gb
```

**Viewer commands** (require a running SeqForge window):

```bash
seqforge open path/to/plasmid.gb
seqforge goto 500
seqforge close
seqforge find ATGC
seqforge enzymes "EcoRI BamHI"        # quote multi-enzyme queries — it's one argument
seqforge enzymes "golden gate"        # preset: BsaI, BsmBI, BbsI, SapI
seqforge enzymes --op add SpeI        # union into the active set (also: --op remove)
```

The query is a **single argument**, so any value with a space — an enzyme list
or a two-word preset — must be quoted (`"golden gate"`, `"EcoRI BamHI, SpeI"`).
Within that argument, names may be separated by spaces or commas. Accepted
presets: `unique`, `unique+dual`, `non-cutters`, `type IIs`, `golden gate`,
`moclo`, `all`, `none`. The same grammar is shared by the GUI Restriction Sites
panel (`⌘E`, where no shell quoting applies) and the CLI.

**Editor commands** (require a running window; positions are 0-based):

```bash
seqforge insert 100 ATGC              # insert bases at a position
seqforge delete 100 110               # delete the range [start, end)
seqforge replace 100 110 GGGG         # replace a range
seqforge reverse-complement 100 110   # revcomp a range in place
seqforge cut 100 110                  # cut / copy / paste operate on the clipboard
seqforge undo                         # also: redo
seqforge save                         # also: save-as <path>
```

Feature editing (`add-feature`, `remove-feature`, `rename-feature`) is wired
the same way. Run `seqforge --help` for the full, generated command list.

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

The workspace has five crates:

| Crate | Role |
|-------|------|
| `seqforge-core` | data model (`Buffer`, `Annotations`, `View`), the typed command surface (`ViewerRequest`, `FileCommand`), and `dispatch`/`dispatch_file` — no GUI deps |
| `seqforge-bio` | GenBank/FASTA loading, DNA utilities, sequence/cut-site search; wraps `gb-io`, `bio`, and `seqforge-restriction` |
| `seqforge-restriction` | REBASE-derived restriction enzyme database + scanner + presets (Type IIs, Golden Gate, MoClo). Unpublished; see [plans/restriction.md](plans/restriction.md) |
| `seqforge-app` | `eframe` + `egui_dock` + `egui_term` GUI shell |
| `seqforge-cli` | Standalone `seqforge` binary |

All user-visible actions go through `dispatch` or `dispatch_file` in `seqforge-core`. Menu clicks and CLI/socket invocations all parse to the same `ViewerRequest`/`FileCommand` enum and call the same dispatch path.
