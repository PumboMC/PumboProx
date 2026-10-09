[PumboProx](../README.md) · [Get started](getting-started.md) · [Servers from the proxy](servers.md) · [Commands](commands.md) · [PumboBridge](bridge.md) · [How it works](how-it-works.md) · **Writing plugins**

---

# Writing plugins

A PumboProx plugin is a WebAssembly component built against the [`pumbo:prox` WIT interface](../wit/pumbo-prox.wit). The [SDK (`pumbo-sdk`)](../crates/pumbo-sdk) and the interface are MIT/Apache-2.0, so your plugin can use any license.

| | What | Where |
| --- | --- | --- |
| 🧰 | **SDK**: commands, events, config, languages, services, `embed!`, a fake host for tests | [`crates/pumbo-sdk`](../crates/pumbo-sdk) |
| 📜 | **Interface**: the contract between the proxy and a plugin, usable from any language that builds WebAssembly components | [`wit/pumbo-prox.wit`](../wit/pumbo-prox.wit) |
| 🤝 | **Contracts**: shared service types, so plugins can talk to each other | [`crates/pumbo-contracts`](../crates/pumbo-contracts) |
| 🧩 | **Shared library**: YAML config, languages and message styles used by the Pumbo plugins | [`crates/pumbo-common`](../crates/pumbo-common) |
| 🌱 | **Example plugin** to copy | [`plugins/example`](../plugins/example) |

The SDK is not on crates.io yet. Add it from Git:

```toml
[dependencies]
pumbo-sdk = { git = "https://github.com/PumboMC/PumboProx" }
```

```sh
rustup target add wasm32-wasip2
cargo build -p pumbo-example --target wasm32-wasip2 --profile plugin
```

[`plugins/example`](../plugins/example) is a small plugin with a command, a config file and a placeholder. Start there.

Plugins that use [PumboPerms](https://github.com/PumboMC/PumboPerms) ranks and placeholders: see its [INTEGRATION.md](https://github.com/PumboMC/PumboPerms/blob/main/INTEGRATION.md).
