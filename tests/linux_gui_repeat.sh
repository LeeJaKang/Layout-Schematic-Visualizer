#!/bin/bash
set -u
BIN=$1; OUT=$2; BACKEND=$3; RUNS=$4; shift 4
mkdir -p "$OUT"
export XDG_RUNTIME_DIR=${XDG_RUNTIME_DIR:-/run/user/$(id -u)}
ok=0; hung=0; other=0
for i in $(seq 1 "$RUNS"); do
  SOCK=lsvrep-$$-$i
  P="$OUT/${BACKEND}_$i"
  rm -f "$P.png" "$P.txt"
  if [ "$BACKEND" = x11 ]; then
    dbus-run-session -- mutter --headless --wayland --virtual-monitor 1600x1000 --wayland-display "$SOCK" -- \
      sh -c 'unset WAYLAND_DISPLAY; exec env WINIT_UNIX_BACKEND=x11 "$@"' sh \
      env LSV_SHOT="$P.png" LSV_SHOT_AFTER=8 LSV_EXIT_AFTER=30 "$BIN" "$@" > "$P.log" 2>&1
  else
    dbus-run-session -- mutter --headless --wayland --no-x11 --virtual-monitor 1600x1000 --wayland-display "$SOCK" -- \
      env WAYLAND_DISPLAY="$SOCK" LSV_SHOT="$P.png" LSV_SHOT_AFTER=8 LSV_EXIT_AFTER=30 "$BIN" "$@" > "$P.log" 2>&1
  fi
  if [ -s "$P.png" ]; then ok=$((ok+1)); r=ok
  elif grep -q "test watchdog" "$P.log"; then hung=$((hung+1)); r=HUNG
  else other=$((other+1)); r=FAILED; fi
  echo "run $i: $r $(cat "$P.txt" 2>/dev/null)"
done
echo "$BACKEND: $RUNS runs, saved $ok, hung $hung, other failures $other"
