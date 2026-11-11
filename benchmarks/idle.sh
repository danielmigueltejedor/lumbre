#!/usr/bin/env sh
# Measures what an idle Lumbre daemon costs: resident memory, threads and CPU time.
#
#   benchmarks/idle.sh [seconds idle, default 20]
#
# Builds the release daemon, starts it in a throw-away LUMBRE_HOME, makes one status request and
# one tool request (so lazily initialised code is paged in), waits, and reports. Exits non-zero
# if resident memory is over the 20 MiB goal. No model is involved: this is the cost of having
# Lumbre installed and ready, not of using it.
set -eu

idle="${1:-20}"
limit_kib=20480
root="$(cd "$(dirname "$0")/.." && pwd)"
home="$(mktemp -d)"
port=47191
trap 'kill "$pid" 2>/dev/null || true; rm -rf "$home"' EXIT

cargo build --release --quiet -p lumbred --manifest-path "$root/Cargo.toml"
LUMBRE_HOME="$home" "$root/target/release/lumbred" --port "$port" 2>"$home/log" &
pid=$!
sleep 1

rss() { ps -o rss= -p "$pid" | tr -d ' '; }
threads() {
  if [ -d "/proc/$pid/task" ]; then ls "/proc/$pid/task" | wc -l | tr -d ' '; else ps -M -p "$pid" | tail -n +2 | wc -l | tr -d ' '; fi
}

started="$(rss)"
token="$(cat "$home/api-token")"
call() { curl -sS -o /dev/null -H "Authorization: Bearer $token" "$@"; }
call "http://127.0.0.1:$port/v1/status"
call -X POST -H 'content-type: application/json' -d '{"text":"what time is it"}' "http://127.0.0.1:$port/v1/chat"
sleep "$idle"
settled="$(rss)"

echo "platform:            $(uname -sm)"
echo "binary size:         $(wc -c <"$root/target/release/lumbred" | tr -d ' ') bytes"
echo "RSS after start:     $started KiB"
echo "RSS after ${idle} s idle: $settled KiB  (goal: under $limit_kib KiB)"
echo "threads while idle:  $(threads)"
echo "CPU time used:       $(ps -o time= -p "$pid" | tr -d ' ')"

[ "$settled" -le "$limit_kib" ] || { echo "FAIL: idle memory is over the goal" >&2; exit 1; }
