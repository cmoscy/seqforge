//! Tiny version-keyed cache helper — Stage 2.5e.
//!
//! Immediate-mode UIs (egui) re-describe the entire widget tree every
//! frame. Anything expensive to compute needs an explicit cache plus
//! an invalidation key. As features land (alignment overlays, primer
//! scores, mutation tracks, edit-time renderings) each adds its own
//! derived data; we want a single idiomatic pattern so cache logic
//! doesn't drift into ten ad-hoc variants.
//!
//! ## Pattern
//!
//! ```ignore
//! let stacked = cache.get_or_compute(
//!     (buffer_id, buffer.version, char_width),  // key
//!     || expensive_stack(&annotations.features), // value producer
//! );
//! ```
//!
//! Returns `&V`. The key types must be `Eq + Clone`. Re-runs the
//! producer iff the key differs from the last computed key.

#![allow(dead_code)]

/// Single-entry cache keyed by an arbitrary `Eq + Clone` value.
/// Holds at most one (key, value) pair; computing with a new key
/// drops the previous.
#[derive(Debug)]
pub struct Cache<K, V>
where
    K: Eq + Clone,
{
    state: Option<(K, V)>,
}

impl<K, V> Default for Cache<K, V>
where
    K: Eq + Clone,
{
    fn default() -> Self {
        Self { state: None }
    }
}

impl<K, V> Cache<K, V>
where
    K: Eq + Clone,
{
    pub fn new() -> Self {
        Self { state: None }
    }

    /// Return the cached value if `key` matches the cached key; else
    /// run `compute`, store, and return a reference. `compute` runs
    /// at most once per distinct key.
    pub fn get_or_compute<F>(&mut self, key: K, compute: F) -> &V
    where
        F: FnOnce() -> V,
    {
        let needs_recompute = match &self.state {
            Some((k, _)) => *k != key,
            None => true,
        };
        if needs_recompute {
            let value = compute();
            self.state = Some((key, value));
        }
        &self.state.as_ref().expect("just set").1
    }

    /// Drop the cached entry. Useful when the producer's inputs go
    /// out of scope (e.g. on doc close) and you want to free the
    /// memory eagerly rather than waiting for the next key change.
    pub fn invalidate(&mut self) {
        self.state = None;
    }

    /// Peek without recomputing. Returns `None` if empty.
    pub fn peek(&self) -> Option<&V> {
        self.state.as_ref().map(|(_, v)| v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn cache_hit_does_not_recompute() {
        let mut c: Cache<u32, String> = Cache::new();
        let calls = Cell::new(0);
        let a = c.get_or_compute(1, || {
            calls.set(calls.get() + 1);
            "one".to_string()
        });
        assert_eq!(a, "one");
        let b = c.get_or_compute(1, || {
            calls.set(calls.get() + 1);
            "should not run".to_string()
        });
        assert_eq!(b, "one");
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn cache_miss_recomputes() {
        let mut c: Cache<u32, String> = Cache::new();
        c.get_or_compute(1, || "one".to_string());
        let b = c.get_or_compute(2, || "two".to_string());
        assert_eq!(b, "two");
    }

    #[test]
    fn invalidate_clears() {
        let mut c: Cache<u32, String> = Cache::new();
        c.get_or_compute(1, || "one".to_string());
        c.invalidate();
        assert!(c.peek().is_none());
    }
}
