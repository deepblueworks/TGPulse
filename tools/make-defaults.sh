#!/usr/bin/env bash
# Regenerates the shipped NVRAM defaults.
#
#   tools/make-defaults.sh [game ...]
#
# These machines keep region, cabinet, difficulty and coinage in battery-backed
# RAM rather than on DIP switches, and most of them ship set to Japan -- which
# is also what selects the Japanese text. There is no register to poke: the
# only supported way to change a setting is the game's own test menu.
#
# So each game has a script under tools/defaults/ that drives that menu through
# the debugger and writes the result out as the game's default. Regenerating is
# reproducible and the scripts are readable, which a captured binary blob would
# not be.
set -u
cd "$(dirname "$0")/.."

BIN=target/release/tgpulse
[ -x "$BIN" ] || { echo "build first: cargo build --release" >&2; exit 1; }

games=("$@")
if [ ${#games[@]} -eq 0 ]; then
  games=()
  for script in tools/defaults/*.dbg; do
    games+=("$(basename "$script" .dbg)")
  done
fi

status=0
for game in "${games[@]}"; do
  script="tools/defaults/$game.dbg"
  if [ ! -f "$script" ]; then
    echo "$game: no script at $script" >&2
    status=1
    continue
  fi
  if [ ! -f "roms/$game.zip" ]; then
    echo "$game: skipped, no romset" >&2
    continue
  fi
  # Start from nothing, so the script sees the settings the game itself
  # initialises rather than a previous run's. That includes the shipped
  # default: the debugger loads it when it exists, and the scripts navigate
  # from the game's own cold-boot settings, so it has to be out of the way.
  rm -f "nvram/$game.nv"
  parked=""
  if [ -f "nvram/defaults/$game.nv" ]; then
    parked="nvram/defaults/$game.nv.parked"
    mv "nvram/defaults/$game.nv" "$parked"
  fi
  if out=$("$BIN" "$game" --debug -f "$script" 2>&1); then
    echo "$game: $(echo "$out" | grep '^nvram' || echo 'no default written')"
    rm -f "$parked"
  else
    echo "$game: FAILED" >&2
    echo "$out" | tail -3 >&2
    # Keep the previous default rather than whatever the failed run left.
    [ -n "$parked" ] && mv "$parked" "nvram/defaults/$game.nv"
    status=1
  fi
  rm -f "nvram/$game.nv"
done
exit $status
