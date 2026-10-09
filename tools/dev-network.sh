#!/usr/bin/env bash
# A test network on this machine: PumboProx from this checkout and two Pumpkin
# servers behind it (lobby, survival) with Velocity forwarding and PumboBridge.
#
# Usage: tools/dev-network.sh up [path/to/pumpkin] | down | reset | logs [proxy|lobby|survival]
#
#   up      builds the proxy, writes the configs on the first run, copies the
#           plugins from PUMBO_PLUGINS_DIR and starts whatever is not running
#   down    stops the proxy and the servers (SIGTERM, a kill after 30 s)
#   reset   down, then deletes the network directory (worlds, configs, data)
#   logs    follows the logs
#
# Pumpkin is not downloaded: pass the binary after `up` or in PUMPKIN_BIN. It
# runs without arguments in its own directory, with the pumpkin.toml written
# here. Plugins: PumboBridge*.wasm in PUMBO_PLUGINS_DIR goes to both servers, every
# other .wasm there to the proxy.
#
# Variables: PUMBO_PORT (25565; the servers get PORT+1 and PORT+2, the bridge PORT+3),
# PUMBO_PROTOCOL of the servers (777), PUMBO_PLUGINS_DIR (dist),
# PUMBO_NETWORK_DIR (target/dev-network).
set -euo pipefail
shopt -s nullglob

ROOT=$(cd "$(dirname "$0")/.." && pwd)
abs() { case $1 in /*) echo "$1" ;; *) echo "$PWD/$1" ;; esac; }
DIR=$(abs "${PUMBO_NETWORK_DIR:-$ROOT/target/dev-network}")
PLUGINS=$(abs "${PUMBO_PLUGINS_DIR:-$ROOT/dist}")
PORT=${PUMBO_PORT:-25565}
PROTOCOL=${PUMBO_PROTOCOL:-777}
SERVERS="lobby survival"
BRIDGE_PORT=$((PORT + 3))

die() { echo "dev-network: $*" >&2; exit 1; }
rand_hex() { od -An -tx1 -N32 /dev/urandom | tr -d ' \n'; }
# Up to 18 digits, so Pumpkin reads it as a number (an i64).
rand_seed() { local s; s=$(od -An -tu8 -N8 /dev/urandom | tr -d ' \n'); echo "${s:0:18}"; }
port_open() { (exec 3<>"/dev/tcp/127.0.0.1/$1") 2>/dev/null; }
server_port() { if [ "$1" = lobby ]; then echo $((PORT + 1)); else echo $((PORT + 2)); fi; }

pid_of() { cut -d' ' -f1 "$DIR/$1/pid"; }

# The pid file holds "<pid> <program name>"; a pid that now belongs to another
# program (after a reboot) does not count. Linux cuts the name to 15 characters.
running() {
  local pid name comm
  [ -f "$DIR/$1/pid" ] || return 1
  read -r pid name < "$DIR/$1/pid"
  kill -0 "$pid" 2>/dev/null || return 1
  comm=$(basename "$(ps -p "$pid" -o comm= 2>/dev/null)")
  [ -n "$comm" ] && case $name in "$comm"*) true ;; *) false ;; esac
}

start() { # <dir name> <program> [args...]
  (
    cd "$DIR/$1" || exit 1
    nohup "${@:2}" < /dev/null >> console.log 2>&1 &
    echo "$! $(basename "$2")" > pid
  )
}

wait_up() { # <dir name> <port>
  for _ in $(seq 1 240); do
    port_open "$2" && { echo "$1: up on 127.0.0.1:$2 (pid $(pid_of "$1"))"; return; }
    kill -0 "$(pid_of "$1")" 2>/dev/null || die "$1 stopped, see $DIR/$1/console.log"
    sleep 0.5
  done
  die "$1 does not listen on $2 after 120 s, see $DIR/$1/console.log"
}

stop() { # <dir name>
  local pid
  if ! running "$1"; then rm -f "$DIR/$1/pid"; return; fi
  pid=$(pid_of "$1")
  kill -TERM "$pid"
  for _ in $(seq 1 60); do kill -0 "$pid" 2>/dev/null || break; sleep 0.5; done
  if kill -0 "$pid" 2>/dev/null; then
    echo "$1 did not stop in 30 s, killing pid $pid"
    kill -KILL "$pid"
  fi
  rm -f "$DIR/$1/pid"
  echo "$1: stopped"
}

# The configs, once: a new forwarding secret and bridge key, a seed per server.
create() {
  local s port secret key
  secret=$(rand_hex)
  key=$(rand_hex)
  mkdir -p "$DIR/proxy/plugins"
  (umask 077 && printf %s "$secret" > "$DIR/proxy/forwarding.secret" && printf %s "$key" > "$DIR/proxy/bridge.key")
  cat > "$DIR/proxy/pumboprox.yml" <<EOF
# Written by tools/dev-network.sh. Edit it as you like: up keeps it, reset deletes it.
listener:
  - bind: "127.0.0.1:$PORT"
status:
  motd: "&6PumboProx &7dev network"
login:
  online-mode: per-player      # per-player | true | false
servers:
  lobby: { address: "127.0.0.1:$((PORT + 1))", protocol: $PROTOCOL }
  survival: { address: "127.0.0.1:$((PORT + 2))", protocol: $PROTOCOL }
routing:
  try: [lobby]
forwarding:
  mode: modern
  secret-file: forwarding.secret
plugins:
  dir: plugins
bridge:
  enabled: true
  listen: "127.0.0.1:$BRIDGE_PORT"
EOF
  for s in $SERVERS; do
    port=$(server_port "$s")
    mkdir -p "$DIR/$s/plugins/data/pumbobridge"
    cat > "$DIR/$s/pumpkin.toml" <<EOF
# Written by tools/dev-network.sh. Pumpkin fills in the options missing here.
seed = "$(rand_seed)"

[networking.java]
address = "127.0.0.1:$port"
online_mode = false
encryption = false
motd = "$s"

[networking.bedrock]
enabled = false

[networking.lan_broadcast]
enabled = false

[networking.proxy]
enabled = true

[networking.proxy.velocity]
enabled = true
secret = "$secret"

[commands]
use_tty = false

[plugins]
ask_permission_confirmation = false

[telemetry]
enabled = false
EOF
    cat > "$DIR/$s/plugins/data/pumbobridge/config.yml" <<EOF
# Written by tools/dev-network.sh.
proxy: 127.0.0.1:$BRIDGE_PORT
key: "$key"
EOF
  done
  echo "the network was created in $DIR" > "$DIR/.dev-network"
}

up() {
  local bin=${1:-${PUMPKIN_BIN:-}} s f bridge=0
  [ -f "$DIR/.dev-network" ] || create
  (cd "$ROOT" && cargo build --release -p pumbo-prox)
  for f in "$PLUGINS"/*.wasm; do
    case $(basename "$f") in
      PumboBridge*) for s in $SERVERS; do cp "$f" "$DIR/$s/plugins/"; done; bridge=1 ;;
      *) cp "$f" "$DIR/proxy/plugins/" ;;
    esac
  done
  [ "$bridge" = 1 ] || echo "no PumboBridge*.wasm in $PLUGINS: the servers run without the bridge"
  if ! running proxy; then
    port_open "$PORT" && die "port $PORT is in use"
    start proxy "${CARGO_TARGET_DIR:-$ROOT/target}/release/pumboprox" run pumboprox.yml
  fi
  wait_up proxy "$PORT"
  for s in $SERVERS; do
    if ! running "$s"; then
      [ -n "$bin" ] || die "give the Pumpkin binary: $0 up path/to/pumpkin (or PUMPKIN_BIN)"
      [ -x "$bin" ] || die "$bin is not an executable file"
      port_open "$(server_port "$s")" && die "port $(server_port "$s") is in use"
      start "$s" "$(abs "$bin")"
    fi
  done
  for s in $SERVERS; do wait_up "$s" "$(server_port "$s")"; done
  echo "connect to 127.0.0.1:$PORT; logs: $0 logs"
}

down() {
  local s
  for s in proxy $SERVERS; do stop "$s"; done
}

case ${1:-} in
  up) up "${2:-}" ;;
  down) down ;;
  reset)
    if [ -d "$DIR" ]; then
      [ -f "$DIR/.dev-network" ] || die "$DIR was not created by this script, not deleting it"
      down
      rm -r -- "$DIR"
      echo "deleted $DIR"
    fi
    ;;
  logs)
    case ${2:-all} in
      all) exec tail -n 20 -F "$DIR"/{proxy,lobby,survival}/console.log ;;
      proxy | lobby | survival) exec tail -n 50 -F "$DIR/$2/console.log" ;;
      *) die "logs proxy|lobby|survival" ;;
    esac
    ;;
  *)
    echo "usage: $0 up [path/to/pumpkin] | down | reset | logs [proxy|lobby|survival]" >&2
    exit 2
    ;;
esac
