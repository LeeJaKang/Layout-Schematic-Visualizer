# IHP SG13G2 (sg13g2_stdcell) example

- `aes_cipher_top_6_final.def.gz`, `aes_cipher_top_6_final.v.gz`: routed DEF and netlist of the aes design from the public
  OpenROAD-flow-scripts CI (job OpenROAD-flow-scripts-Public/master, build 1817, 2026-11-21, artifact
  `final_report_aes_ihp-sg13g2_base_2026-11-21_23-30.tar.gz`), unchanged apart from gzip. The AES core license is in
  `../aes/LICENSE`.
- `sg13g2_tech.lef`, `sg13g2_stdcell.lef`, `sg13g2_stdcell_typ_1p20V_25C.lib.gz`, `sg13g2_stdcell.cdl`,
  `sg13g2_stdcell.gds.gz`: from OpenROAD-flow-scripts `flow/platforms/ihp-sg13g2` (gzipped where marked), the IHP
  Open PDK standard cells (IHP GmbH), released under the Apache License 2.0 in `LICENSE`.
