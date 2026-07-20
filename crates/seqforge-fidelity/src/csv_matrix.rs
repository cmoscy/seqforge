//! Parse Potapov/Pryor ligation-frequency CSVs into dense base-4-indexed tables.
//!
//! Matrices have no quoted commas — a simple split is enough (same shape as
//! EGF tatapov_data / SI extracts).

use std::collections::BTreeMap;

/// Parse an unaltered count CSV into a row-major `n×n` table of `u16` counts,
/// indexed by base-4 overhang encoding (`n = 4^len`).
pub fn parse_matrix(csv: &str, len: u8) -> Vec<u16> {
    let mut lines = csv.lines().filter(|l| !l.trim().is_empty());
    let header = lines.next().expect("header");
    let cols: Vec<&str> = split_csv_line(header);
    assert_eq!(cols.first().map(|s| s.trim()), Some("Overhang"));
    let overhangs: Vec<String> = cols
        .iter()
        .skip(1)
        .map(|s| s.trim().to_ascii_uppercase())
        .collect();
    let n = 1usize << (2 * len as usize);
    assert_eq!(overhangs.len(), n, "expected {n} columns for len={len}");

    let mut col_index: BTreeMap<String, usize> = BTreeMap::new();
    for (i, oh) in overhangs.iter().enumerate() {
        assert_eq!(oh.len(), len as usize, "col {oh}");
        col_index.insert(oh.clone(), i);
    }

    let mut flat = vec![0u16; n * n];
    let mut rows_seen = 0usize;
    for line in lines {
        let cells = split_csv_line(line);
        let row_oh = cells[0].trim().to_ascii_uppercase();
        assert_eq!(row_oh.len(), len as usize, "row {row_oh}");
        let ri = encode(row_oh.as_bytes(), len as usize);
        assert_eq!(cells.len() - 1, n, "row {row_oh} cell count");
        for (ci_oh, cell) in overhangs.iter().zip(cells.iter().skip(1)) {
            let ci = encode(ci_oh.as_bytes(), len as usize);
            let v: u32 = cell
                .trim()
                .parse()
                .unwrap_or_else(|_| panic!("bad count {cell:?} at {row_oh}/{ci_oh}"));
            assert!(
                v <= u32::from(u16::MAX),
                "count {v} exceeds u16 at {row_oh}/{ci_oh}"
            );
            flat[ri * n + ci] = v as u16;
        }
        rows_seen += 1;
        let _ = col_index; // silence — validated via encode coverage
    }
    assert_eq!(rows_seen, n, "row count");
    flat
}

fn split_csv_line(line: &str) -> Vec<&str> {
    line.split(',').collect()
}

fn encode(seq: &[u8], len: usize) -> usize {
    assert_eq!(seq.len(), len);
    let mut idx = 0usize;
    for &b in seq {
        let d = match b {
            b'A' | b'a' => 0,
            b'C' | b'c' => 1,
            b'G' | b'g' => 2,
            b'T' | b't' => 3,
            _ => panic!("non-ACGT in {seq:?}"),
        };
        idx = (idx << 2) | d;
    }
    idx
}
