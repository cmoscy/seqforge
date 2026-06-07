//! Declarative keymap.
//!
//! See [`docs/focus-refactor.md`](../../../docs/focus-refactor.md) §2.4.
//!
//! Stage 4 of the focus refactor: every keyboard shortcut lives in the
//! [`KEYMAP`] table below. [`dispatch`] is called once per frame from
//! `app.rs::update()`; nothing else in the app may call
//! `ctx.input_mut().consume_key(...)` for app-level shortcuts.
//!
//! ## Adding a new hotkey
//!
//! 1. Add an [`AppCommand`](crate::command::AppCommand) variant if no
//!    existing one fits.
//! 2. Make sure [`is_enabled`](crate::command::is_enabled) returns the
//!    right thing for it.
//! 3. Append a [`Binding`] row to [`KEYMAP`] below with the chord and
//!    the context tags that must all be present.
//!
//! ## Why bindings carry context tags, not pane enums
//!
//! Using `&'static str` tags keeps the keymap table inspectable and
//! lets future overlays and plugins introduce new contexts without
//! changing this module's type signatures. See the `KeyContext`
//! constants on [`crate::focus::KeyContext`] for the canonical set.

use egui::{Key, Modifiers};

use crate::app::AppState;
use crate::command::{self, AppCommand, SplitDirection};
use crate::focus::{FocusState, KeyContext};
use crate::overlay::Overlay;

/// One row of [`KEYMAP`]. A chord fires when:
/// 1. every tag in `when_context` is present on `focus.context`, **and**
/// 2. `(command)()` is reported as enabled by [`command::is_enabled`].
///
/// `consume_key` is called before the enablement check, so a chord whose
/// context matches always eats the keystroke even if the command is
/// disabled — this prevents disabled hotkeys from falling through to
/// the terminal and causing surprising side effects (the legacy
/// pre-Stage-4 behavior was the same).
#[derive(Debug)]
pub struct Binding {
    pub chord: (Modifiers, Key),
    pub when_context: &'static [&'static str],
    pub command: fn() -> AppCommand,
}

/// The full app-level keymap. Order is not significant: each binding
/// matches independently. Two bindings sharing a chord but with
/// disjoint contexts is the intended way to overload a shortcut by
/// pane — e.g. when sequence-editor keys land, an unmodified letter
/// will fire in `Pane:Viewer` and pass through (no binding) in
/// `Pane:Terminal`. Cmd-modified chords stay workspace-scoped by
/// convention (Zed, VS Code, JetBrains).
pub const KEYMAP: &[Binding] = &[
    // ── Workspace-wide ──────────────────────────────────────────────
    // Cmd-letter chords are app-level operations. is_enabled gates
    // availability (e.g. ⌘F is a no-op when no doc is open).
    Binding {
        chord: (Modifiers::COMMAND, Key::O),
        when_context: &[KeyContext::WORKSPACE],
        command: || AppCommand::PromptOpenFile,
    },
    Binding {
        chord: (Modifiers::COMMAND, Key::W),
        when_context: &[KeyContext::WORKSPACE],
        command: || AppCommand::CloseDoc,
    },
    Binding {
        chord: (Modifiers::COMMAND, Key::F),
        when_context: &[KeyContext::WORKSPACE],
        command: || AppCommand::OpenFind,
    },
    Binding {
        chord: (Modifiers::COMMAND, Key::G),
        when_context: &[KeyContext::WORKSPACE],
        command: || AppCommand::OpenGoTo,
    },
    Binding {
        chord: (Modifiers::COMMAND, Key::E),
        when_context: &[KeyContext::WORKSPACE],
        command: || AppCommand::OpenEnzymes,
    },
    // ── Tab cycling ────────────────────────────────────────────────
    // Cmd+Shift+] / [ matches the macOS browser / VSCode convention.
    // is_enabled returns false when the active pane has fewer than two
    // tabs, so the chord becomes a no-op (but still consumed) in the
    // single-tab case — same pattern as ⌘F when no doc is open.
    Binding {
        chord: (Modifiers::COMMAND.plus(Modifiers::SHIFT), Key::CloseBracket),
        when_context: &[KeyContext::WORKSPACE],
        command: || AppCommand::NextTab,
    },
    Binding {
        chord: (Modifiers::COMMAND.plus(Modifiers::SHIFT), Key::OpenBracket),
        when_context: &[KeyContext::WORKSPACE],
        command: || AppCommand::PrevTab,
    },
    // ── Pane split / nav (Stage 2.5c) ──────────────────────────────
    // ⌘\ splits the active viewer pane horizontally (Zed convention).
    // ⌘1..⌘9 focuses the Nth viewer pane in pane_order. Out-of-range
    // indices are a no-op (the chord is still consumed so it doesn't
    // leak into the terminal).
    Binding {
        chord: (Modifiers::COMMAND, Key::Backslash),
        when_context: &[KeyContext::WORKSPACE],
        command: || AppCommand::SplitPane {
            direction: SplitDirection::Horizontal,
        },
    },
    Binding {
        chord: (Modifiers::COMMAND, Key::Num1),
        when_context: &[KeyContext::WORKSPACE],
        command: || AppCommand::FocusPaneByIndex(1),
    },
    Binding {
        chord: (Modifiers::COMMAND, Key::Num2),
        when_context: &[KeyContext::WORKSPACE],
        command: || AppCommand::FocusPaneByIndex(2),
    },
    Binding {
        chord: (Modifiers::COMMAND, Key::Num3),
        when_context: &[KeyContext::WORKSPACE],
        command: || AppCommand::FocusPaneByIndex(3),
    },
    Binding {
        chord: (Modifiers::COMMAND, Key::Num4),
        when_context: &[KeyContext::WORKSPACE],
        command: || AppCommand::FocusPaneByIndex(4),
    },
    Binding {
        chord: (Modifiers::COMMAND, Key::Num5),
        when_context: &[KeyContext::WORKSPACE],
        command: || AppCommand::FocusPaneByIndex(5),
    },
    Binding {
        chord: (Modifiers::COMMAND, Key::Num6),
        when_context: &[KeyContext::WORKSPACE],
        command: || AppCommand::FocusPaneByIndex(6),
    },
    Binding {
        chord: (Modifiers::COMMAND, Key::Num7),
        when_context: &[KeyContext::WORKSPACE],
        command: || AppCommand::FocusPaneByIndex(7),
    },
    Binding {
        chord: (Modifiers::COMMAND, Key::Num8),
        when_context: &[KeyContext::WORKSPACE],
        command: || AppCommand::FocusPaneByIndex(8),
    },
    Binding {
        chord: (Modifiers::COMMAND, Key::Num9),
        when_context: &[KeyContext::WORKSPACE],
        command: || AppCommand::FocusPaneByIndex(9),
    },
    // ── Overlay-scoped ──────────────────────────────────────────────
    // Escape dismisses the topmost overlay regardless of which widget
    // (terminal, viewer, bar text field) has egui focus. Gated on the
    // generic `Overlay` tag, which `OverlayStack::context_tags` emits
    // whenever any overlay is on the stack — so Escape passes through
    // to the terminal as usual when no overlay is open.
    Binding {
        chord: (Modifiers::NONE, Key::Escape),
        when_context: &[Overlay::TAG_ACTIVE],
        command: || AppCommand::DismissOverlay,
    },
];

/// Run the keymap once. Returns every command whose chord fired this
/// frame. The frame lifecycle (`app.rs::update`) appends these to
/// `pending_commands` immediately after the socket drain.
pub fn dispatch(focus: &FocusState, state: &AppState, ctx: &egui::Context) -> Vec<AppCommand> {
    let mut out = Vec::new();
    ctx.input_mut(|i| {
        // ── User keybinding overrides (consulted first) ────────────────
        // Any chord listed in `keybindings.toml` wins over the built-in
        // KEYMAP. Skipped when any overlay is active so that overlays
        // (Find bar, GoTo bar, file dialog) remain the rightful owners
        // of their keystrokes — the override file carries no context tag
        // and would otherwise fire unconditionally.
        let ws_ok = focus.context.contains(KeyContext::WORKSPACE)
            && !focus.context.contains(Overlay::TAG_ACTIVE);
        if ws_ok {
            for (mods, key, action) in state.config.keybindings.entries.iter() {
                if i.consume_key(*mods, *key) {
                    let cmd = action.to_command();
                    if command::is_enabled(&cmd, state) {
                        out.push(cmd);
                    }
                }
            }
        }

        for b in KEYMAP.iter() {
            // Context gate first — cheapest filter, and skipping it
            // means we do *not* call `consume_key`, so a chord that
            // doesn't apply to the current pane falls through to
            // whoever is listening (e.g. the terminal).
            let ctx_ok = b.when_context.iter().all(|tag| focus.context.contains(tag));
            if !ctx_ok {
                continue;
            }
            // Consume *before* the enablement check so a context-matched
            // but state-disabled chord still eats the key. Preserves the
            // pre-refactor behavior and avoids leaking disabled shortcuts
            // through to background panes.
            if !i.consume_key(b.chord.0, b.chord.1) {
                continue;
            }
            let cmd = (b.command)();
            if command::is_enabled(&cmd, state) {
                out.push(cmd);
            }
        }
    });
    out
}
