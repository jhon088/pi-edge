#!/usr/bin/env bash
#
# Start the PX Kiosk demo: core, menu, screen.
#
# Three existing binaries in the order they depend on each other. This script
# owns no logic of its own — it does not open the database, mint a token or
# know what a sale is. If it fails it says which step failed and stops, because
# a kiosk that starts half-configured is worse than one that does not start.
set -euo pipefail

# The repo this script lives in, not wherever it was invoked from. Every path
# below is absolute from here, which is the whole point: the daemon and the
# window have to agree on one database directory.
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DB="$ROOT/data/pi-edge.db"
SOCK="$ROOT/data/.pi-edge/d.sock"

say() { printf '\033[1;36m==>\033[0m %s\n' "$1"; }
die() { printf '\033[1;31mdemo-kiosk: %s\033[0m\n' "$1" >&2; exit 1; }

command -v cargo >/dev/null || die "cargo is not on PATH."
command -v pnpm  >/dev/null || die "pnpm is not on PATH."

cd "$ROOT"

DAEMON_PID=""
cleanup() {
  if [ -n "$DAEMON_PID" ] && kill -0 "$DAEMON_PID" 2>/dev/null; then
    say "stopping edge-daemon ($DAEMON_PID)"
    kill "$DAEMON_PID" 2>/dev/null || true
    wait "$DAEMON_PID" 2>/dev/null || true
  fi
}
trap cleanup EXIT INT TERM

say "building core binaries"
cargo build -p edge-daemon -p edge-cli

# The OS lock is the only authority on whether a daemon already holds this
# installation. A socket file proves nothing — a crashed daemon leaves one
# behind — so try to talk to it and believe the answer.
if "$ROOT/target/debug/edge-cli" --db "$DB" core-info >/dev/null 2>&1; then
  say "edge-daemon is already serving $DB"
else
  say "starting edge-daemon on $DB"
  "$ROOT/target/debug/edge-daemon" --db "$DB" &
  DAEMON_PID=$!
  # Up to ten seconds, asking twenty times a second. Long enough for a cold
  # start, short enough that a daemon which is never coming back says so.
  ready=""
  for _ in $(seq 1 200); do
    if "$ROOT/target/debug/edge-cli" --db "$DB" core-info >/dev/null 2>&1; then
      ready=yes
      break
    fi
    kill -0 "$DAEMON_PID" 2>/dev/null \
      || die "edge-daemon exited during startup; its output is above."
    sleep 0.05
  done
  [ -n "$ready" ] || die "edge-daemon never answered on $SOCK after 10s."
fi

# Safe to run over an installation that already has a demo in it, including
# one seeded before the retail vertical existed: the seed reconciles its
# version against what the core already holds, so an unchanged dataset costs
# nothing and a changed one takes the next version and leaves the old snapshot
# intact (ADR 047). There is no `rm` here, and there never should be.
say "seeding the demo catalog — restaurant and retail, offline"
"$ROOT/target/debug/edge-cli" --db "$DB" demo-seed

say "socket in use: $SOCK"
say "opening PX Kiosk"
cd "$ROOT/apps/tauri-pos"
# Same installation as the daemon above, stated rather than inferred: the
# window must never be left to resolve a relative path against whatever
# directory `tauri dev` happens to run it from.
PI_EDGE_DB="$DB" pnpm tauri dev
