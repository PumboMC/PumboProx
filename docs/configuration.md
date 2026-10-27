[PumboProx](../README.md) · [Get started](getting-started.md) · **Configuration** · [Servers from the proxy](servers.md) · [Commands](commands.md) · [PumboBridge](bridge.md) · [How it works](how-it-works.md) · [Writing plugins](writing-plugins.md)

---

# Configuration

Everything the proxy reads from `pumboprox.yml`. The file written on the first start holds only a few sections (see [Get started](getting-started.md#the-config)); every key below has a default and can be left out.

- `pumboprox` reads `pumboprox.yml` in the directory it starts in; `pumboprox run <file>` reads another file. Relative paths in the file (secrets, `plugins/`, `servers/`, the server icon) are relative to that directory too.
- `pumboprox check-config [file]` checks the file and the configs of the plugins without starting the proxy. Keys the proxy does not know are mostly ignored, so a misspelled key keeps its default: run the check after editing.
- `reload` in the console (or `/prox reload`) reads the file again. Players stay online. The **Applies** column says what a change needs:
  - **reload**: a `reload` is enough. Settings of a login (limits, `login`, forwarding, authentication, translation) apply to connections made after the reload.
  - **restart**: the proxy reads it once at start; stop the proxy and start it again.

Times are in milliseconds unless the key says otherwise (`-secs`, `-seconds`). Texts take `&` colour codes (`&a`, `&#39FF88`) or text tags (`<green>`).

## Contents

[listener](#listener) · [status](#status) · [login](#login) · [authentication](#authentication) · [forwarding](#forwarding) · [servers](#servers) · [routing](#routing) · [forced-hosts](#forced-hosts) · [translation](#translation) · [storage](#storage) · [limits](#limits) · [logging](#logging) · [switching](#switching) · [commands](#commands) · [plugin-messages](#plugin-messages) · [bungeecord-channel](#bungeecord-channel) · [messages](#messages) · [virtual](#virtual) · [tab](#tab) · [pumbox](#pumbox) · [bridge](#bridge) · [managed-servers](#managed-servers) · [plugins](#plugins) · [services](#services) · [permissions](#permissions) · [placeholders](#placeholders) · [server-group](#server-group) · [style](#style)

## listener

A list: one entry per address the proxy listens on. Without any entry the proxy accepts no players.

```yaml
listener:
  - bind: "0.0.0.0:25565"
  - bind: "0.0.0.0:25566"
    proxy-protocol: true
    trusted-proxies: ["10.0.0.0/8"]
```

| Key | Default | Description | Applies |
| --- | --- | --- | --- |
| `bind` | required | `ip:port` to listen on | restart |
| `transport` | `java-tcp` | Listener type; `java-tcp` (Java Edition over TCP) is the only one | restart |
| `proxy-protocol` | `false` | Read a PROXY protocol header (v1 or v2) before the handshake, for a proxy or DDoS protection in front. Needs `trusted-proxies` | restart |
| `trusted-proxies` | `[]` | CIDR ranges (`203.0.113.0/24`, `2001:db8::/32`) allowed to send that header. From other addresses no header is read: those connections count as direct | restart |

## status

What the server list shows.

| Key | Default | Description | Applies |
| --- | --- | --- | --- |
| `motd` | the PumboProx banner with the version | MOTD, two lines separated by `\n` | reload |
| `max-players` | `500` | The maximum shown in the server list | reload |
| `version-name` | empty | Version text in the server list; empty shows `PumboProx <oldest>-<newest>` | reload |
| `favicon` | `server-icon.png` | Server icon, a 64x64 PNG. Empty: no icon. When the default file is missing the proxy shows the Pumbo icon | reload |

## login

| Key | Default | Description | Applies |
| --- | --- | --- | --- |
| `online-mode` | `per-player`, with a warning at every start (the file of a new install sets `true`) | `true`: every player needs a premium account. `false`: nobody is checked. `per-player`: premium names are checked, other names join offline. With `false` or `per-player` anyone can join under any name without a password: use them only with PumboAuth and `auth` in `plugins.required-gates` | reload |
| `encrypt-offline` | `false` | Encrypt the connections of offline players too | reload |
| `prevent-proxy-connections` | `false` | Send the player's IP address with the premium check, so the session service can refuse a login from another address | reload |
| `compression-threshold` | `256` | Packets from this size in bytes are compressed between client and proxy; negative turns compression off | reload |
| `accept-transfers` | `false` | Accept clients sent here by a `transfer` from another server | reload |

## authentication

| Key | Default | Description | Applies |
| --- | --- | --- | --- |
| `service` | `mojang` | Premium check; `mojang` (the Minecraft session service) is the only one | reload |
| `timeout-ms` | `5000` | How long one check may take | reload |

## forwarding

How the proxy tells a server the player's real name, UUID, skin and address.

| Key | Default | Description | Applies |
| --- | --- | --- | --- |
| `mode` | `modern` | `modern`: signed with a shared secret. `none`: nothing is forwarded; allowed only for servers on `127.0.0.1` (tests with vanilla servers) | reload |
| `secret-file` | `forwarding.secret` | File with the secret (`modern`). A missing file is created with a random secret; on Linux and macOS it must not be readable by other users (mode `0600`) | reload |
| `secret-env` | not set | Name of an environment variable holding the secret; wins over `secret-file` | reload |

Every server needs the same secret: see [Your own Pumpkin servers](getting-started.md#your-own-pumpkin-servers). Servers created by the proxy get it on their own.

## servers

Servers you run yourself, by name. Names that differ only in letter case are refused.

```yaml
servers:
  lobby: { address: "127.0.0.1:25566", protocol: 777 }
  survival:
    address: "10.0.0.5:25566"
    protocol: 777
    signed-chat-cancel: kick
```

| Key | Default | Description | Applies |
| --- | --- | --- | --- |
| `address` | required | `host:port`; IPv6 in brackets (`[::1]:25566`). A server outside loopback and private networks gives a warning: reach it through a tunnel | reload |
| `protocol` | not set | The server's protocol number (`777` is 26.3). With it the proxy can translate older clients for this server; without it a client joins with its own version unchanged | reload |
| `chat-session-forwarding` | `true` | Pass the client's chat session to the server. `false`: the server treats all chat as unsigned | reload |
| `signed-chat-cancel` | `drop` | What happens when a plugin cancels a signed chat message on this server. `drop`: the message is dropped and the player stays. `kick`: the player is disconnected with `messages.signed-chat-blocked`. Vanilla and Paper reject every later signed message after a dropped one until the player reconnects; Pumpkin does not check | reload |
| `allow-transfers` | `false` | Pass the server's `transfer` packets on to the client. Without it a transfer to another server of the network becomes a normal switch and any other transfer is dropped | reload |

Servers of [`managed-servers`](#managed-servers) join this list on their own; a server here wins over one of the proxy with the same name.

## routing

Which server a player joins.

| Key | Default | Description | Applies |
| --- | --- | --- | --- |
| `selector` | `try` | How a server is picked; `try` is the only one | reload |
| `try` | `[]` | Server names in the order players try them; offline servers are skipped, and a player dropped from one goes to the next | reload |

`/prox route` shows and edits this from the game or the console: see [The way into the network](servers.md#the-way-into-the-network).

## forced-hosts

A map from the address a player typed (lower case, without a trailing dot) to a list of servers tried instead of `routing.try`. Applies on reload.

```yaml
forced-hosts:
  "minigames.example.org": [arena, lobby]
```

## translation

| Key | Default | Description | Applies |
| --- | --- | --- | --- |
| `chain` | `[multiversion, viaproxy, passthrough]` | Translation providers in order; the first one with a decision for a client and a server wins. `multiversion`: clients from 1.21 on a 26.3 server, inside the proxy. `passthrough`: the client and the server speak the same version | reload |

A provider named in `chain` reads its options from a top-level section of the same name. The `viaproxy` provider sends clients to an external translation service:

| Key (section `viaproxy`) | Default | Description | Applies |
| --- | --- | --- | --- |
| `address` | `127.0.0.1:25568` | `ip:port` of the service | reload |
| `route` | `backend-mismatch` | `below-native`: only clients older than `min-native-protocol`. `backend-mismatch`: also clients whose version the chosen server does not speak | reload |
| `min-native-protocol` | `767` | Oldest protocol the proxy serves itself | reload |

## storage

| Key | Default | Description | Applies |
| --- | --- | --- | --- |
| `backend` | `memory` | Key-value store of the proxy; `memory` is the only one | reload |

## limits

Protection built into the proxy, before any plugin. The per-address limits count by the player's IP address (behind `proxy-protocol`, the address from the header).

| Key | Default | Description | Applies |
| --- | --- | --- | --- |
| `connections-per-ip-per-second` | `3` | New connections from one address per second | reload |
| `concurrent-per-ip` | `5` | Open connections from one address | reload |
| `status-per-ip-per-second` | `10` | Server list pings from one address per second | reload |
| `max-pending-logins` | `2000` | Connections still logging in, for the whole proxy; must be more than 0 | reload |
| `max-concurrent-has-joined` | `64` | Premium checks in progress at once, for the whole proxy; must be more than 0 | reload |
| `client-packets-per-second` | `500` | Packets from one player per second | reload (new players) |
| `client-bytes-per-second` | `1048576` | Bytes from one player per second, as sent; after decompression a player may send four times this | reload (new players) |
| `command-suggestions-per-second` | `10` | Command completion requests from one player per second | reload (new players) |
| `handshake-timeout-ms` | `5000` | Time for the first packet of a connection | reload |
| `login-timeout-ms` | `30000` | Time for the whole login | reload |
| `idle-timeout-ms` | `30000` | A session with no data from the client or the server for this long is closed | reload (new players) |

## logging

| Key | Default | Description | Applies |
| --- | --- | --- | --- |
| `format` | `text` | `text` or `json` (one JSON object per line) | restart |
| `level` | `info` | `error`, `warn`, `info`, `debug` or `trace` | restart |
| `log-ips` | `true` | `false`: addresses in logs become a hash with a random key per run, the same address giving the same hash until a restart | restart |
| `metrics-bind` | empty (off) | `ip:port` for Prometheus metrics at `http://<ip:port>/metrics`; only a loopback address (`127.0.0.1:9100`) is accepted | restart |

## switching

Moving players between servers.

| Key | Default | Description | Applies |
| --- | --- | --- | --- |
| `config-buffer-bytes` | `16777216` (16 MiB) | Packets of the next server held while the client leaves the old one | reload |
| `config-buffer-ms` | `10000` | How long they are held | reload |
| `resource-packs-on-switch` | `pop` | `pop`: the previous server's resource packs are removed on a switch. `keep`: they stay | reload |
| `reconnect-cooldown-ms` | `30000` | Minimum time between two reconnects of one player to the same server | reload |
| `fallback-reasons` | `["server closed", "server stopped", "restarting", "shutting down"]` | A kick whose text contains one of these (any letter case), or the vanilla shutdown message, sends the player to the next server instead of disconnecting them | reload |

## commands

| Key | Default | Description | Applies |
| --- | --- | --- | --- |
| `operators` | `[]` | Player UUIDs with every `pumbo.proxy.*` permission. `/prox op`, `/prox deop` and `/prox claim` edit this list | reload |
| `sensitive` | `[]` | Command names that never reach a server, also when no plugin handles them (for example `login`); plugins add their own | reload |

## plugin-messages

| Key | Default | Description | Applies |
| --- | --- | --- | --- |
| `server-brand-suffix` | `" (PumboProx)"` | Added to the server brand the client shows (F3); empty leaves it unchanged | reload |

## bungeecord-channel

The `bungeecord:main` plugin message channel, for older server plugins that ask the proxy for players and servers.

| Key | Default | Description | Applies |
| --- | --- | --- | --- |
| `enabled` | `true` | Answer messages on the channel | reload |
| `subchannels` | all eight | The subchannels answered: `Connect`, `ConnectOther`, `IP`, `UUID`, `GetServer`, `GetServers`, `PlayerCount`, `PlayerList`. Other names are refused | reload |

## messages

| Key | Default | Description | Applies |
| --- | --- | --- | --- |
| `no-server` | `No server is available right now.` | No server could be reached | reload |
| `no-server-for-version` | `This network has no server for Minecraft {version}. Please join with {supported}.` | No server speaks the client's version. `{version}`: the client's release, `{supported}`: the releases of the servers | reload |
| `signed-chat-blocked` | `Your message was blocked. Please reconnect.` | Kick text for `signed-chat-cancel: kick` | reload |

## virtual

Worlds the proxy shows itself, without a server.

| Key | Default | Description | Applies |
| --- | --- | --- | --- |
| `gate-timeout-ms` | `600000` | Longest time a plugin gate may hold a player in a virtual world | reload |
| `test-gate` | `false` | For tests: every player first waits in a test world of the proxy, then goes to a server. `gate <player>` in the console sends a player there | reload |
| `test-gate-seconds` | `10` | How long the test world holds a player; `0`: until the player types `/gate release` | reload |

## tab

Header and footer of the player list, rendered per player with placeholders. When set, they replace the server's.

| Key | Default | Description | Applies |
| --- | --- | --- | --- |
| `header` | empty | Header text; empty leaves it to the server | reload |
| `footer` | empty | Footer text; empty leaves it to the server | reload |
| `refresh-ms` | `1000` | How often placeholders in them are updated | reload |

## pumbox

| Key | Default | Description | Applies |
| --- | --- | --- | --- |
| `enabled` | `true` | Players wait in an empty world of the proxy while no server can take them, and join one when it is ready. `false`: they are disconnected with `messages.no-server` | reload |

## bridge

Where the [PumboBridge](bridge.md) plugin on your Pumpkin servers connects. Unknown keys here are an error.

| Key | Default | Description | Applies |
| --- | --- | --- | --- |
| `enabled` | `true` | Accept bridges | restart |
| `listen` | `127.0.0.1:25578` | `ip:port` the bridges connect to; loopback or a private network | restart |
| `key-file` | `bridge.key` | The bridge key (64 hex digits), created on the first start. `prox bridge key` in the console prints it | restart |
| `command-timeout-ms` | `5000` | How long a command sent to a server waits for its answer | restart |
| `trusted-plugins` | `[pumbo-core]` | Plugins that may send commands to the servers; any plugin that uses the bridge may read from them | restart |
| `stats-ms` | `5000` | How often a server reports its TPS and load | restart |
| `player-stats-ms` | `2000` | How often a server reports its player data | restart |

## managed-servers

Servers the proxy downloads, creates and runs: see [Servers from the proxy](servers.md). Every change here needs a restart.

| Key | Default | Description | Applies |
| --- | --- | --- | --- |
| `enabled` | `true` | Servers from the proxy | restart |
| `dir` | `servers` | Server folders, `servers.yml`, `templates/`, `.trash/` | restart |
| `versions-dir` | `versions` | Downloads, in `versions/<software>/<tag>/` | restart |
| `ports` | `25700-25799` | Ports for new servers, on `127.0.0.1`, as `first-last` | restart |
| `download-from-game` | `true` | Allow `/prox download` from the game; the console always can | restart |
| `allow-unverified` | `false` | Accept releases without a SHA256 checksum (development builds) | restart |
| `stop-timeout-secs` | `30` | How long a server gets after `stop` before it is killed | restart |
| `templates` | one template, `default` | How new servers are made, by template name | restart |

Keys of a template:

| Key | Default | Description |
| --- | --- | --- |
| `source` | `pumpkin` | Server software |
| `version` | `latest` | `latest` (the newest downloaded release) or a release tag |
| `autostart` | `true` | Start the server right after `new` and with the proxy |
| `restart-on-crash` | `3` | Restarts after crashes within 10 minutes; after that the server stays down |

## plugins

The plugin host. Apart from `required-gates`, the plugin host reads this section once at start.

| Key | Default | Description | Applies |
| --- | --- | --- | --- |
| `dir` | `plugins` | Plugin files (`.wasm`), with a folder per plugin for its `config.yml` and `data/` for its data | restart |
| `required-gates` | `[]` | Gates that must be loaded and running, for example `auth`; while one is missing or disabled, logins are closed. `/prox route gates` edits it | reload |
| `required-plugins` | `[]` | The same for plugins that are not gates | restart |
| `memory-mb` | `256` | Memory limit of one plugin | restart |
| `budget-ms` | `200` | Longest stretch a plugin may run without returning to the proxy | restart |
| `load-budget-ms` | `2000` | The same while a plugin loads (also after a reload or a restart of the plugin) | restart |
| `init-timeout-ms` | `10000` | Time for a plugin to start | restart |
| `gate-timeout-ms` | `300000` | Longest time one gate may hold a player | restart |
| `gate-timeouts` | `{}` | The same per gate name, for example `{ auth: 120000 }` | restart |
| `gate-reload-wait-ms` | `10000` | How long players at a gate wait for that gate's plugin while it reloads or restarts | restart |
| `timeouts` | `{}` | Deadline per event kind, for example `{ chat: 150 }`. Defaults: `handshake` and `status` 100, `pre-login`, `profile` and `login` 5000, `chat`, `backend-command` and `plugin-message` 200, `server-connect` and `server-kicked` 2000, `placeholder` 50, `permission-load` 2000, others 5000 | restart |
| `on-failure` | `{}` | What a failed or late plugin means per event kind: `allow` or `deny`. It can only make an event stricter than the plugin's own setting. Defaults: `deny` for `gate`, `pre-login`, `profile` and `login`, `allow` for the rest | restart |
| `http-allow-private` | `false` | Let plugins make HTTP requests to private and loopback addresses | restart |
| `allow-premium-profile-changes` | `false` | Let plugins change the name, UUID or skin of premium players | restart |
| `restart-backoff-ms` | `[1000, 5000, 30000]` | Waits before restarting a crashed plugin, one per attempt | restart |
| `max-failures` | `5` | Crashes within `failure-window-ms` after which a plugin is disabled until `/prox plugins reload` | restart |
| `failure-window-ms` | `300000` | See `max-failures` | restart |
| `mailbox` | `4096` | Events waiting per plugin | restart |
| `timers-per-plugin` | `1024` | Timers one plugin may have | restart |
| `log-lines-per-second` | `100` | Log lines one plugin may write per second | restart |
| `messages` | see below | Texts of the plugin host | restart |
| `<plugin id>` | enabled everywhere | Where a plugin is enabled and what it may do, see below | restart |

Event kinds for `timeouts` and `on-failure`: `handshake`, `status`, `pre-login`, `profile`, `login`, `gate`, `server-connect`, `server-connected`, `server-kicked`, `disconnect`, `chat`, `backend-command`, `plugin-message`, `context-changed`, `command`, `timer`, `virtual-input`, `service-call`, `service-changed`, `placeholder`, `permission-load`, `bus-event`, `admin`.

`plugins.messages` (text tags and [style](#style) tags):

| Key | Default |
| --- | --- |
| `login-unavailable` | `<err>Logging in is temporarily unavailable. Please try again in a moment.` |
| `command-unavailable` | `<err>This command is temporarily unavailable.` |
| `no-permission` | `<err>You do not have permission to use this command.` |
| `gate-failed` | `<err>Could not verify the connection. Please try again.` |
| `permissions-unavailable` | `<err>Permissions are temporarily unavailable. Please try again in a moment.` |
| `plugin-restarting` | `<err>{plugin} is restarting. Please try again in a moment.` (`{plugin}`: the plugin id) |

`plugins.<plugin id>` limits a plugin to some servers. Without `servers` and `groups` it is enabled everywhere; `except` wins over both. Unknown keys here are an error.

```yaml
plugins:
  dir: plugins
  required-gates: [auth]
  pumbo-example:
    groups: [minigames]
    except: [bedwars]
  shop:
    grants: [permissions-write]
```

| Key | Default | Description |
| --- | --- | --- |
| `servers` | `[]` | Server names |
| `groups` | `[]` | Names from [`server-group`](#server-group) |
| `except` | `[]` | Servers or groups where the plugin is off |
| `grants` | `[]` | What you allow the plugin beyond its sandbox. `permissions-write`: it may change permissions, when its manifest asks for it |

## services

How plugins call each other. Read once at start.

| Key | Default | Description | Applies |
| --- | --- | --- | --- |
| `max-depth` | `8` | How deep calls between plugins may nest | restart |
| `default-timeout-ms` | `1000` | Deadline of a call that sets none | restart |
| `queue-per-provider` | `1024` | Calls waiting for one providing plugin | restart |
| `in-flight-per-pair` | `64` | Calls in progress from one plugin to another | restart |
| `restart-wait-ms` | `2000` | How long a call waits for a providing plugin that is restarting | restart |
| `providers` | `{}` | Which plugin provides a service when several declare it: service name → plugin id | restart |
| `bus-queue` | `1024` | Bus events waiting per subscribing plugin | restart |

## permissions

Where permissions come from. `reload` reads the permissions file again; the keys below are read once at start.

| Key | Default | Description | Applies |
| --- | --- | --- | --- |
| `provider` | `auto` | `auto`: the installed permission plugin, else the file. `file`: only the file. Or the id of a permission plugin | restart |
| `file` | `permissions.yml` | The permissions file, see [Commands](commands.md#permissions) | restart |
| `load-at` | `after-gates` | When a player's permissions are loaded: `after-gates` or `after-profile` | restart |
| `load-timeout-ms` | `2000` | Time for the permission plugin to load one player | restart |
| `on-load-failure` | `deny` | When the permission plugin cannot load a player: `deny` refuses the login, `file-only` lets them in with the permissions from the file | restart |
| `offline-timeout-ms` | `500` | Time for a permission check of a player who is not online | restart |
| `offline-cache-ms` | `30000` | How long such an answer is reused | restart |

## placeholders

| Key | Default | Description | Applies |
| --- | --- | --- | --- |
| `unresolved` | `keep` | A placeholder no plugin answers: `keep` shows it as written, `empty` removes it | restart |
| `pull-timeout-ms` | `50` | Time for a plugin to answer one placeholder | restart |
| `aliases` | `{}` | Short names for placeholders, for example `{ rank: pumboperms_rank }` | restart |
| `rich` | `{}` | Namespaces whose values may keep click and hover events, for example `{ pumboperms: true }` | restart |
| `cache-entries` | `10000` | Placeholder values kept in the cache | restart |

## server-group

A list of named groups of servers, for plugin scopes and permission contexts. A server may be in several groups; the first group in the file comes first. Applies after a restart.

```yaml
server-group:
  - name: minigames
    servers: [arena, bedwars]
  - name: survivals
    servers: [survival, survival2]
```

## style

The colours of the style tags (`<p>`, `<err>` and the others) in texts of plugins and of the plugin host, as text tags. Applies after a restart.

| Key | Default |
| --- | --- |
| `p` | `<aqua>` |
| `s` | `<gray>` |
| `ok` | `<green>` |
| `warn` | `<yellow>` |
| `err` | `<red>` |
| `muted` | `<dark_gray>` |
