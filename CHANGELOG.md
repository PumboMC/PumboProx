# Changelog

## 0.2.0-beta

### Added

- Servers from the proxy (`managed-servers`, off by default): `/prox download
  pumpkin` downloads Pumpkin releases with a SHA256 check, `/prox servers
  new|start|stop|restart|logs|delete` creates and runs servers that join
  the network at once, with templates, autostart, restarts after crashes and
  taking over servers left running by a crashed proxy. Downloads show a
  progress bar to the player who started them and can be stopped.
- `/prox route`: the way into the network (domains, gates, servers,
  fallback), changed from the game into `pumboprox.yml`.

### Changed

- One place for every command: `/prox servers` (was `/prox server`),
  `/prox plugins [reload|load|unload <id>]` (was `/prox plugin`), `/prox debug
  perms|services` (was `/prox perms`, `/prox services`), `/prox download`
  (one permission `pumbo.proxy.download`; `paper` is coming soon). `/pumbo`
  only lists the plugins; a click opens a plugin's help.
- `/prox help` lists groups (`route`, `download`, `servers`) with help pages
  of their own; lines fit the chat, details are in the tooltip.
- Arguments: the client shows `/server <server>` and the other argument
  names, Tab completes every position, a missing argument answers with a
  clickable usage line, `/server stop arena` suggests `/prox servers stop
  arena`. `/send current <server>` moves everyone on your server.

### Removed

| Removed | Use instead |
| --- | --- |
| `/pumbo proxy plugin reload\|load\|unload <id>`, `/prox plugin …` | `/prox plugins reload\|load\|unload <id>` |
| `/pumbo proxy perms …`, `/prox perms …` | `/prox debug perms …` |
| `/pumbo proxy services`, `/prox services` | `/prox debug services` |
| `/pumbo bridge [key]` | `/prox bridge [key]` |
| `/pumbo <plugin> <command>` | the plugin's command, e.g. `/pb history Steve` |
| `/prox server …` | `/prox servers …` |
| console `plugin …`, `bridge` | `prox plugins …`, `prox bridge` |
| permissions `pumbo.proxy.plugin`, `.perms`, `.services`, `.servers.versions`, `.servers.download`, `.servers.list` | `pumbo.proxy.plugins.manage`, `.debug`, `.debug`, `.download`, `.download`, `.servers` |

### Fixed

- Players already online see servers added by `/prox reload` (and by
  servers from the proxy) at once in `/server`, `/send` and Tab completion.

### Known issues

- Tab completion for plugin commands (`/pp`, `/pb`…) suggests subcommands and player names only.
- `/prox download paper` is coming soon.

## 0.1.1-beta

- Plugin help pages: the page number works again.
- A failed plugin reload shows the failure in `/pumbo` right away.

## 0.1.0-beta.1

- First public beta.
