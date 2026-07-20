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

use serde::{Deserialize, Serialize};

use crate::span::{Pieces, Span};
use crate::{
    Annotations, Feature, FeatureId, Lineage, LineageOp, Location, Primer, Strand, Topology,
};

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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
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

    // Slice bytes — concatenate the span's 1-or-2 linear runs (`linear_pieces`).
    let mut bytes: Vec<u8> = Vec::with_capacity(span.len);
    for run in span.linear_pieces(total).iter() {
        bytes.extend_from_slice(&text[run.start..run.end.min(total)]);
    }
    let slice_len = bytes.len();

    let map = PosMap::from_span(span, total);

    let mut features = Vec::new();
    for f in ann.iter() {
        if let Some(mut nf) = localize_feature(f, &map, policy) {
            // Stamp lineage (the merge key) unless the feature already carries
            // one — an existing provenance propagates so multi-hop lineage holds.
            if nf.lineage.is_none() {
                nf.lineage = Some(Lineage {
                    source_doc: source_doc.to_string(),
                    source_range: lineage_source_range(f, total),
                    op: LineageOp::Extract,
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
///
/// The extract window is one linear run `[start, end)` or, when `wrap`, the two
/// runs `[start, total)` · `[0, end)` concatenated into a contiguous slice (same
/// order as [`Span::linear_pieces`]).
struct PosMap {
    start: usize,
    end: usize,
    total: usize,
    wrap: bool,
    /// Slice length (= extract span length).
    slice_len: usize,
}

impl PosMap {
    fn from_span(span: Span, total: usize) -> Self {
        let wrap = span.wraps(total);
        let start = span.start;
        let end = if wrap {
            span.end(total)
        } else {
            span.start + span.len
        };
        PosMap {
            start,
            end,
            total,
            wrap,
            slice_len: span.len,
        }
    }

    /// The 1-or-2 linear source runs the extract window covers (5′→3′ order).
    fn extract_runs(&self) -> Pieces {
        if self.slice_len == 0 || self.total == 0 {
            return Pieces::None;
        }
        if !self.wrap {
            Pieces::One(self.start..self.end)
        } else if self.end == 0 {
            Pieces::One(self.start..self.total)
        } else {
            Pieces::Two(self.start..self.total, 0..self.end)
        }
    }

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
    /// in **slice** order (a linear piece that does not jump the wrap gap).
    fn contains(&self, a: usize, b: usize) -> bool {
        if a >= b {
            return a == b && self.pos(a).is_some();
        }
        match (self.pos(a), self.pos(b)) {
            (Some(la), Some(lb)) => lb >= la && lb - la == b - a,
            _ => false,
        }
    }

    /// Does `[a, b)` overlap the extracted window at all? (Non-wrap only — used
    /// for primer detach; feature truncate uses [`Self::extract_runs`].)
    fn overlaps_linear(&self, a: usize, b: usize) -> bool {
        !self.wrap && a < self.end && b > self.start
    }
}

/// Provenance identity for merge: wrap-aware geometry, not lossy [`Location::bounds`].
///
/// - `Simple` (incl. origin-wrap): `span.start .. span.start + span.len` (end may
///   exceed molecule length — same encoding digest uses for wrapping arcs).
/// - `Join` / multi-segment: first piece start .. start + Σ piece lengths.
fn lineage_source_range(f: &Feature, total: usize) -> Range<usize> {
    if let Some(span) = f.location.as_span() {
        return span.start..span.start + span.len;
    }
    let pieces = f.location.pieces(total);
    let start = pieces.first().map(|r| r.start).unwrap_or(0);
    let len: usize = pieces.iter().map(|r| r.end.saturating_sub(r.start)).sum();
    start..start + len
}

/// Re-home one feature into slice coords per `policy`, or `None` if it doesn't
/// survive. Uses **piece-wise / wrap-aware** geometry — never lossy `bounds()`.
fn localize_feature(f: &Feature, map: &PosMap, policy: PartialPolicy) -> Option<Feature> {
    let location = localize_location(&f.location, map, policy)?;
    Some(Feature {
        location,
        ..f.clone()
    })
}

fn localize_location(loc: &Location, map: &PosMap, policy: PartialPolicy) -> Option<Location> {
    match loc {
        Location::Complement(inner) => {
            let inner_loc = localize_location(inner, map, policy)?;
            Some(Location::Complement(Box::new(inner_loc)))
        }
        Location::Simple {
            span,
            before,
            after,
        } => localize_simple(*span, *before, *after, map, policy),
        Location::Join(parts) => {
            let mut kept = Vec::new();
            for p in parts {
                match localize_location(p, map, policy) {
                    Some(Location::Join(sub)) => kept.extend(sub),
                    Some(other) => kept.push(other),
                    None => {
                        if policy == PartialPolicy::DropPartials {
                            return None;
                        }
                    }
                }
            }
            if kept.is_empty() {
                None
            } else {
                Some(collapse_abutting_simples(kept))
            }
        }
    }
}

/// Map a (possibly wrapping) simple region into the extract slice.
fn localize_simple(
    span: Span,
    before: bool,
    after: bool,
    map: &PosMap,
    policy: PartialPolicy,
) -> Option<Location> {
    let pieces: Vec<Range<usize>> = span.linear_pieces(map.total).iter().collect();
    if pieces.is_empty() {
        return None;
    }

    // Wholly inside the window: every linear piece maps contiguously, and the
    // arc unwraps to one Simple on the (linear) slice.
    if pieces.iter().all(|r| map.contains(r.start, r.end)) {
        let local_start = map.pos(span.start)?;
        // Sanity: the unwrapped arc must fit the slice.
        if local_start + span.len > map.slice_len {
            return None;
        }
        return Some(Location::Simple {
            span: Span::new(local_start, span.len),
            before,
            after,
        });
    }

    match policy {
        PartialPolicy::DropPartials => None,
        PartialPolicy::TruncatePartials => {
            let mut segs: Vec<(Range<usize>, bool, bool)> = Vec::new();
            for piece in pieces {
                segs.extend(truncate_piece(&piece, map));
            }
            coalesce_truncated_segs(segs)
        }
    }
}

/// Intersect one linear feature piece with each extract run; map survivors to
/// slice coords with fuzzy markers on clamped edges.
fn truncate_piece(piece: &Range<usize>, map: &PosMap) -> Vec<(Range<usize>, bool, bool)> {
    let mut out = Vec::new();
    for run in map.extract_runs().iter() {
        let a = piece.start.max(run.start);
        let b = piece.end.min(run.end);
        if a >= b {
            continue;
        }
        let (Some(la), Some(lb)) = (map.pos(a), map.pos(b)) else {
            continue;
        };
        if lb < la {
            continue;
        }
        let before = a > piece.start;
        let after = b < piece.end;
        out.push((la..lb, before, after));
    }
    out
}

fn coalesce_truncated_segs(mut segs: Vec<(Range<usize>, bool, bool)>) -> Option<Location> {
    if segs.is_empty() {
        return None;
    }
    segs.sort_by_key(|(r, _, _)| (r.start, r.end));
    // Merge abutting/overlapping slice ranges.
    let mut merged: Vec<(Range<usize>, bool, bool)> = Vec::new();
    for (r, before, after) in segs {
        if let Some((last, lb, la)) = merged.last_mut() {
            if r.start <= last.end {
                last.end = last.end.max(r.end);
                *lb = *lb || before;
                *la = *la || after;
                continue;
            }
        }
        merged.push((r, before, after));
    }
    if merged.len() == 1 {
        let (r, before, after) = merged.pop().unwrap();
        Some(Location::Simple {
            span: Span::from_range(r),
            before,
            after,
        })
    } else {
        Some(Location::Join(
            merged
                .into_iter()
                .map(|(r, before, after)| Location::Simple {
                    span: Span::from_range(r),
                    before,
                    after,
                })
                .collect(),
        ))
    }
}

/// Collapse a list of localized parts: one part → itself; abutting crisp Simples
/// → one Simple (unwrap of an origin-join into a linear fragment); else Join.
fn collapse_abutting_simples(parts: Vec<Location>) -> Location {
    if parts.len() == 1 {
        return parts.into_iter().next().unwrap();
    }
    // Only collapse when every part is a crisp Simple and they form one run.
    let mut segs: Vec<Range<usize>> = Vec::new();
    for p in &parts {
        match p {
            Location::Simple {
                span,
                before: false,
                after: false,
            } => segs.push(span.range()),
            _ => return Location::Join(parts),
        }
    }
    segs.sort_by_key(|r| (r.start, r.end));
    let mut merged: Vec<Range<usize>> = Vec::new();
    for r in segs {
        if let Some(last) = merged.last_mut() {
            if r.start <= last.end {
                last.end = last.end.max(r.end);
                continue;
            }
        }
        merged.push(r);
    }
    if merged.len() == 1 {
        Location::simple(merged[0].clone())
    } else {
        Location::Join(parts)
    }
}

/// Carry a primer iff its authored `binding` is fully inside the range; a
/// straddler **detaches** (`binding = None`, the reagent survives); outside /
/// already-detached primers are not carried. `sequence`/`strand` ride verbatim.
fn localize_primer(p: &Primer, map: &PosMap, _slice_len: usize) -> Option<Primer> {
    let binding = p.binding.as_ref()?;
    let (b_start, b_end) = (binding.start, binding.start + binding.len);
    if map.contains(b_start, b_end) {
        let base = map.pos(b_start)?;
        Some(Primer {
            binding: Some(Span::new(base, binding.len)),
            ..p.clone()
        })
    } else if map.overlaps_linear(b_start, b_end) {
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
            Orient::Rev => offset_location(&f.location.mirrored(l), at as isize),
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
            Orient::Identity => b.shift(at as isize),
            // Mirror within the slice, then offset — same Span leaves as features.
            Orient::Rev => b.mirrored(l).shift(at as isize),
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

/// Coalesce features that share a **source identity** ([`Lineage::same_source`])
/// *and* are adjacent/joinable — the reconstruction of a feature split across a
/// boundary. The `op` label is not part of identity, so a differing op cannot
/// silently block a reunion.
///
/// - contiguous same-source → one crisp `Simple`;
/// - gapped same-source → one `Join` (SnapGene "segments");
/// - different source (or any `None`) → left separate, **even if names match**
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
        let Some(pi) = &fi.lineage else {
            continue;
        };
        for (offset, fj) in features[i + 1..].iter().enumerate() {
            // Same source identity — always joinable (contiguous → Simple,
            // gapped → Join). `op` is metadata, not part of the key.
            if fj.lineage.as_ref().is_some_and(|pj| pj.same_source(pi)) {
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
/// fuzzy flags. Total (no leaf drops), so `map_spans` always yields `Some`.
/// (Reverse-complement placement uses [`Location::mirrored`].)
fn offset_location(loc: &Location, delta: isize) -> Location {
    loc.map_spans(&|s| Some(s.shift(delta)))
        .expect("offset is total")
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
            lineage: None,
        }
    }

    fn primer(binding: Option<Range<usize>>, strand: Strand) -> Primer {
        Primer {
            id: Default::default(),
            name: "p".into(),
            sequence: "ACGT".into(),
            binding: binding.map(Span::from_range),
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
        assert_eq!(s.features[0].bounds(s.len()), 1..6);
        assert_eq!(s.bytes.len(), 8);
        // Lineage stamped with the original span (the merge source identity).
        let prov = s.features[0].lineage.as_ref().unwrap();
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
        assert_eq!(s.features[0].bounds(s.len()), 0..3);
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
        assert_eq!(s.features[0].bounds(s.len()), 1..3);
    }

    #[test]
    fn extract_wrapping_feature_inside_wrapping_window_survives() {
        // L=20; extract [16..4) (len 8). Wrapping feature start=17 len=5 covers
        // [17..20)+[0..2) — wholly inside the window → unwraps to local [1..6).
        let mut f = feat(0, 0, Strand::Forward);
        f.location = Location::Simple {
            span: Span::new(17, 5),
            before: false,
            after: false,
        };
        f.label = "ori".into();
        let a = ann(vec![f], vec![]);
        let s = extract(
            TEXT20,
            &a,
            Span::new(16, 8),
            PartialPolicy::DropPartials,
            "src",
        );
        assert_eq!(
            s.features.len(),
            1,
            "wrapping feature must survive wrap extract"
        );
        assert_eq!(s.features[0].bounds(s.len()), 1..6);
        assert!(matches!(s.features[0].location, Location::Simple { .. }));
        let prov = s.features[0].lineage.as_ref().unwrap();
        assert_eq!(
            prov.source_range,
            17..22,
            "lineage must use wrap-aware span, not lossy 0..L"
        );
        assert_ne!(prov.source_range, 0..20);
    }

    #[test]
    fn extract_two_wrapping_features_get_distinct_lineage() {
        let mut a_feat = feat(0, 0, Strand::Forward);
        a_feat.location = Location::Simple {
            span: Span::new(17, 5),
            before: false,
            after: false,
        };
        a_feat.label = "oriA".into();
        let mut b_feat = feat(0, 0, Strand::Forward);
        b_feat.location = Location::Simple {
            span: Span::new(18, 4),
            before: false,
            after: false,
        };
        b_feat.label = "oriB".into();
        let a = ann(vec![a_feat, b_feat], vec![]);
        let s = extract(
            TEXT20,
            &a,
            Span::new(16, 8),
            PartialPolicy::DropPartials,
            "src",
        );
        assert_eq!(s.features.len(), 2);
        let r0 = s.features[0].lineage.as_ref().unwrap().source_range.clone();
        let r1 = s.features[1].lineage.as_ref().unwrap().source_range.clone();
        assert_ne!(
            r0, r1,
            "distinct wrapping features must not share 0..L identity"
        );
        assert_ne!(r0, 0..20);
        assert_ne!(r1, 0..20);
    }

    #[test]
    fn extract_truncate_on_wrapping_window_clamps_straddler() {
        // Extract [16..4); feature [14..18) straddles the left cut at 16.
        // Surviving [16..18) → local [0..2), before=true.
        let a = ann(vec![feat(14, 18, Strand::Forward)], vec![]);
        let s = extract(
            TEXT20,
            &a,
            Span::new(16, 8),
            PartialPolicy::TruncatePartials,
            "src",
        );
        assert_eq!(s.features.len(), 1);
        assert_eq!(s.features[0].bounds(s.len()), 0..2);
        assert_eq!(s.features[0].location.fuzzy_ends(), (true, false));
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
        assert_eq!(s.primers[0].binding, Some(Span::new(2, 4)));
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
        assert_eq!(placed.bounds(30), 11..13);
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
        assert_eq!(placed.bounds(10), 6..9);
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
        assert_eq!(p.binding, Some(Span::new(6, 3)));
        assert_eq!(p.strand, Strand::Reverse);
        assert_eq!(p.sequence, "ACGT"); // physical oligo unchanged
    }

    // ── merge (provenance-gated) ─────────────────────────────────────────────

    fn with_prov(mut f: Feature, src_range: Range<usize>) -> Feature {
        f.lineage = Some(Lineage {
            source_doc: "src".into(),
            source_range: src_range,
            op: LineageOp::Extract,
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
        let spans: Vec<_> = dst.iter().map(|f| f.bounds(30)).collect();
        assert_eq!(spans, vec![3..9, 20..25]);
        assert_eq!(dst.primers().next().unwrap().binding, Some(Span::new(4, 4)));
    }
}
