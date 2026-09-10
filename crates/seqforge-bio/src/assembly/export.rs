//! Writing assembly products to disk — the one place a product becomes a file.
//!
//! Both faces call this: the CLI's `assemble --out` and the GUI's "Save all
//! products…". Keeping it here (not in either shell) is what makes the two
//! genuinely one code path rather than two implementations that agree today.
//!
//! Also holds the `--combos` selector grammar, shared by the CLI flag and the
//! `RunRecipe` viewer request.

use std::path::{Path, PathBuf};

use seqforge_core::{Annotations, Buffer};

use super::NamedProduct;
use crate::BioError;

/// On-disk format for an exported product.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ProductFormat {
    /// GenBank — keeps the product's features and primers.
    #[default]
    GenBank,
    /// FASTA — sequence only; annotations are dropped.
    Fasta,
}

impl ProductFormat {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "genbank" | "gb" | "gbk" => Some(ProductFormat::GenBank),
            "fasta" | "fa" | "fna" => Some(ProductFormat::Fasta),
            _ => None,
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            ProductFormat::GenBank => "gb",
            ProductFormat::Fasta => "fa",
        }
    }
}

/// Write one product into `dir`, returning the path written.
///
/// The filename is the product's name sanitized for the filesystem, plus the
/// format's extension. Collisions get a `_2`, `_3`, … suffix rather than
/// silently overwriting — a combinatorial run must never lose a product to a
/// name clash.
pub fn write_product(
    product: &NamedProduct,
    dir: &Path,
    format: ProductFormat,
) -> Result<PathBuf, BioError> {
    std::fs::create_dir_all(dir)?;
    let path = unique_path(dir, &sanitize(&product.name), format.extension());

    let buf = Buffer::new(
        product.name.clone(),
        Some(path.clone()),
        product.fragment.slice.bytes.clone(),
        product.fragment.topology,
    );
    match format {
        ProductFormat::GenBank => {
            let ann = Annotations::from_parts(
                product.fragment.slice.features.clone(),
                product.fragment.slice.primers.clone(),
            );
            crate::genbank::write(&buf, &ann, &path)?;
        }
        ProductFormat::Fasta => crate::fasta::write(&buf, &path)?,
    }
    Ok(path)
}

/// Write every product into `dir`, in order. Returns one path per product.
pub fn write_products(
    products: &[NamedProduct],
    dir: &Path,
    format: ProductFormat,
) -> Result<Vec<PathBuf>, BioError> {
    products
        .iter()
        .map(|p| write_product(p, dir, format))
        .collect()
}

/// Replace anything awkward in a filename with `_`, collapsing runs.
fn sanitize(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for ch in name.chars() {
        let keep = ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | '+');
        if keep {
            out.push(ch);
        } else if !out.ends_with('_') {
            out.push('_');
        }
    }
    let trimmed = out.trim_matches(['_', '.']).to_string();
    if trimmed.is_empty() {
        "product".to_string()
    } else {
        trimmed
    }
}

fn unique_path(dir: &Path, stem: &str, ext: &str) -> PathBuf {
    let first = dir.join(format!("{stem}.{ext}"));
    if !first.exists() {
        return first;
    }
    for n in 2..u32::MAX {
        let candidate = dir.join(format!("{stem}_{n}.{ext}"));
        if !candidate.exists() {
            return candidate;
        }
    }
    first
}

/// Parse a combo selector into concrete indices against a run of `total`
/// combos. Grammar: comma-separated `N`, `A-B` ranges, and `!`-prefixed
/// exclusions applied after the includes. An empty include set means "all",
/// so `!12` alone reads as "everything except combo 12".
///
/// ```text
/// 0,3,7        → [0, 3, 7]
/// 0-4          → [0, 1, 2, 3, 4]
/// !12          → every index except 12
/// 0-31,!12     → 0..=31 without 12
/// ```
pub fn parse_combo_spec(spec: &str, total: usize) -> Result<Vec<usize>, String> {
    let mut include: Vec<usize> = Vec::new();
    let mut exclude: Vec<usize> = Vec::new();
    let mut saw_include = false;

    for raw in spec.split(',') {
        let token = raw.trim();
        if token.is_empty() {
            continue;
        }
        let (negated, body) = match token.strip_prefix('!') {
            Some(rest) => (true, rest.trim()),
            None => (false, token),
        };
        let range = parse_range(body, total)?;
        if negated {
            exclude.extend(range);
        } else {
            saw_include = true;
            include.extend(range);
        }
    }

    if !saw_include {
        include = (0..total).collect();
    }
    include.sort_unstable();
    include.dedup();
    include.retain(|i| !exclude.contains(i));
    if include.is_empty() {
        return Err(format!("combo selector {spec:?} selects nothing"));
    }
    Ok(include)
}

fn parse_range(body: &str, total: usize) -> Result<Vec<usize>, String> {
    let bounds = match body.split_once('-') {
        Some((a, b)) => {
            let lo: usize = a
                .trim()
                .parse()
                .map_err(|_| format!("bad combo index {a:?}"))?;
            let hi: usize = b
                .trim()
                .parse()
                .map_err(|_| format!("bad combo index {b:?}"))?;
            if lo > hi {
                return Err(format!("combo range {body:?} runs backwards"));
            }
            (lo, hi)
        }
        None => {
            let n: usize = body
                .parse()
                .map_err(|_| format!("bad combo index {body:?}"))?;
            (n, n)
        }
    };
    if bounds.1 >= total {
        return Err(format!(
            "combo index {} is out of range (this run has {total} combo{})",
            bounds.1,
            if total == 1 { "" } else { "s" }
        ));
    }
    Ok((bounds.0..=bounds.1).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selects_indices_ranges_and_exclusions() {
        assert_eq!(parse_combo_spec("0,3,7", 8).unwrap(), vec![0, 3, 7]);
        assert_eq!(parse_combo_spec("0-4", 8).unwrap(), vec![0, 1, 2, 3, 4]);
        assert_eq!(parse_combo_spec("0-3,!1", 8).unwrap(), vec![0, 2, 3]);
    }

    #[test]
    fn bare_exclusion_means_everything_else() {
        assert_eq!(parse_combo_spec("!2", 5).unwrap(), vec![0, 1, 3, 4]);
    }

    #[test]
    fn rejects_out_of_range_and_empty_selection() {
        assert!(parse_combo_spec("9", 5).is_err());
        assert!(parse_combo_spec("0-9", 5).is_err());
        assert!(parse_combo_spec("2,!2", 5).is_err());
        assert!(parse_combo_spec("3-1", 5).is_err());
    }

    #[test]
    fn sanitizes_names_into_filenames() {
        assert_eq!(sanitize("VH-PVP + VL-PP #3"), "VH-PVP_+_VL-PP_3");
        assert_eq!(sanitize("///"), "product");
    }
}

// ── Product origin ────────────────────────────────────────────────────────────

/// Where a circular product's origin should sit.
///
/// A Golden Gate product's position 0 falls wherever the first bin's fragment
/// happened to start — a restriction cut, which is arbitrary as a display
/// origin. Naming the vector's own landmark instead makes a whole combinatorial
/// library open in the same frame, which is the difference between 31 plasmids
/// you can compare at a glance and 31 you cannot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OriginSpec {
    /// The start of the single feature carrying this label.
    Feature(String),
    /// An explicit 0-based coordinate.
    Index(usize),
}

impl std::str::FromStr for OriginSpec {
    type Err = std::convert::Infallible;
    /// An all-digits spec is a coordinate; anything else is a feature label.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        Ok(match s.parse::<usize>() {
            Ok(n) => OriginSpec::Index(n),
            Err(_) => OriginSpec::Feature(s.to_string()),
        })
    }
}

/// Resolve `spec` against `features` to a 0-based rotation point.
///
/// A label matching no feature — or more than one — is an error, never a guess:
/// silently rotating to the wrong copy of an ambiguous label would misplace
/// every downstream coordinate with nothing to show for it.
pub fn resolve_origin(
    spec: &OriginSpec,
    features: &[seqforge_core::Feature],
    len: usize,
) -> Result<usize, String> {
    let index = match spec {
        OriginSpec::Index(n) => *n,
        OriginSpec::Feature(label) => {
            let mut hits = features
                .iter()
                .filter(|f| f.label.eq_ignore_ascii_case(label))
                // A spliced Join has no single arc; its bounding start is the
                // only sensible landmark, and a marker feature is never spliced.
                .map(|f| {
                    f.location
                        .as_span()
                        .map(|sp| sp.start)
                        .unwrap_or_else(|| f.location.bounds(len).start)
                });
            let first = hits
                .next()
                .ok_or_else(|| format!("no feature labelled {label:?}"))?;
            let extra = hits.count();
            if extra > 0 {
                return Err(format!(
                    "{} features are labelled {label:?} — the origin would be ambiguous",
                    extra + 1
                ));
            }
            first
        }
    };
    if len == 0 {
        return Err("cannot set the origin of an empty molecule".to_string());
    }
    if index >= len {
        return Err(format!(
            "origin {index} is past the end of a {len} bp molecule"
        ));
    }
    Ok(index)
}

/// Rotate a product in place so `spec` becomes position 0.
///
/// Delegates the actual move to [`seqforge_core::rotate_origin`], which re-homes
/// every feature and primer wrap-aware. Linear products are left alone — an
/// origin is only meaningful on a circle.
pub fn set_product_origin(product: &mut NamedProduct, spec: &OriginSpec) -> Result<(), String> {
    if product.fragment.topology != seqforge_core::Topology::Circular {
        return Ok(());
    }
    let slice = &mut product.fragment.slice;
    let n = resolve_origin(spec, &slice.features, slice.bytes.len())
        .map_err(|e| format!("{}: {e}", product.name))?;

    let mut ann = Annotations::from_parts(
        std::mem::take(&mut slice.features),
        std::mem::take(&mut slice.primers),
    );
    seqforge_core::rotate_origin(&mut slice.bytes, &mut ann, n);
    let (features, primers) = ann.into_parts();
    slice.features = features;
    slice.primers = primers;
    Ok(())
}

/// Rotate every product, stopping at the first that cannot be resolved.
pub fn set_origins(products: &mut [NamedProduct], spec: &OriginSpec) -> Result<(), String> {
    products
        .iter_mut()
        .try_for_each(|p| set_product_origin(p, spec))
}
