//! Keyboard focus state and key-context stack.
//!
//! See [`docs/focus-refactor.md`](../../../docs/focus-refactor.md) for the
//! full architecture rationale. This module is Stage 1 of that refactor:
//! it introduces the focus types and pane-scope tracking, but does **not**
//! yet drive keymap dispatch (that lands in Stage 4).
//!
//! State flows *outward*: the app sets `FocusState` from explicit signals
//! (pane clicks, programmatic `FocusPane` commands), and widgets read it.
//! Widgets must not probe `egui::Memory` to reconstruct focus after the
//! fact — that is the anti-pattern this refactor exists to remove.

use serde::{Deserialize, Serialize};

/// Which pane "owns" the keyboard when no overlay is active.
///
/// Sticky: set by pane clicks and by explicit `AppCommand::FocusPane`
/// (the latter lands in Stage 2). Not persisted across restarts —
/// startup always begins on [`FocusScope::Terminal`] to preserve the
/// pre-refactor behaviour where the terminal eagerly captures input.
#[derive(Copy, Clone, Eq, PartialEq, Debug, Default, Serialize, Deserialize)]
pub enum FocusScope {
    Viewer,
    #[default]
    Terminal,
    Browser,
}

impl FocusScope {
    /// The `KeyContext` tag pushed onto the stack when this scope is active.
    pub fn pane_tag(self) -> &'static str {
        match self {
            FocusScope::Viewer => KeyContext::PANE_VIEWER,
            FocusScope::Terminal => KeyContext::PANE_TERMINAL,
            FocusScope::Browser => KeyContext::PANE_BROWSER,
        }
    }
}

/// Stack of `&'static str` tags describing the current input situation.
///
/// Top of stack is the innermost active context. Keymap `Binding`s
/// (Stage 4) match against this stack via `when_context` predicates.
///
/// Example stack while a Find bar is open over the Viewer pane:
/// `["Workspace", "Pane:Viewer", "Overlay:FindBar", "TextInput"]`.
///
/// Tags are `&'static str` for cheap comparison and to keep the set
/// inspectable. Plugin-defined tags (future) will use a
/// `"plugin:<id>:<tag>"` namespace; see §7 of the refactor doc.
///
/// Several methods are unused in Stage 1 — they're scaffolding for
/// Stage 4 (keymap dispatcher reads `contains`/`tags`) and Stage 5
/// (overlay stack uses `pop` and `TEXT_INPUT`).
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct KeyContext {
    stack: Vec<&'static str>,
}

#[allow(dead_code)]
impl KeyContext {
    pub const WORKSPACE: &'static str = "Workspace";
    pub const PANE_VIEWER: &'static str = "Pane:Viewer";
    pub const PANE_TERMINAL: &'static str = "Pane:Terminal";
    pub const PANE_BROWSER: &'static str = "Pane:Browser";
    pub const TEXT_INPUT: &'static str = "TextInput";

    pub fn new() -> Self {
        Self { stack: vec![Self::WORKSPACE] }
    }

    pub fn push(&mut self, tag: &'static str) {
        self.stack.push(tag);
    }

    pub fn pop(&mut self) -> Option<&'static str> {
        // Never pop the root Workspace tag.
        if self.stack.len() > 1 {
            self.stack.pop()
        } else {
            None
        }
    }

    pub fn contains(&self, tag: &'static str) -> bool {
        self.stack.contains(&tag)
    }

    pub fn tags(&self) -> impl Iterator<Item = &&'static str> {
        self.stack.iter()
    }

    /// Reset to just `["Workspace"]`. Used by `FocusState` when rebuilding
    /// the base context after a scope change.
    pub fn clear_to_workspace(&mut self) {
        self.stack.clear();
        self.stack.push(Self::WORKSPACE);
    }
}

impl Default for KeyContext {
    fn default() -> Self {
        Self::new()
    }
}

/// Combined focus state held in `AppState`.
///
/// Stage 1: only `scope` is wired (set by pane clicks). The `context`
/// stack is maintained alongside but no consumer reads it yet — Stage 4's
/// keymap dispatcher will be the first reader, and Stage 5's overlay
/// stack will be the first producer of non-pane tags.
#[derive(Debug, Clone)]
pub struct FocusState {
    pub scope: FocusScope,
    pub context: KeyContext,
}

impl FocusState {
    pub fn new() -> Self {
        let mut s = Self { scope: FocusScope::default(), context: KeyContext::new() };
        s.rebuild_base_context();
        s
    }

    /// Sets the active pane. No-op if `scope` is unchanged. Rebuilds the
    /// base context tags; any overlay tags layered on top will be pushed
    /// fresh each frame by the overlay stack (Stage 5).
    pub fn set_scope(&mut self, scope: FocusScope) {
        if self.scope == scope {
            return;
        }
        self.scope = scope;
        self.rebuild_base_context();
    }

    fn rebuild_base_context(&mut self) {
        self.context.clear_to_workspace();
        self.context.push(self.scope.pane_tag());
    }
}

impl Default for FocusState {
    fn default() -> Self {
        Self::new()
    }
}
