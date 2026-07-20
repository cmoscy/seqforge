#!/usr/bin/env python3
"""Convert tatapov_data xlsx (+ SapI S5) into unaltered-count CSVs for codegen."""
from __future__ import annotations

import csv
import sys
from pathlib import Path

try:
    import openpyxl
except ImportError as e:
    sys.stderr.write("openpyxl required: python3 -m pip install openpyxl\n")
    raise SystemExit(1) from e

MAP = [
    ("potapov2018/FileS1_01h_25C.xlsx", "potapov_2018_01h_25C.csv"),
    ("potapov2018/FileS2_01h_37C.xlsx", "potapov_2018_01h_37C.csv"),
    ("potapov2018/FileS3_18h_25C.xlsx", "potapov_2018_18h_25C.csv"),
    ("potapov2018/FileS4_18h_37C.xlsx", "potapov_2018_18h_37C.csv"),
    ("pryor2021/pone.0238592.s001.xlsx", "pryor_2020_BsaI.csv"),
    ("pryor2021/pone.0238592.s002.xlsx", "pryor_2020_BsmBI.csv"),
    ("pryor2021/pone.0238592.s003.xlsx", "pryor_2020_Esp3I.csv"),
    ("pryor2021/pone.0238592.s004.xlsx", "pryor_2020_BbsI.csv"),
]


def xlsx_to_csv(src: Path, dst: Path) -> None:
    wb = openpyxl.load_workbook(src, read_only=True, data_only=True)
    ws = wb.active
    with dst.open("w", newline="") as f:
        w = csv.writer(f)
        for row in ws.iter_rows(values_only=True):
            w.writerow(list(row))
    wb.close()
    print(f"wrote {dst}")


def main() -> None:
    if len(sys.argv) != 4:
        print(
            f"usage: {sys.argv[0]} <tatapov_data-main> <sapi_s5.xlsx> <out_data_dir>",
            file=sys.stderr,
        )
        raise SystemExit(2)
    root = Path(sys.argv[1])
    sapi = Path(sys.argv[2])
    out = Path(sys.argv[3])
    out.mkdir(parents=True, exist_ok=True)
    for rel, name in MAP:
        xlsx_to_csv(root / rel, out / name)
    xlsx_to_csv(sapi, out / "pryor_2020_SapI_3nt.csv")


if __name__ == "__main__":
    main()
