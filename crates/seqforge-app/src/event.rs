//! App-level event bus.
//!
//! See [`docs/focus-refactor.md`](../../../docs/focus-refactor.md) §2.3.
//!
//! Stage 2: type definitions and a no-op sink. The sink drops events on
//! the floor for now so `apply()` can call `emit()` at the right places
//! without callers having to care yet. Stage 3 swaps the no-op for a
//! real broadcast channel and adds the status-bar subscriber.

use crate::focus::FocusScope;

/// Broadcast after `command::apply` finishes mutating state.
///
/// Variants are deliberately coarse — one event per user-visible state
/// change, not one event per field touched. Subscribers re-read state
/// from `AppState` for full detail; events are notifications, not
/// payloads.
#[allow(dead_code)] // Stage 3 adds real subscribers.
#[derive(Debug, Clone)]
pub enum AppEvent {
    DocOpened { name: String, len: usize },
    DocClosed,
    SearchCompleted { hits: usize },
    FocusChanged(FocusScope),
    OverlayOpened(&'static str),
    OverlayClosed(&'static str),
}

/// No-op event sink. Stage 3 replaces this with a `crossbeam` channel
/// and an in-memory `EventLog` consumed by the status bar.
#[derive(Debug, Default)]
pub struct EventSink {
    _private: (),
}

#[allow(dead_code)] // Stage 3 wires the first real caller (apply emits).
impl EventSink {
    pub fn new() -> Self {
        Self::default()
    }

    /// Stage 2: no-op. Stage 3: broadcast to subscribers.
    pub fn emit(&self, _event: AppEvent) {
        // Intentionally empty until Stage 3.
    }
}
