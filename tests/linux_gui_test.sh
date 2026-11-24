#!/bin/bash
set -u
BIN=$1; OUT=$2; TAB=$3; shift 3
mkdir -p "$OUT"
export XDG_RUNTIME_DIR=${XDG_RUNTIME_DIR:-/run/user/$(id -u)}
SOCK=lsvtest-$$
rm -f "$OUT/$TAB.png" "$OUT/$TAB.txt"
dbus-run-session -- mutter --headless --wayland --no-x11 --virtual-monitor 1600x1000 --wayland-display "$SOCK" -- \
  env WAYLAND_DISPLAY="$SOCK" LSV_SHOT="$OUT/$TAB.png" LSV_SHOT_AFTER=10 LSV_EXIT_AFTER=40 "$BIN" --tab "$TAB" "$@" \
  > "$OUT/$TAB.log" 2>&1
echo "exit $? | $(cat "$OUT/$TAB.txt" 2>/dev/null) | log lines $(wc -l < "$OUT/$TAB.log")"
