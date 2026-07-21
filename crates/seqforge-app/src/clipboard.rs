//! OS-authoritative clipboard with a rich in-process cache.
//!
//! ## Model
//!
//! - **Copy/cut** write a [`SeqSlice`] into [`ClipboardState`] and mirror the
//!   bases as plain text onto the system clipboard (`arboard`).
//! - **Paste / enablement / preview** call [`sync_from_os`] (or
//!   [`sync_with_plain_hint`]) so the cache reflects the *current* OS clipboard.
//! - The rich annotated slice is kept only while pasteboard **generation**
//!   still matches the value recorded after our last write. An external copy
//!   bumps generation → cache becomes bytes-only from the new OS text.
//!
//! Headless unit tests use `memory_only` (default under `cfg(test)`): no OS I/O,
//! so existing tests that fill the cache directly keep working.

use seqforge_core::SeqSlice;

use crate::app::AppState;

/// IUPAC nucleotide alphabet (DNA + ambiguity codes). Shared by the silent
/// GUI filter and the strict CLI/agent parser.
pub const IUPAC: &[u8] = b"ACGTURYSWKMBDHVN";

/// Keep only IUPAC codes, upper-cased; drop everything else (whitespace, junk).
/// Used for typed bases and plain-text OS paste.
pub fn filter_bases(s: &str) -> String {
    s.chars()
        .filter_map(|c| {
            let u = c.to_ascii_uppercase();
            (u.is_ascii() && IUPAC.contains(&(u as u8))).then_some(u)
        })
        .collect()
}

/// Uppercase, strip ASCII whitespace, validate IUPAC. Returns the clean bytes
/// or an error naming the first offending character (CLI/agent path).
pub fn parse_bases(s: &str) -> Result<Vec<u8>, seqforge_core::DispatchError> {
    let mut out = Vec::with_capacity(s.len());
    for ch in s.chars() {
        if ch.is_ascii_whitespace() {
            continue;
        }
        let up = ch.to_ascii_uppercase();
        if up.is_ascii() && IUPAC.contains(&(up as u8)) {
            out.push(up as u8);
        } else {
            return Err(seqforge_core::DispatchError::InvalidInput(format!(
                "`{ch}` is not an IUPAC nucleotide code"
            )));
        }
    }
    Ok(out)
}

/// Session clipboard: rich cache + ownership metadata for OS sync.
#[derive(Debug, Clone)]
pub struct ClipboardState {
    /// What paste will insert (annotated when we still own the OS clipboard).
    pub slice: Option<SeqSlice>,
    /// Pasteboard generation recorded after our last successful OS write.
    owned_gen: Option<u64>,
    /// Plain text we last wrote (Linux ownership fallback; debugging).
    last_text: Option<String>,
    /// Last OS generation we reconciled against (skips redundant reads).
    last_seen_gen: Option<u64>,
    /// When true, never touch the OS clipboard (tests / degraded headless).
    memory_only: bool,
}

#[allow(clippy::derivable_impls)] // `memory_only: cfg!(test)` is not derivable
impl Default for ClipboardState {
    fn default() -> Self {
        Self {
            slice: None,
            owned_gen: None,
            last_text: None,
            last_seen_gen: None,
            // Unit tests construct `AppState::default()` without a display;
            // keep them memory-only. The GUI opts into OS I/O via [`for_gui`].
            memory_only: cfg!(test),
        }
    }
}

impl ClipboardState {
    /// GUI session: mirror to / sync from the system clipboard.
    pub fn for_gui() -> Self {
        Self {
            slice: None,
            owned_gen: None,
            last_text: None,
            last_seen_gen: None,
            memory_only: false,
        }
    }

    pub fn bytes(&self) -> Option<&[u8]> {
        self.slice.as_ref().map(|s| s.bytes())
    }

    pub fn is_empty(&self) -> bool {
        self.slice.as_ref().is_none_or(|c| c.is_empty())
    }

    /// Test / headless: force memory-only (or re-enable OS for ownership tests).
    #[cfg(test)]
    pub fn set_memory_only(&mut self, memory_only: bool) {
        self.memory_only = memory_only;
    }

    #[cfg(test)]
    pub fn memory_only(&self) -> bool {
        self.memory_only
    }
}

/// Write a slice into the cache and mirror bases onto the OS clipboard.
pub fn set_slice(state: &mut AppState, slice: SeqSlice) {
    set_slice_cache(&mut state.clipboard, slice);
}

/// Write into a [`ClipboardState`] directly (same as [`set_slice`]).
pub fn set_slice_cache(clip: &mut ClipboardState, slice: SeqSlice) {
    let text = String::from_utf8_lossy(&slice.bytes).into_owned();
    clip.slice = Some(slice);
    clip.last_text = Some(text.clone());

    if clip.memory_only {
        clip.owned_gen = Some(0);
        clip.last_seen_gen = Some(0);
        return;
    }

    match os_set_text(&text) {
        Ok(()) => {
            let pb_gen = os_generation();
            clip.owned_gen = pb_gen;
            clip.last_seen_gen = pb_gen;
            #[cfg(test)]
            test_os::note_write(&text, pb_gen);
        }
        Err(_) => {
            // Degrade: keep the rich cache, stop talking to the OS.
            clip.memory_only = true;
            clip.owned_gen = Some(0);
            clip.last_seen_gen = Some(0);
        }
    }
}

/// Reconcile the cache with the system clipboard (no paste-event hint).
pub fn sync_from_os(state: &mut AppState) {
    sync_with_plain_hint(&mut state.clipboard, None);
}

/// Reconcile using optional plain text from `egui::Event::Paste` (avoids an
/// extra OS read on the paste keystroke). Generation still decides rich vs
/// bytes-only.
pub fn sync_with_plain_hint(clip: &mut ClipboardState, plain_hint: Option<&str>) {
    if clip.memory_only {
        return;
    }

    let pb_gen = os_generation();

    if we_still_own(clip, pb_gen, plain_hint) {
        clip.last_seen_gen = pb_gen.or(clip.last_seen_gen);
        return;
    }

    // Already reconciled this foreign generation and have a cache entry.
    if pb_gen.is_some() && clip.last_seen_gen == pb_gen && clip.slice.is_some() {
        return;
    }

    let text = plain_hint
        .map(str::to_owned)
        .or_else(|| os_get_text().ok())
        .unwrap_or_default();

    let filtered = filter_bases(&text);
    clip.slice = if filtered.is_empty() {
        None
    } else {
        Some(SeqSlice {
            bytes: filtered.into_bytes(),
            features: Vec::new(),
            primers: Vec::new(),
        })
    };
    clip.owned_gen = None;
    clip.last_text = None;
    clip.last_seen_gen = pb_gen;
}

fn we_still_own(clip: &ClipboardState, os_gen: Option<u64>, plain_hint: Option<&str>) -> bool {
    let Some(owned) = clip.owned_gen else {
        return false;
    };
    match os_gen {
        Some(cur) => cur == owned,
        None => {
            // No platform generation (e.g. Linux): text-equality fallback.
            let os_text = plain_hint
                .map(str::to_owned)
                .or_else(|| os_get_text().ok())
                .unwrap_or_default();
            clip.last_text.as_deref() == Some(os_text.as_str())
        }
    }
}

// ── OS I/O ────────────────────────────────────────────────────────────────────

fn os_set_text(text: &str) -> Result<(), ()> {
    #[cfg(test)]
    if let Some(r) = test_os::try_set_text(text) {
        return r;
    }
    arboard::Clipboard::new()
        .and_then(|mut c| c.set_text(text.to_owned()))
        .map_err(|_| ())
}

fn os_get_text() -> Result<String, ()> {
    #[cfg(test)]
    if let Some(r) = test_os::try_get_text() {
        return r;
    }
    arboard::Clipboard::new()
        .and_then(|mut c| c.get_text())
        .map_err(|_| ())
}

fn os_generation() -> Option<u64> {
    #[cfg(test)]
    if let Some(g) = test_os::try_generation() {
        return g;
    }
    platform_generation()
}

#[cfg(target_os = "macos")]
fn platform_generation() -> Option<u64> {
    use objc2_app_kit::NSPasteboard;
    // `changeCount` bumps on every write to the general pasteboard.
    Some(NSPasteboard::generalPasteboard().changeCount() as u64)
}

#[cfg(windows)]
fn platform_generation() -> Option<u64> {
    // SAFETY: Win32 clipboard sequence number is a process-wide counter.
    Some(unsafe { windows_sys::Win32::System::DataExchange::GetClipboardSequenceNumber() } as u64)
}

#[cfg(not(any(target_os = "macos", windows)))]
fn platform_generation() -> Option<u64> {
    None
}

// ── Test double (ownership / foreign-copy unit tests) ─────────────────────────

#[cfg(test)]
mod test_os {
    use std::cell::RefCell;

    #[derive(Clone, Default)]
    struct FakeOs {
        text: Option<String>,
        generation: u64,
        /// When false, fall through to real arboard (unused in unit tests).
        active: bool,
    }

    thread_local! {
        static FAKE: RefCell<FakeOs> = const { RefCell::new(FakeOs {
            text: None,
            generation: 0,
            active: false,
        }) };
    }

    pub fn install() {
        FAKE.with(|f| {
            *f.borrow_mut() = FakeOs {
                text: None,
                generation: 1,
                active: true,
            };
        });
    }

    pub fn clear() {
        FAKE.with(|f| {
            *f.borrow_mut() = FakeOs {
                text: None,
                generation: 0,
                active: false,
            };
        });
    }

    /// Simulate an external app writing `text` (bumps generation).
    pub fn foreign_write(text: &str) {
        FAKE.with(|f| {
            let mut g = f.borrow_mut();
            assert!(g.active, "test OS not installed");
            g.generation = g.generation.wrapping_add(1);
            g.text = Some(text.to_owned());
        });
    }

    pub fn note_write(text: &str, pb_gen: Option<u64>) {
        FAKE.with(|f| {
            let mut g = f.borrow_mut();
            if !g.active {
                return;
            }
            g.text = Some(text.to_owned());
            if let Some(pb_gen) = pb_gen {
                g.generation = pb_gen;
            }
        });
    }

    pub fn try_set_text(text: &str) -> Option<Result<(), ()>> {
        FAKE.with(|f| {
            let mut g = f.borrow_mut();
            if !g.active {
                return None;
            }
            g.generation = g.generation.wrapping_add(1);
            g.text = Some(text.to_owned());
            Some(Ok(()))
        })
    }

    pub fn try_get_text() -> Option<Result<String, ()>> {
        FAKE.with(|f| {
            let g = f.borrow();
            if !g.active {
                return None;
            }
            Some(g.text.clone().ok_or(()))
        })
    }

    pub fn try_generation() -> Option<Option<u64>> {
        FAKE.with(|f| {
            let g = f.borrow();
            if !g.active {
                return None;
            }
            Some(Some(g.generation))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use seqforge_core::{Feature, Location, Strand};

    fn rich_slice() -> SeqSlice {
        SeqSlice {
            bytes: b"ATGC".to_vec(),
            features: vec![Feature {
                id: Default::default(),
                location: Location::simple(0..4),
                raw_kind: "misc_feature".into(),
                label: "x".into(),
                strand: Strand::Forward,
                qualifiers: Default::default(),
                lineage: None,
            }],
            primers: Vec::new(),
        }
    }

    fn state_gui() -> AppState {
        let mut s = AppState::default();
        s.clipboard.set_memory_only(false);
        test_os::install();
        s
    }

    #[test]
    fn set_slice_keeps_rich_while_generation_ours() {
        let mut s = state_gui();
        set_slice(&mut s, rich_slice());
        assert_eq!(s.clipboard.slice.as_ref().unwrap().features.len(), 1);

        sync_from_os(&mut s);
        assert_eq!(
            s.clipboard.slice.as_ref().unwrap().features.len(),
            1,
            "unchanged generation keeps annotations"
        );
        test_os::clear();
    }

    #[test]
    fn foreign_copy_replaces_with_bytes_only() {
        let mut s = state_gui();
        set_slice(&mut s, rich_slice());
        test_os::foreign_write("GGCC");

        sync_from_os(&mut s);
        let slice = s.clipboard.slice.as_ref().expect("foreign bases");
        assert_eq!(slice.bytes(), b"GGCC");
        assert!(slice.features.is_empty(), "external paste is bytes-only");
        assert!(slice.primers.is_empty());
        test_os::clear();
    }

    #[test]
    fn identical_text_foreign_copy_still_drops_rich() {
        // Generation bumps even when the sequence text matches — no collision.
        let mut s = state_gui();
        set_slice(&mut s, rich_slice());
        test_os::foreign_write("ATGC");

        sync_from_os(&mut s);
        let slice = s.clipboard.slice.as_ref().unwrap();
        assert_eq!(slice.bytes(), b"ATGC");
        assert!(
            slice.features.is_empty(),
            "same bases from outside must not revive features"
        );
        test_os::clear();
    }

    #[test]
    fn paste_hint_filters_non_iupac() {
        let mut s = state_gui();
        // No prior ownership — hint alone populates the cache.
        sync_with_plain_hint(&mut s.clipboard, Some("at gc\nZZ"));
        assert_eq!(s.clipboard.bytes(), Some(b"ATGC".as_slice()));
        test_os::clear();
    }

    #[test]
    fn memory_only_ignores_os() {
        let mut s = AppState::default();
        assert!(s.clipboard.memory_only());
        s.clipboard.slice = Some(rich_slice());
        test_os::install();
        test_os::foreign_write("AAAA");
        sync_from_os(&mut s);
        assert_eq!(
            s.clipboard.slice.as_ref().unwrap().features.len(),
            1,
            "memory_only must not clobber the cache"
        );
        test_os::clear();
    }

    #[test]
    fn filter_bases_uppercases_and_drops_junk() {
        assert_eq!(filter_bases("atgc"), "ATGC");
        assert_eq!(filter_bases("A1T-G zJ C"), "ATGC");
        assert_eq!(filter_bases("123"), "");
    }

    #[test]
    fn parse_bases_rejects_non_iupac() {
        assert!(parse_bases("ATGC").is_ok());
        assert!(parse_bases("AT X").is_err());
    }
}
