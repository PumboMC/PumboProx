#!/usr/bin/env bash
# Writes the pumpkin.toml template the backend tests read (PUMBO_PUMPKIN_TEMPLATE,
# or the file next to the binary): Pumpkin is started once in the binary's
# directory with the options the tests need and fills in all the others.
#
# Usage: tools/pumpkin-template.sh path/to/pumpkin [port]   (port 25645)
set -euo pipefail
bin=$(cd "$(dirname "$1")" && pwd)/$(basename "$1")
port=${2:-25645}
dir=$(dirname "$bin")
[ -x "$bin" ] || { echo "pumpkin-template: $bin is not an executable file" >&2; exit 1; }
port_open() { (exec 3<>"/dev/tcp/127.0.0.1/$1") 2>/dev/null; }
port_open "$port" && { echo "pumpkin-template: port $port is in use" >&2; exit 1; }
# A fixed seed: the translation test throws arrows, and on some worlds they land
# too early, so every run uses the same world.
cat > "$dir/pumpkin.toml" <<TOML
seed = "1793726720637615494"

[networking.java]
address = "127.0.0.1:$port"
online_mode = false
encryption = false

[networking.bedrock]
enabled = false

[networking.lan_broadcast]
enabled = false

[networking.proxy]
enabled = false

[networking.proxy.velocity]
enabled = false
secret = ""

[commands]
use_tty = false

[telemetry]
enabled = false
TOML
fifo=$(mktemp -u)
mkfifo "$fifo"
(cd "$dir" && exec "$bin" < "$fifo" > "$dir/template.log" 2>&1) &
pid=$!
exec 4> "$fifo"
rm -f "$fifo"
for _ in $(seq 1 240); do
  port_open "$port" && break
  kill -0 "$pid" 2>/dev/null || { cat "$dir/template.log" >&2; echo "pumpkin-template: Pumpkin stopped" >&2; exit 1; }
  sleep 0.5
done
port_open "$port" || { kill "$pid"; echo "pumpkin-template: Pumpkin does not listen on $port" >&2; exit 1; }
echo stop >&4
for _ in $(seq 1 60); do kill -0 "$pid" 2>/dev/null || break; sleep 0.5; done
kill -0 "$pid" 2>/dev/null && { echo "pumpkin-template: Pumpkin did not stop, killing pid $pid" >&2; kill -KILL "$pid"; }
exec 4>&-
wait "$pid" 2>/dev/null || true
grep -q '^\[networking.proxy\]$' "$dir/pumpkin.toml" || { echo "pumpkin-template: Pumpkin did not write $dir/pumpkin.toml" >&2; exit 1; }
echo "$dir/pumpkin.toml"
