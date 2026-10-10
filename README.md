<p align="center">
  <img src="assets/hero.webp" alt="PumboProx: everything you need to run a network on Pumpkin" width="100%">
</p>

<p align="center">
  <a href="https://github.com/PumboMC/PumboProx/stargazers"><img src="https://img.shields.io/github/stars/PumboMC/PumboProx?style=social" alt="GitHub stars"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-GPL--3.0-blue" alt="License: GPL-3.0"></a>
  <img src="https://img.shields.io/badge/built%20with-Rust-orange?logo=rust" alt="Built with Rust">
  <img src="https://img.shields.io/badge/Minecraft-1.21%20%E2%80%93%2026.3-62B47A" alt="Minecraft 1.21 to 26.3">
  <a href="https://github.com/Pumpkin-MC/Pumpkin"><img src="https://img.shields.io/badge/Pumpkin-0.2.0-F28C28" alt="Pumpkin 0.2.0"></a>
  <img src="https://img.shields.io/badge/plugins-WebAssembly-654FF0?logo=webassembly&logoColor=white" alt="WebAssembly plugins">
  <img src="https://img.shields.io/badge/status-beta-yellow" alt="Status: beta">
  <img src="https://img.shields.io/badge/telemetry-none-brightgreen" alt="No telemetry">
</p>

<p align="center">
  <b>PumboProx</b> is a Minecraft proxy written 100% in Rust for the best performance with low resource use: about 20 MB of RAM.<br>
  It sits in front of your <a href="https://github.com/Pumpkin-MC/Pumpkin">Pumpkin</a> servers, the way Velocity sits in front of Paper.<br>
  Plugins run on the proxy as small WebAssembly files.<br>
  It also downloads and runs your Pumpkin servers; installing other server software and plugins (with PumboPM) straight from the proxy is planned.
</p>

<p align="center">
  <a href="#the-proxy">The proxy</a> ·
  <a href="#the-plugins">The plugins</a> ·
  <a href="docs/getting-started.md">Get started</a> ·
  <a href="#documentation">Documentation</a> ·
  <a href="#roadmap">Roadmap</a> ·
  <a href="#important">Important</a> ·
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

<p align="center"><sub><i>A big refactor is planned, to make the project easier and more comfortable to work on. Until 1.0, the config format and the plugin API may still change.</i></sub></p>

---

## What's new in 0.2.0-beta

- **Download Pumpkin from the proxy:** `/prox download pumpkin`, with a progress bar and a checksum check.
- **Run servers directly in game, from the proxy:** `/prox servers new|start|stop|restart|logs|delete`. No proxy restart needed, and a crashed server starts again on its own.
- **Set the way in:** `/prox route` for start servers, domains (`pvp.example.com → arena`) and required login gates.
- **Move groups:** `/send current <server>` moves everyone on your server.

https://github.com/user-attachments/assets/6ae651c3-0782-4c79-ac05-6018fe869001

Pumpkin downloaded from the game, the server `demo` created and joined with `/server demo`, all without touching a file. More in [Servers from the proxy](docs/servers.md), every change in the [changelog](CHANGELOG.md).

<a name="see-it-in-action"></a>
<p align="center"><img src="assets/section-action.webp" alt="See it in action" width="60%"></p>

https://github.com/user-attachments/assets/224d9859-b9b5-4844-a02f-295abef80e18

Two players on one network: the bot check in a virtual world, a premium login without a password, a new account with `/register`, `/server` between lobby and survival, and a ban that follows the player across the whole network.

> [!NOTE]
> PumboProx is in **beta** (0.2.0-beta). Everything marked as available below works and is covered by tests against real Pumpkin servers. Try it on a test network before you put players on it.

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
| 🖥️ | **Servers from the proxy** | Download Pumpkin, then create, start and stop servers from the game. [How](docs/servers.md) |
| 🌉 | **PumboBridge** | Moving players between servers works without it. With [PumboBridge](docs/bridge.md) on a server, the proxy can also act inside it: teleport within the world, change game modes, read inventories, apply its ranks and see the server's TPS. |
| 🛡️ | **Built-in protection** | Connection limits, packet limits, flood kicks, encrypted logins. |
| 📄 | **Readable YAML config** | Plain files with comments. Errors point to the exact line. |
| 📊 | **Metrics** | A Prometheus endpoint for your dashboards. |
| 🙈 | **No telemetry** | Nothing leaves your machine. |

<a name="the-plugins"></a>
<p align="center"><img src="assets/section-plugins.webp" alt="Plugins" width="60%"></p>

| | Feature | |
| --- | --- | --- |
| 📦 | **One file per plugin** | Drop the plugin's `.wasm` file into `plugins/`. On the first start the plugin creates its folder and a commented `config.yml`. |
| 🔗 | **Plugins find each other** | Pumbo plugins detect each other and work together. Take one away and the rest keep running. |
| 🌍 | **Whole network or solo** | Run a plugin on the proxy for the whole network, or standalone on a plain Pumpkin server. |
| 🗂️ | **Per-server and per-group config** | One plugin, different settings for lobby, survival and minigames. |
| 🔐 | **Network-wide permissions** | Server, group and global contexts in one `permissions.yml`. |
| ♻️ | **Reload without a restart** | Update a plugin and reload it while players stay online. |
| 🧪 | **Sandboxed plugins** | A plugin sees only its own config and data folders and has no network access. |
| 🧰 | **Plugin SDK** | MIT/Apache-2.0. Write your own plugins under any license. |

<h2 align="center">Pumbo plugins</h2>

<p align="center">Every plugin keeps its logic in one place and comes in two builds:<br>one for the proxy (the whole network) and one for a plain Pumpkin server.</p>

| Plugin | What it does | Proxy | Pumpkin | Status |
| :---: | --- | :---: | :---: | --- |
| 🛡️<br>[**PumboFilter**](https://github.com/PumboMC/PumboFilter) | Anti-bot checks in a virtual world | ✅ | ✅ | ![beta](https://img.shields.io/badge/-beta-orange) |
| 🔒<br>[**PumboAuth**](https://github.com/PumboMC/PumboAuth) | One account for the whole network, premium auto-login, passwords, 2FA | ✅ | ✅ | ![beta](https://img.shields.io/badge/-beta-orange) |
| 🚫<br>[**PumboBans**](https://github.com/PumboMC/PumboBans) | Bans, mutes and warnings across all servers | ✅ | ✅ | ![beta](https://img.shields.io/badge/-beta-orange) |
| 👥<br>[**PumboPerms**](https://github.com/PumboMC/PumboPerms) | Ranks, groups and permissions for the whole network, imports what you already have | ✅ | ✅ | ![beta](https://img.shields.io/badge/-beta-orange) |
| 🌉<br>[**PumboBridge**](https://github.com/PumboMC/PumboBridge) | Lets the proxy act inside each server: teleports within the world, game modes, inventories, ranks, TPS | ✅ | ✅ | ![beta](https://img.shields.io/badge/-beta-orange) |
| 📦<br>**PumboPM** | Plugin manager: install, update and turn plugins on and off from the proxy | 🔜 | 🔜 | ![coming soon](https://img.shields.io/badge/-coming%20soon-lightgrey) |
| 🏷️<br>**PumboPHAPI** | Placeholders for every plugin on a standalone Pumpkin server (the proxy has its own built in) | | 🔜 | ![coming soon](https://img.shields.io/badge/-coming%20soon-lightgrey) |
| 📋<br>**PumboTabasco** | Tab list, scoreboard, boss bars, name tags and network chat | 🔜 | | ![coming soon](https://img.shields.io/badge/-coming%20soon-lightgrey) |
| 🗄️<br>**PumboDB** | One SQL database for all Pumbo plugins: SQLite, MySQL, MariaDB, PostgreSQL | 🔜 | 🔜 | ![coming soon](https://img.shields.io/badge/-coming%20soon-lightgrey) |
| 🏠<br>**PumboCore** | Everyday essentials: `/home`, `/spawn`, `/tp` and more | 🔜 | 🔜 | ![coming soon](https://img.shields.io/badge/-coming%20soon-lightgrey) |
| 🏰<br>**PumboGuard** | Region protection, managed from the proxy | 🔜 | 🔜 | ![coming soon](https://img.shields.io/badge/-coming%20soon-lightgrey) |

<p align="center"><sub>Want to write your own? See the <a href="docs/writing-plugins.md">plugin SDK</a> (MIT/Apache-2.0, any license for your plugin).</sub></p>

### How it fits together

Pumbo plugins find each other and work together, and a new player passes the bot check and the login in a virtual world before any server sees them: see [How it works](docs/how-it-works.md).

<img src="assets/diagram.webp" alt="PumboProx: the core, PumboAPI and plugins/ on the proxy; forwarding and the bridge to lobby, survival and skyblock, each with PumboBridge" width="100%">

<a name="get-started"></a>
<p align="center"><img src="assets/section-start.webp" alt="Get started" width="60%"></p>

1. **Get PumboProx:** the file for Linux, Windows or macOS from [Releases](https://github.com/PumboMC/PumboProx/releases/latest), the Docker image `ghcr.io/pumbomc/pumboprox`, or build it with Cargo.
2. **Write `pumboprox.yml`:** where to listen and which servers you have.
3. **Point your Pumpkin servers at it** with Velocity forwarding, or let the proxy [run them for you](docs/servers.md).
4. **Start it** with `./pumboprox run pumboprox.yml` and drop plugins into `plugins/`.

Step by step, with the config and Docker: **[Get started](docs/getting-started.md)**.

### Commands at a glance

| Command | What it does |
| --- | --- |
| `/server <server>` | Go to another server, or list them |
| `/send <player\|all\|current> <server>` | Move players to a server |
| `/prox` | The proxy commands you may use (help with pages and tooltips) |
| `/prox servers` | Servers run by the proxy |
| `/pf` `/pa` `/pb` `/pp` | PumboFilter, PumboAuth, PumboBans, PumboPerms |

Every command and permission: [Commands](docs/commands.md).

## Documentation

| | Page | What's in it |
| --- | --- | --- |
| 🚀 | [Get started](docs/getting-started.md) | Downloads, Docker, the config, setting up Pumpkin, plugins, permissions |
| 🖥️ | [Servers from the proxy](docs/servers.md) | Downloading Pumpkin, creating and running servers, the way into the network |
| ⌨️ | [Commands](docs/commands.md) | Every command and permission of the proxy, plugin commands |
| 🌉 | [PumboBridge](docs/bridge.md) | Setting up the bridge and what the proxy can do with it |
| 🧭 | [How it works](docs/how-it-works.md) | The whole picture, how a player gets in, how plugins work together |
| 🧰 | [Writing plugins](docs/writing-plugins.md) | The SDK, the interface and an example plugin |
| 📝 | [Changelog](CHANGELOG.md) | What changed in each release |

## Roadmap

| | What | |
| --- | --- | --- |
| ✅ | Core proxy, forwarding, server switching | done |
| ✅ | Plugin host (PumboAPI), virtual worlds (PumboVir) | done |
| ✅ | Version translation 1.21 – 26.2 → 26.3 | done |
| ✅ | PumboFilter, PumboAuth, PumboBans, PumboBridge | beta |
| ✅ | PumboPerms for the whole network | beta |
| ✅ | 0.1.0-beta.1: downloads for Linux, macOS and Windows, Docker image | released |
| ✅ | Server management from the proxy: download Pumpkin, create and manage servers (thanks to [@Uncover-F](https://github.com/Uncover-F) for the great idea) | released (0.2.0-beta) |
| 🔜 | Paper servers from the proxy: download and run them like Pumpkin | planned |
| 🔜 | HTTP API for panels and hosting: servers, players, bans | planned |
| 🔜 | Pterodactyl and Pelican egg, setup wizard, signed Windows files | planned |
| 🔜 | Translation in both directions (newer clients on older servers) | planned |
| 🔜 | PumboPM, PumboPHAPI, PumboTabasco, PumboDB, PumboCore, PumboGuard | planned |
| 🔜 | Web panel built into the proxy | planned |
| 🔜 | Clients older than 1.21, in our own translator | planned |
| 🔜 | Bedrock players | planned |

## Important

> [!WARNING]
> **Pumpkin 0.2.0 lets every player use `/tp`, `/xp`, `/banip` and `/pardonip`.** In the official 0.2.0 release (and in 0.1.0-dev) these aliases skip the permission check of the command they stand for, so on a plain server anyone can teleport, give experience and ban IP addresses. Pumpkin fixed it after 0.2.0 ([#3801](https://github.com/Pumpkin-MC/Pumpkin/pull/3801)). PumboBridge and PumboPerms block the four aliases on the server for players without the permission. If you run neither of them, update Pumpkin to a build with the fix.

- **Tab completion for plugin commands is partial.** For a plugin command the proxy suggests the plugin's subcommands and then the names of online players. Suggestions for every argument (durations, group names, servers) come once plugins can answer completions themselves.
- A server on the older Pumpkin 0.1.0-dev (Minecraft 26.2) accepts only 26.2 players for now; every version will be able to join it once the translation works in both directions.
- A big refactor of the code is planned.
- **PumboPM, PumboPHAPI, PumboTabasco, PumboDB, PumboCore and PumboGuard are planned**, not part of this release. PumboDB will be one shared SQL database (SQLite, MySQL, MariaDB, PostgreSQL) for all Pumbo plugins.
- `/prox download paper` answers "coming soon": Paper servers from the proxy come in a later beta.

## Contributing

Bug reports, fixes and plugins are welcome. [CONTRIBUTING.md](CONTRIBUTING.md) explains how to build the proxy, run the tests and start a test network on your machine with one command. Report security problems privately, as described in [SECURITY.md](SECURITY.md).

## License

PumboProx is licensed under the [GNU General Public License v3.0](LICENSE). The plugin SDK, the WIT interface and the shared libraries for plugins are dual-licensed under MIT and Apache-2.0.

Windows files are signed starting with 0.2.1-beta: see the [code signing policy](docs/code-signing.md).

PumboProx is not affiliated with Mojang, Microsoft or the Pumpkin project.

<details>
<summary>Everything on one picture</summary>
<br>
<img src="assets/banner.jpg" alt="PumboProx features and plugins on one picture" width="100%">
</details>

<p align="center"><sub>Like PumboProx? A ⭐ on GitHub helps other server owners find it.</sub></p>
