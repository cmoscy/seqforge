//! The bin-token grammar: `SOURCE[@5′..3′][span]` → a [`Bin`](seqforge_core::Bin).
//!
//! One grammar, both shells (ROADMAP decisions 21 and 27). It used to live in
//! `seqforge-cli`, which meant the socket could not accept the same tokens the
//! command line did — so an assembly authored by an agent had to be spelled as
//! a recipe file while the identical CLI invocation used tokens. Parsing is
//! pure (it produces `SourceRef` values, resolving nothing), so it belongs
//! beside the engine that consumes it.
//!
//! Errors are `String` rather than `anyhow`: this crate is a library, and the
//! CLI adds its own context on the way out.

use std::path::Path;

/// Parse a bin token `SOURCE[@FROM..TO]` into a [`Bin`] (decision 26).
///
/// - `SOURCE` may be a **glob** (`parts/*.gb`) → every match becomes a source in
///   the **same** bin (bulk, shared 5′→3′ prepare).
/// - `@E1..E2` is the digest 5′→3′ walk (`EcoRI..PstI`, `BsaI..BsaI`, `EcoRI@410..BamHI`).
/// - `@pcr:fwd..rev` / `@as-is` for PCR and pass-through.
/// - Trailing `[5′..3′]` is a per-source span override (rare `@pos` exception).
///
/// Without `@…`, defaults to `Digest(E..E)` from `--enzymes` (GG sugar:
/// bare path + `--enzymes BsaI` → `BsaI..BsaI`), else `AsIs`.
pub fn parse_bin_token(
    token: &str,
    default_enzymes: Option<&str>,
) -> Result<seqforge_core::Bin, String> {
    use seqforge_core::{Bin, Boundary, PrepareKind, Source, SourceRef, SpanEnds};

    // Split off a trailing [span] (per-input override).
    let (rest, span_override) = match (token.find('['), token.ends_with(']')) {
        (Some(open), true) => {
            let inner = &token[open + 1..token.len() - 1];
            let span = inner
                .parse::<SpanEnds>()
                .map_err(|e| format!("bad span override in {token:?}: {e}"))?;
            (&token[..open], Some(span))
        }
        _ => (token, None),
    };

    // Split off @prepare / @5′..3′.
    let (source, prepare) = match rest.split_once('@') {
        Some((src, spec)) => (src, parse_prepare(spec)?),
        None => {
            let prep = match default_enzymes {
                Some(e) => {
                    let names: Vec<String> = normalize_enzymes(e)
                        .split_whitespace()
                        .map(str::to_string)
                        .collect();
                    match names.as_slice() {
                        [] => PrepareKind::AsIs,
                        [one] => PrepareKind::Digest {
                            five_prime: Boundary::enzyme(one.clone()),
                            three_prime: Boundary::enzyme(one.clone()),
                        },
                        [a, b, ..] => PrepareKind::Digest {
                            five_prime: Boundary::enzyme(a.clone()),
                            three_prime: Boundary::enzyme(b.clone()),
                        },
                    }
                }
                None => PrepareKind::AsIs,
            };
            (rest, prep)
        }
    };
    if source.is_empty() {
        return Err(format!("empty source in bin token {token:?}"));
    }

    // `buffer:<n>` names an open document in a running SeqForge rather than a
    // file on disk — the other half of `SourceRef`, and the reason a recipe
    // authored in the workbench round-trips through the CLI. Resolving one needs
    // a session, so the local file resolver rejects it with that message; over
    // the socket it resolves against the buffer store.
    let sources: Vec<Source> = if let Some(handle) = source.strip_prefix("buffer:") {
        let id: u64 = handle.trim().parse().map_err(|_| {
            format!("bad buffer handle in {token:?}: expected `buffer:<n>`, got {handle:?}")
        })?;
        vec![Source {
            ref_: SourceRef::Buffer(seqforge_core::BufferId(id)),
            pin: None,
            span: span_override.clone(),
        }]
    } else {
        let paths = crate::expand_glob(source);
        if paths.is_empty() {
            return Err(format!("no files match {source:?}"));
        }
        paths
            .into_iter()
            .map(|p| Source {
                ref_: SourceRef::Path(p),
                pin: None,
                span: span_override.clone(),
            })
            .collect()
    };

    Ok(Bin {
        role: bin_role(source),
        sources,
        prepare,
    })
}
/// A bin role from the source token: a glob → its parent directory name; a plain
/// path → its file stem; `buffer:<n>` → `buffer<n>`.
pub fn bin_role(source: &str) -> String {
    if let Some(handle) = source.strip_prefix("buffer:") {
        return format!("buffer{}", handle.trim());
    }
    if source.contains('*') {
        Path::new(source)
            .parent()
            .and_then(|p| p.file_name())
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "bin".to_string())
    } else {
        Path::new(source)
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| source.to_string())
    }
}
pub fn parse_prepare(spec: &str) -> Result<seqforge_core::PrepareKind, String> {
    use seqforge_core::{Boundary, PrepareKind, SpanEnds};
    let spec = spec.trim();
    if spec.eq_ignore_ascii_case("as-is") || spec.eq_ignore_ascii_case("asis") {
        return Ok(PrepareKind::AsIs);
    }
    if let Some(pair) = spec.strip_prefix("pcr:") {
        let span: SpanEnds = pair
            .parse()
            .map_err(|e| format!("pcr prepare needs fwd..rev, got {spec:?}: {e}"))?;
        let (fwd, rev) = match (&span.five_prime, &span.three_prime) {
            (
                Boundary::EnzymeSite {
                    enzyme: f,
                    at: None,
                },
                Boundary::EnzymeSite {
                    enzyme: r,
                    at: None,
                },
            ) => (f.clone(), r.clone()),
            _ => {
                return Err(format!(
                    "pcr prepare needs primer names (fwd..rev), got {spec:?}"
                ));
            }
        };
        return Ok(PrepareKind::Pcr { fwd, rev });
    }
    // Optional legacy `digest:` prefix, then 5′..3′.
    let span_text = spec.strip_prefix("digest:").unwrap_or(spec);
    let span: SpanEnds = span_text
        .parse()
        .map_err(|e| format!("bad prepare {spec:?}: {e}"))?;
    Ok(PrepareKind::Digest {
        five_prime: span.five_prime,
        three_prime: span.three_prime,
    })
}
/// Enzyme lists accept `,`, `+`, or `/` separators; the query grammar wants whitespace.
pub fn normalize_enzymes(list: &str) -> String {
    list.replace([',', '+', '/'], " ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use seqforge_core::{Bin, Boundary, PrepareKind, Source, SourceRef, SpanEnds};

    /// Parity: the bin a CLI token parses to is byte-identical to the bin a GUI
    /// would author, and it survives serde + the 5′→3′ Display/FromStr round-trip.
    #[test]
    fn cli_token_equals_gui_authored_bin() {
        let bin = parse_bin_token("pUC19.gb@BamHI..EcoRI", None).unwrap();

        let expected = Bin {
            role: "pUC19".into(),
            sources: vec![Source {
                ref_: SourceRef::Path("pUC19.gb".into()),
                pin: None,
                span: None,
            }],
            prepare: PrepareKind::Digest {
                five_prime: Boundary::enzyme("BamHI"),
                three_prime: Boundary::enzyme("EcoRI"),
            },
        };
        assert_eq!(bin, expected, "CLI token must equal the GUI-authored bin");

        let json = serde_json::to_string(&bin).unwrap();
        assert_eq!(serde_json::from_str::<Bin>(&json).unwrap(), bin);

        let span = SpanEnds::new(Boundary::enzyme("BamHI"), Boundary::enzyme("EcoRI"));
        assert_eq!(span.to_string(), "BamHI..EcoRI");
        assert_eq!("BamHI..EcoRI".parse::<SpanEnds>().unwrap(), span);
    }

    /// A per-input `[5′..3′]` with an `@pos` occurrence rides each source.
    /// The other half of `SourceRef`. `buffer:<n>` was documented in
    /// plans/assembly.md but unimplemented, so the CLI could only ever name a
    /// path — half a `core` type was GUI-only in practice (ROADMAP decision 27).
    #[test]
    fn buffer_token_parses_to_a_buffer_source() {
        let bin = parse_bin_token("buffer:3@BsaI..BsaI", None).unwrap();
        assert_eq!(bin.sources.len(), 1);
        assert_eq!(
            bin.sources[0].ref_,
            SourceRef::Buffer(seqforge_core::BufferId(3)),
            "a buffer handle must not be mistaken for a path"
        );
        assert_eq!(bin.role, "buffer3");
    }

    /// Parity: the value survives the wire, so a recipe authored in the
    /// workbench and one authored on the command line are the same document.
    #[test]
    fn buffer_source_round_trips_through_serde() {
        let bin = parse_bin_token("buffer:7", None).unwrap();
        let json = serde_json::to_string(&bin).unwrap();
        assert!(json.contains("\"buffer\""), "serde tag: {json}");
        let back: seqforge_core::Bin = serde_json::from_str(&json).unwrap();
        assert_eq!(back, bin);
    }

    #[test]
    fn a_malformed_buffer_handle_is_an_error_not_a_path() {
        let err = parse_bin_token("buffer:xyz", None).unwrap_err();
        assert!(err.contains("bad buffer handle"), "{err}");
    }

    #[test]
    fn per_input_span_override_is_carried_on_the_source() {
        let bin = parse_bin_token("geneC.gb@EcoRI..BamHI[EcoRI@410..BamHI]", None).unwrap();
        let span = bin.sources[0].span.as_ref().expect("span override");
        assert_eq!(span.to_string(), "EcoRI@410..BamHI");
    }

    /// A glob source expands to N sources in **one** bin (bulk).
    #[test]
    fn glob_expands_to_n_sources_in_one_bin() {
        let dir = std::env::temp_dir().join(format!("sf_glob_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["a.gb", "b.gb", "c.gb", "skip.txt"] {
            std::fs::write(dir.join(name), b">x\nACGT\n").unwrap();
        }
        let pattern = format!("{}/*.gb", dir.display());
        let bin = parse_bin_token(&format!("{pattern}@EcoRI..EcoRI"), None).unwrap();
        assert_eq!(bin.sources.len(), 3, "3 .gb files, not the .txt");
        let combos: usize = [bin.sources.len()].iter().product();
        assert_eq!(combos, 3);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Bare path + `--enzymes BsaI` sugars to `Digest { BsaI..BsaI }`.
    #[test]
    fn golden_gate_bare_path_sugars_to_bsai_span() {
        let bin = parse_bin_token("vector.gb", Some("BsaI")).unwrap();
        assert_eq!(
            bin.prepare,
            PrepareKind::Digest {
                five_prime: Boundary::enzyme("BsaI"),
                three_prime: Boundary::enzyme("BsaI"),
            }
        );
    }
}
