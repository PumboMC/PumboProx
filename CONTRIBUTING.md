# Contributing to PumboProx

Thanks for taking the time. Bug reports, fixes, tests, docs and new plugins are all welcome. This page explains how to build the proxy, run the tests and send a pull request.

Found a security problem? Don't open an issue, see [SECURITY.md](SECURITY.md).

## What you need

| | For |
| --- | --- |
| **Rust stable** (1.96 or newer) with the `wasm32-wasip2` target | everything: `rustup target add wasm32-wasip2` (the SDK, the example plugin and the plugins the host tests load are WebAssembly) |
| [`cargo-deny`](https://github.com/EmbarkStudios/cargo-deny) | the license and source check of `tools/ci-local.sh` (`cargo install cargo-deny --locked`) |
| A [Pumpkin](https://github.com/Pumpkin-MC/Pumpkin) server binary | the local test network and the tests against Pumpkin |
| zig and [`cargo-zigbuild`](https://github.com/rust-cross/cargo-zigbuild) (optional) | Linux builds from macOS or another Linux (`tools/build-linux.sh`), the way the release does them |
| Java 25 (optional) | regenerating the protocol tables and the tests against vanilla servers |
| Rust nightly and `cargo-fuzz` (optional) | the fuzz targets in `fuzz/` |
| Docker (optional) | trying the release image (`Dockerfile`) |

The repository builds on Linux, macOS and Windows. The scripts in `tools/` are bash.

## Building

```sh
cargo build --release -p pumbo-prox
./target/release/pumboprox version
```

`pumboprox check-config pumboprox.yml` checks a config without starting the proxy.

## Running the tests

```sh
cargo test --workspace          # all fast tests
tools/ci-local.sh               # what CI runs: fmt, clippy, tests, cargo deny, fuzz build
```

Run `tools/ci-local.sh` before you open a pull request; CI runs the same script on Linux. Fuzz targets are built only when nightly and `cargo-fuzz` are installed.

Tests that start real servers are `#[ignore]`d, since they take minutes. Each file says at the top what it needs and how to run it, for example:

```sh
PUMBO_PUMPKIN_BIN=/path/to/pumpkin PUMBO_PUMPKIN_TEMPLATE=/path/to/pumpkin.toml \
  cargo test -p pumbo-prox --test backends -- --ignored --nocapture --test-threads=1
```

| File in `crates/pumbo-prox/tests/` | Needs |
| --- | --- |
| `backends.rs`, `backends_virtual.rs` | vanilla server jars (`PUMBO_JARS`), a Pumpkin binary (`PUMBO_PUMPKIN_BIN`) |
| `backends_switching.rs` | the same, plus a Paper jar (`PUMBO_PAPER_JAR`) |
| `translate_mv.rs` | Pumpkin 0.2.0 and a vanilla 26.3 server |

`cargo run -p pumbo-datagen --release -- tables` downloads the vanilla jars into `~/.cache/pumbo-datagen` and regenerates the tables in `crates/pumbo-data/tables`; they must not change unless you add a Minecraft version.

## A test network on your machine

`tools/dev-network.sh` starts the proxy from your build and two Pumpkin servers behind it, `lobby` and `survival`, in `target/dev-network/`:

```sh
tools/dev-network.sh up /path/to/pumpkin    # or: PUMPKIN_BIN=/path/to/pumpkin tools/dev-network.sh up
tools/dev-network.sh logs                   # follow all logs; logs proxy|lobby|survival for one
tools/dev-network.sh down                   # stop everything
tools/dev-network.sh reset                  # stop and delete the network (worlds, configs, data)
```

Connect your client to `127.0.0.1:25565`. What `up` sets up:

- the proxy from `cargo build --release -p pumbo-prox`, with Velocity modern forwarding and PumboBridge turned on;
- two servers on 25566 and 25567, listening on `127.0.0.1` only, offline behind the proxy, telemetry off, each with its own random seed;
- a new forwarding secret and bridge key, written into the proxy's files and into every server's `pumpkin.toml` and PumboBridge `config.yml`;
- plugins from `dist/`: `PumboBridge*.wasm` goes to both servers, every other `.wasm` to the proxy. Download them from the plugin releases or build them in the plugin repositories, and put only the PumboBridge build that matches your Pumpkin version there. Without PumboBridge the servers run without the bridge.

`up` on an existing network keeps its configs, worlds and data, copies the plugins from `dist/` again and only starts what is not running. So after rebuilding a plugin, or to try a change in `target/dev-network/proxy/pumboprox.yml` or a server's `pumpkin.toml`, run `down` and `up`. Pumpkin is started as it is, without arguments, in its own folder.

| Variable | Default | |
| --- | --- | --- |
| `PUMPKIN_BIN` | (none) | the Pumpkin binary, if not given after `up` |
| `PUMBO_PORT` | `25565` | the proxy; the servers get `PUMBO_PORT+1` and `+2`, the bridge `+3` |
| `PUMBO_PROTOCOL` | `777` | the protocol of your Pumpkin (777 for 26.3, 776 for 26.2) |
| `PUMBO_PLUGINS_DIR` | `dist` | where the plugin files come from |
| `PUMBO_NETWORK_DIR` | `target/dev-network` | where the network lives |

## Writing plugins

[`plugins/example`](plugins/example) is a small plugin with a command, a config file and a placeholder. Build it with:

```sh
cargo build -p pumbo-example --target wasm32-wasip2 --profile plugin
```

The plugin interface is [`wit/pumbo-prox.wit`](wit/pumbo-prox.wit), the Rust SDK is [`crates/pumbo-sdk`](crates/pumbo-sdk) and the shared service types are in [`crates/pumbo-contracts`](crates/pumbo-contracts). The Pumbo plugins (PumboFilter, PumboAuth, PumboBans, PumboPerms, PumboBridge) have their own repositories in [PumboMC](https://github.com/PumboMC); changes to a plugin go there.

## Code style

- `cargo fmt --all` before you commit.
- `cargo clippy --workspace --all-targets -- -D warnings` must be clean. The workspace denies `unwrap`, `expect`, `panic!`, indexing that can panic and `unsafe` outside tests: return an error or handle the case instead.
- Comments and docs are in English and say why, not what. Keep them short.
- Tests next to the code they test; tests that need a real server are `#[ignore]` with the reason.
- New dependencies must pass `cargo deny check` (permissive licenses only, from crates.io). Say in the pull request why the dependency is needed.

## Commits and pull requests

1. Fork the repository and branch off `main`: `feat/<topic>` for something new, `fix/<topic>` for a bug, `chore/<topic>` for the rest.
2. Keep one topic per pull request. Small pull requests get reviewed faster.
3. Write commit messages as one plain sentence that says what the change does, starting with a capital letter and without a full stop, for example `Send the next gate's command tree when a player changes virtual worlds`. Prefix the part of the code when that helps: `pumbo-bridge-proto: re-export uuid and ciborium`. We don't use Conventional Commits prefixes (`feat:`, `fix:`).
4. Add or update tests for what you change, and run `tools/ci-local.sh`.
5. Open the pull request against `main` and fill in the template: what changes, why, and how you tested it (with the client and Pumpkin versions if you tried it in the game).

A maintainer reviews it, may ask for changes, and merges it when CI is green.

## License of contributions

PumboProx (the proxy and its crates) is licensed under GPL-3.0-only. The parts plugins build on are dual-licensed under MIT OR Apache-2.0, so plugins can use any license: `crates/pumbo-sdk`, `crates/pumbo-contracts`, `crates/pumbo-bridge-proto`, `crates/pumbo-common`, `wit/` and `plugins/example`.

By sending a pull request you agree that your contribution is licensed under the license of the files it changes: GPL-3.0-only for the proxy, MIT OR Apache-2.0 for the parts listed above. There is no separate contributor agreement.

## Questions

Open an [issue](https://github.com/PumboMC/PumboProx/issues) for bugs and ideas. If you are not sure whether something is a bug, or want to talk about a bigger change before you write it, open an issue first and describe what you have in mind.
