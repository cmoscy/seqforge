use gb_io::reader::SeqReader;
use gb_io::seq::{Location, Topology as GbTopology};
use seqforge_core::{Document, Feature, FeatureKind, Strand, Topology};
use std::collections::BTreeMap;
use std::fs::File;
use std::io::BufReader;
use std::path::Path;

use crate::BioError;

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

fn map_feature(f: &gb_io::seq::Feature) -> Option<Feature> {
    let bounds = f.location.find_bounds().ok()?;
    let start = bounds.0.max(0) as usize;
    let end = bounds.1.max(0) as usize;
    if start >= end {
        return None;
    }

    let strand = location_strand(&f.location);
    let kind = parse_kind(f.kind.as_ref());

    let label = f
        .qualifiers
        .iter()
        .find(|(k, _)| k == "label" || k == "gene" || k == "product" || k == "note")
        .and_then(|(_, v)| v.clone())
        .unwrap_or_else(|| f.kind.to_string());

    let qualifiers = f
        .qualifiers
        .iter()
        .filter_map(|(k, v)| v.as_ref().map(|val| (k.to_string(), val.clone())))
        .collect::<BTreeMap<_, _>>();

    Some(Feature {
        range: start..end,
        kind,
        label,
        strand,
        qualifiers,
    })
}

fn location_strand(loc: &Location) -> Strand {
    match loc {
        Location::Complement(_) => Strand::Reverse,
        _ => Strand::Forward,
    }
}

fn parse_kind(kind: &str) -> FeatureKind {
    match kind {
        "gene" => FeatureKind::Gene,
        "CDS" => FeatureKind::Cds,
        "promoter" => FeatureKind::Promoter,
        "terminator" => FeatureKind::Terminator,
        "rep_origin" => FeatureKind::Rep,
        "source" => FeatureKind::Source,
        "misc_feature" | "misc_binding" => FeatureKind::Misc,
        _ => FeatureKind::Other,
    }
}
