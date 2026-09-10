//! Product naming — from the recipe's template, with a `#n` suffix
//! disambiguating a library. The template is resolved against the combo's
//! per-bin contributions so a combinatorial run yields provenance-bearing
//! names instead of `a+b+c #7`.
//!
//! Supported tokens (unknown tokens are left verbatim):
//!
//! | Token | Expands to |
//! |---|---|
//! | `{roles}` | the bin roles joined with `+` (the default base) |
//! | `{n}` | the combo index (0-based) |
//! | `{i}` | the product ordinal within the run (1-based) |
//! | `{bin0}` … `{binN}` | the file stem of the source that bin *N* contributed |
//! | `{bin0:6}` | the same, truncated to 6 characters |
//! | `{bin0/1}` | field 1 of that stem, splitting on `_` (0-based) |
//!
//! The field selector exists because hierarchical assembly composes names: a
//! level-2 product is named from its level-1 inputs, whose own names already
//! carry a project stem and a backbone suffix. `{bin1/1}` pulls the part that
//! actually varies (`6H8_VH-PVP_pGGa` → `VH-PVP`) instead of nesting the whole
//! stem inside the new name.

use seqforge_core::{Fragment, Recipe};

use super::{ComboPart, NamedProduct};

/// A product awaiting a name, carrying the provenance the template reads.
pub(super) struct Pending {
    pub combo_index: usize,
    pub parts: Vec<ComboPart>,
    pub fragment: Fragment,
}

pub(super) fn name_products(recipe: &Recipe, products: Vec<Pending>) -> Vec<NamedProduct> {
    let roles = recipe
        .bins
        .iter()
        .map(|b| b.role.clone())
        .collect::<Vec<_>>()
        .join("+");
    let template = recipe.name_template.clone();
    let multi = products.len() > 1;
    products
        .into_iter()
        .enumerate()
        .map(|(i, p)| {
            let name = match &template {
                Some(t) => expand_template(t, &roles, p.combo_index, i + 1, &p.parts),
                None if multi => format!("{roles} #{}", i + 1),
                None => roles.clone(),
            };
            NamedProduct {
                name,
                fragment: p.fragment,
                combo_index: p.combo_index,
                parts: p.parts,
            }
        })
        .collect()
}

/// Substitute `{…}` tokens in `template`. An unrecognized token is left as-is
/// (a literal brace run is therefore always safe).
fn expand_template(
    template: &str,
    roles: &str,
    combo_index: usize,
    ordinal: usize,
    parts: &[ComboPart],
) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let Some(close) = after.find('}') else {
            out.push_str(&rest[open..]);
            return out;
        };
        let token = &after[..close];
        match resolve_token(token, roles, combo_index, ordinal, parts) {
            Some(value) => out.push_str(&value),
            None => {
                out.push('{');
                out.push_str(token);
                out.push('}');
            }
        }
        rest = &after[close + 1..];
    }
    out.push_str(rest);
    out
}

fn resolve_token(
    token: &str,
    roles: &str,
    combo_index: usize,
    ordinal: usize,
    parts: &[ComboPart],
) -> Option<String> {
    // `{binN}`, `{binN:width}`, `{binN/field}` — the stem of bin N's source,
    // optionally truncated or reduced to one `_`-separated field.
    let (name, width) = match token.split_once(':') {
        Some((n, w)) => (n, Some(w.parse::<usize>().ok()?)),
        None => (token, None),
    };
    let (name, field) = match name.split_once('/') {
        Some((n, f)) => (n, Some(f.parse::<usize>().ok()?)),
        None => (name, None),
    };
    let value = match name {
        "roles" => roles.to_string(),
        "n" => combo_index.to_string(),
        "i" => ordinal.to_string(),
        _ => {
            let idx: usize = name.strip_prefix("bin")?.parse().ok()?;
            stem(&parts.get(idx)?.source_name)
        }
    };
    let value = match field {
        Some(f) => value.split('_').nth(f)?.to_string(),
        None => value,
    };
    Some(match width {
        Some(w) => value.chars().take(w).collect(),
        None => value,
    })
}

/// A source's bare stem — drop any directory prefix and a single extension.
fn stem(source: &str) -> String {
    let base = source.rsplit(['/', '\\']).next().unwrap_or(source);
    match base.rsplit_once('.') {
        Some((head, _)) if !head.is_empty() => head.to_string(),
        _ => base.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parts(names: &[&str]) -> Vec<ComboPart> {
        names
            .iter()
            .map(|n| ComboPart {
                source_name: (*n).to_string(),
                length: 10,
            })
            .collect()
    }

    #[test]
    fn expands_bin_stems_index_and_ordinal() {
        let p = parts(&["pgga-paqci.gb", "H1_library.gb", "H2_parental.gb"]);
        assert_eq!(
            expand_template("VH-{bin1}-{bin2}_c{n}_p{i}", "roles", 5, 6, &p),
            "VH-H1_library-H2_parental_c5_p6"
        );
    }

    #[test]
    fn field_selector_picks_one_underscore_field() {
        // A level-2 product named from level-1 inputs that already carry a
        // project stem and a backbone suffix.
        let p = parts(&[
            "TGEX-TM.gbk",
            "6H8_VH-PVP_pGGa.gb",
            "G4Sx3.gbk",
            "6H8_VL-VV_pGGa.gb",
        ]);
        assert_eq!(
            expand_template("6H8_{bin1/1}_{bin3/1}_TGEX-TM", "roles", 0, 1, &p),
            "6H8_VH-PVP_VL-VV_TGEX-TM"
        );
        // Out-of-range field is left verbatim rather than silently empty.
        assert_eq!(expand_template("{bin0/9}", "r", 0, 1, &p), "{bin0/9}");
    }

    #[test]
    fn width_truncates_and_unknown_tokens_survive() {
        let p = parts(&["vector.gb", "insert.gb"]);
        assert_eq!(expand_template("{bin1:3}", "r", 0, 1, &p), "ins");
        assert_eq!(expand_template("{nope}-{roles}", "r", 0, 1, &p), "{nope}-r");
        assert_eq!(expand_template("{bin9}", "r", 0, 1, &p), "{bin9}");
    }

    #[test]
    fn unclosed_brace_is_literal() {
        assert_eq!(
            expand_template("a{bin0", "r", 0, 1, &parts(&["v.gb"])),
            "a{bin0"
        );
    }
}
