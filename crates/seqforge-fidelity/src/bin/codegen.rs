//! Refresh committed Potapov/Pryor CSVs (`--fetch` only).
//!
//! Manual step (not `build.rs`). Runtime embeds `data/*.csv` via `include_str!`;
//! this bin only re-downloads / converts upstream xlsx when a maintainer
//! intentionally refreshes the tables.
//!
//! ```text
//! cargo run -p seqforge-fidelity --bin codegen -- --fetch
//! ```

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

struct Spec {
    csv: &'static str,
    len: u8,
}

const SPECS: &[Spec] = &[
    Spec {
        csv: "potapov_2018_18h_25C.csv",
        len: 4,
    },
    Spec {
        csv: "potapov_2018_01h_25C.csv",
        len: 4,
    },
    Spec {
        csv: "potapov_2018_18h_37C.csv",
        len: 4,
    },
    Spec {
        csv: "potapov_2018_01h_37C.csv",
        len: 4,
    },
    Spec {
        csv: "pryor_2020_BsaI.csv",
        len: 4,
    },
    Spec {
        csv: "pryor_2020_BsmBI.csv",
        len: 4,
    },
    Spec {
        csv: "pryor_2020_Esp3I.csv",
        len: 4,
    },
    Spec {
        csv: "pryor_2020_BbsI.csv",
        len: 4,
    },
    Spec {
        csv: "pryor_2020_SapI_3nt.csv",
        len: 3,
    },
];

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let from_root = Path::new("crates/seqforge-fidelity/data").is_dir()
        || Path::new("crates/seqforge-fidelity/src").is_dir();
    let data_dir = if from_root {
        PathBuf::from("crates/seqforge-fidelity/data")
    } else {
        PathBuf::from("data")
    };

    if !args.iter().any(|a| a == "--fetch") {
        eprintln!(
            "usage: cargo run -p seqforge-fidelity --bin codegen -- --fetch\n\
             \n\
             Downloads tatapov_data + Pryor SapI S5, converts to data/*.csv,\n\
             and smoke-parses each matrix. Runtime builds do not need this."
        );
        std::process::exit(2);
    }

    fetch_csvs(&data_dir);

    for spec in SPECS {
        let path = data_dir.join(spec.csv);
        let text = fs::read_to_string(&path).unwrap_or_else(|e| {
            panic!("missing {} after --fetch ({e})", path.display());
        });
        let flat = seqforge_fidelity::csv_matrix::parse_matrix(&text, spec.len);
        let n = 1usize << (2 * spec.len as usize);
        assert_eq!(flat.len(), n * n, "{} size", spec.csv);
        eprintln!("ok {} ({}×{})", spec.csv, n, n);
    }
    eprintln!("CSVs ready in {}", data_dir.display());
}

fn fetch_csvs(data_dir: &Path) {
    fs::create_dir_all(data_dir).expect("mkdir data");
    let tmp = std::env::temp_dir().join("seqforge_fidelity_fetch");
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp).expect("tmpdir");

    eprintln!("fetching tatapov_data…");
    let status = Command::new("curl")
        .args([
            "-sL",
            "-o",
            tmp.join("data.zip").to_str().unwrap(),
            "https://github.com/Edinburgh-Genome-Foundry/tatapov_data/archive/refs/heads/main.zip",
        ])
        .status()
        .expect("curl");
    assert!(status.success(), "curl tatapov_data failed");
    let status = Command::new("unzip")
        .args([
            "-q",
            "-o",
            tmp.join("data.zip").to_str().unwrap(),
            "-d",
            tmp.to_str().unwrap(),
        ])
        .status()
        .expect("unzip");
    assert!(status.success(), "unzip failed");

    eprintln!("fetching Pryor SapI S5…");
    let status = Command::new("curl")
        .args([
            "-sL",
            "-o",
            tmp.join("sapi_s5.xlsx").to_str().unwrap(),
            "https://doi.org/10.1371/journal.pone.0238592.s005",
        ])
        .status()
        .expect("curl s5");
    assert!(status.success(), "curl SapI S5 failed");

    let converter = data_dir.parent().unwrap().join("scripts/xlsx_to_csv.py");
    if !converter.is_file() {
        panic!(
            "--fetch needs crates/seqforge-fidelity/scripts/xlsx_to_csv.py \
             (openpyxl) to convert Excel → CSV"
        );
    }
    let root = tmp.join("tatapov_data-main");
    let status = Command::new("python3")
        .arg(&converter)
        .arg(&root)
        .arg(tmp.join("sapi_s5.xlsx"))
        .arg(data_dir)
        .status()
        .expect("python convert");
    assert!(status.success(), "xlsx→csv conversion failed");
}
