# GF180MCU (gf180mcu_fd_sc_mcu9t5v0) example

- `aes_cipher_top_6_final.def.gz`, `aes_cipher_top_6_final.v.gz`: routed DEF and netlist of the aes design from the public
  OpenROAD-flow-scripts CI (job OpenROAD-flow-scripts-Public/master, build 1817, 2026-11-21, artifact
  `final_report_aes_gf180_base_2026-11-21_23-19.tar.gz`), unchanged apart from gzip. The AES core license is in
  `../aes/LICENSE`.
- `gf180mcu_5LM_1TM_9K_9t_tech.lef`, `gf180mcu_5LM_1TM_9K_9t_sc.lef.gz`, `gf180mcu_fd_sc_mcu9t5v0__tt_025C_5v00.lib.gz`:
  from OpenROAD-flow-scripts `flow/platforms/gf180` (gzipped where marked).
- `gf180mcu_fd_sc_mcu9t5v0.cdl.gz`, `gf180mcu_fd_sc_mcu9t5v0.gds.gz`: the per-cell CDL and GDS files of
  https://github.com/google/globalfoundries-pdk-libs-gf180mcu_fd_sc_mcu9t5v0 (commit 3781fd2951da), joined into one file
  each and gzipped.
- The GF180MCU standard cells are released under the Apache License 2.0 in `LICENSE`; the files keep their own copyright notices.
