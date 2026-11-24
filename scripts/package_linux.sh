#!/bin/bash
set -eu
OUT=${1:-dist}
T=${TARGET_DIR:-target}/x86_64-unknown-linux-gnu/release
P="$OUT/Layout-Schematic-Visualizer-linux-x86_64"
mkdir -p "$P/examples/aes" "$P/examples/cells"
cp "$T/lsv" "$T/lsv-lvs" "$T/lsv-check" "$T/lsv-func" "$P/"
cp LICENSE THIRD_PARTY_LICENSES.txt "$P/"
cp examples/aes/* "$P/examples/aes/"
cp examples/cells/* "$P/examples/cells/"
cp examples/ASAP7_LICENSE "$P/examples/"
cat > "$P/open_aes.sh" <<'EOF'
#!/bin/sh
cd "$(dirname "$0")"
E=examples/aes
C=examples/cells
exec ./lsv --tab all --lib $E/asap7sc7p5t_RVT_TT_lite.lib --netlist $E/aes_cipher_top_6_final.v.gz \
  --lef $E/asap7_tech_1x_201209.lef --lef $E/asap7sc7p5t_28_R_1x_220121a.lef \
  --def $E/aes_cipher_top_6_final.def.gz --cdl $C/asap7sc7p5t_28_R.cdl --gds $C/asap7sc7p5t_28_R_220121a.gds --cell-lef $C/asap7sc7p5t_28_R_1x_220121a.lef "$@"
EOF
cat > "$P/open_cells.sh" <<'EOF'
#!/bin/sh
cd "$(dirname "$0")"
C=examples/cells
exec ./lsv --tab cell --lib examples/aes/asap7sc7p5t_RVT_TT_lite.lib --cdl $C/asap7sc7p5t_28_R.cdl --gds $C/asap7sc7p5t_28_R_220121a.gds --cell-lef $C/asap7sc7p5t_28_R_1x_220121a.lef --cell AOI22xp5_ASAP7_75t_R "$@"
EOF
chmod +x "$P/lsv" "$P/lsv-lvs" "$P/lsv-check" "$P/lsv-func" "$P/open_aes.sh" "$P/open_cells.sh"
tar -C "$OUT" -czf "$OUT/Layout-Schematic-Visualizer-linux-x86_64.tar.gz" Layout-Schematic-Visualizer-linux-x86_64
ls -la "$OUT/Layout-Schematic-Visualizer-linux-x86_64.tar.gz"
