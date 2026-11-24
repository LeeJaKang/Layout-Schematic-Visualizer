# SKY130 (sky130_fd_sc_hd) example

- `aes_cipher_top_6_final.def.gz`, `aes_cipher_top_6_final.v.gz`: routed DEF and netlist of the aes design from the public
  OpenROAD-flow-scripts CI (job OpenROAD-flow-scripts-Public/master, build 1817, 2026-11-21, artifact
  `final_report_aes_sky130hd_base_2026-11-21_23-31.tar.gz`), unchanged apart from gzip. The AES core license is in
  `../aes/LICENSE`.
- `sky130_fd_sc_hd.tlef`, `sky130_fd_sc_hd_merged.lef.gz`, `sky130_fd_sc_hd__tt_025C_1v80.lib.gz`, `sky130hd.cdl.gz`,
  `sky130_fd_sc_hd.gds.gz`: from OpenROAD-flow-scripts `flow/platforms/sky130hd` (gzipped). They are the SKY130
  standard cells, released under the Apache License 2.0 in `LICENSE`; the files keep their own copyright notices.
- `magic_pex/`: post-layout netlists of eight cells of `sky130_fd_sc_hd.gds.gz`, extracted with the open-source Magic
  8.3.706 (commit 1d42dfc) and the `sky130A.tech` of open_pdks (commit 2fdf40d). For each cell, `.pex.spice` is the
  ext2spice output, `.res.ext` holds the resistor network with node points and `.ext` the plain extraction. Commands:
  `gds read sky130_fd_sc_hd.gds; load <cell>; extract all; ext2sim labels on; ext2sim; extresist threshold 0;
  extresist mindelay 0; extresist all; ext2spice lvs; ext2spice cthresh 0; ext2spice extresist on; ext2spice -o
  <cell>.pex.spice`.
