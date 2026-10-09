<p align="center">
  <img src="assets/hero.webp" alt="PumboProx: everything you need to run a network on Pumpkin" width="100%">
</p>

<p align="center">
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-GPL--3.0-blue" alt="License: GPL-3.0"></a>
  <img src="https://img.shields.io/badge/built%20with-Rust-orange?logo=rust" alt="Built with Rust">
  <img src="https://img.shields.io/badge/Minecraft-1.21%20%E2%80%93%2026.3-62B47A" alt="Minecraft 1.21 to 26.3">
  <a href="https://github.com/Pumpkin-MC/Pumpkin"><img src="https://img.shields.io/badge/Pumpkin-0.2.0-F28C28" alt="Pumpkin 0.2.0"></a>
  <img src="https://img.shields.io/badge/plugins-WebAssembly-654FF0?logo=webassembly&logoColor=white" alt="WebAssembly plugins">
  <img src="https://img.shields.io/badge/status-beta-yellow" alt="Status: beta">
  <img src="https://img.shields.io/badge/telemetry-none-brightgreen" alt="No telemetry">
</p>

<p align="center">
  <b>PumboProx</b> sits in front of your <a href="https://github.com/Pumpkin-MC/Pumpkin">Pumpkin</a> servers, the way Velocity sits in front of Paper.<br>
  Players connect to one address, log in once and move between servers. Plugins run on the proxy as small WebAssembly files.
</p>

<p align="center">
  <a href="#the-proxy">The proxy</a> ·
  <a href="#the-plugins">The plugins</a> ·
  <a href="#get-started">Get started</a> ·
  <a href="#roadmap">Roadmap</a> ·
  <a href="#writing-plugins">Writing plugins</a> ·
  <a href="#known-issues">Known issues</a> ·
  <a href="CONTRIBUTING.md">Contributing</a> ·
  <a href="SECURITY.md">Security</a>
</p>

<table>
<tr>
<td width="50%" valign="top">

**🧱 1.21 – 26.3**<br>
Players on older clients join your 26.3 servers. The proxy translates the protocol.

</td>
<td width="50%" valign="top">

**👤 Premium + offline**<br>
Mojang accounts log in on their own, everyone else uses a password. Decided per player.

</td>
</tr>
<tr>
<td valign="top">

**🧩 WASM plugins**<br>
One file in `plugins/`. Each plugin runs in a sandbox and reloads while players stay online.

</td>
<td valign="top">

**🛡️ Checked before they join**<br>
Bot checks and logins happen in a virtual world on the proxy, before a player reaches any server.

</td>
</tr>
</table>

---

## See it in action

https://github.com/user-attachments/assets/224d9859-b9b5-4844-a02f-295abef80e18

Two players on one network: the bot check in a virtual world, a premium login without a password, a new account with `/register`, `/server` between lobby and survival, and a ban that follows the player across the whole network.

> [!NOTE]
> PumboProx is in **beta**. Everything marked as available below works and is covered by tests against real Pumpkin servers, but the first release (0.1) is not out yet. Try it on a test network before you put players on it.

<a name="the-proxy"></a>
<p align="center"><img src="assets/section-proxy.webp" alt="Proxy" width="60%"></p>

| | Feature | |
| --- | --- | --- |
| ⚙️ | **Written in Rust** | One small program, no Java. About 20 MB of RAM on its own; each plugin adds about 20 MB, so about 100 MB with Filter, Auth, Bans and Perms. |
| 🧱 | **Versions 1.21 – 26.3** | Players on older clients join your 26.3 servers. The proxy translates the protocol. |
| 👤 | **Premium and offline together** | Mojang accounts log in automatically, everyone else uses a password (with PumboAuth). Decided per player. |
| ➡️ | **Velocity modern forwarding** | The real UUID, IP and skin reach every server. Pumpkin supports it out of the box. |
| 🔀 | **Server switching** | `/server`, `/glist`, `/send`, `/find`, `/alert`, a fallback server when one goes down, forced hosts. |
| 🧩 | **PumboAPI** | One API for every plugin: services, placeholders, permissions, per-server and per-group config. |
| 🌌 | **PumboVir** | Virtual worlds on the proxy. Bot checks and logins happen before a player reaches any server. |
| 🌉 | **PumboBridge** | The proxy can teleport players, change game modes, read inventories and apply its ranks on every server. |
| 🛡️ | **Built-in protection** | Connection limits, packet limits, flood kicks, encrypted logins. |
| 📄 | **Readable YAML config** | Plain files with comments. Errors point to the exact line. |
| 📊 | **Metrics** | A Prometheus endpoint for your dashboards. |
| 🙈 | **No telemetry** | Nothing leaves your machine. |

<a name="the-plugins"></a>
<p align="center"><img src="assets/section-plugins.webp" alt="Plugins" width="60%"></p>

| | Feature | |
| --- | --- | --- |
| 📦 | **One file per plugin** | Drop `pumbo-filter.wasm` into `plugins/`. On the first start the plugin creates its folder and a commented `config.yml`. |
| 🔗 | **Plugins find each other** | Pumbo plugins detect each other and work together. Take one away and the rest keep running. |
| 🌍 | **Whole network or solo** | Run a plugin on the proxy for the whole network, or standalone on a plain Pumpkin server. |
| 🗂️ | **Per-server and per-group config** | One plugin, different settings for lobby, survival and minigames. |
| 🔐 | **Network-wide permissions** | Server, group and global contexts in one `permissions.yml`. |
| ♻️ | **Reload without a restart** | Update a plugin and reload it while players stay online. |
| 🧪 | **Sandboxed plugins** | A plugin sees only its own config and data folders and has no network access. |
| 🧰 | **Plugin SDK** | MIT/Apache-2.0. Write your own plugins under any license. |

### Pumbo plugins

Every plugin keeps its logic in one place and comes in two builds: one for the proxy (the whole network) and one for a plain Pumpkin server.

| Plugin | What it does | Proxy | Pumpkin | Status |
| --- | --- | :---: | :---: | --- |
| 🛡️ [**PumboFilter**](https://github.com/PumboMC/PumboFilter) | Anti-bot checks in a virtual world | ✅ | ✅ | ![beta](https://img.shields.io/badge/-beta-orange) |
| 🔒 [**PumboAuth**](https://github.com/PumboMC/PumboAuth) | One account for the whole network, premium auto-login, passwords, 2FA | ✅ | ✅ | ![beta](https://img.shields.io/badge/-beta-orange) |
| 🚫 [**PumboBans**](https://github.com/PumboMC/PumboBans) | Bans, mutes and warnings across all servers | ✅ | ✅ | ![beta](https://img.shields.io/badge/-beta-orange) |
| 👥 [**PumboPerms**](https://github.com/PumboMC/PumboPerms) | Ranks, groups and permissions for the whole network, imports what you already have | ✅ | ✅ | ![beta](https://img.shields.io/badge/-beta-orange) |
| 🌉 [**PumboBridge**](https://github.com/PumboMC/PumboBridge) | Lets the proxy control your servers: teleports, game modes, inventories, ranks | ✅ | ✅ | ![beta](https://img.shields.io/badge/-beta-orange) |
| 🏰 **PumboGuard** | Region protection, managed from the proxy | 🔜 | 🔜 | ![coming soon](https://img.shields.io/badge/-coming%20soon-lightgrey) |
| 📋 **PumboTabasco** | Tab list, scoreboard, boss bars, name tags and network chat (`/tbsc`) | 🔜 | | ![coming soon](https://img.shields.io/badge/-coming%20soon-lightgrey) |
| 🏠 **PumboCore** | `/home`, `/spawn`, `/tp` and everyday commands | 🔜 | 🔜 | ![coming soon](https://img.shields.io/badge/-coming%20soon-lightgrey) |
| 🎨 **PumboSkins** | Skins for every player, premium or not | 🔜 | | ![coming soon](https://img.shields.io/badge/-coming%20soon-lightgrey) |

### Plugins work together

You don't wire anything up. Each Pumbo plugin looks for the others when it starts and uses what it finds. Take one away and the rest keep running on their own.

| When you have | What happens |
| --- | --- |
| **PumboFilter + PumboAuth** | A new player passes the bot check and goes straight to the login, in the same connection, with no reconnect screen. |
| **PumboFilter + PumboBans** | Addresses that keep failing the bot check get a temporary IP ban through PumboBans (`auto-ban` in the config). |
| **PumboBans + PumboAuth** | A ban ends the player's login sessions, so after the ban is lifted they type their password again. |
| **PumboPerms on the proxy + PumboBridge** | Ranks you set on the proxy with `/pp` apply on every server right away. A PumboPerms that also runs on a server steps back and lets the proxy decide. |
| **Any plugin + PumboBridge** | The plugin can teleport players, change game modes or open inventories on any server, and read `%server_tps:lobby%` and other placeholders. |

### PumboBridge

PumboBridge is a small plugin you put on every Pumpkin server behind the proxy. It is the same file and the same `config.yml` on all of them.

1. Turn on `bridge:` in `pumboprox.yml`. The proxy creates a key on the first start (`/pumbo bridge key` shows it).

   <img src="assets/bridge-key.png" alt="/pumbo bridge key in the game, with part of the key hidden" width="100%">

2. Put `PumboBridge.wasm` into `plugins/` of each server, with the proxy's address and the key in its `config.yml`.
3. Each server connects to the proxy on its own and finds out which server it is. `/pumbo bridge` shows them all:

<img src="assets/bridge-status.png" alt="/pumbo bridge in the game: lobby and survival connected, version 0.1.0 / 1.0, ping 35 ms, connected for 50 minutes" width="100%">

With the bridge, the proxy and its plugins can:

| | |
| --- | --- |
| 🧭 **Move players** | on the server: teleport within a world, to another world, to spawn or to another player, and send a player to a server with a teleport after they arrive |
| 🎮 **Change their state** | game mode, health, effects, flight |
| 🎒 **Look into inventories** | read them and show a player's inventory to staff |
| 🔐 **Apply ranks** | the proxy's permissions become the server's permissions, so `/gamemode` on lobby can be allowed for one rank and not on survival |
| 📊 **Read the server** | TPS, MSPT and player data as placeholders, and events like join, death and world change |

Every message between the proxy and a server is signed with the key, so a fake proxy or a fake server is refused. A server without PumboBridge still works behind the proxy; plugins just can't control it, and they are told so.

### How it fits together

<img src="assets/diagram.webp" alt="PumboProx: the core, PumboAPI and plugins/ on the proxy; forwarding and the bridge to lobby, survival and skyblock, each with PumboBridge" width="100%">

Players connect to the proxy. It checks them, logs them in and sends them to a server with Velocity forwarding, so every server sees the real UUID, IP and skin. Every server also runs PumboBridge, so the proxy can teleport players, change game modes and apply its ranks there.

<details>
<summary>Text version</summary>

```
Player (Minecraft Java 1.21 – 26.3)
   │
   ▼
PumboProx ──────────────────────────────── one program (Rust), one config
├── core
│   ├── protocol 1.21 – 26.3
│   ├── premium and offline logins       encryption, Mojang sessions
│   ├── version translation              older clients on 26.3 servers
│   └── forwarding                       name, UUID, IP, skin → server
│
├── PumboAPI ─────────────────────────── what every plugin builds on
│   ├── services, placeholders, permissions, events
│   ├── contexts: server > group > everywhere
│   ├── per-server and per-group plugin config
│   └── PumboVir: virtual worlds, checks before any server
│
└── plugins/ (.wasm) ─────────────────── the logic, the data, the config
    ├── PumboFilter    anti-bot checks in a virtual world
    ├── PumboAuth      accounts, premium auto-login, 2FA
    ├── PumboBans      bans, mutes, warnings
    ├── PumboPerms     ranks and permissions for the whole network
    ├── PumboGuard     region protection                        (soon)
    ├── PumboTabasco   tab list, scoreboard, boss bars, chat    (soon)
    ├── PumboCore      /home, /spawn, /tp …                     (soon)
    └── PumboSkins     skins for every player                   (soon)
   │
   │  forwarding + bridge (signed with a key)
   ├──────────────────────┬────────────────────────┐
   ▼                      ▼                        ▼
lobby                  survival-1, survival-2    skyblock
(plain Pumpkin)        (group "survival")        (plain Pumpkin)
└─ PumboBridge.wasm    └─ PumboBridge.wasm       ├─ PumboBridge.wasm
                                                 └─ any other plugins
```

</details>

<details>
<summary><b>How a player gets in</b>: PumboFilter and PumboAuth hold a new player in a virtual world on the proxy until the checks and the login are done</summary>

```
Player joins
   │
   ▼
Login on the proxy ──────────────────────── before encryption
   ├── PumboFilter   counts connections, attack mode, auto-ban of addresses that keep failing
   └── PumboAuth     refuses bad nicknames, locked accounts, new players while registrations are closed
   │
   ▼
Gates in a virtual world (PumboVir) ─────── no server sees the player yet
   │
   ├── 1. PumboFilter                       gate "filter"
   │      ├── let through: premium (outside an attack), verified in the last 12 h,
   │      │                whitelist, pumbo.filter.bypass
   │      ├── gravity check      falls for 6.4 s, compared with vanilla physics
   │      ├── client check       brand and settings of the game
   │      ├── CAPTCHA if needed  a code on a map in hand → /captcha <code>
   │      └── passed → next gate in the same connection, no reconnect screen
   │
   └── 2. PumboAuth                         gate "auth"
          ├── premium            logged in by Mojang, no password
          ├── session            joined from here in the last 60 min
          ├── new player         /register <password> <password>
          ├── known player       /login <password>   (+ /2fa <code> with 2FA on)
          ├── countdown bar, lockout after wrong passwords, argon2id
          └── logged in → a server
   │
   ▼
lobby (Pumpkin) ─────────────────────────── /changepassword, /logout, /2fa, /premium
                                            are taken by the proxy and never reach a server
```

</details>

Plugins from the Pumpkin market keep working on your servers as usual. The proxy only adds what a network needs on top.

<a name="get-started"></a>
<p align="center"><img src="assets/section-start.webp" alt="Get started" width="60%"></p>

**1. Get PumboProx.** Pick one:

- **Download** the file for your system from [Releases](https://github.com/PumboMC/PumboProx/releases/latest): Linux (x86_64 and ARM64, also static builds), Windows (x86_64 and ARM64) or macOS. Unpack it and you have the `pumboprox` program.
- **Docker**: an image for `linux/amd64` and `linux/arm64`, `ghcr.io/pumbomc/pumboprox:<version>` (`latest` is the newest stable release). It holds only the program and runs as a non-root user. Put `pumboprox.yml` into a directory and mount it as `/data`; whatever the proxy writes (secrets, data, `plugins/`) lands there too.

  ```sh
  mkdir pumbo && cd pumbo        # pumboprox.yml goes here
  docker run -dit --name pumboprox -p 25565:25565 \
    -v "$PWD:/data" --user "$(id -u):$(id -g)" \
    ghcr.io/pumbomc/pumboprox:latest
  ```

  Inside the container `127.0.0.1` is the container itself: give the servers in `pumboprox.yml` addresses the container can reach (another container's name on the same Docker network, or `--network host` on Linux). Console: `docker attach pumboprox`, leave it with Ctrl+P Ctrl+Q. `docker stop pumboprox` shuts the proxy down cleanly.
- **Build from source** (Rust stable):

  ```sh
  git clone https://github.com/PumboMC/PumboProx
  cd PumboProx
  cargo build --release -p pumbo-prox
  ```

**2. Configure it.** `pumboprox.yml`:

```yaml
listener:
  - bind: "0.0.0.0:25565"
status:
  motd: "&6My network"
  max-players: 100
login:
  online-mode: per-player      # per-player | true | false
servers:
  lobby:    { address: "127.0.0.1:25566", protocol: 777 }
  survival: { address: "127.0.0.1:25567", protocol: 777 }
routing:
  try: [lobby]
forwarding:
  mode: modern                 # modern | legacy | none
  secret-file: forwarding.secret
plugins:
  dir: plugins
```

**3. Point your Pumpkin servers at it.** In each server's `pumpkin.toml`, turn on Velocity forwarding with the secret from `forwarding.secret`, and let the server listen on `127.0.0.1` only:

```toml
[networking.proxy]
enabled = true

[networking.proxy.velocity]
enabled = true
secret = "<contents of forwarding.secret>"
```

On Pumpkin 0.2.0, read the [warning under Known issues](#known-issues) first.

**4. Start it:**

```sh
./pumboprox run pumboprox.yml     # Docker starts it for you
```

**5. Add plugins.** Put the `.wasm` files into `plugins/` and restart the proxy, or load them with `/prox plugin load <id>`.

<img src="assets/pumbo-plugins.png" alt="/pumbo in the game: pumbo-auth, pumbo-bans, pumbo-filter and pumbo-perms 0.1.0, all running" width="100%">

### Commands

| Command | What it does |
| --- | --- |
| `/server [name]` | Go to another server, or list them |
| `/glist` | Players on every server |
| `/send <player\|all> <server>` | Move players to a server |
| `/find <player>` | Which server a player is on |
| `/alert <message>` | A message to the whole network |
| `/prox` | The proxy commands you may use, like `/velocity` on Velocity |
| `/prox version` | The proxy's version and the protocols it speaks |
| `/prox reload` | Reload `pumboprox.yml` |
| `/prox plugins` | Plugins, their versions and state (also `/pumbo`) |
| `/prox plugin reload\|load\|unload <id>` | Manage plugins while the proxy runs |
| `/prox bridge [status\|key]` | Which servers run PumboBridge, and the bridge key |
| `/prox perms list\|check <player> <node> [server]` | Permission nodes, and how one is decided for a player |
| `/prox services` | The services plugins offer each other |

`/prox` is the short form of the admin commands under `/pumbo`: `/prox plugin …` is `/pumbo proxy plugin …` and `/prox bridge` is `/pumbo bridge`. In the proxy console the same commands work without the slash.

Every Pumbo plugin has its own command, a short form of it, and a place under `/pumbo`. `/pumbo <plugin>` on its own shows the plugin's help:

| Plugin | Command | Short | Under `/pumbo` |
| --- | --- | --- | --- |
| PumboFilter | `/pumbofilter` | `/pf` | `/pumbo filter` |
| PumboAuth | `/pumboauth` | `/pa` | `/pumbo auth` |
| PumboBans | `/pumbobans` | `/pb` | `/pumbo bans` |
| PumboPerms | `/pumboperms` | `/pp` | `/pumbo perms` |

Player commands such as `/login` or `/ban`, and their aliases, are listed in each plugin's README.

Permissions live in `permissions.yml`, with server, group and global contexts:

```yaml
groups:
  default:
    permissions: [pumbo.proxy.server]
  staff:
    inherits: [default]
    permissions:
      - "pumbo.proxy.*"
      - "pumbo.bans.*"
players:
  Steve:
    groups: [staff]
```

## Roadmap

| | What | |
| --- | --- | --- |
| ✅ | Core proxy, forwarding, server switching | done |
| ✅ | Plugin host (PumboAPI), virtual worlds (PumboVir) | done |
| ✅ | Version translation 1.21 – 26.2 → 26.3 | done |
| ✅ | PumboFilter, PumboAuth, PumboBans, PumboBridge | beta |
| ✅ | PumboPerms for the whole network | beta |
| 🔜 | Release 0.1: downloads, Docker image, Pterodactyl and Pelican egg, setup wizard | next |
| 🔜 | Translation in both directions (newer clients on older servers) | planned |
| 🔜 | PumboTabasco, PumboGuard, PumboCore | planned |
| 🔜 | Web panel built into the proxy | planned |
| 🔜 | Clients older than 1.21, in our own translator | planned |
| 🔜 | Bedrock players | planned |
| 🔜 | PumboSkins | planned |

## Writing plugins

A PumboProx plugin is a WebAssembly component built against the [`pumbo:prox` WIT interface](https://github.com/PumboMC/PumboProx/tree/main/wit/pumbo-prox.wit). The [SDK (`pumbo-sdk`)](https://github.com/PumboMC/PumboProx/tree/main/crates/pumbo-sdk) and the interface are MIT/Apache-2.0, so your plugin can use any license.

| | What | Where |
| --- | --- | --- |
| 🧰 | **SDK**: commands, events, config, languages, services, `embed!`, a fake host for tests | [`crates/pumbo-sdk`](https://github.com/PumboMC/PumboProx/tree/main/crates/pumbo-sdk) |
| 📜 | **Interface**: the contract between the proxy and a plugin, usable from any language that builds WebAssembly components | [`wit/pumbo-prox.wit`](https://github.com/PumboMC/PumboProx/tree/main/wit/pumbo-prox.wit) |
| 🤝 | **Contracts**: shared service types, so plugins can talk to each other | [`crates/pumbo-contracts`](https://github.com/PumboMC/PumboProx/tree/main/crates/pumbo-contracts) |
| 🧩 | **Shared library**: YAML config, languages and message styles used by the Pumbo plugins | [`crates/pumbo-common`](https://github.com/PumboMC/PumboProx/tree/main/crates/pumbo-common) |
| 🌱 | **Example plugin** to copy | [`plugins/example`](https://github.com/PumboMC/PumboProx/tree/main/plugins/example) |

The SDK goes to crates.io with release 0.1. Until then, add it from Git:

```toml
[dependencies]
pumbo-sdk = { git = "https://github.com/PumboMC/PumboProx" }
```

```sh
rustup target add wasm32-wasip2
cargo build -p pumbo-example --target wasm32-wasip2 --profile plugin
```

[`plugins/example`](https://github.com/PumboMC/PumboProx/tree/main/plugins/example) is a small plugin with a command, a config file and a placeholder. Start there.

## Known issues

> [!WARNING]
> **Pumpkin 0.2.0 lets every player use `/tp`, `/xp`, `/banip` and `/pardonip`.** In the official 0.2.0 release (and in 0.1.0-dev) these aliases skip the permission check of the command they stand for, so on a plain server anyone can teleport, give experience and ban IP addresses. Pumpkin fixed it after 0.2.0 ([#3801](https://github.com/Pumpkin-MC/Pumpkin/pull/3801)). PumboBridge and PumboPerms block the four aliases on the server for players without the permission. If you run neither of them, update Pumpkin to a build with the fix.

- **Tab completion for plugin commands is partial.** For a plugin command the proxy suggests the plugin's subcommands and then the names of online players. Suggestions for every argument (durations, group names, servers) come once plugins can answer completions themselves.
- **Servers on Pumpkin 0.1.0-dev (Minecraft 26.2) work behind the proxy only for 26.2 players:** the translator turns older clients into 26.3 today. Newer clients on older servers come with translation in both directions.
- **This release:** PumboProx with [PumboFilter](https://github.com/PumboMC/PumboFilter), [PumboAuth](https://github.com/PumboMC/PumboAuth), [PumboBans](https://github.com/PumboMC/PumboBans), [PumboPerms](https://github.com/PumboMC/PumboPerms) and [PumboBridge](https://github.com/PumboMC/PumboBridge). Each plugin has its own repository and its own release with the `.wasm` files.
- **PumboDB, PumboTabasco, PumboGuard, PumboCore and PumboSkins are planned**, not part of this release. PumboDB will be one shared SQL database (SQLite, MySQL, MariaDB, PostgreSQL) for all Pumbo plugins.

## Contributing

Bug reports, fixes and plugins are welcome. [CONTRIBUTING.md](CONTRIBUTING.md) explains how to build the proxy, run the tests and start a test network on your machine with one command. Report security problems privately, as described in [SECURITY.md](SECURITY.md).

## License

PumboProx is licensed under the [GNU General Public License v3.0](LICENSE). The plugin SDK, the WIT interface and the shared libraries for plugins are dual-licensed under MIT and Apache-2.0.

PumboProx is not affiliated with Mojang, Microsoft or the Pumpkin project.

<details>
<summary>Everything on one picture</summary>
<br>
<img src="assets/banner.jpg" alt="PumboProx features and plugins on one picture" width="100%">
</details>
