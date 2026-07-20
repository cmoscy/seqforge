//! Product naming — from the recipe's template (default = the bin roles joined),
//! with a `#n` suffix disambiguating a library. The full provenance-template
//! machinery (decision 9 collisions) is deferred; A1 keeps it simple.

use seqforge_core::{Fragment, Recipe};

use super::NamedProduct;

pub(super) fn name_products(recipe: &Recipe, products: Vec<Fragment>) -> Vec<NamedProduct> {
    let base = recipe.name_template.clone().unwrap_or_else(|| {
        recipe
            .bins
            .iter()
            .map(|b| b.role.clone())
            .collect::<Vec<_>>()
            .join("+")
    });
    let multi = products.len() > 1;
    products
        .into_iter()
        .enumerate()
        .map(|(i, fragment)| {
            let name = if multi {
                format!("{base} #{}", i + 1)
            } else {
                base.clone()
            };
            NamedProduct { name, fragment }
        })
        .collect()
}
