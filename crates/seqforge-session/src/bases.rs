//! IUPAC base validation — shared by the GUI's silent typing filter and the
//! strict CLI/agent parser, so both surfaces accept exactly the same alphabet.
//!
//! Validation is the *command* layer's job, so a malformed CLI/agent insert is
//! rejected with a clear error rather than silently corrupting a buffer; the
//! GUI keystroke path pre-filters before it ever reaches a command.

/// IUPAC nucleotide alphabet (DNA + ambiguity codes). Shared by the silent
/// GUI filter and the strict CLI/agent parser.
pub const IUPAC: &[u8] = b"ACGTURYSWKMBDHVN";

/// Keep only IUPAC codes, upper-cased; drop everything else (whitespace, junk).
/// Used for typed bases and plain-text OS paste.
pub fn filter_bases(s: &str) -> String {
    s.chars()
        .filter_map(|c| {
            let u = c.to_ascii_uppercase();
            (u.is_ascii() && IUPAC.contains(&(u as u8))).then_some(u)
        })
        .collect()
}

/// Uppercase, strip ASCII whitespace, validate IUPAC. Returns the clean bytes
/// or an error naming the first offending character (CLI/agent path).
pub fn parse_bases(s: &str) -> Result<Vec<u8>, seqforge_core::DispatchError> {
    let mut out = Vec::with_capacity(s.len());
    for ch in s.chars() {
        if ch.is_ascii_whitespace() {
            continue;
        }
        let up = ch.to_ascii_uppercase();
        if up.is_ascii() && IUPAC.contains(&(up as u8)) {
            out.push(up as u8);
        } else {
            return Err(seqforge_core::DispatchError::InvalidInput(format!(
                "`{ch}` is not an IUPAC nucleotide code"
            )));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_bases_rejects_non_iupac() {
        assert!(parse_bases("ATGC").is_ok());
        assert!(parse_bases("AT X").is_err());
    }

    #[test]
    fn filter_bases_drops_junk_and_upcases() {
        assert_eq!(filter_bases("atgc"), "ATGC");
        assert_eq!(filter_bases("AT-GC 1"), "ATGC");
    }
}
