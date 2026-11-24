# Nangate45 (FreePDK45 Nangate Open Cell Library) example

- `aes_cipher_top_6_final.def.gz`, `aes_cipher_top_6_final.v.gz`: routed DEF and netlist of the aes design from the public
  OpenROAD-flow-scripts CI (job OpenROAD-flow-scripts-Public/master, build 1817, 2026-11-21, artifact
  `final_report_aes_nangate45_base_2026-11-21_23-19.tar.gz`), unchanged apart from gzip. The AES core license is in
  `../aes/LICENSE`.
- `NangateOpenCellLibrary.tech.lef`, `NangateOpenCellLibrary.macro.mod.lef`, `NangateOpenCellLibrary_typical.lib.gz`,
  `NangateOpenCellLibrary.cdl`, `NangateOpenCellLibrary.gds`: from OpenROAD-flow-scripts `flow/platforms/nangate45`
  (the Liberty file gzipped), under the license in `LICENSE`, which is that folder's license file.
