#!/bin/bash
# Pixel-accurate comparison of our renderer against MAME, headless.
#
#   tools/mamediff.sh <game> <frame> [frame...]
#
# Captures MAME and our own output at the same frame numbers and the same
# native resolution (496x384), then reports per-frame pixel agreement. Both
# sides run headless; nothing opens a window.
set -u
GAME=${1:?usage: mamediff.sh <game> <frame> [frame...]}
shift
FRAMES=("$@")
ROOT=$(cd "$(dirname "$0")/.." && pwd)
MAME=/tmp/mame-root/usr/lib/mame
SNAP=/tmp/mamediff/$GAME
mkdir -p "$SNAP"

# --- MAME side: snapshot at each requested frame ---
LUA=/tmp/mamediff/$GAME.lua
{
  echo "local want = { $(printf '[%s]=true,' "${FRAMES[@]}") }"
  echo "local last = math.max($(IFS=,; echo "${FRAMES[*]}"))"
  echo "local n = 0"
  echo "emu.register_frame_done(function()"
  echo "  n = n + 1"
  echo "  if want[n] then manager.machine.video:snapshot() end"
  echo "  if n > last then os.exit() end"
  echo "end)"
} > "$LUA"

rm -rf "$SNAP/mame"
SECS=$(( $(printf '%s\n' "${FRAMES[@]}" | sort -n | tail -1) / 55 + 5 ))
( cd "$MAME" && SDL_VIDEODRIVER=offscreen LD_LIBRARY_PATH=/tmp/mamesnap/mamelibs \
    timeout $((SECS * 12)) ./mame "$GAME" -rompath "$ROOT" -seconds_to_run "$SECS" \
    -nothrottle -sound none -skip_gameinfo -noplugins \
    -nvram_directory /tmp/mamesnap/nv -snapshot_directory "$SNAP/mame" \
    -autoboot_script "$LUA" >/dev/null 2>&1 )

# --- Our side, then the diff ---
i=0
for f in "${FRAMES[@]}"; do
  ours=$SNAP/ours_$f.ppm
  timeout 3600 "$ROOT/target/release/shot" "$f" "$ours" single "$ROOT/$GAME.zip" >/dev/null 2>&1
  mame_png=$(printf '%s/mame/%s/%04d.png' "$SNAP" "$GAME" "$i")
  python3 - "$ours" "$mame_png" "$f" <<'PY'
import sys
from PIL import Image
import numpy as np
ours, mame, frame = sys.argv[1], sys.argv[2], sys.argv[3]
try:
    a = np.asarray(Image.open(ours).convert("RGB")).astype(int)
    b = np.asarray(Image.open(mame).convert("RGB")).astype(int)
except Exception as e:
    print(f"frame {frame}: MISSING ({e})")
    sys.exit()
if a.shape != b.shape:
    print(f"frame {frame}: size mismatch {a.shape} vs {b.shape}")
    sys.exit()
d = np.abs(a - b)
total = a.shape[0] * a.shape[1]
exact = int((d.sum(2) == 0).sum())
close = int((d.max(2) <= 8).sum())
print(f"frame {frame}: exact {exact}/{total} ({100*exact/total:.2f}%) "
      f"| within8 {100*close/total:.2f}% | mean|d| {d.mean():.1f} "
      f"| ours {a.reshape(-1,3).mean(0).round(1)} mame {b.reshape(-1,3).mean(0).round(1)}")
PY
  i=$((i + 1))
done
