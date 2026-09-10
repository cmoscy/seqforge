//! The seam between headless dispatch and whatever is driving it.
//!
//! Command handlers in this crate are pure with respect to presentation: they
//! mutate a [`Workspace`](crate::Workspace) and nothing else. The two things
//! they still need from their caller — telling the user something happened, and
//! reaching the clipboard — differ per shell, so they arrive through this trait
//! rather than through a concrete GUI type.
//!
//! `seqforge-app` implements it with toasts and the OS pasteboard;
//! a headless caller implements it with stderr and an in-process buffer.

use seqforge_core::SeqSlice;

/// Severity of a [`Host::notify`] message. Mirrors the GUI's toast levels; a
/// headless host is free to collapse these onto stderr or drop them entirely.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Info,
    Success,
    Warning,
    Error,
}

/// What a command handler needs from its shell.
pub trait Host {
    /// Surface a message to the user. Advisory: dropping it must never change
    /// the outcome of the command that emitted it.
    fn notify(&mut self, level: Level, msg: String);

    /// Publish a copied slice. The GUI additionally mirrors the bases onto the
    /// OS pasteboard; a headless host need only remember it.
    fn clipboard_set(&mut self, slice: SeqSlice);

    /// What a paste should insert, reconciling against the OS pasteboard first
    /// where there is one. `None` when the clipboard is empty.
    fn clipboard_get(&mut self) -> Option<SeqSlice>;
}

/// A `Host` that discards notifications and keeps the clipboard in memory.
///
/// This is what makes the dispatch layer testable without a renderer, and it is
/// the starting point for the CLI's own host.
#[derive(Debug, Default)]
pub struct NullHost {
    clipboard: Option<SeqSlice>,
    /// Every notification this host was handed, in order — so a test can assert
    /// on user-facing messaging without a GUI.
    pub notices: Vec<(Level, String)>,
}

impl Host for NullHost {
    fn notify(&mut self, level: Level, msg: String) {
        self.notices.push((level, msg));
    }

    fn clipboard_set(&mut self, slice: SeqSlice) {
        self.clipboard = Some(slice);
    }

    fn clipboard_get(&mut self) -> Option<SeqSlice> {
        self.clipboard.clone()
    }
}
