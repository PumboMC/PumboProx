[PumboProx](../README.md) · [Get started](getting-started.md) · [Configuration](configuration.md) · [Servers from the proxy](servers.md) · [Commands](commands.md) · [PumboBridge](bridge.md) · [Bridge protocol](bridge-protocol.md) · [How it works](how-it-works.md) · [Writing plugins](writing-plugins.md) · **Plugin reference**

---

# Plugin reference

What a plugin declares, what it gets and what the proxy holds it to. Start with [Writing plugins](writing-plugins.md) and the [example plugin](../plugins/example); the full interface is [`wit/pumbo-prox.wit`](../wit/pumbo-prox.wit).

## API version

The proxy speaks plugin API **0.1** (the WIT package `pumbo:prox@0.1.0`). Your manifest says `api: "0.1"`. The proxy compares the two as plain text: a plugin with any other value does not load, and the log says to use a build of the plugin for this proxy version. There is no range and no promise beyond that: when a release changes the API version, plugins need a new build against the SDK of that release.

Build against the SDK at the tag of the proxy release you build for (`vX.Y.Z` below), not the moving `main` branch:

```toml
[dependencies]
pumbo-sdk = { git = "https://github.com/PumboMC/PumboProx", tag = "vX.Y.Z" }
```

## Files

| Where | What |
| --- | --- |
| `plugins/<any name>.wasm` | The plugin, with its manifest, default config and language files built in by `pumbo_sdk::embed!` |
| `plugins/<id>.yml` | Optional: a manifest next to the `.wasm` replaces the built-in one |
| `plugins/<id>/` | `config.yml` and `lang/`, written from the built-in defaults on the first start. The plugin sees this folder as `/config`, read-only |
| `plugins/data/<id>/` | The plugin's own data, seen as `/data`, read and write |

The plugin sees nothing else: no other files, no sockets, no environment variables. Outgoing HTTPS goes through the host, see [Limits](#limits).

When a new version of the plugin brings a new default `config.yml` or language file, the proxy replaces the old file only if nobody changed it. A changed file stays, and the log names the options or texts it lacks.

## Manifest

```yaml
id: pumbo-example
version: 0.1.0
api: "0.1"
events: [status, login, disconnect]
short-name: example
permissions:
  - { node: pumbo.example.hello, default: true, description: Use /hello }
```

| Key | Default | What it does |
| --- | --- | --- |
| `id` | required | Lowercase letters, digits and `-`; not `data` or `proxy` |
| `version` | required | Your version, shown in `/prox plugins` |
| `api` | required | `"0.1"`, see [API version](#api-version) |
| `events` | `[]` | The events the plugin gets, see [Events](#events) |
| `gate` | none | `{ name, priority, on-failure }`: the plugin holds new players in a virtual world until it lets them pass (needs the `gate` event). The owner lists required gates in `plugins.required-gates` |
| `profile` | none | `{ priority, on-failure }` for the `profile` event (needs it in `events`) |
| `on-failure` | see [Events](#events) | `allow` or `deny` per event kind: what a crash or a late answer means |
| `http` | `[]` | Host names the plugin may reach over HTTPS |
| `sensitive-commands` | `[]` | Commands that never reach a server, even while the plugin is down (for example `login`) |
| `command-filter` | `[]` | Server commands the plugin sees in `backend-command`; `"*"` for all |
| `profile-properties` | `[]` | Profile properties the plugin may change, for example `textures` |
| `provides` | `[]` | Services it offers: `{ service: "ns:name", version: "1.0", max-timeout-ms }` |
| `uses` | `[]` | Services it calls: `{ service, version, required }` |
| `publishes`, `subscribes` | `[]` | Bus topics, `namespace:name@1.0` |
| `placeholders` | none | `namespace` (`[a-z0-9]+`), `keys` (`name`, `scope`, `mode` `push` or `pull`, `public`, `fallback`, `ttl-ms`, `timeout-ms`) and `suggested-aliases` |
| `permission-provider` | `false` | The plugin answers permission lookups; the owner picks it with `permissions.provider` |
| `permissions-write` | `false` | The plugin wants to change permissions; needs the owner's grant, see [Grants](#grants) |
| `short-name` | none | The plugin's commands under `/pumbo <short-name>` |
| `short-alias` | none | A short root command, such as `pf` for `/pumbofilter`; needs `short-name` |
| `permissions` | `[]` | `{ node, default, description }` per permission node |

The manifest rejects unknown keys. Permission nodes must start with `pumbo.<id without pumbo->.`, `pumbo.<id>.` or `<id>:`, and nodes under `pumbo.proxy.` or `minecraft:` cannot be on by default.

## Events

A plugin gets an event only when its manifest lists it. Commands, timers, service calls, placeholders, permission lookups and bus events need no entry: the matching manifest keys or registrations bring them. `on-placeholder` has 50 ms and `on-permission-load` 2000 ms.

| Event | Export | Deadline (ms) | On failure |
| --- | --- | --- | --- |
| `handshake` | `on-handshake` | 100 | allow |
| `status` | `on-status` | 100 | allow |
| `pre-login` | `on-pre-login` | 5000 | deny |
| `profile` | `on-profile` | 5000 | deny |
| `login` | `on-login` | 5000 | deny |
| `gate` | `on-gate` | `plugins.gate-timeout-ms` (120000) | deny |
| `server-connect` | `on-server-connect` | 2000 | allow |
| `server-connected` | `on-server-connected` | 5000 | allow |
| `server-kicked` | `on-server-kicked` | 2000 | allow |
| `disconnect` | `on-disconnect` | 5000 | allow |
| `chat` | `on-chat` | 200 | allow |
| `backend-command` | `on-backend-command` | 200 | allow |
| `plugin-message` | `on-plugin-message` | 200 | allow |
| `context-changed` | `on-context-changed` | 5000 | allow |
| `virtual-input` | `on-virtual-input` | 5000 | allow |

Also exported: `init` and `describe` at start, `shutdown`, `on-command`, `on-timer`, `on-service-call`, `on-service-changed`, `on-bus-event` and `on-admin-action`, and optionally `suggest` for tab completion of your commands (in releases after 0.2.5-beta).

The owner can change deadlines in `plugins.timeouts` and make `on-failure` stricter in `plugins.on-failure`, never looser ([Configuration](configuration.md#plugins)). Every handler is an `async func`.

## Limits

| What | Default | Set by the owner in |
| --- | --- | --- |
| Memory per plugin | 256 MB | `plugins.memory-mb` |
| Work without returning to the proxy | 200 ms (2 s while loading) | `plugins.budget-ms`, `plugins.load-budget-ms` |
| Time to start | 10 s | `plugins.init-timeout-ms` |
| Events waiting | 4096 | `plugins.mailbox` |
| Timers | 1024 | `plugins.timers-per-plugin` |
| Log lines | 100 a second | `plugins.log-lines-per-second` |
| Service calls | 1 s deadline when the caller sets none, 8 calls deep | `services.default-timeout-ms`, `services.max-depth` |
| HTTP | HTTPS only, hosts from `http`, public addresses only, at most 5 redirects to listed hosts, 10 s, 1 MB, 16 requests at a time | `plugins.http-allow-private` |

A plugin that runs past its budget, runs out of memory or traps is restarted after 1, 5 and 30 seconds. After 5 crashes in 5 minutes it stays off until `/prox plugins reload <id>`.

## Reload

`/prox plugins reload <id>` loads the code of the plugin's file again while players stay online. A changed manifest is not applied: the log names the changed keys, and they wait for a restart of the proxy. A file that now holds another plugin is refused.

## Grants

Some things a manifest can only ask for. The owner allows them in `pumboprox.yml`:

```yaml
plugins:
  shop:
    grants: [permissions-write]
```

`permissions-write` lets the plugin change permissions through `permissions.set`. It can only set its own nodes, never `*`, `pumbo.proxy.`, `minecraft:` or `group.` nodes, and the proxy logs every change. Without the grant the plugin gets a warning at start and an error from the call. A new grant needs a restart of the proxy.

## Tests

`pumbo_sdk::testing` is a fake host for `cargo test`, without WebAssembly: players, commands, permissions, services, placeholders, the bus, config files and the virtual world. State is per thread; call `reset()` at the start of each test. The [example plugin](../plugins/example/src/lib.rs) has tests to copy.
