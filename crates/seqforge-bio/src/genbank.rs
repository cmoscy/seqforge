use gb_io::reader::SeqReader;
use gb_io::seq::{After, Before, Feature as GbFeature, Location, Seq, Topology as GbTopology};
use seqforge_core::{
    Annotations, Buffer, Document, Feature, Location as CoreLocation, Primer, Provenance,
    Span as CoreSpan, Strand, Topology,
};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufReader, BufWriter};
use std::path::Path;

use crate::BioError;

/// Qualifier key used to round-trip [`Provenance`] as JSON.
const PROVENANCE_KEY: &str = "seqforge_provenance";

/// GenBank feature kind that carries an oligo binding. Diverted to `Primer`
/// (never a `Feature`) on load; emitted from `primers` only on write.
const PRIMER_BIND_KIND: &str = "primer_bind";

/// Qualifier key used to round-trip the **full oligo** (5'→3', tail included) as
/// JSON, mirroring the `/seqforge_provenance` pattern. A `primer_bind` location
/// records only the annealed footprint; a 5' tail has no template position, so
/// the authored sequence is preserved here. Absent on a foreign import, where
/// the oligo is reconstructed best-effort from the template at the binding.
const PRIMER_KEY: &str = "seqforge_primer";

/// JSON payload of the `/seqforge_primer` qualifier. Kept minimal and additive
/// (a tail boundary joins once internal bulges land — see `plans/primers.md`).
#[derive(Serialize, Deserialize)]
struct PrimerNote {
    /// Full oligo 5'→3', including any 5' tail.
    sequence: String,
}

pub fn load(path: &Path) -> Result<Document, BioError> {
    let file = File::open(path)?;
    let mut reader = SeqReader::new(BufReader::new(file));
    let gb_seq = reader.next().ok_or(BioError::EmptyFile)??;

    let name = gb_seq.name.clone().unwrap_or_else(|| {
        path.file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_owned()
    });

    let topology = match gb_seq.topology {
        GbTopology::Circular => Topology::Circular,
        GbTopology::Linear => Topology::Linear,
    };

    let sequence: Vec<u8> = gb_seq.seq.iter().map(|&b| b.to_ascii_uppercase()).collect();

    // Partition: `primer_bind` records become authored `Primer`s (decision 14),
    // everything else a `Feature`. This is the parse-side of the diversion — the
    // writer emits `primer_bind` from `primers` only, so no record is double-mapped.
    let mut features = Vec::new();
    let mut primers = Vec::new();
    for f in &gb_seq.features {
        if f.kind.as_ref() == PRIMER_BIND_KIND {
            if let Some(p) = map_primer(f, &sequence) {
                primers.push(p);
            }
        } else if let Some(feat) = map_feature(f, sequence.len(), topology == Topology::Circular) {
            features.push(feat);
        }
    }

    Ok(Document {
        name,
        sequence,
        topology,
        features,
        primers,
        source_path: Some(path.to_owned()),
    })
}

fn map_feature(f: &GbFeature, len: usize, circular: bool) -> Option<Feature> {
    // Map the full location grammar losslessly (join/fuzzy/complement) instead
    // of flattening to a bounding range. The overall strand is normalized into
    // `Feature.strand`; a mixed-strand join is single-stranded (the stub).
    let (location, strand) = map_location(&f.location, len, circular)?;
    let raw_kind = f.kind.to_string();

    let label = f
        .qualifiers
        .iter()
        .find(|(k, _)| k == "label" || k == "gene" || k == "product" || k == "note")
        .and_then(|(_, v)| v.clone())
        .unwrap_or_else(|| raw_kind.clone());

    // Preserve every qualifier, including flag-style (`None`-valued) ones.
    // Pull the provenance qualifier out into the typed field so it doesn't
    // get written back twice.
    let mut qualifiers: BTreeMap<String, Option<String>> = BTreeMap::new();
    let mut provenance = None;
    for (k, v) in &f.qualifiers {
        if k == PROVENANCE_KEY {
            // GenBank wraps long qualifier values across lines and gb-io's
            // reader keeps the continuation newlines verbatim. Our provenance
            // JSON is compact (no real newlines), so stripping them rejoins
            // the original value before parsing.
            provenance = v.as_ref().and_then(|s| {
                let joined: String = s.split(['\n', '\r']).collect();
                serde_json::from_str::<Provenance>(&joined).ok()
            });
        } else {
            qualifiers.insert(k.to_string(), v.clone());
        }
    }

    Some(Feature {
        // Placeholder; `Annotations::new` re-mints a session-scoped id on load.
        id: Default::default(),
        location,
        raw_kind,
        label,
        strand,
        qualifiers,
        provenance,
    })
}

/// Map a GenBank `primer_bind` record to an authored [`Primer`] (decision 14).
///
/// `binding` comes from the location, `strand` from `complement(...)`. The full
/// oligo comes from our `/seqforge_primer` note when present (lossless, our own
/// files); on a foreign import it is reconstructed **best-effort** from the
/// template at the binding (forward = the footprint, reverse = its
/// reverse-complement) — i.e. assuming a perfect anneal with no 5' tail. The
/// name is derived like a feature label and also round-trips natively via the
/// preserved `/note` (or `/label`) qualifier.
fn map_primer(f: &GbFeature, seq: &[u8]) -> Option<Primer> {
    let bounds = f.location.find_bounds().ok()?;
    let start = bounds.0.max(0) as usize;
    let end = bounds.1.max(0) as usize;
    if start >= end || end > seq.len() {
        return None;
    }

    let strand = location_strand(&f.location);

    // Name from a name-bearing qualifier; left empty when the record has none,
    // so `Annotations::from_parts` assigns a unique `Primer N` default via the
    // shared generator (decision 9) rather than a colliding literal.
    let name = f
        .qualifiers
        .iter()
        .find(|(k, _)| k == "label" || k == "gene" || k == "product" || k == "note")
        .and_then(|(_, v)| v.clone())
        .unwrap_or_default();

    // Preserve every qualifier except our own `/seqforge_primer` note, which is
    // pulled into the typed `sequence` (mirrors the provenance handling above).
    let mut qualifiers: BTreeMap<String, Option<String>> = BTreeMap::new();
    let mut note = None;
    for (k, v) in &f.qualifiers {
        if k == PRIMER_KEY {
            note = v.as_ref().and_then(|s| {
                let joined: String = s.split(['\n', '\r']).collect();
                serde_json::from_str::<PrimerNote>(&joined).ok()
            });
        } else {
            qualifiers.insert(k.to_string(), v.clone());
        }
    }

    let sequence = note
        .map(|n| n.sequence)
        .unwrap_or_else(|| best_effort_oligo(&seq[start..end], strand));

    Some(Primer {
        // Placeholder; `Annotations::from_parts` re-mints a session-scoped id.
        id: Default::default(),
        name,
        sequence,
        binding: Some(CoreSpan::from_range(start..end)),
        strand,
        qualifiers,
    })
}

/// Reconstruct an oligo from the template footprint it binds, for a foreign
/// import with no `/seqforge_primer` note: a forward primer *is* the top-strand
/// footprint; a reverse primer is its reverse-complement. `region` is already
/// upper-cased.
fn best_effort_oligo(region: &[u8], strand: Strand) -> String {
    let bytes = match strand {
        Strand::Reverse => crate::reverse_complement(region),
        _ => region.to_vec(),
    };
    String::from_utf8_lossy(&bytes).into_owned()
}

fn location_strand(loc: &Location) -> Strand {
    match loc {
        Location::Complement(_) => Strand::Reverse,
        _ => Strand::Forward,
    }
}

/// Map a `gb_io` location to core geometry + overall strand. The strand comes
/// from the outermost `complement(...)`; the returned [`CoreLocation`] is
/// strand-free geometry (join/fuzzy) — inner complements (a mixed-strand join)
/// are dropped, which single-strands trans-splicing (the stub). `None` if no
/// segment resolves to a non-empty range. `len`/`circular` drive origin-join
/// normalization (a `join` across the origin becomes one wrapping `Simple`).
fn map_location(loc: &Location, len: usize, circular: bool) -> Option<(CoreLocation, Strand)> {
    let strand = location_strand(loc);
    Some((gb_geometry(loc, len, circular)?, strand))
}

/// Recursively map `gb_io` geometry to [`CoreLocation`], discarding every
/// `complement` wrapper (strand is tracked separately). `Range` carries its
/// `<`/`>` fuzzy flags across; `Join`/`Order` become segments; unusual variants
/// (`Bond`/`OneOf`/`External`/`Gap`) fall back to their bounding hull.
///
/// On a **circular** molecule an origin-adjacent two-part `join` — the first
/// segment ending at `len`, the second starting at `0` — is normalized to a
/// single wrapping [`CoreSpan`] on a `Simple`, retiring the GenBank
/// `join(...)`-for-origin-wrap overload (`plans/span.md` decision 1). `write`
/// inverts this on export for byte-level round-trip.
fn gb_geometry(loc: &Location, len: usize, circular: bool) -> Option<CoreLocation> {
    match loc {
        Location::Range((a, Before(before)), (b, After(after))) => {
            let start = (*a).max(0) as usize;
            let end = (*b).max(0) as usize;
            (start < end).then_some(CoreLocation::Simple {
                span: CoreSpan::from_range(start..end),
                before: *before,
                after: *after,
            })
        }
        Location::Complement(inner) => gb_geometry(inner, len, circular),
        Location::Join(parts) | Location::Order(parts) => {
            let segs: Vec<CoreLocation> = parts
                .iter()
                .filter_map(|p| gb_geometry(p, len, circular))
                .collect();
            match segs.len() {
                0 => None,
                1 => segs.into_iter().next(),
                2 => Some(fold_origin_join(segs, len, circular)),
                _ => Some(CoreLocation::Join(segs)),
            }
        }
        // Between / Bond / OneOf / External / Gap — no first-class mapping; keep
        // the bounding hull so the feature at least survives round-trip.
        _ => {
            let (a, b) = loc.find_bounds().ok()?;
            let start = a.max(0) as usize;
            let end = b.max(0) as usize;
            (start < end).then_some(CoreLocation::simple(start..end))
        }
    }
}

/// Collapse a two-segment `join` that hugs the origin into one wrapping `Simple`
/// (the geometry a circular molecule actually means); otherwise keep it a
/// `Join`. Origin-adjacent = crisp `Simple` head ending at `len`, crisp `Simple`
/// tail starting at `0`. Fuzzy `<`/`>` markers ride the outer ends; the (now
/// continuous) origin junction is crisp.
fn fold_origin_join(mut segs: Vec<CoreLocation>, len: usize, circular: bool) -> CoreLocation {
    if circular && len > 0 {
        if let [
            CoreLocation::Simple {
                span: head, before, ..
            },
            CoreLocation::Simple {
                span: tail, after, ..
            },
        ] = segs.as_slice()
        {
            if head.start + head.len == len && tail.start == 0 {
                return CoreLocation::Simple {
                    span: CoreSpan::new(head.start, head.len + tail.len),
                    before: *before,
                    after: *after,
                };
            }
        }
    }
    CoreLocation::Join(std::mem::take(&mut segs))
}

/// Build a `gb_io` location from core geometry, then wrap it in `complement`
/// iff the feature's overall strand is reverse (the inverse of [`map_location`]).
fn core_location_to_gb(loc: &CoreLocation, strand: Strand, len: usize) -> Location {
    let base = geometry_to_gb(loc, len);
    match strand {
        Strand::Reverse => Location::Complement(Box::new(base)),
        _ => base,
    }
}

fn geometry_to_gb(loc: &CoreLocation, len: usize) -> Location {
    match loc {
        CoreLocation::Simple {
            span,
            before,
            after,
        } if span.wraps(len) => {
            // The inverse of `fold_origin_join`: an origin-wrapping `Simple` is
            // emitted as the GenBank `join((start..len),(0..tail))` it round-trips
            // from. Fuzzy `<`/`>` ride the outer ends; the origin junction is crisp.
            let tail = span.end(len);
            Location::Join(vec![
                Location::Range(
                    (span.start as i64, Before(*before)),
                    (len as i64, After(false)),
                ),
                Location::Range((0, Before(false)), (tail as i64, After(*after))),
            ])
        }
        CoreLocation::Simple {
            span,
            before,
            after,
        } => Location::Range(
            (span.start as i64, Before(*before)),
            ((span.start + span.len) as i64, After(*after)),
        ),
        CoreLocation::Join(parts) => {
            Location::Join(parts.iter().map(|p| geometry_to_gb(p, len)).collect())
        }
        CoreLocation::Complement(inner) => {
            Location::Complement(Box::new(geometry_to_gb(inner, len)))
        }
    }
}

/// Write a `Buffer` + `Annotations` to a GenBank file at `path`.
pub fn write(buf: &Buffer, ann: &Annotations, path: &Path) -> Result<(), BioError> {
    let mut seq = Seq::empty();
    seq.name = Some(buf.name.clone());
    seq.topology = match buf.topology {
        Topology::Circular => GbTopology::Circular,
        Topology::Linear => GbTopology::Linear,
    };
    seq.molecule_type = Some("DNA".to_string());
    seq.len = Some(buf.text.len());
    seq.seq = buf.text.clone();

    // Emit features, then primers as `primer_bind`. Primers are written from
    // `primers` **only** (decision 14); defensively skip any feature that still
    // carries the `primer_bind` kind so a record is never emitted twice.
    let len = buf.text.len();
    let mut gb_features: Vec<GbFeature> = ann
        .iter()
        .filter(|f| f.raw_kind != PRIMER_BIND_KIND)
        .map(|f| feature_to_gb(f, len))
        .collect();
    gb_features.extend(ann.primers().filter_map(primer_to_gb));
    seq.features = gb_features;

    let file = File::create(path)?;
    seq.write(BufWriter::new(file))
        .map_err(|e| BioError::Write(e.to_string()))
}

fn feature_to_gb(f: &Feature, len: usize) -> GbFeature {
    // Emit the full geometry (join/fuzzy), re-wrapping in `complement` from the
    // authoritative `Feature.strand` — the inverse of the import mapping.
    let location = core_location_to_gb(&f.location, f.strand, len);

    let mut qualifiers: Vec<(Cow<'static, str>, Option<String>)> = f
        .qualifiers
        .iter()
        .map(|(k, v)| (Cow::Owned(k.clone()), v.clone()))
        .collect();

    if let Some(prov) = &f.provenance {
        if let Ok(json) = serde_json::to_string(prov) {
            qualifiers.push((Cow::Borrowed(PROVENANCE_KEY), Some(json)));
        }
    }

    GbFeature {
        kind: Cow::Owned(f.raw_kind.clone()),
        location,
        qualifiers,
    }
}

/// Emit a [`Primer`] as a GenBank `primer_bind` feature. Returns `None` for a
/// **detached** primer (`binding = None`): GenBank has no location-free record,
/// so an unattached oligo has no `primer_bind` to write. (Detached primers only
/// arise from edits — Phase 2.x — which is where a note-only carrier lands; a
/// freshly-loaded file has none.) The full authored oligo, including any 5'
/// tail, is preserved in the `/seqforge_primer` note.
fn primer_to_gb(p: &Primer) -> Option<GbFeature> {
    let binding = p.binding?;
    // Linear footprint end (`start + len`). Primers don't yet anneal across the
    // origin; a wrapping binding would need the join(...) split (as features do)
    // once that lands.
    let base = Location::Range(
        (binding.start as i64, Before(false)),
        ((binding.start + binding.len) as i64, After(false)),
    );
    let location = match p.strand {
        Strand::Reverse => Location::Complement(Box::new(base)),
        _ => base,
    };

    let mut qualifiers: Vec<(Cow<'static, str>, Option<String>)> = p
        .qualifiers
        .iter()
        .map(|(k, v)| (Cow::Owned(k.clone()), v.clone()))
        .collect();

    // The name is derived from a name-bearing qualifier on load. When the primer
    // carries none — e.g. one created programmatically (Phase 2.1), not parsed
    // from a file — emit a `/label` so the name survives the round-trip. When one
    // already exists (a preserved `/note`, the usual loaded case), it round-trips
    // natively and we don't double-write.
    let has_name_qualifier = p
        .qualifiers
        .keys()
        .any(|k| k == "label" || k == "gene" || k == "product" || k == "note");
    if !has_name_qualifier && !p.name.is_empty() {
        qualifiers.push((Cow::Borrowed("label"), Some(p.name.clone())));
    }

    if let Ok(json) = serde_json::to_string(&PrimerNote {
        sequence: p.sequence.clone(),
    }) {
        qualifiers.push((Cow::Borrowed(PRIMER_KEY), Some(json)));
    }

    Some(GbFeature {
        kind: Cow::Borrowed(PRIMER_BIND_KIND),
        location,
        qualifiers,
    })
}
