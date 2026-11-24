#!/bin/bash
set -eu
OUT=${1:-dist}
T=${TARGET_DIR:-target}/release
P="$OUT/Layout-Schematic-Visualizer-macos-arm64"
APP="$P/Layout Schematic Visualizer.app"
VER=$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)
plist() { # name executable identifier
  cat <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>$1</string>
  <key>CFBundleDisplayName</key><string>$1</string>
  <key>CFBundleExecutable</key><string>$2</string>
  <key>CFBundleIdentifier</key><string>$3</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$VER</string>
  <key>CFBundleVersion</key><string>$VER</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
EOF
}
mkdir -p "$APP/Contents/MacOS" "$P/examples/aes" "$P/examples/cells"
cp "$T/lsv" "$APP/Contents/MacOS/lsv"
plist "Layout Schematic Visualizer" lsv local.lsv.viewer > "$APP/Contents/Info.plist"
cp "$T/lsv-lvs" "$T/lsv-check" "$T/lsv-func" "$P/"
cp LICENSE THIRD_PARTY_LICENSES.txt "$P/"
cp examples/aes/* "$P/examples/aes/"
cp examples/cells/* "$P/examples/cells/"
cp examples/ASAP7_LICENSE "$P/examples/"
launcher() { # app name, id, viewer arguments (paths relative to the package folder as $E)
  local name=$1 id=$2; shift 2
  local L="$P/$name.app"
  mkdir -p "$L/Contents/MacOS"
  plist "$name" run "$id" > "$L/Contents/Info.plist"
  {
    echo '#!/bin/sh'
    echo 'D="$(cd "$(dirname "$0")/../../.." && pwd)"'
    echo 'E="$D/examples"'
    echo "exec \"\$D/Layout Schematic Visualizer.app/Contents/MacOS/lsv\" $*"
  } > "$L/Contents/MacOS/run"
  chmod +x "$L/Contents/MacOS/run"
  codesign --force -s - "$L"
}
A='"$E/aes'
C='"$E/cells'
launcher "Open aes example" local.lsv.example.aes --tab all --lib $A/asap7sc7p5t_RVT_TT_lite.lib\" --netlist $A/aes_cipher_top_6_final.v.gz\" \
  --lef $A/asap7_tech_1x_201209.lef\" --lef $A/asap7sc7p5t_28_R_1x_220121a.lef\" \
  --def $A/aes_cipher_top_6_final.def.gz\" --cdl $C/asap7sc7p5t_28_R.cdl\" --gds $C/asap7sc7p5t_28_R_220121a.gds\" --cell-lef $C/asap7sc7p5t_28_R_1x_220121a.lef\"
launcher "Open cells example" local.lsv.example.cells --tab cell --lib $A/asap7sc7p5t_RVT_TT_lite.lib\" --cdl $C/asap7sc7p5t_28_R.cdl\" --gds $C/asap7sc7p5t_28_R_220121a.gds\" --cell-lef $C/asap7sc7p5t_28_R_1x_220121a.lef\" --cell AOI22xp5_ASAP7_75t_R
codesign --force -s - "$APP"
codesign --verify --verbose=1 "$APP"
(cd "$OUT" && ditto -c -k --keepParent Layout-Schematic-Visualizer-macos-arm64 Layout-Schematic-Visualizer-macos-arm64.zip)
ls -la "$OUT/Layout-Schematic-Visualizer-macos-arm64.zip"
