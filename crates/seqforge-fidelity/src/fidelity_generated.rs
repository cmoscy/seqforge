//! Embed committed Potapov/Pryor CSVs and expose dense `&'static [u16]` tables.
//!
//! Data: Potapov 2018 / Pryor 2020 ligation-frequency counts, unaltered,
//! redistributed under CC BY-ND 4.0 via EGF tatapov_data (+ Pryor S5 SapI).
//! See ../ATTRIBUTION.md.

use std::sync::OnceLock;

use crate::csv_matrix::parse_matrix;
use crate::types::Dataset;

fn view(csv: &'static str, len: u8, cell: &'static OnceLock<Vec<u16>>) -> &'static [u16] {
    cell.get_or_init(|| parse_matrix(csv, len)).as_slice()
}

/// Potapov 2018 T4 25C 18h (default)
static CSV_T4_25C_18H: &str = include_str!("../data/potapov_2018_18h_25C.csv");
static DECODED_T4_25C_18H: OnceLock<Vec<u16>> = OnceLock::new();

/// Potapov 2018 T4 25C 01h
static CSV_T4_25C_01H: &str = include_str!("../data/potapov_2018_01h_25C.csv");
static DECODED_T4_25C_01H: OnceLock<Vec<u16>> = OnceLock::new();

/// Potapov 2018 T4 37C 18h
static CSV_T4_37C_18H: &str = include_str!("../data/potapov_2018_18h_37C.csv");
static DECODED_T4_37C_18H: OnceLock<Vec<u16>> = OnceLock::new();

/// Potapov 2018 T4 37C 01h
static CSV_T4_37C_01H: &str = include_str!("../data/potapov_2018_01h_37C.csv");
static DECODED_T4_37C_01H: OnceLock<Vec<u16>> = OnceLock::new();

/// Pryor 2020 BsaI
static CSV_BSAI: &str = include_str!("../data/pryor_2020_BsaI.csv");
static DECODED_BSAI: OnceLock<Vec<u16>> = OnceLock::new();

/// Pryor 2020 BsmBI
static CSV_BSMBI: &str = include_str!("../data/pryor_2020_BsmBI.csv");
static DECODED_BSMBI: OnceLock<Vec<u16>> = OnceLock::new();

/// Pryor 2020 Esp3I
static CSV_ESP3I: &str = include_str!("../data/pryor_2020_Esp3I.csv");
static DECODED_ESP3I: OnceLock<Vec<u16>> = OnceLock::new();

/// Pryor 2020 BbsI
static CSV_BBSI: &str = include_str!("../data/pryor_2020_BbsI.csv");
static DECODED_BBSI: OnceLock<Vec<u16>> = OnceLock::new();

/// Pryor 2020 SapI 3-nt (PLoS ONE S5)
static CSV_SAPI: &str = include_str!("../data/pryor_2020_SapI_3nt.csv");
static DECODED_SAPI: OnceLock<Vec<u16>> = OnceLock::new();

pub fn matrix(dataset: Dataset) -> &'static [u16] {
    match dataset {
        Dataset::T4_25C_18h => view(CSV_T4_25C_18H, 4, &DECODED_T4_25C_18H),
        Dataset::T4_25C_01h => view(CSV_T4_25C_01H, 4, &DECODED_T4_25C_01H),
        Dataset::T4_37C_18h => view(CSV_T4_37C_18H, 4, &DECODED_T4_37C_18H),
        Dataset::T4_37C_01h => view(CSV_T4_37C_01H, 4, &DECODED_T4_37C_01H),
        Dataset::BsaI => view(CSV_BSAI, 4, &DECODED_BSAI),
        Dataset::BsmBI => view(CSV_BSMBI, 4, &DECODED_BSMBI),
        Dataset::Esp3I => view(CSV_ESP3I, 4, &DECODED_ESP3I),
        Dataset::BbsI => view(CSV_BBSI, 4, &DECODED_BBSI),
        Dataset::SapI => view(CSV_SAPI, 3, &DECODED_SAPI),
    }
}
