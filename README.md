# SeqForge

A Rust-based sequence viewer for molecular cloning workflows, with an embedded terminal and a unified command layer — every operation is invokable from both the GUI menu and the terminal.

> **Status:** MVP in progress — read-only viewer, file browser, embedded terminal, restriction-site detection (Phase 7), and search are the current targets. See [PLAN.md](PLAN.md) for the full roadmap.

---

## Install

**Prerequisites:** Rust toolchain via [rustup](https://rustup.rs).

```bash
git clone <repo-url>
cd seqforge
```

### GUI app

```bash
cargo build --release
./target/release/seqforge-app
```

> **Note:** use `cargo build` (not `cargo build -p seqforge-app`) so the `seqforge` CLI binary is built alongside the app. The embedded terminal automatically adds it to `PATH`.

### CLI only

```bash
cargo install --path crates/seqforge-cli
```

This puts `seqforge` in `~/.cargo/bin/`, which is on `PATH` if Rust was installed via rustup.

---

## Usage

### GUI

Open the app, then use the file browser on the left to navigate to a `.gb` or `.fasta` file. Double-click to open it in the viewer.

The viewer shows the dual-strand sequence with ATGC colouring, a position ruler, and stacked annotation bars. Click an annotation to select its range; click and drag on the strand to select a custom range.

### Terminal (embedded)

The bottom pane is a real shell. Prefix a line with `:` to send a viewer command directly — it is intercepted before reaching the shell:

```
:open path/to/plasmid.gb      open a file in the viewer
:goto 1234                    place cursor at position 1234
:find ATGCNNNNGCAT            search (IUPAC; Phase 7)
:enzymes EcoRI BamHI          show cut sites (Phase 7)
:close                        close the current document
```

Plain shell commands work normally — `:` is the only magic prefix.

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
seqforge enzymes EcoRI BamHI
```

When the GUI is running, it sets `SEQFORGE_SOCKET` in the embedded terminal's environment. Any `seqforge` viewer command executed there — or in any shell that has `SEQFORGE_SOCKET` set — routes to the live viewer. If the variable is absent, viewer commands exit with a clear error.

### Updating the CLI inside the app

If you change CLI code and want the embedded terminal to pick up the new binary without restarting the app:

```bash
# In a separate terminal (not the embedded one):
cargo build -p seqforge-cli

# The new binary is now at target/debug/seqforge.
# Restart the SeqForge app to pick it up (the PATH injection happens at startup).
```

For a release build: `cargo build --release -p seqforge-cli`, then restart the app pointing at `target/release/`.

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

The workspace has four crates:

| Crate | Role |
|-------|------|
| `seqforge-core` | `Document`, `ViewerState`, `ViewerCommand`, `dispatch_*` — no GUI deps |
| `seqforge-bio` | GenBank/FASTA loading, DNA utilities; wraps `gb-io`, `na_seq`, `bio` |
| `seqforge-app` | `eframe` + `egui_dock` + `egui_term` GUI shell |
| `seqforge-cli` | Standalone `seqforge` binary |

All user-visible actions go through `dispatch_viewer` or `dispatch_file` in `seqforge-core`. Menu clicks, terminal `:commands`, and CLI invocations all parse to the same `ViewerCommand`/`FileCommand` enum and call the same dispatch function.
