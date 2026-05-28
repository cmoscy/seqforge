# SeqForge Refactor Plan

Post-config-landing cleanup. Each item came out of a review of the
config module and its integration points; items are listed in
suggested-priority order. Mark `✅ DONE` when landed.

Scope rule: this file tracks *non-feature* improvements (correctness,
maintainability, UX polish) that don't have a home in `PLAN.md`'s
phase / tier structure. Anything that turns into a feature (e.g. new
config sections for the editor) graduates to `PLAN.md`.

---

## Tier A — Correctness / trust

These are the items that make the config system either lie to the user
or behave non-deterministically. Land these before adding more config
surface area.

### A1. Deterministic keybinding override priority
**Status:** ✅ DONE
**Where:** `config/keybindings.rs:88-103` (`parse`), uses `HashMap`.
**Problem:** `RawFile` deserializes into `HashMap<String, String>`, then
`parse()` iterates it and pushes into `entries` — but `HashMap`
iteration is randomized. Doc comment claims "load order = priority
order"; in practice priority shuffles between launches.
**Fix:** Deserialize via `toml::value::Table` (or `indexmap::IndexMap`
with the `serde` feature) so document order is preserved. Update doc
comment to spell out that first-listed wins.
**Acceptance:** A user file with two chords binding the same key gives
the same active action across N launches; add a unit test that parses
a fixture and asserts entry order.

### A2. Overlay-context gating for user keybinding overrides
**Status:** ✅ DONE
**Where:** `keymap.rs:178-188`.
**Problem:** Overrides only check `KeyContext::WORKSPACE` (always on);
they bypass the overlay tag gate that built-in `KEYMAP` bindings honor.
Rebinding e.g. `cmd+w` fires it even while a Find bar is capturing
input.
**Fix (smallest):** When iterating overrides, also require
`!state.overlays.is_overlay_active_consuming(...)` — i.e. fall through
when the overlay stack would otherwise be the rightful owner of the
key.
**Fix (proper):** Extend the override file format to accept an optional
`when = "workspace" | "overlay" | ...` per binding; default to
`workspace` for back-compat. Route through the same context match the
built-in keymap uses.
**Acceptance:** With `keybindings.toml` rebinding `cmd+f`, pressing
`cmd+f` while the Find bar is already open does not re-trigger
`OpenFind` (it stays consumed by the overlay or is a no-op).

### A3. Lenient parsing with surfaced errors
**Status:** ✅ DONE
**Where:** `config/mod.rs:90-160` and `config/schema.rs` / `theme.rs`
struct attrs.
**Problem:** Every struct uses `#[serde(default, deny_unknown_fields)]`
and parse errors are handled by returning `Settings::default()` /
`Theme::default()`. A single typo discards the *entire* user file, and
the only signal is an `eprintln!` the GUI user never sees.
**Fix:**
1. Drop `deny_unknown_fields` from the leaf structs. Unknown keys
   should warn, not nuke. Optionally keep it only on the top-level
   `Settings` so a misnamed *section* still warns prominently.
2. Surface parse errors as toasts on load and on `ReloadConfig`. Today
   `ReloadConfig` toasts "Reloaded config" even when parsing fell back
   to defaults — that's a lie.
3. Bonus: parse section-by-section so a malformed `[minimap]` doesn't
   discard a valid `[font]`.
**Acceptance:** A `settings.toml` with one typo'd key keeps every other
field. A toast says "settings.toml: unknown key 'fnot' at line 14
(ignored)".

### A4. Linux `open_in_editor` doesn't launch TTY editors for GUI files
**Status:** ✅ DONE
**Where:** `config/mod.rs:183-206`.
**Problem:** Falls through to `$EDITOR` first on Linux. Common values
(`vim`, `nano`) need a TTY; spawning them detached from one fails
silently. Also wrong for "Open Config Folder" (`vim some-dir/`).
**Fix:** Prefer `xdg-open` for both files and directories on Linux.
Only fall back to `$VISUAL`/`$EDITOR` when `xdg-open` is missing
*and* the caller is plausibly headless (heuristic: no `DISPLAY` /
`WAYLAND_DISPLAY`). For directories, always use `xdg-open`.
**Acceptance:** On a stock Linux GUI session with `EDITOR=vim`, "Open
Settings…" launches the system default text editor; "Open Config
Folder" opens a file manager.

---

## Tier B — Drift and dead surface area

Items that don't actively break things but invite future bugs by
duplicating state or shipping knobs that don't do anything.

### B1. Collapse `Theme::default()` drift with `defaults/default-dark.toml`
**Status:** ✅ DONE
**Where:** `config/theme.rs` (Default impls) vs
`config/defaults/default-dark.toml`.
**Problem:** Two sources of truth for the dark palette: code-baked
`Default` impls and the embedded TOML. They already diverge in places
(`feature_color` fallback colors aren't in the file, base colors are
duplicated).
**Fix:** Make `Theme::default()` lazily parse `DEFAULT_DARK` once via
`std::sync::LazyLock` and clone from that. Drop the hardcoded defaults
on `BaseColors`, `UiColors`, `StrandColors`, `MinimapColors`. The
`feature_color` fallback either reads from the same parsed default, or
the legacy palette is moved into the TOML and the code fallback
deleted.
**Acceptance:** Editing a color in `default-dark.toml` and recompiling
visibly changes the app even when no user theme file is present.

### B2. Remove or wire `MinimapSettings.selection_alpha`
**Status:** ✅ DONE
**Where:** `config/schema.rs:121`, template line 44, never read.
**Problem:** Documented config field that does nothing. Theme's
`minimap.selection` already carries alpha in the hex string.
**Fix:** Delete the field, the default, and the template line. (If we
ever want a non-theme alpha override, reintroduce explicitly.)

### B3. Reload-config terminal-shell behavior
**Status:** ✅ DONE (minimum fix: toasts when shell changes)
**Where:** `terminal.rs`, `app.rs` (no shell-respawn path), `command/mod.rs:288`.
**Problem:** `ReloadConfig` toasts "Reloaded config" but `terminal.shell`
takes no effect until a restart — no signal to the user.
**Fix (minimum):** When `ReloadConfig` detects that
`new.settings.terminal.shell != old.settings.terminal.shell`, toast
"Terminal shell change applies after restart."
**Fix (better):** Tear down + rebuild `TerminalPane` when the shell
changes. Lose terminal scrollback as a trade-off.

---

## Tier C — Performance / polish

Smaller wins. Land opportunistically.

### C1. Avoid per-frame `theme.clone()` / `settings.clone()` in minimap
**Status:** ✅ DONE (clones were unnecessary; cfg captured by ref in closure)
**Where:** `minimap.rs:311-312`.
**Problem:** Geometry compute closure captures by-value clones because
the borrow checker rejects holding `&cfg.theme` across the nested
`with_active_buffer`. Theme clone copies a `HashMap<String, HexColor>`
each paint.
**Fix:** Restructure so the cache key is computed first, the
geometry-build closure either reuses an `Arc<Theme>` (if we wrap
`Theme` in `Arc` inside `Config`) or restructures to pull annotations
*before* the cache lookup so the closure borrows `&Theme`.
**Acceptance:** No allocations in the minimap hot path on a frame
where the cache hits.

### C2. Cache key includes config epoch where geometry can drift
**Status:** ✅ DONE
**Where:** `viewer.rs:181-187` (`feature_cache`).
**Problem:** Cache key is `(buffer_id, buffer.version)`. Today the
cached `StackLayout` only depends on feature ranges, so theme/font
reloads are correctly no-op for this cache. But if a future change
folds pixel-aware data into the layout (cut-label stacking already
does), the cache would silently go stale after a `ReloadConfig`.
**Fix:** Add `cfg.epoch` to the key as a forward-compatible guard;
match the minimap pattern.

### C3. Single dock construction at startup
**Status:** ✅ DONE
**Where:** `app.rs:124-167` (`AppState::default()`) + `app.rs:268`
(`rebuild_default_dock`).
**Problem:** `AppState::default()` builds a dock from
`Config::default()` fractions, then `SeqForgeApp::new()` immediately
rebuilds with the loaded config. Wasted work, two code paths for
"build the initial dock".
**Fix:** Take `&Config` into a new `AppState::new(cfg)` and drop the
`Default` impl, *or* have `Default` produce an empty (no-splits) dock
that `new` populates exactly once.

### C4. `apply_split_pane` error variant
**Status:** ✅ DONE
**Where:** `command/layout.rs:96-99`.
**Problem:** Returns `NoActiveView` when the dock can't locate the
active view's tab; misleading because the workspace *did* have an
active view.
**Fix:** Use `DispatchError::ViewNotFound(active_vid)` instead.

### C5. `Action::ALL` linear scan
**Status:** ✅ DONE
**Where:** `config/keybindings.rs:48-53` (`from_name`).
**Problem:** O(N) match per chord at load time. Trivial scale, but it
duplicates the closed-enum surface in three places (`ALL`,
`to_command`, and the help text in `keybindings.toml` template).
**Fix:** Replace with a single `match s` in `from_name`, drop `ALL`,
generate the docs section of the keybindings template from a helper.

### C6. Template files: ship uncommented defaults
**Status:** ✅ DONE (also fixed drifted label_size 11.0 → 12.0)
**Where:** `config/defaults/settings.toml` (every key commented).
**Problem:** The template is all-commented, which combined with
`deny_unknown_fields` (Tier A3) makes the most common user action
(uncomment one line) brittle to typos. Also harder to teach the schema.
**Fix:** Ship the template with default values uncommented so users
override-by-edit instead of override-by-uncomment. Bonus: generate the
template at build time from the `Default` impls so they can't drift.
Depends on A3 landing first to be useful.

---

## Post-landing polish

Minor cleanups noticed during the post-implementation review. Not big
enough for their own tier; landed alongside the Tier A–C work.

- ✅ Dropped unused `Theme.name` field (and `name = ...` from
  `defaults/default-{dark,light}.toml`) — was parsed but never read,
  produced a `dead_code` warning.
- ✅ Trimmed `parse_order_is_deterministic` from a 20× loop to a single
  assert (renamed `parse_preserves_document_order`). `IndexMap` ordering
  is deterministic by construction; the loop was a leftover habit from
  the `HashMap`-era bug.

---

## How to work this list

- Pick an item, switch its status to 🟡 In progress, and open a branch
  named after the section id (e.g. `refactor/a1-keybinding-order`).
- Land each item as its own commit so the diff is auditable.
- Flip to ✅ DONE in the same commit that lands the fix.
- Tier A items should land before adding new config sections for the
  editor (Phase 10) — otherwise the trust issues compound with surface
  area.
