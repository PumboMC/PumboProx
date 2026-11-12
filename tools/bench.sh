#!/usr/bin/env bash
# A short load benchmark: the test network of tools/dev-network.sh (proxy and
# two Pumpkin servers) and BENCH_BOTS offline bots from pumbo-testclient's
# loadgen example, which join at BENCH_RATE a second and stay BENCH_HOLD
# seconds. No threshold: shared machines differ by tens of percent between
# runs, so the result is only recorded.
#
# Usage: tools/bench.sh path/to/pumpkin
#
# Variables: BENCH_BOTS (200), BENCH_RATE (20), BENCH_HOLD (30), BENCH_MODE
# (idle | move | chat), BENCH_OUT (target/bench; loadgen.txt, joins.csv,
# result.md), PUMBO_PORT (25700, the servers get PORT+1..PORT+3).
# Each run adds a row to docs/test-results.md.
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
bin=${1:?usage: tools/bench.sh path/to/pumpkin}
bots=${BENCH_BOTS:-200}
rate=${BENCH_RATE:-20}
hold=${BENCH_HOLD:-30}
mode=${BENCH_MODE:-idle}
out=${BENCH_OUT:-$ROOT/target/bench}
export PUMBO_PORT=${PUMBO_PORT:-25700}
export PUMBO_NETWORK_DIR=$out/network
export PUMBO_ONLINE_MODE=false
# No plugins: the proxy and the servers alone.
export PUMBO_PLUGINS_DIR=$out/no-plugins
net="$ROOT/tools/dev-network.sh"
mkdir -p "$out" "$PUMBO_PLUGINS_DIR"
[ -d "$PUMBO_NETWORK_DIR" ] && "$net" reset
trap '"$net" down >/dev/null 2>&1 || true' EXIT

# The first start writes the configs; the bots then need higher per-address
# limits than the test network has, and no managed servers (their default
# ports are 25700-25799).
"$net" up "$bin"
"$net" down
cfg=$PUMBO_NETWORK_DIR/proxy/pumboprox.yml
limits='limits: { connections-per-ip-per-second: 100, concurrent-per-ip: 100 }'
grep -qxF "$limits" "$cfg" || { echo "bench: $cfg has no line: $limits" >&2; exit 1; }
n=$((bots * 2))
sed -i.bak "s/^limits: .*/limits: { connections-per-ip-per-second: $n, concurrent-per-ip: $n }/" "$cfg"
rm -f "$cfg.bak"
printf 'managed-servers:\n  enabled: false\n' >> "$cfg"
"$net" up "$bin"

(cd "$ROOT" && cargo build --release --locked -p pumbo-testclient --example loadgen)
loadgen=${CARGO_TARGET_DIR:-$ROOT/target}/release/examples/loadgen
"$loadgen" "127.0.0.1:$PUMBO_PORT" 777 "$bots" "$hold" "$mode" "$rate" "$out/joins.csv" | tee "$out/loadgen.txt"
"$net" down

summary=$(grep '^SUMMARY ' "$out/loadgen.txt")
field() { echo "$summary" | tr ' ' '\n' | sed -n "s/^$1=//p"; }
commit=$(git -C "$ROOT" rev-parse --short HEAD)
os="$(uname -s) $(uname -m)"
date=$(date -u +%Y-%m-%dT%H:%MZ)
row="| $date | $commit | $os | $bots $mode at $rate/s, $hold s | $(field ok) | $(field stayed) | $(field ramp_s) | $(field p50) | $(field p95) | $(field max) |"
header='| date | commit | machine | bots | joined | stayed | ramp s | join p50 ms | p95 ms | max ms |
|---|---|---|---|---|---|---|---|---|---|'
results=$ROOT/docs/test-results.md
if [ ! -f "$results" ]; then
  printf '# Test results\n\n## Load benchmark\n\n`tools/bench.sh`: bots through the proxy to Pumpkin 0.2.0, without a pass mark.\n\n%s\n' "$header" > "$results"
fi
echo "$row" >> "$results"
printf '### Load benchmark\n\n%s\n%s\n' "$header" "$row" > "$out/result.md"
grep '^ERROR ' "$out/loadgen.txt" | sed 's/^/    /' >> "$out/result.md" || true
cat "$out/result.md"
