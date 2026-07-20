# Data attribution

The ligation-frequency matrices in this crate are **unaltered counts** from
published supplementary data, redistributed under **Creative Commons
Attribution-NoDerivatives 4.0 International (CC BY-ND 4.0)**.

## Sources

1. **Potapov et al. 2018** — *Comprehensive Profiling of Four Base Overhang
   Ligation Fidelity by T4 DNA Ligase and Application to DNA Assembly.*
   ACS Synth. Biol. 7(11):2665–2674. https://doi.org/10.1021/acssynbio.8b00333
   Four-base T4 tables (01h/18h × 25 °C/37 °C), via
   [tatapov_data/potapov2018](https://github.com/Edinburgh-Genome-Foundry/tatapov_data).

2. **Pryor et al. 2020** — *Enabling one-pot Golden Gate assemblies of
   unprecedented complexity using data-optimized assembly design.*
   PLoS ONE 15(9):e0238592. https://doi.org/10.1371/journal.pone.0238592
   Four-base enzyme tables (BsaI, BsmBI, Esp3I, BbsI) via
   [tatapov_data/pryor2021](https://github.com/Edinburgh-Genome-Foundry/tatapov_data);
   SapI three-base table from Supporting Information S5
   (`10.1371/journal.pone.0238592.s005`).

3. **Packaging** — EGF [tatapov_data](https://github.com/Edinburgh-Genome-Foundry/tatapov_data)
   (CC BY-ND 4.0) provides the unaltered Excel extracts used as codegen input.

## License note

CC BY-ND 4.0 requires attribution and forbids sharing **adapted** material.
This crate stores the published counts without modification; only the
encoding (CSV → Rust `static` arrays) changes for compilation. Scoring code is
original SeqForge (MIT).
