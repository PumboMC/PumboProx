# Changelog

All notable changes to PumboProx, one section per release. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

### Security

- A server's `transfer` packet (for example from the `/transfer` command) no longer reaches the player: one to another server of the network becomes a switch inside the proxy, any other is dropped and logged once per server. `servers.<name>.allow-transfers: true` lets a server's transfers through.
- `online-mode: per-player` keeps the premium-name answers (a premium name for a day, any other for ten minutes) and limits lookups of new names for the whole proxy, so a flood of logins with random names no longer uses up the account service's limit for everyone. A refusal for too many requests (HTTP 429) has its own log line and metric.
- A new install writes `login.online-mode: true`. With `per-player` or `false` and no `auth` gate in `plugins.required-gates`, the proxy warns at start that players without a premium account can join under any name, and names offline UUIDs in `commands.operators`.
- `/prox claim` and `/prox op` from the game refuse an offline UUID unless the required `auth` gate runs; `prox op <uuid>` in the console still works, and `deop` always does.
- A plugin that changes permissions now needs the owner's consent: `plugins.<id>.grants: [permissions-write]` in `pumboprox.yml` besides the request in its manifest. It can only set its own nodes, never `*`, `pumbo.proxy.`, `minecraft:` or `group.` nodes, and every change is logged. A plugin that asks without the grant gets a warning at start; a new grant needs a restart, not `reload`.
- The proxy no longer follows a link out of a plugin's data folder when it writes the plugin's default files or loads its structures. Structure files must be regular files of at most 16 MiB.

### Changed

- A `pumboprox.yml` without `login.online-mode` stays on `per-player`, now with a warning at start; set the key to keep that choice. New installs get `online-mode: true`.
- The `/alert` help and the log hint for a server that does not ask for forwarding use neutral wording and name the setting for Pumpkin and Paper.

### Fixed

- A plugin stuck in a loop that keeps calling the proxy is stopped instead of holding a worker thread, and `reload`, `unload` and stopping the proxy get through while a plugin is busy.
- A flood of player input (movement, chat, plugin messages) can no longer pile up in one batch for a plugin: low-priority input is dropped first and counted in `pumbo_virtual_inputs_dropped_total`.
- Less work per connection: compression state is reused between packets, and a session no longer re-creates its timer on every packet, which cut lock waits with many players online.
- Plugins see the current server list: servers added by `reload` or `/prox servers new` and removed ones show up at once, a server run by the proxy is online only while it is ready, and player counts per server are real.
- `reload` checks the whole file, the plugin host sections and the permissions file too, before it changes anything, and keeps the old config on any error. It applies `logging.level` at once and names the changed settings that only take effect after a restart, instead of reporting success for them.
- A connection lost for a reason other than a plain quit (for example a write timeout) is logged with the error instead of as `left`.

## [0.2.5-beta] - 2026-10-11

Run and play: start `pumboprox` in an empty folder, join, and set up the whole network from the game.

### Added

- First start: a missing `pumboprox.yml` is written with the default settings, together with `forwarding.secret` and `plugins/`.
- Pumbox: while no server is available, players wait in an empty virtual world on the proxy (PumboVir) instead of being kicked, also when their server goes down with no fallback. Proxy commands and Tab completion work there; as soon as a server is ready, everyone moves to it and the Pumbox is gone. `pumbox.enabled` turns it off.
- `/prox claim <code>`: on a new network, without an operator and a server, the console shows a one-time code; the first player to type it becomes an operator. The code expires with the first server or after five wrong tries.
- `/prox op <player|uuid>` and `/prox deop <player|uuid>` edit `commands.operators` in `pumboprox.yml`, comments kept.

### Changed

- Servers from the proxy and downloads from the game are on by default: `/prox download` and `/prox servers` work without a line in `pumboprox.yml`.
- The first server from the proxy that is ready while `routing.try` is empty becomes the server players join.
- `routing.try` may name a server from the proxy that does not exist yet: a warning instead of a failed start.
- A missing `plugins/` folder is created on every start.

## [0.2.0-beta] - 2026-10-09

### Added

- Servers from the proxy (`managed-servers`, off by default): `/prox download pumpkin` downloads Pumpkin with a SHA256 check and a progress bar, `/prox servers` creates and runs servers that join the network at once.
- `/prox route`: domains, plugin gates, start servers and fallback, changed from the game into `pumboprox.yml`.
- `/send current <server>` moves everyone on your server.

### Changed

- Every proxy command lives under `/prox`; `/pumbo` only lists the plugins.
- `/prox help` lists groups with pages of their own; lines fit the chat, details are in the tooltip.
- Tab completion and a clickable usage line for every proxy command; `/server stop arena` points to `/prox servers stop arena`.

### Removed

| Removed | Use instead |
| --- | --- |
| `/prox plugin …`, `/pumbo proxy plugin …` | `/prox plugins reload\|load\|unload <id>` |
| `/prox perms …`, `/prox services`, `/pumbo proxy perms\|services` | `/prox debug perms\|services` |
| `/pumbo bridge [key]` | `/prox bridge [key]` |
| `/pumbo <plugin> <command>` | the plugin's command, e.g. `/pb history Steve` |
| `/prox server …` | `/prox servers …` |
| permissions `pumbo.proxy.plugin`, `.perms`, `.services` | `pumbo.proxy.plugins.manage`, `.debug` |

### Fixed

- Players already online see servers added by `/prox reload` or `/prox servers new` at once.

## [0.1.1-beta] - 2026-10-09

### Fixed

- Plugin help pages: the page number after `help` works again (shared plugin library).
- A failed plugin reload shows the failure in `/pumbo` right away.

## [0.1.0-beta.1] - 2026-10-09

First public beta.

[0.2.5-beta]: https://github.com/PumboMC/PumboProx/compare/v0.2.0-beta...v0.2.5-beta
[0.2.0-beta]: https://github.com/PumboMC/PumboProx/compare/v0.1.1-beta...v0.2.0-beta
[0.1.1-beta]: https://github.com/PumboMC/PumboProx/compare/v0.1.0-beta.1...v0.1.1-beta
[0.1.0-beta.1]: https://github.com/PumboMC/PumboProx/releases/tag/v0.1.0-beta.1
