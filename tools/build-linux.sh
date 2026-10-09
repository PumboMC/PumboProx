#!/usr/bin/env bash
# Cross-build for Linux (aarch64 and x86_64, glibc) from macOS or Linux with
# cargo-zigbuild. Needs `cargo install cargo-zigbuild --locked`, zig on PATH
# (or ZIG_DIR), `rustup target add aarch64-unknown-linux-gnu x86_64-unknown-linux-gnu`.
# Usage: tools/build-linux.sh [package] (default spike-rsa)
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
PKG="${1:-spike-rsa}"
if [[ -n "${ZIG_DIR:-}" ]]; then
  export PATH="$ZIG_DIR:$PATH"
fi
command -v zig >/dev/null || { echo "zig not on PATH (set ZIG_DIR)" >&2; exit 1; }

cd "$ROOT"
for t in aarch64-unknown-linux-gnu x86_64-unknown-linux-gnu; do
  start=$(date +%s)
  cargo zigbuild --release --locked -p "$PKG" --target "$t"
  echo "built $t in $(( $(date +%s) - start )) s"
done
