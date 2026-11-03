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
- A dialog click (`custom_click_action`) sent by a player in play is checked against the same size and NBT depth limits as in configuration instead of passing to the server unread.
- `limits.client-bytes-per-second` counted only compressed data, so a player could make the proxy decompress far more. Decompressed data now has its own limit, four times that value, with the same kick. A packet counts against it with the size it declares, before the proxy decompresses anything.
- A player waiting in the Pumbox can run five commands at once and then one a second; more are ignored. These commands are logged at `debug` instead of `info`, so they no longer flood the log.
- The forwarding secret in a managed server's `pumpkin.toml`, its PumboBridge `config.yml` and `bridge.key` are readable only by their owner (mode 0600). Files an older version wrote are tightened when the server starts, `bridge.key` when the proxy starts.
- After a crash the proxy takes over a managed server it left running only when the process is a `pumpkin` with the recorded start time running in the server's folder. Any other process that got the same PID is left alone, with no signal. A server left running by an older version has no recorded start time and is not taken over; stop that process yourself.
- A plugin's bcrypt check refuses a hash with a cost above 14 at once, with a warning in the log, instead of holding a thread for hours.
- `permissions.provider: auto` uses only PumboPerms (`pumbo-perms`); any other permission plugin must be named in `permissions.provider`. With only another one installed, or a PumboPerms that does not load, the proxy does not start and says what to set; next to PumboPerms it is ignored with a warning. Only the chosen plugin answers `pumbo:permissions` calls of other plugins, and `services.providers` cannot pick another one.
- `/prox op` and `/prox deop` work only from the console or for an operator, and the log names who made each change. `pumbo.proxy.op`, `pumbo.proxy.bridge.key` and `pumbo.proxy.route.edit` are no longer granted by `*` or `pumbo.proxy.*`, only by an entry for exactly that node; a negated wildcard still takes them away.
- Cookies a server stores for a player stay in the proxy for that session, one set per server: another server of the network can no longer read or replace them, and a server gets its own cookies back when the player returns. Cookies that do not fit (over 5120 bytes, or more than 64 per server) are dropped with a `debug` log line. Servers with `allow-transfers: true` still use the player's own cookies in configuration and play, which go with a transfer; a cookie such a server asks for during its login comes back empty.
- A kick from a plugin (for example a ban) and a gate's decision to let a player in no longer wait in the session's command queue, so a busy session cannot lose them. A kick wins over a waiting release, and the first kick's reason is the one the player sees. Other commands dropped because a session is behind are counted in `pumboprox_dropped_packets_total{kind="session_command"}`, logged once per player at `debug` and at most once a minute at `warn`.
- A server `address` outside loopback, private networks, the 100.64.0.0/10 range (Tailscale) and IPv6 ULA now stops the start and fails `reload`. Set `servers.<name>.allow-public: true` for a server whose connection is protected some other way. Host names (for example Docker service names) are resolved at start and `reload` and must all point to private addresses; a name that does not resolve only gives a warning.
- A `bridge.listen` on `0.0.0.0`, `[::]` or a public address now stops the start and fails `reload`, unless `bridge.allow-public: true`.
- `listener[].proxy-protocol: true` now means `required`: a connection from an address outside `trusted-proxies` is refused before the handshake, warned about once per listener and counted in `pumboprox_connections_rejected_total{reason="proxy_untrusted"}`. The old behaviour, a header only from trusted addresses and plain connections from anyone else, is `proxy-protocol: optional`. `trusted-proxies` wider than /8 for public IPv4 or /16 for public IPv6 (such as `0.0.0.0/0`) stops the start unless `allow-any-proxy: true`.
- NBT lists and container contents from a server that claim more entries than the packet holds are refused, instead of costing the proxy unbounded memory or CPU, also while it translates for a player on another version.

### Added

- `limits.backend-login-timeout-ms` (default 30000): how long the proxy waits to connect and log in to a server. It was a fixed 10 s, which dropped players while a busy server was slow to let them in.
- `docs/configuration.md`: every key of `pumboprox.yml` with its default, what it does and whether `reload` applies it.
- `limits.max-players` (default `0`, off): the most players online at once. Others are kicked with `messages.network-full`; operators and players with `pumbo.proxy.join-full` still get in. `status.max-players` stays only the number shown in the server list.
- `limits.max-in-gates` (default `500`, `0` turns it off): the most players a login gate may hold in its waiting world at once. The next one is kicked with `messages.gates-full`. The number held is the gauge `pumboprox_players_in_gates`.
- `servers.<name>.allow-public`, `bridge.allow-public` and `listener[].allow-any-proxy` (all `false`), see Security.
- Connections refused by `limits.connections-per-ip-per-second` or `limits.concurrent-per-ip` are logged at `debug` with the address and the limit.
- Metric `pumboprox_backend_online{server="..."}`: 1 while the proxy counts a server as online.
- `pumboprox --help` (`-h`, `help`) and `pumboprox --version` (`-V`) exit with 0, and the console has `help`. An unknown command on the command line exits with 2.
- A key in `pumboprox.yml` that nothing reads (a typo such as `limits.max-player`) is a warning at start and on `reload`, with its line and the closest known key; `pumboprox check-config` counts it as an error and exits with 1. Errors about a bad value name the line of their key.
- `logging.filters`: log levels per module, for example `pumbo_host: debug`; they apply with `reload` like `logging.level`.
- `bridge.key-env`: the bridge key from an environment variable instead of `bridge.key`. Servers created by the proxy get the key the bridge runs with.
- `config-version` and `pumboprox migrate-config [file]`, which renames old keys in the file: comments and layout stay, the file keeps its mode and owner, and the old file is kept as `pumboprox.yml.bak` (`.bak.1` and up when that exists). Older releases do not start with a file that has `config-version: 2` (0.2.5-beta stops with "invalid type: integer `2`, expected a map"): to go back to an older PumboProx, restore `pumboprox.yml.bak`. A new install's file has no `config-version`.

### Changed

- A permission plugin named in `permissions.provider` that is not installed, or that provides no permissions, now stops the start instead of leaving an old `permissions.yml` in charge.
- A `pumboprox.yml` without `login.online-mode` stays on `per-player`, now with a warning at start; set the key to keep that choice. New installs get `online-mode: true`.
- The Docker tag `latest` moves only with stable releases; pre-releases get `beta`, and their GitHub release is marked as a pre-release.
- A release is published only from a commit whose CI passed, and its binaries and Docker image are started once with a fresh config before they go out.
- The `/alert` help and the log hint for a server that does not ask for forwarding use neutral wording and name the setting for Pumpkin and Paper.
- `virtual.gate-timeout-ms` and `plugins.gate-timeout-ms` default to 120000 (two minutes; they were 600000 and 300000). Values set in the file stay. The start warns when a plugin gate may take longer than `virtual.gate-timeout-ms`. A gate plugin with its own limit above two minutes (PumboAuth `auth.timeout-seconds`, PumboFilter `captcha.timeout-seconds`) now needs both keys raised; the proxy does not see that limit and does not warn.
- A server whose logins fail three times in a row (no connection, timeout, closed before login) counts as offline: plugins see it offline, and joins and fallbacks try it last. One player tries it again after 5 s, then after a wait that doubles up to 60 s; the first successful login brings it back. Each change is logged once. A kick from the server never counts. `/server <name>` still goes to it directly.
- Servers run by the proxy count as offline for joins and fallbacks while they are not ready.
- SIGTERM or Ctrl+C while the proxy is still starting stops it cleanly with exit code 0, plugins and managed servers included. A second signal while it stops ends it at once with exit code 130.
- `bungeecord-channel` is now `messaging-channel`. The old key still works, with a warning that names its line; when both are set, `messaging-channel` wins. `pumboprox migrate-config` renames it.
- Relative paths in `pumboprox.yml` (secrets, `plugins/`, `servers/`, `versions/`, `permissions.yml`, the server icon) are read next to the config file, wherever the proxy starts. When one of them exists only in the directory the proxy starts in, or next to the config only as an empty folder (the `plugins/` that 0.2.5-beta created there), the proxy keeps reading all of them from there and warns. Docker, which starts in `/data`, is not affected.
- A `logging.level` that is not a level stops the start (and `reload` keeps the old config) instead of running at `info`; `warning` is read as `warn`, and `info,pumbo_host=debug` still works. Replies to console commands go to stderr directly instead of through the log, so they show at every level and are plain text with `format: json`.
- The file written on the first start shows the bridge, status, limits and logging settings with their defaults as comments, and has the `bridge` section written out.

### Fixed

- `reload`, `/prox op`, `/prox route` changes and the server list updates of servers run by the proxy happen one at a time, so two changes at once no longer lose one of them. `pumboprox.yml` is written through a new temporary file that is never more open than the old one and is synced to disk before it replaces it.
- A login that timed out while a plugin was still handling the player's profile or login no longer leaves that player behind in the plugins; they now get the player's `disconnect`.
- A plugin stuck in a loop that keeps calling the proxy is stopped instead of holding a worker thread, and `reload`, `unload` and stopping the proxy get through while a plugin is busy.
- A flood of player input (movement, chat, plugin messages) can no longer pile up in one batch for a plugin: low-priority input is dropped first and counted in `pumbo_virtual_inputs_dropped_total`.
- Less work per connection: compression state is reused between packets, and a session no longer re-creates its timer on every packet, which cut lock waits with many players online.
- Plugins see the current server list: servers added by `reload` or `/prox servers new` and removed ones show up at once, a server run by the proxy is online only while it is ready, and player counts per server are real.
- `reload` checks the whole file, the plugin host sections and the permissions file too, before it changes anything, and keeps the old config on any error. It applies `logging.level` at once and names the changed settings that only take effect after a restart, instead of reporting success for them.
- A connection lost for a reason other than a plain quit (for example a write timeout) is logged with the error instead of as `left`.
- A server that stops reading no longer freezes the player's session for up to 30 s: the proxy keeps relaying to the player, answering keep-alives and taking kicks and commands while it waits. It stops reading the player while 2 MiB wait for the server, and gives the server up after 30 s without progress, as before.
- A connection's buffers give back their memory once a burst (such as the chunks sent at a join) has gone through, instead of keeping their largest size for the rest of the session. In a local test with 500 and 1000 idle players the proxy used about 0.25 MB per player instead of 0.6 MB.
- Sessions no longer fight over one shared counter on every packet. In a local test with 500 idle players the proxy's own code took less than half of the CPU samples it took before.
- Server links and report details that a server sets while the player plays now go away when the player switches to another server, as those set in configuration already did. A network that wants the same links everywhere sets them on each server.
- The warning that the permission provider is down and new logins are refused no longer shows on a plugin reload, a restart after a crash or when the proxy stops; it shows when the provider stays down.
- A malformed chunk from a server no longer ends the session of a player on an older version; the chunk is dropped instead.
- When the metrics endpoint cannot accept a connection (for example out of file descriptors), it logs a warning and waits briefly instead of keeping a CPU core busy.
- `reload` no longer starts fresh counters for `limits.max-pending-logins` and `limits.max-concurrent-has-joined`; logins in progress keep counting, and a smaller value takes effect as they finish.
- The bridge to a server that stopped reading is closed after 10 s without progress instead of showing as connected forever (a slow link that keeps reading stays), and a permission set that did not fit in a full bridge queue is sent again.
- Compiling a plugin at start, `reload` or `/prox plugins load` no longer holds up player sessions.
- `pumboprox check-config` no longer creates `forwarding.secret`, plugin folders or default plugin files; a missing secret is reported as created on the first start. A `bridge.key` that other users can read and whose mode cannot be changed (a read-only mount, another owner) is a warning instead of stopping the start.
- An unknown top-level key with a plain value (such as `limit: 5`) no longer fails the load with a wrong line number; it is reported as an unknown key.

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
