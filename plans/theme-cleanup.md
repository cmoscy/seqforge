# Theme & Config Cleanup

> **Status: Tier 1 done (2025-07-27). Tier 2 done (2026-08-09).**
>
> Tier 1 completed: `[translation]`, `[ui].hover_wash`, `[inspector]` added to TOML templates.
>
> Tier 2 completed: `DiffColors` + `[diff]` section, 5 new `UiColors` keys, the 3
> `sequence.rs` consts and all 6 inspector color sites replaced. The inspector access
> strategy resolved to **Option A** — `inspector.show` takes `&Theme`, threaded from
> `app.rs`. `cargo clippy --workspace` clean, 170 tests pass.
>
> Tiers 3+ (panel sizes, grid spacings, `font.ui_size`) remain open.

---

## Tier 2 — Theme the hardcoded semantic colors

### Scope

~20 hardcoded `Color32::from_rgb(...)` calls across 5 modules need to flow through the theme
system. Two-part change:

1. **Add theme keys** — new `DiffColors` struct + `[diff]` TOML section; extend `UiColors` with
   5 new keys (`danger`, `danger_hover`, `warn`, `error`, `methyl_blocked`).
2. **Replace hardcoded values** — remove 3 module-level `const`s in `sequence.rs`; replace ~12
   inline `Color32::from_rgb(...)` calls with theme lookups.

### Exact sites to change

#### Diff colors — 3 module-level consts → theme

**`viewer/tracks/sequence.rs:18-22`**

```rust
// Before (delete these 3 consts):
const DIFF_ADD_BG: Color32 = Color32::from_rgba_premultiplied(40, 120, 75, 70);
const DIFF_DEL_BG: Color32 = Color32::from_rgba_premultiplied(120, 45, 45, 70);
const DIFF_DEL_LINE: Color32 = Color32::from_rgb(214, 92, 92);

// After — use ctx.diff_add_bg.0 / ctx.diff_del_bg.0 / ctx.diff_del_line.0
// These come from ViewerStyle, which is already available as `cfg` in the render loop.
```

**New theme struct** (theme.rs, after `MinimapColors`):

```rust
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct DiffColors {
    pub add_bg: HexColor,   // green wash behind added bases
    pub del_bg: HexColor,   // red wash behind deleted bases
    pub del_line: HexColor, // strikethrough line on deleted bases
}

impl Default for DiffColors {
    fn default() -> Self {
        Self {
            add_bg: HexColor(Color32::from_rgba_premultiplied(40, 120, 75, 70)),
            del_bg: HexColor(Color32::from_rgba_premultiplied(120, 45, 45, 70)),
            del_line: HexColor(Color32::from_rgb(214, 92, 92)),
        }
    }
}
```

Add `pub diff: DiffColors` to `Theme` struct.

**ViewerStyle wiring** (viewer/track.rs:595-609 — add after `orf_wash`):

```rust
pub diff_add_bg: Color32,
pub diff_del_bg: Color32,
pub diff_del_line: Color32,
```

**ViewerStyle construction** (viewer/mod.rs:910-927 — add after `orf_wash`):

```rust
diff_add_bg: cfg.theme.diff.add_bg.0,
diff_del_bg: cfg.theme.diff.del_bg.0,
diff_del_line: cfg.theme.diff.del_line.0,
```

**TOML** — add to both `default-dark.toml` and `default-light.toml`:

```toml
[diff]
add_bg   = "#28784B46"
del_bg   = "#782D2B46"
del_line = "#D65C5C"
```

#### Semantic UI colors — 5 new UiColors fields

**New UiColors fields** (theme.rs, after `hover_wash`):

```rust
pub danger: HexColor,         // destructive actions (delete confirm fill)
pub danger_hover: HexColor,   // danger button hover state (remove button hover)
pub warn: HexColor,           // warning indicators (primer drifted state dot)
pub error: HexColor,          // error text / labels
pub methyl_blocked: HexColor, // methylated/blocked enzyme state
```

Defaults matching current hardcoded values:

```rust
danger:       HexColor(Color32::from_rgb(0xB0, 0x30, 0x30)),
danger_hover: HexColor(Color32::from_rgb(0xE0, 0x60, 0x60)),
warn:         HexColor(Color32::from_rgb(0xE0, 0xA0, 0x30)),
error:        HexColor(Color32::from_rgb(0xE0, 0x60, 0x60)),
methyl_blocked: HexColor(Color32::GRAY),
```

**~~ViewerStyle wiring~~ — superseded.** These 5 are consumed only by the inspector, which
does not render through `ViewerStyle`. Under Option A they are read directly off
`theme.ui.danger` / `.danger_hover` / `.warn` / `.error` / `.methyl_blocked` at the call
sites, and `ViewerStyle` gains only the `diff_*` trio above.

**TOML** — add to both theme files in `[ui]` section (after `hover_wash`):

```toml
danger         = "#B03030"
danger_hover   = "#E06060"
warn           = "#E0A030"
error          = "#E06060"
methyl_blocked = "#808080"
```

#### Inspector inline color replacements

These 6 sites currently use hardcoded `Color32::from_rgb(...)` calls. They need to be replaced with theme lookups. **The access strategy is under audit** (see Inspector Access Strategy below).

| File:Line | Current code | Replacement | Semantic |
|-----------|-------------|-------------|----------|
| `inspector/row.rs:63` | `Color32::from_rgb(0xE0, 0x60, 0x60)` | `ctx.danger_hover.0` | Remove button hover |
| `inspector/row.rs:198` | `Color32::from_rgb(0xE0, 0xA0, 0x30)` | `ctx.warn.0` | `Tone::Warn` dot (primer drifted) |
| `inspector/primer.rs:407` | `Color32::from_rgb(0xE0, 0x60, 0x60)` | `ctx.error.0` | Error label text |
| `inspector/primer.rs:543` | `Color32::from_rgb(0xB0, 0x30, 0x30)` | `ctx.danger.0` | Delete confirm button fill |
| `inspector/feature.rs:207` | `Color32::from_rgb(0xB0, 0x30, 0x30)` | `ctx.danger.0` | Delete confirm button fill |
| `inspector/cutsite.rs:75` | `Color32::GRAY` | `ctx.methyl_blocked.0` | Blocked enzyme text |

#### Inspector Access Strategy — under audit

The inspector modules (`row.rs`, `primer.rs`, `feature.rs`, `cutsite.rs`) currently render via
egui callbacks that receive `ui: &Ui` but do **not** have direct access to `ViewerStyle` /
`config.theme`. Two approaches are on the table:

**Option A: Pass theme through inspector render path**

The inspector is rendered from `app.rs` where `config: &Config` is already available. Thread
`&config.theme` (or a borrowed `ViewerStyle`) through the inspector render calls as an additional
parameter. This is the cleanest approach — the inspector modules get theme data without
dependencies on the config crate.

Pros: Fully themeable, consistent with how the viewer already works.
Cons: Requires plumbing changes through the inspector render call chain; needs audit of how
deep the changes propagate.

**Option B: Shared module-level const**

Create a single shared const in `config/theme.rs` or `app.rs`:

```rust
pub const DELETE_CONFIRM: Color32 = Color32::from_rgb(0xB0, 0x30, 0x30);
```

Import in both `primer.rs` and `feature.rs` to eliminate the duplicate literal.

Pros: Minimal code change, eliminates duplication immediately.
Cons: Not themeable — falls back to dark-theme defaults for all themes.

**Decided: Option A** (2026-08-09). The plumbing was light — `inspector.show` gained a
`&Theme` parameter, passed from the two call sites in `app.rs` where `config` was already in
scope, and forwarded to `show_features` / `show_primers` / `show_cutsites` / `row_shell`.
No `ViewerStyle` involvement, so the 5 semantic keys are read straight off `theme.ui`.

### Out of scope for Tier 2

- **Gamma multiply alpha adjustments** (e.g. `0.45`, `0.7`, `0.28`) — design decisions about
  relative brightness, not semantic color choices. Separate effort.
- **`Color32::WHITE` / `BLACK` / `TRANSPARENT`** — generic utility colors, not semantic.
- **`Tone::Warn` vs `theme.ui.mismatch`** — these serve different contexts (inspector dot vs
  viewer mismatch cell) and are intentionally ~5 units apart in each channel. The new `ui.warn`
  key captures the inspector dot value; `ui.mismatch` stays as-is for the viewer.

### Execution order

1. **theme.rs** — add `DiffColors` struct, extend `UiColors` with 5 fields, update `Default` impls
2. **Theme struct** — add `pub diff: DiffColors`
3. **viewer/track.rs** — add 3 new fields to `ViewerStyle` (the `diff_*` trio; the 5
   semantic keys go to the inspector via Option A, not through `ViewerStyle`)
4. **viewer/mod.rs** — wire theme values into `ViewerStyle` construction
5. **viewer/tracks/sequence.rs** — delete 3 consts, replace with `ctx.diff_xxx.0`
6. **Inspector modules** — replace hardcoded colors (strategy TBD — see above)
7. **Both TOMLs** — add `[diff]` and `[ui]` new keys
8. **Build + test + clippy**

Each step compiles independently. Steps 1-5 and 7 are straightforward. Step 6 is gated on
the inspector access strategy decision.

---

## Problem

1. **Schema is ahead of the TOML templates.** Several config structs have fields that deserialize fine but are invisible to users — no `[minimap]`, `[inspector]`, `[translation]`, or `[ui].hover_wash` keys appear in the embedded TOML defaults or the settings template. Users creating a custom theme won't know these exist.
2. **`font.ui_size` is dead code.** It deserializes but is never read anywhere.
3. **~20 hardcoded colors** are scattered across inspector/viewer modules — diff colors, warning/error states, delete confirmation buttons, methylation blocked state. These are semantic colors that should be themeable.
4. **Panel default sizes are hardcoded** in `app.rs` (Inspector 280, range 220-560; Minimap 200, range 48-∞) while the settings comment says "egui remembers it."
5. **5 different hardcoded grid spacings** across feature form, translation window, inspector editors, assembly workbench — no unified spacing config.

The architecture is sound; the gap is between what the schema supports and what's actually wired up or discoverable.

## Goals

- Every config struct field has a corresponding key in the TOML template (users can discover and override everything that exists).
- All semantic colors (diff, warning, error, destructive actions) are themeable.
- `font.ui_size` is either used or removed.
- Panel sizes and grid spacings are configurable (or at least consolidated).
- Dark/light themes cover the full UI, not just the viewer.

## Non-goals

- No redesign of the config loading pipeline (it works well).
- No new UI features — this is purely about wiring and consistency.
- No changes to `seqforge-core` or the command dispatch architecture.

---

## Tiers

### Tier 1 — Surface existing schema in TOML templates

Fix the "invisible config" problem. Every struct in `schema.rs` and `theme.rs` that has fields should have those fields present in the embedded TOML defaults. Users should be able to copy a default template and customize everything.

**1.1 Add `[translation]` to both theme TOMLs**

- `TranslationColors` (theme.rs:211-229) has `stop`, `start`, `orf_wash` — parsed via serde but absent from `default-dark.toml` and `default-light.toml`.
- Add `[translation]` section with dark defaults to `default-dark.toml` and light defaults to `default-light.toml`.
- The `Default` impl in theme.rs will still serve as fallback for user themes that omit the section.

```toml
# default-dark.toml — add to end
[translation]
stop    = "#D24646"
start   = "#46AF5A"
orf_wash = "#5AAF6A1A"
```

```toml
# default-light.toml — add to end (stop/start shared with dark per current Default impl)
[translation]
stop    = "#D24646"
start   = "#46AF5A"
orf_wash = "#5AAF6A1A"
```

**1.2 Add `[ui].hover_wash` to both theme TOMLs**

- `UiColors.hover_wash` (theme.rs:173) is parsed but absent from both TOMLs. Falls through to hardcoded `Color32::from_rgba_unmultiplied(150, 155, 165, 64)`.
- Add to both theme files.

```toml
# default-dark.toml [ui] section — add
hover_wash = "#969BA540"

# default-light.toml [ui] section — add (reuse same value or pick a light-appropriate one)
hover_wash = "#969BA540"
```

**1.3 Add `[minimap]` to `settings.toml` template**

- `MinimapSettings` has 9 keys (theme.rs:232-251) — all parsed, none in the settings template.
- Add a `[minimap]` section to `settings.toml` with all keys and comments. It already exists in the template (lines 34-43), so **this is already done**. No action needed.

**1.4 Add `[inspector]` to `settings.toml` template**

- `InspectorSettings` has `follow_selection: bool` — parsed but not in the settings template. Already added in Tier 1 implementation.
- `terminal.shell` exists in both schema and template. Already covered.
- `font.ui_size` exists in schema and settings template but is **never read** in code (see Tier 3).

---

### Tier 3 — Fix `font.ui_size` (use it or remove it)

`font.ui_size` (default 13.0) exists in the schema and settings template but is never read. Two options:

**Option A: Wire it in** (preferred — keeps the config key alive)

Use `ui_size` for the inspector panel text, header rows, and icon buttons. Currently:
- `inspector/row.rs:71,103` — icon buttons use `FontId::proportional(14.0)` hardcoded
- Various header/description text in inspector uses egui defaults

Replace the hardcoded `FontId::proportional(14.0)` with `FontId::proportional(ui_size)`. This makes icon sizes scale with the user's UI font preference.

**Option B: Remove it** (if it doesn't have a clear home)

If `ui_size` genuinely has no good place in the current UI, remove it from the schema, template, and defaults. Don't leave dead config keys.

---

### Tier 4 — Consolidate grid spacings

Currently 5 different hardcoded `[x, y]` spacing values:

| Location | Value | File:Line |
|----------|-------|-----------|
| Feature form | `[12.0, 6.0]` | `app.rs:447` |
| Translation window | `[10.0, 4.0]` | `app.rs:626` |
| Inspector editor grids | `[10.0, 5.0]` | `primer.rs:424`, `feature.rs:152` |
| Assembly workbench sources | `[12.0, 4.0]` | `workbench.rs:248` |
| Assembly workbench combo | `[8.0, 2.0]` | `workbench.rs:1150` |

Add a `[ui].spacing` section to `UiColors` (or a new `SpacingSettings` struct):

```rust
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct SpacingSettings {
    /// Default form grid (label, input).
    pub form: [f32; 2],
    /// Tight grid for compact controls (combos, small inputs).
    pub compact: [f32; 2],
    /// Inspector editor grid.
    pub inspector: [f32; 2],
}
```

Defaults:

```rust
form: [12.0, 6.0],
compact: [8.0, 2.0],
inspector: [10.0, 5.0],
```

Replace hardcoded `spacing([x, y])` calls with the appropriate named spacing. The translation window `[10.0, 4.0]` is the outlier — either add a third variant or accept that `form` and `compact` don't perfectly cover it (in which case, just standardize to one of the existing values).

---

### Tier 5 — Make panel sizes configurable

Current hardcoded values in `app.rs:1887-1898`:

| Panel | Default | Range |
|-------|---------|-------|
| Inspector | 280.0 | 220.0..=560.0 |
| Minimap | 200.0 | 48.0..=f32::INFINITY |

The settings comment says "egui remembers it" — and it does, for *resized* panels. But the **initial** size is still hardcoded. Add to `EditorSettings` (or a new `PanelSettings`):

```rust
pub inspector_width: f32,
pub inspector_width_range: (f32, f32),  // (min, max)
pub minimap_height: f32,
pub minimap_height_range: (f32, f32),   // (min, max)
```

Defaults: `inspector_width = 280.0`, `inspector_width_range = (220.0, 560.0)`, `minimap_height = 200.0`, `minimap_height_range = (48.0, f32::INFINITY)`.

Replace the hardcoded `default_width`/`width_range`/`default_height` calls with values from config.

---

### Tier 6 — Housekeeping

**6.1 Fix the comment in `settings.toml`** (line 45-46)

Current text says panel sizes are "managed by the window manager" and "egui remembers it." After Tier 5, update to reflect that initial sizes are configurable.

**6.2 Add `[ui]` keys for light theme that differ from dark**

Currently `hover_wash` (2.2) and `translation` (1.1) are identical in both themes. That's fine for now — note it as a future enhancement if light-mode-specific variants are desired.

**6.3 Add a "Open Theme" command or menu item**

The settings template mentions "see Open Theme to author your own" (line 10-11) but there's no such menu item or command. Either:
- Add a simple "Open Config Dir" command that opens `$XDG_CONFIG_HOME/seqforge` (or platform equivalent) in the file manager, OR
- Remove the "Open Theme" reference from the template comment, OR
- Build a minimal theme-editor overlay (deferred — overkill for now)

The cheapest option is #2: remove the reference. The second cheapest is #1: one line in the Settings menu.

---

## Implementation order

1. **Tier 1** (1.1, 1.2, 1.4) — safest, zero behavior change, just TOML additions
2. **Tier 2** (2.1, 2.2, 2.3) — adds theme keys and replaces hardcoded colors
3. **Tier 3** (3.1) — either wire in `ui_size` or remove it
4. **Tier 4** — consolidate spacings
5. **Tier 5** — configurable panel sizes
6. **Tier 6** — docs/comments cleanup

Each tier compiles and runs independently. No tier changes user-facing behavior except by making more things configurable.

## Risk

- **Low.** All changes are additive (new config keys, new theme sections) or literal replacements (hardcoded `Color32::from_rgb(X)` → `theme.ui.danger.0`). No API changes, no data model changes.
- The one risk is `deny_unknown_fields` on `Theme` — if user themes from a previous version reference keys that no longer exist, they'll get a parse error. But since we're only *adding* keys (not removing), this is a non-issue. Users with old themes get the new defaults via `#[serde(default)]`.
