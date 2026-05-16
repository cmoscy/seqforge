//! App-level event bus.
//!
//! See [`docs/focus-refactor.md`](../../../docs/focus-refactor.md) §2.3.
//!
//! [`EventSink`] wraps a `Sender<AppEvent>`; `app::update()` drains the
//! matching receiver into a bounded [`EventLog`] each frame.
//! [`crate::command::apply`] emits through `EventSink::emit` and does
//! not care who is listening — subscribers attach on their own.
//!
//! There is currently a single consumer (the log drainer); when a
//! second one appears (future panels or plugins), swap the single
//! channel for `tokio::sync::broadcast` or hand-rolled fan-out at this
//! one point. Callers do not change.

use std::collections::VecDeque;
use std::sync::mpsc;

use seqforge_core::{Selection, ViewId};

use crate::focus::FocusScope;

/// Soft cap on the in-memory event log. Older entries are dropped.
pub const EVENT_LOG_CAP: usize = 100;

/// Broadcast after `command::apply` mutates state.
///
/// Variants are coarse — one event per user-visible state change, not
/// one per field touched. Subscribers re-read `AppState` for full
/// detail; events are notifications, not payloads.
///
/// Field-level `#[allow(dead_code)]` is intentional: emitters populate
/// payloads now so subscribers (future panels, plugins) have the
/// detail when they land. Removing the data and adding it back later
/// would touch every emit site.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub enum AppEvent {
    DocOpened { name: String, len: usize },
    DocClosed,
    SelectionChanged { selection: Option<Selection> },
    SearchCompleted { hits: usize },
    FocusChanged(FocusScope),
    /// Active tab changed (`SwitchTab` / `NextTab` / `PrevTab`).
    TabSwitched { view: ViewId },
    /// A tab was closed. When this is the last view referencing its
    /// buffer, `DocClosed` is also emitted (issued by `apply_close`).
    TabClosed { view: ViewId },
    /// An overlay (Find bar, GoTo bar, CLI status, future modals)
    /// became active. Tag is a `&'static str` identifier; Stage 5
    /// formalises these as named constants on `OverlayStack`.
    OverlayPushed(&'static str),
    /// An overlay was dismissed.
    OverlayPopped(&'static str),
}

#[allow(dead_code)] // Read by future status / debug panels.
impl AppEvent {
    /// Short one-line label for status-bar display. Format is chosen
    /// to stay readable when the bar is narrow.
    pub fn short_label(&self) -> String {
        match self {
            AppEvent::DocOpened { name, len } => format!("opened {name} ({len} bp)"),
            AppEvent::DocClosed => "closed".to_owned(),
            AppEvent::SelectionChanged { selection: Some(sel) } if sel.is_cursor() => {
                format!("cursor @ {}", sel.anchor + 1)
            }
            AppEvent::SelectionChanged { selection: Some(sel) } => {
                let (s, e) = sel.ordered();
                format!("sel {s}–{e}")
            }
            AppEvent::SelectionChanged { selection: None } => "selection cleared".to_owned(),
            AppEvent::SearchCompleted { hits } => format!("found {hits}"),
            AppEvent::FocusChanged(scope) => format!("focus → {scope:?}"),
            AppEvent::TabSwitched { view } => format!("switched → {view}"),
            AppEvent::TabClosed { view } => format!("closed tab {view}"),
            AppEvent::OverlayPushed(tag) => format!("overlay+ {tag}"),
            AppEvent::OverlayPopped(tag) => format!("overlay− {tag}"),
        }
    }
}

// ── Sink (producer side) ──────────────────────────────────────────────────────

/// The producer half of the event bus. Held by `AppState`; passed
/// implicitly to `apply` via `state.events.emit(...)`.
#[derive(Debug)]
pub struct EventSink {
    tx: mpsc::Sender<AppEvent>,
}

impl EventSink {
    /// Construct a new sink and its paired receiver. Caller stores the
    /// receiver alongside (`AppState::event_rx`).
    pub fn channel() -> (Self, mpsc::Receiver<AppEvent>) {
        let (tx, rx) = mpsc::channel();
        (Self { tx }, rx)
    }

    /// Send an event. Failures are ignored: emit is best-effort, and
    /// the only path to failure is the receiver being dropped, which
    /// only happens at shutdown.
    pub fn emit(&self, event: AppEvent) {
        let _ = self.tx.send(event);
    }
}

impl Default for EventSink {
    /// Convenience for `AppState::default()`. The matching receiver is
    /// returned; callers that need it should use [`EventSink::channel`]
    /// directly. This default *drops* its receiver immediately, so
    /// emitted events go nowhere — fine for tests and for the brief
    /// window before `SeqForgeApp::new` swaps in a real channel.
    fn default() -> Self {
        let (sink, _drop_rx) = Self::channel();
        sink
    }
}

// ── Log (single-frame consumer side) ──────────────────────────────────────────

/// Bounded ring of recent events. Drained from the receiver each
/// frame; consumed by the status bar (and eventually by future
/// debug/inspector panels).
#[derive(Debug, Default)]
pub struct EventLog {
    entries: VecDeque<AppEvent>,
}

#[allow(dead_code)] // `latest` is the future-panel read surface.
impl EventLog {
    pub fn push(&mut self, event: AppEvent) {
        if self.entries.len() == EVENT_LOG_CAP {
            self.entries.pop_front();
        }
        self.entries.push_back(event);
    }

    pub fn latest(&self) -> Option<&AppEvent> {
        self.entries.back()
    }

    /// Drain a receiver into this log. Non-blocking.
    pub fn drain_from(&mut self, rx: &mpsc::Receiver<AppEvent>) {
        while let Ok(ev) = rx.try_recv() {
            self.push(ev);
        }
    }
}
