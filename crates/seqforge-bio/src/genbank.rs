use gb_io::reader::SeqReader;
use gb_io::seq::{After, Before, Feature as GbFeature, Location, Seq, Topology as GbTopology};
use seqforge_core::{Annotations, Buffer, Document, Feature, Provenance, Strand, Topology};
use std::borrow::Cow;
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufReader, BufWriter};
use std::path::Path;

use crate::BioError;

/// Qualifier key used to round-trip [`Provenance`] as JSON.
const PROVENANCE_KEY: &str = "seqforge_provenance";

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

    let sequence = gb_seq.seq.iter().map(|&b| b.to_ascii_uppercase()).collect();

    let features = gb_seq.features.iter().filter_map(map_feature).collect();

    Ok(Document {
        name,
        sequence,
        topology,
        features,
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
        range: start..end,
        raw_kind,
        label,
        strand,
        qualifiers,
        provenance,
    })
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
    seq.features = ann.features.iter().map(feature_to_gb).collect();

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
