#!/usr/bin/env bash
# The same steps as .github/workflows/ci.yml (job "check"), run locally.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

step() { echo; echo "== $*"; }

step fmt
cargo fmt --all --check
(cd tests/spikes/wasm-async/guest && cargo fmt --check)

step clippy
cargo clippy --workspace --all-targets --locked -- -D warnings
(cd tests/spikes/wasm-async/guest && cargo clippy --target wasm32-wasip2 --locked \
  --target-dir "$ROOT/target/spike-guest" -- -D warnings)
# The SDK and plugins as WebAssembly (E5); the host tests build them too.
cargo clippy -p pumbo-sdk -p pumbo-example -p pumbo-host-test-plugin --target wasm32-wasip2 --locked \
  --target-dir "$ROOT/target/wasm-plugins" -- -D warnings

step test
cargo test --workspace --locked --exclude spike-wasm-async
cargo test -p spike-wasm-async --release --locked -- --nocapture --test-threads=1

step deny
cargo deny --locked check

step build-aws-lc
cargo build --release --locked -p spike-rsa
./target/release/spike-rsa

step fuzz-build
# Fuzz targets need nightly and cargo-fuzz; skipped when they are missing.
if cargo +nightly fuzz --version >/dev/null 2>&1; then
  (cd fuzz && cargo +nightly fuzz build -O)
else
  echo "nightly or cargo-fuzz missing, skipped"
fi

echo
echo "local CI: OK"
