//! `Span` — a circular-native contiguous-region abstraction (`plans/span.md`).
//!
//! A [`Span`] is a run of bases on a molecule of length `L` that may **wrap the
//! origin** on a circular molecule. It centralizes wrap-awareness in one place:
//! a wrapping selection and an origin-spanning feature are the same shape, and
//! both are a `Span`. The 1-or-2 linear runs a span occupies (the
//! render/highlight/copy primitive) come from [`Span::linear_pieces`].
//!
//! A `Span` is a pure geometric value and does **not** store `L` (decision 8:
//! derived-not-stored). Wrap-dependent methods take `len_total`; the owning
//! `Feature` / `Selection` / `Primer` always has the `Buffer` length in hand.

use std::ops::Range;

use serde::{Deserialize, Serialize};

/// Contiguous region on a molecule of length `L`, possibly wrapping the origin.
///
/// `start ∈ 0..L`, `len ∈ 0..=L`; the span covers `start, start+1, …,
/// start+len-1` taken **mod L**. `len == 0` is empty; `len == L` is the whole
/// molecule. `start + len > L` means it wraps.
///
/// Chosen over `start..end` (which can't tell empty from full-circle when
/// `start == end`) and over an enum split (the wrap split is a rendering
/// projection — [`Span::linear_pieces`] — not the identity).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Span {
    pub start: usize,
    pub len: usize,
}

/// The 1-or-2 linear half-open runs a [`Span`] occupies, in 5'→3' order — the
/// render / highlight / copy primitive. Alloc-free (this is produced in hot
/// per-feature-per-block render loops, and `smallvec` is not a dependency).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pieces {
    /// Empty span.
    None,
    /// One non-wrapping run.
    One(Range<usize>),
    /// A wrapping span: the head `[start..L)` then the tail `[0..t)`.
    Two(Range<usize>, Range<usize>),
}

impl Pieces {
    /// Iterate the runs in 5'→3' order (0, 1, or 2 of them).
    pub fn iter(&self) -> impl Iterator<Item = Range<usize>> + '_ {
        let (a, b) = match self {
            Pieces::None => (None, None),
            Pieces::One(r) => (Some(r.clone()), None),
            Pieces::Two(r1, r2) => (Some(r1.clone()), Some(r2.clone())),
        };
        a.into_iter().chain(b)
    }

    /// Number of runs (0, 1, or 2).
    pub fn count(&self) -> usize {
        match self {
            Pieces::None => 0,
            Pieces::One(_) => 1,
            Pieces::Two(_, _) => 2,
        }
    }

    pub fn is_empty(&self) -> bool {
        matches!(self, Pieces::None)
    }
}

impl Span {
    pub fn new(start: usize, len: usize) -> Self {
        Span { start, len }
    }

    /// From a non-wrapping half-open range (`len = end - start`).
    pub fn from_range(r: Range<usize>) -> Self {
        Span {
            start: r.start,
            len: r.end.saturating_sub(r.start),
        }
    }

    /// The empty span anchored at `start`.
    pub fn empty(start: usize) -> Self {
        Span { start, len: 0 }
    }

    /// The whole molecule.
    pub fn full(len_total: usize) -> Self {
        Span {
            start: 0,
            len: len_total,
        }
    }

    /// The region swept from `a` to `b` in the increasing (+strand) direction,
    /// mod `L` — the directed constructor backing wrapping selection. `a == b`
    /// yields the **empty** span at `a` (not the full circle).
    pub fn between(a: usize, b: usize, len_total: usize) -> Self {
        if len_total == 0 {
            return Span::empty(a);
        }
        let (a, b) = (a % len_total, b % len_total);
        let len = (b + len_total - a) % len_total; // 0 when a == b
        Span { start: a, len }
    }

    pub fn length(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// True if the span crosses the origin (`start + len > L`).
    pub fn wraps(&self, len_total: usize) -> bool {
        self.start + self.len > len_total
    }

    /// Inter-base end position (exclusive), mod `L`. For a full-circle span
    /// (`len == L`) this equals `start` — callers should test `len == len_total`
    /// first where the distinction matters.
    pub fn end(&self, len_total: usize) -> usize {
        if len_total == 0 {
            return self.start;
        }
        (self.start + self.len) % len_total
    }

    /// Whether base index `pos` falls inside the span (circular).
    pub fn contains(&self, pos: usize, len_total: usize) -> bool {
        if self.len == 0 || len_total == 0 {
            return false;
        }
        // Forward distance from start to pos, mod L, is within len.
        let d = (pos + len_total - self.start % len_total) % len_total;
        d < self.len
    }

    /// The 1-or-2 linear half-open runs this span occupies — the single origin
    /// split used by rendering, highlighting, and copy. This is what both the
    /// main viewer and the minimap derive geometry from, so they can't drift.
    pub fn linear_pieces(&self, len_total: usize) -> Pieces {
        if self.len == 0 || len_total == 0 {
            return Pieces::None;
        }
        let start = self.start % len_total;
        if start + self.len <= len_total {
            return Pieces::One(start..start + self.len);
        }
        let head = len_total - start; // [start..L)
        let tail = self.len - head; // [0..tail)
        if tail == 0 {
            Pieces::One(start..len_total)
        } else {
            Pieces::Two(start..len_total, 0..tail)
        }
    }

    /// Linear bounding box `[min, max)`. **Lossy** for a wrapping span (returns
    /// `0..L`). Explicit so bounds-only consumers (stacking / LOD) opt in rather
    /// than getting bounding-box semantics by default; every other consumer wants
    /// [`Span::linear_pieces`] (lossless) or [`Span::contains`].
    pub fn bounds(&self, len_total: usize) -> Range<usize> {
        match self.linear_pieces(len_total) {
            Pieces::None => self.start..self.start,
            Pieces::One(r) => r,
            Pieces::Two(_, _) => 0..len_total,
        }
    }

    /// Translate the whole span by `delta` (a plain move with no wrap policy —
    /// used when placing/pasting into a destination frame; the caller ensures the
    /// result is valid for that frame).
    pub fn shift(&self, delta: isize) -> Span {
        Span {
            start: (self.start as isize + delta) as usize,
            len: self.len,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_range_round_trips_non_wrapping() {
        let s = Span::from_range(3..9);
        assert_eq!(s, Span::new(3, 6));
        assert_eq!(s.linear_pieces(20), Pieces::One(3..9));
        assert_eq!(s.bounds(20), 3..9);
        assert!(!s.wraps(20));
        assert_eq!(s.end(20), 9);
    }

    #[test]
    fn empty_and_full() {
        assert!(Span::empty(4).is_empty());
        assert_eq!(Span::empty(4).linear_pieces(20), Pieces::None);
        let full = Span::full(20);
        assert_eq!(full.length(), 20);
        // Full circle anchored at 0 is a single run.
        assert_eq!(full.linear_pieces(20), Pieces::One(0..20));
        assert!(!full.wraps(20)); // 0 + 20 == 20, not > 20
    }

    #[test]
    fn between_forward_wrapping_and_degenerate() {
        // forward, no wrap
        assert_eq!(Span::between(3, 9, 20), Span::new(3, 6));
        // wraps the origin: 16 → 4 forward is len (20-16)+4 = 8
        let w = Span::between(16, 4, 20);
        assert_eq!(w, Span::new(16, 8));
        assert!(w.wraps(20));
        // a == b → empty (not full circle)
        assert_eq!(Span::between(7, 7, 20), Span::empty(7));
    }

    #[test]
    fn wraps_predicate() {
        assert!(!Span::new(0, 20).wraps(20));
        assert!(!Span::new(5, 10).wraps(20));
        assert!(Span::new(16, 8).wraps(20));
        assert!(Span::new(19, 2).wraps(20));
    }

    #[test]
    fn contains_non_wrapping() {
        let s = Span::new(5, 5); // covers 5,6,7,8,9
        assert!(!s.contains(4, 20));
        assert!(s.contains(5, 20));
        assert!(s.contains(9, 20));
        assert!(!s.contains(10, 20));
    }

    #[test]
    fn contains_wrapping_both_arms() {
        let s = Span::new(16, 8); // covers 16..20 and 0..4
        assert!(s.contains(16, 20));
        assert!(s.contains(19, 20));
        assert!(s.contains(0, 20));
        assert!(s.contains(3, 20));
        assert!(!s.contains(4, 20));
        assert!(!s.contains(15, 20));
    }

    #[test]
    fn linear_pieces_wrapping() {
        let s = Span::new(16, 8);
        assert_eq!(s.linear_pieces(20), Pieces::Two(16..20, 0..4));
        // iterate in 5'→3' order
        let runs: Vec<_> = s.linear_pieces(20).iter().collect();
        assert_eq!(runs, vec![16..20, 0..4]);
        assert_eq!(s.linear_pieces(20).count(), 2);
    }

    #[test]
    fn linear_pieces_full_circle_offset_is_two_runs() {
        // len == L but anchored off-origin → covers the whole circle as two runs.
        let s = Span::new(5, 20);
        assert_eq!(s.linear_pieces(20), Pieces::Two(5..20, 0..5));
        assert_eq!(s.bounds(20), 0..20);
    }

    #[test]
    fn hull_is_lossy_on_wrap() {
        assert_eq!(Span::new(5, 5).bounds(20), 5..10); // non-wrap: exact
        assert_eq!(Span::new(16, 8).bounds(20), 0..20); // wrap: lossy whole molecule
    }

    #[test]
    fn end_positions() {
        assert_eq!(Span::new(5, 5).end(20), 10);
        assert_eq!(Span::new(16, 8).end(20), 4); // wraps
        assert_eq!(Span::full(20).end(20), 0); // full circle → start again
    }

    #[test]
    fn shift_translates_start_only() {
        assert_eq!(Span::new(3, 6).shift(10), Span::new(13, 6));
        assert_eq!(Span::new(13, 6).shift(-10), Span::new(3, 6));
    }
}
