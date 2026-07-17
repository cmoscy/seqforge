//! The annotation-transport primitive — `extract` / `place` / `merge` — shared
//! by copy/paste, PCR, and ligation (ROADMAP decision 23; `plans/feature-model.md`).
//!
//! The operation is always *extract an annotated subsequence, then place it into
//! a destination coordinate frame*; ligation adds *merge* (rejoin split pieces).
//! Deciding the partial-feature policy **once**, here, is what keeps the three
//! consumers from growing three divergent partial-feature bugs.
//!
//! Only **authored, positionally-bound** annotations ride the carrier — features
//! (positional sub-ranges) and primers (by their authored `binding`). Everything
//! *derived* (cut sites, ORFs, translation, QC) is recomputed on the destination
//! (decision 8) and never carried.

use std::ops::Range;

use crate::span::Span;
use crate::{Annotations, Feature, FeatureId, Location, Primer, Provenance, Strand, Topology};

/// The carrier: an annotated subsequence in **local** (0-based, slice-relative)
/// coordinates. The blunt/linear degenerate case of the assembly track's
/// `Fragment` (decision 21) — it carries no ends. Deliberately leaner than a
/// `Document` (no name/topology/source_path).
#[derive(Debug, Clone, Default)]
pub struct SeqSlice {
    pub bytes: Vec<u8>,
    /// Features in local coords; ids are placeholders (re-minted on `place`).
    pub features: Vec<Feature>,
    /// Primers with local `binding`; `sequence` is the verbatim authored oligo.
    pub primers: Vec<Primer>,
}

impl SeqSlice {
    /// The raw bases — the clipboard-duality projection (a biologist still pastes
    /// plain letters into an email).
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    /// Promote the slice to a standalone [`crate::Document`] — used only when a
    /// *new* buffer is materialized (paste-as-new, a PCR product). Ids are the
    /// placeholders; `Annotations::from_parts` re-mints on adoption.
    pub fn into_document(self, name: String, topology: Topology) -> crate::Document {
        crate::Document {
            name,
            sequence: self.bytes,
            topology,
            features: self.features,
            primers: self.primers,
            source_path: None,
        }
    }
}

/// What happens to a feature that straddles the extracted boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PartialPolicy {
    /// Drop straddlers entirely (the Biopython/pydna `record[a:b]` behavior).
    #[default]
    DropPartials,
    /// Clamp a straddler to the range and mark the cut edge fuzzy (`<`/`>`).
    TruncatePartials,
}

/// Orientation a slice is placed in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Orient {
    /// Same orientation (copy/paste, PCR).
    #[default]
    Identity,
    /// Reverse-complemented: mirror coords + flip strands (ligation).
    Rev,
}

// ── extract ─────────────────────────────────────────────────────────────────

/// Extract `span` from `text` + `ann` into a [`SeqSlice`] in local coordinates.
///
/// The [`Span`] is the single wrap encoding — `span.wraps(text.len())` drives
/// origin crossing, so there is no separate `circular` flag or `start > end`
/// convention. The partial-feature policy is decided **here**. Each carried
/// feature/primer is re-homed to slice-local coordinates; provenance is stamped
/// on features so a later [`place`] with `merge` can reunite split pieces by
/// lineage.
///
/// `source_doc` names the origin buffer (for the provenance lineage key).
pub fn extract(
    text: &[u8],
    ann: &Annotations,
    span: Span,
    policy: PartialPolicy,
    source_doc: &str,
) -> SeqSlice {
    let total = text.len();
    let wrap = span.wraps(total);
    // `PosMap` keeps the `start > end` convention internally; derive it from the
    // span (a wrapping span's tail end is `span.end(total) < start`).
    let start = span.start;
    let end = if wrap {
        span.end(total)
    } else {
        span.start + span.len
    };

    // Slice bytes — concatenate the span's 1-or-2 linear runs (`linear_pieces`).
    let mut bytes: Vec<u8> = Vec::with_capacity(span.len);
    for run in span.linear_pieces(total).iter() {
        bytes.extend_from_slice(&text[run.start..run.end.min(total)]);
    }
    let slice_len = bytes.len();

    let map = PosMap {
        start,
        end,
        total,
        wrap,
    };

    let mut features = Vec::new();
    for f in ann.iter() {
        if let Some(mut nf) = localize_feature(f, &map, policy) {
            // Stamp lineage (the merge key) unless the feature already carries
            // one — an existing provenance propagates so multi-hop lineage holds.
            if nf.provenance.is_none() {
                nf.provenance = Some(Provenance {
                    source_doc: source_doc.to_string(),
                    source_range: f.hull(total),
                    operation: "extract".to_string(),
                });
            }
            features.push(nf);
        }
    }

    let mut primers = Vec::new();
    for p in ann.primers() {
        if let Some(np) = localize_primer(p, &map, slice_len) {
            primers.push(np);
        }
    }

    SeqSlice {
        bytes,
        features,
        primers,
    }
}

/// Coordinate mapper from source-template positions to slice-local positions.
struct PosMap {
    start: usize,
    end: usize,
    total: usize,
    wrap: bool,
}

impl PosMap {
    /// Map an inclusive-capable template position into slice coords. Boundary
    /// positions (`== end`/`== total`) map so a half-open range's `end` resolves.
    fn pos(&self, p: usize) -> Option<usize> {
        if !self.wrap {
            (p >= self.start && p <= self.end).then(|| p - self.start)
        } else if p >= self.start && p <= self.total {
            Some(p - self.start)
        } else if p <= self.end {
            Some(self.total - self.start + p)
        } else {
            None
        }
    }

    /// True if `[a, b)` lies wholly inside the extracted window, contiguously
    /// (no crossing the wrap gap).
    fn contains(&self, a: usize, b: usize) -> bool {
        match (self.pos(a), self.pos(b)) {
            (Some(la), Some(lb)) => lb >= la && lb - la == b - a,
            _ => false,
        }
    }

    /// Does `[a, b)` overlap the extracted window at all? (Non-wrap only — used
    /// for the truncate straddle decision.)
    fn overlaps_linear(&self, a: usize, b: usize) -> bool {
        !self.wrap && a < self.end && b > self.start
    }
}

/// Re-home one feature into slice coords per `policy`, or `None` if it doesn't
/// survive. Containment is decided on the feature's **span** (the doc's
/// trisection); a contained feature keeps its full segmentation.
fn localize_feature(f: &Feature, map: &PosMap, policy: PartialPolicy) -> Option<Feature> {
    let span = f.hull(map.total);
    if map.contains(span.start, span.end) {
        // Every leaf lies inside — shift each into slice coords.
        let base = map.pos(span.start)?; // == span.start - start (contiguous)
        let delta = base as isize - span.start as isize;
        let location = offset_location(&f.location, delta);
        return Some(Feature {
            location,
            ..f.clone()
        });
    }

    // Straddle or outside.
    match policy {
        PartialPolicy::DropPartials => None,
        PartialPolicy::TruncatePartials => {
            // Truncation across a wrap gap is ambiguous — only the linear case.
            if !map.overlaps_linear(span.start, span.end) {
                return None;
            }
            let a = span.start.max(map.start);
            let b = span.end.min(map.end);
            if a >= b {
                return None;
            }
            let before = span.start < map.start; // 5' cut → `<`
            let after = span.end > map.end; // 3' cut → `>`
            Some(Feature {
                location: Location::Simple {
                    span: Span::from_range((a - map.start)..(b - map.start)),
                    before,
                    after,
                },
                ..f.clone()
            })
        }
    }
}

/// Carry a primer iff its authored `binding` is fully inside the range; a
/// straddler **detaches** (`binding = None`, the reagent survives); outside /
/// already-detached primers are not carried. `sequence`/`strand` ride verbatim.
fn localize_primer(p: &Primer, map: &PosMap, _slice_len: usize) -> Option<Primer> {
    let binding = p.binding.as_ref()?;
    if map.contains(binding.start, binding.end) {
        let base = map.pos(binding.start)?;
        let len = binding.end - binding.start;
        Some(Primer {
            binding: Some(base..base + len),
            ..p.clone()
        })
    } else if map.overlaps_linear(binding.start, binding.end) {
        // Straddler — detach but keep the reagent.
        Some(Primer {
            binding: None,
            ..p.clone()
        })
    } else {
        None
    }
}

// ── place ─────────────────────────────────────────────────────────────────

/// Place `slice`'s annotations into `ann` at `at` (destination coords), with
/// `orient` and provenance-gated `merge`. **Byte insertion is the caller's job**
/// (the paste transaction splices bytes first, which shifts existing annotations
/// to make room); `place` only re-homes and adds the carried annotations.
/// Returns the freshly-minted feature ids (decision 12: placed annotations get
/// new ids — two buffers never share id identity, and pasting twice yields
/// distinct features). `len_total` is the destination molecule length **after**
/// the caller's byte splice — threaded through to `merge` so origin-split leaf
/// geometry (`Location::pieces`) is computed against the right frame.
pub fn place(
    ann: &mut Annotations,
    slice: &SeqSlice,
    at: usize,
    orient: Orient,
    merge: bool,
    len_total: usize,
) -> Vec<FeatureId> {
    let l = slice.len();
    let mut ids = Vec::with_capacity(slice.features.len());

    for f in &slice.features {
        let location = match orient {
            Orient::Identity => offset_location(&f.location, at as isize),
            Orient::Rev => offset_location(&mirror_location(&f.location, l), at as isize),
        };
        let strand = match orient {
            Orient::Identity => f.strand,
            Orient::Rev => flip_strand(f.strand),
        };
        ids.push(ann.add(Feature {
            location,
            strand,
            ..f.clone()
        }));
    }

    for p in &slice.primers {
        let binding = p.binding.as_ref().map(|b| match orient {
            Orient::Identity => (b.start + at)..(b.end + at),
            // Mirror within the slice, then offset.
            Orient::Rev => (l - b.end + at)..(l - b.start + at),
        });
        let strand = match orient {
            Orient::Identity => p.strand,
            Orient::Rev => flip_strand(p.strand),
        };
        ann.add_primer(Primer {
            binding,
            strand,
            ..p.clone()
        });
    }

    if merge {
        merge_features(ann, len_total);
    }
    ids
}

// ── merge (provenance-gated) ──────────────────────────────────────────────────

/// Coalesce features that share **lineage** (equal `Some(Provenance)`) *and* are
/// adjacent/joinable — the reconstruction of a feature split across a boundary.
///
/// - contiguous same-lineage → one crisp `Simple`;
/// - gapped same-lineage → one `Join` (SnapGene "segments");
/// - different lineage (or any `None`) → left separate, **even if names match**
///   (name-only merge is a footgun). Idempotent.
fn merge_features(ann: &mut Annotations, len_total: usize) {
    while let Some((keep, drop)) = find_merge_pair(&ann.features) {
        let merged_loc = combine_locations(
            &ann.features[keep].location,
            &ann.features[drop].location,
            len_total,
        );
        ann.features[keep].location = merged_loc;
        ann.features.remove(drop);
    }
}

/// Find two features (by index) that share a non-`None` provenance and are
/// adjacent or gapped (joinable). Returns `(keep_idx, drop_idx)` with
/// `keep_idx < drop_idx` so the earlier feature survives the coalesce.
fn find_merge_pair(features: &[Feature]) -> Option<(usize, usize)> {
    for (i, fi) in features.iter().enumerate() {
        let Some(pi) = &fi.provenance else {
            continue;
        };
        for (offset, fj) in features[i + 1..].iter().enumerate() {
            // Same lineage — always joinable (contiguous → Simple, gapped → Join).
            if fj.provenance.as_ref() == Some(pi) {
                return Some((i, i + 1 + offset));
            }
        }
    }
    None
}

/// Union two locations into the minimal covering location: collect all leaf
/// segments, sort, coalesce touching/overlapping ones; one run → `Simple`,
/// otherwise `Join`. Internal fuzzy boundaries are cleared (now continuous).
fn combine_locations(a: &Location, b: &Location, len_total: usize) -> Location {
    let mut segs: Vec<Range<usize>> = a
        .pieces(len_total)
        .into_iter()
        .chain(b.pieces(len_total))
        .collect();
    segs.sort_by_key(|r| (r.start, r.end));

    let mut merged: Vec<Range<usize>> = Vec::new();
    for r in segs {
        match merged.last_mut() {
            Some(last) if r.start <= last.end => last.end = last.end.max(r.end),
            _ => merged.push(r),
        }
    }

    if merged.len() == 1 {
        Location::simple(merged.pop().unwrap())
    } else {
        Location::Join(merged.into_iter().map(Location::simple).collect())
    }
}

// ── location transforms ───────────────────────────────────────────────────────

/// Shift every leaf range by `delta` (may be negative), preserving structure and
/// fuzzy flags.
fn offset_location(loc: &Location, delta: isize) -> Location {
    match loc {
        Location::Simple {
            span,
            before,
            after,
        } => Location::Simple {
            span: span.shift(delta),
            before: *before,
            after: *after,
        },
        Location::Complement(inner) => {
            Location::Complement(Box::new(offset_location(inner, delta)))
        }
        Location::Join(parts) => {
            Location::Join(parts.iter().map(|p| offset_location(p, delta)).collect())
        }
    }
}

/// Mirror every leaf range within a window of length `l` (`[a, b)` → `[l-b, l-a)`),
/// swapping fuzzy ends and reversing `Join` order — the coordinate half of a
/// reverse-complement placement (strand flip is applied separately).
fn mirror_location(loc: &Location, l: usize) -> Location {
    match loc {
        Location::Simple {
            span,
            before,
            after,
        } => Location::Simple {
            // [a, b) → [l-b, l-a); with start+len form: new start = l-(start+len).
            span: Span::new(l.saturating_sub(span.start + span.len), span.len),
            // 5'/3' swap under mirroring.
            before: *after,
            after: *before,
        },
        Location::Complement(inner) => Location::Complement(Box::new(mirror_location(inner, l))),
        Location::Join(parts) => {
            Location::Join(parts.iter().rev().map(|p| mirror_location(p, l)).collect())
        }
    }
}

fn flip_strand(s: Strand) -> Strand {
    match s {
        Strand::Forward => Strand::Reverse,
        Strand::Reverse => Strand::Forward,
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    const TEXT20: &[u8] = &[b'A'; 20];

    fn feat(start: usize, end: usize, strand: Strand) -> Feature {
        Feature {
            id: Default::default(),
            location: Location::simple(start..end),
            raw_kind: "misc_feature".into(),
            label: "f".into(),
            strand,
            qualifiers: BTreeMap::new(),
            provenance: None,
        }
    }

    fn primer(binding: Option<Range<usize>>, strand: Strand) -> Primer {
        Primer {
            id: Default::default(),
            name: "p".into(),
            sequence: "ACGT".into(),
            binding,
            strand,
            qualifiers: BTreeMap::new(),
        }
    }

    fn ann(features: Vec<Feature>, primers: Vec<Primer>) -> Annotations {
        Annotations::from_parts(features, primers)
    }

    // ── extract: feature trisection ──────────────────────────────────────────

    #[test]
    fn extract_contained_feature_is_localized() {
        // text 0..20; feature [5,10); extract [4,12) → local [1,6).
        let a = ann(vec![feat(5, 10, Strand::Forward)], vec![]);
        let s = extract(
            TEXT20,
            &a,
            Span::from_range(4..12),
            PartialPolicy::DropPartials,
            "src",
        );
        assert_eq!(s.features.len(), 1);
        assert_eq!(s.features[0].hull(s.len()), 1..6);
        assert_eq!(s.bytes.len(), 8);
        // Provenance stamped with the original span (the merge lineage key).
        let prov = s.features[0].provenance.as_ref().unwrap();
        assert_eq!(prov.source_range, 5..10);
    }

    #[test]
    fn extract_straddler_dropped_by_default() {
        // feature [2,8); extract [5,15) → straddles the left edge → dropped.
        let a = ann(vec![feat(2, 8, Strand::Forward)], vec![]);
        let s = extract(
            TEXT20,
            &a,
            Span::from_range(5..15),
            PartialPolicy::DropPartials,
            "src",
        );
        assert!(s.features.is_empty());
    }

    #[test]
    fn extract_straddler_truncated_and_fuzzy_marked() {
        // feature [2,8); extract [5,15) → clamp to [5,8) → local [0,3), before=true.
        let a = ann(vec![feat(2, 8, Strand::Forward)], vec![]);
        let s = extract(
            TEXT20,
            &a,
            Span::from_range(5..15),
            PartialPolicy::TruncatePartials,
            "src",
        );
        assert_eq!(s.features.len(), 1);
        assert_eq!(s.features[0].hull(s.len()), 0..3);
        assert_eq!(s.features[0].location.fuzzy_ends(), (true, false));
    }

    #[test]
    fn extract_circular_wrap_contained_feature() {
        // total 20; circular range [16, 4) wraps origin → slice len 8.
        // feature [17,19) is inside the first arm → local [1,3).
        let a = ann(vec![feat(17, 19, Strand::Forward)], vec![]);
        // A wrapping span: start 16, len 8 on L=20 crosses the origin.
        let wrap = Span::new(16, 8);
        let s = extract(TEXT20, &a, wrap, PartialPolicy::DropPartials, "src");
        assert_eq!(s.bytes.len(), 8);
        assert_eq!(s.features.len(), 1);
        assert_eq!(s.features[0].hull(s.len()), 1..3);
    }

    // ── extract: primer transfer ─────────────────────────────────────────────

    #[test]
    fn extract_primer_contained_carries_with_shifted_binding() {
        let p = primer(Some(6..10), Strand::Forward);
        let s = extract(
            TEXT20,
            &ann(vec![], vec![p]),
            Span::from_range(4..14),
            PartialPolicy::DropPartials,
            "src",
        );
        assert_eq!(s.primers.len(), 1);
        assert_eq!(s.primers[0].binding, Some(2..6));
        assert_eq!(s.primers[0].sequence, "ACGT"); // verbatim
    }

    #[test]
    fn extract_primer_straddler_detaches_but_carries() {
        let p = primer(Some(2..8), Strand::Forward);
        let s = extract(
            TEXT20,
            &ann(vec![], vec![p]),
            Span::from_range(5..14),
            PartialPolicy::DropPartials,
            "src",
        );
        assert_eq!(s.primers.len(), 1);
        assert_eq!(s.primers[0].binding, None);
    }

    #[test]
    fn extract_primer_outside_or_detached_not_carried() {
        let outside = primer(Some(15..19), Strand::Forward);
        let detached = primer(None, Strand::Forward);
        let s = extract(
            TEXT20,
            &ann(vec![], vec![outside, detached]),
            Span::from_range(0..10),
            PartialPolicy::DropPartials,
            "src",
        );
        assert!(s.primers.is_empty());
    }

    // ── place: offset, orient, re-mint ───────────────────────────────────────

    #[test]
    fn place_identity_shifts_and_mints_fresh_ids() {
        let slice = SeqSlice {
            bytes: vec![b'A'; 5],
            features: vec![feat(1, 3, Strand::Forward)],
            primers: vec![],
        };
        let mut dst = ann(vec![], vec![]);
        let ids = place(&mut dst, &slice, 10, Orient::Identity, false, 30);
        assert_eq!(ids.len(), 1);
        let placed = dst.get(ids[0]).unwrap();
        assert_eq!(placed.hull(30), 11..13);
        assert_ne!(placed.id, FeatureId(0)); // freshly minted

        // Pasting the same slice again yields distinct ids (decision 12).
        let ids2 = place(&mut dst, &slice, 20, Orient::Identity, false, 30);
        assert_ne!(ids[0], ids2[0]);
        assert_eq!(dst.len(), 2);
    }

    #[test]
    fn place_rev_mirrors_coords_and_flips_strand() {
        // slice len 10; feature [1,4) forward → mirror → [6,9), reverse; + at=0.
        let slice = SeqSlice {
            bytes: vec![b'A'; 10],
            features: vec![feat(1, 4, Strand::Forward)],
            primers: vec![],
        };
        let mut dst = ann(vec![], vec![]);
        let ids = place(&mut dst, &slice, 0, Orient::Rev, false, 10);
        let placed = dst.get(ids[0]).unwrap();
        assert_eq!(placed.hull(10), 6..9);
        assert_eq!(placed.strand, Strand::Reverse);
    }

    #[test]
    fn place_rev_mirrors_primer_binding() {
        let slice = SeqSlice {
            bytes: vec![b'A'; 10],
            features: vec![],
            primers: vec![primer(Some(1..4), Strand::Forward)],
        };
        let mut dst = ann(vec![], vec![]);
        place(&mut dst, &slice, 0, Orient::Rev, false, 10);
        let p = dst.primers().next().unwrap();
        assert_eq!(p.binding, Some(6..9));
        assert_eq!(p.strand, Strand::Reverse);
        assert_eq!(p.sequence, "ACGT"); // physical oligo unchanged
    }

    // ── merge (provenance-gated) ─────────────────────────────────────────────

    fn with_prov(mut f: Feature, src_range: Range<usize>) -> Feature {
        f.provenance = Some(Provenance {
            source_doc: "src".into(),
            source_range: src_range,
            operation: "extract".into(),
        });
        f
    }

    #[test]
    fn merge_same_lineage_contiguous_collapses_to_simple() {
        // Two halves of source feature 0..20, now abutting at 10 in the product.
        let mut a = ann(
            vec![
                with_prov(feat(0, 10, Strand::Forward), 0..20),
                with_prov(feat(10, 20, Strand::Forward), 0..20),
            ],
            vec![],
        );
        merge_features(&mut a, 100);
        assert_eq!(a.len(), 1);
        assert_eq!(a.iter().next().unwrap().location, Location::simple(0..20));
    }

    #[test]
    fn merge_same_lineage_gapped_becomes_join() {
        let mut a = ann(
            vec![
                with_prov(feat(0, 10, Strand::Forward), 0..30),
                with_prov(feat(20, 30, Strand::Forward), 0..30),
            ],
            vec![],
        );
        merge_features(&mut a, 100);
        assert_eq!(a.len(), 1);
        assert_eq!(
            a.iter().next().unwrap().location,
            Location::Join(vec![Location::simple(0..10), Location::simple(20..30)])
        );
    }

    #[test]
    fn merge_different_lineage_same_name_stays_separate() {
        // Same name/label, different source_range → NOT merged (name-merge footgun).
        let mut a = ann(
            vec![
                with_prov(feat(0, 10, Strand::Forward), 0..10),
                with_prov(feat(10, 20, Strand::Forward), 100..110),
            ],
            vec![],
        );
        merge_features(&mut a, 100);
        assert_eq!(a.len(), 2);
    }

    #[test]
    fn merge_none_provenance_never_merges() {
        // A loaded feature (provenance None) is never fused by a paste.
        let mut a = ann(
            vec![feat(0, 10, Strand::Forward), feat(10, 20, Strand::Forward)],
            vec![],
        );
        merge_features(&mut a, 100);
        assert_eq!(a.len(), 2);
    }

    #[test]
    fn merge_is_idempotent() {
        let mut a = ann(
            vec![
                with_prov(feat(0, 10, Strand::Forward), 0..20),
                with_prov(feat(10, 20, Strand::Forward), 0..20),
            ],
            vec![],
        );
        merge_features(&mut a, 100);
        let once = a.iter().next().unwrap().location.clone();
        merge_features(&mut a, 100);
        assert_eq!(a.len(), 1);
        assert_eq!(a.iter().next().unwrap().location, once);
    }

    #[test]
    fn place_merge_false_is_strict() {
        let slice = SeqSlice {
            bytes: vec![b'A'; 10],
            features: vec![with_prov(feat(0, 10, Strand::Forward), 0..20)],
            primers: vec![],
        };
        // Destination already holds the other half (same lineage), abutting.
        let mut dst = ann(vec![with_prov(feat(0, 10, Strand::Forward), 0..20)], vec![]);
        place(&mut dst, &slice, 10, Orient::Identity, false, 30);
        assert_eq!(dst.len(), 2, "merge=false leaves pieces separate");
    }

    // ── round-trip property ──────────────────────────────────────────────────

    #[test]
    fn extract_then_place_round_trips_a_whole_buffer() {
        let text = vec![b'A'; 30];
        let a = ann(
            vec![feat(3, 9, Strand::Forward), feat(20, 25, Strand::Reverse)],
            vec![primer(Some(4..8), Strand::Forward)],
        );
        let s = extract(
            &text,
            &a,
            Span::from_range(0..30),
            PartialPolicy::DropPartials,
            "src",
        );
        let mut dst = ann(vec![], vec![]);
        place(&mut dst, &s, 0, Orient::Identity, false, 30);
        let spans: Vec<_> = dst.iter().map(|f| f.hull(30)).collect();
        assert_eq!(spans, vec![3..9, 20..25]);
        assert_eq!(dst.primers().next().unwrap().binding, Some(4..8));
    }
}
