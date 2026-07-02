use gb_io::reader::SeqReader;
use gb_io::seq::{After, Before, Feature as GbFeature, Location, Seq, Topology as GbTopology};
use seqforge_core::{Annotations, Buffer, Document, Feature, Primer, Provenance, Strand, Topology};
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
        } else if let Some(feat) = map_feature(f) {
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

fn map_feature(f: &GbFeature) -> Option<Feature> {
    let bounds = f.location.find_bounds().ok()?;
    let start = bounds.0.max(0) as usize;
    let end = bounds.1.max(0) as usize;
    if start >= end {
        return None;
    }

    let strand = location_strand(&f.location);
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
        range: start..end,
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

    let name = f
        .qualifiers
        .iter()
        .find(|(k, _)| k == "label" || k == "gene" || k == "product" || k == "note")
        .and_then(|(_, v)| v.clone())
        .unwrap_or_else(|| "primer".to_string());

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
        binding: Some(start..end),
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
    let mut gb_features: Vec<GbFeature> = ann
        .iter()
        .filter(|f| f.raw_kind != PRIMER_BIND_KIND)
        .map(feature_to_gb)
        .collect();
    gb_features.extend(ann.primers().filter_map(primer_to_gb));
    seq.features = gb_features;

    let file = File::create(path)?;
    seq.write(BufWriter::new(file))
        .map_err(|e| BioError::Write(e.to_string()))
}

fn feature_to_gb(f: &Feature) -> GbFeature {
    let base = Location::Range(
        (f.range.start as i64, Before(false)),
        (f.range.end as i64, After(false)),
    );
    let location = match f.strand {
        Strand::Reverse => Location::Complement(Box::new(base)),
        _ => base,
    };

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
    let binding = p.binding.clone()?;
    let base = Location::Range(
        (binding.start as i64, Before(false)),
        (binding.end as i64, After(false)),
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
