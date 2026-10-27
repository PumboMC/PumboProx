[PumboProx](../README.md) · [Get started](getting-started.md) · [Servers from the proxy](servers.md) · **Commands** · [PumboBridge](bridge.md) · [How it works](how-it-works.md) · [Writing plugins](writing-plugins.md)

---

# Commands

`<…>` is required, `[…]` is optional, `a|b` means one of them. In the proxy console the same commands work without the slash. `/prox` and `/prox help [page]` show only the commands you may use, with a tooltip on each line; `/prox route help`, `/prox download help` and `/prox servers help` show a group.

## Players and staff

| Command | Arguments | What it does | Permission |
| --- | --- | --- | --- |
| `/server` | `[server]` | Without a name: your server and the list. With a name: switches to it. Example: `/server lobby` | `pumbo.proxy.server` |
| `/glist` | | Players on each server | `pumbo.proxy.glist` |
| `/send` | `<player\|all\|current> <server>` | Sends players to a server. `current`: everyone on your server. Example: `/send Steve lobby` | `pumbo.proxy.send` |
| `/find` | `<player>` | Shows a player's server | `pumbo.proxy.find` |
| `/alert` | `<message>` | A message to the whole network, colours with `&` codes or text tags such as `<red>` and `<bold>` | `pumbo.proxy.alert` |

## The proxy: `/prox`

| Command | Arguments | What it does | Permission |
| --- | --- | --- | --- |
| `/prox` | `[help] [page]` | The proxy commands you may use | |
| `/prox version` | | The version and the protocols it speaks | |
| `/prox reload` | | Reloads `pumboprox.yml`; players stay. Checks the whole file first and keeps the old config on any error, then names the changed settings that need a restart (`listener`, `logging.format`, `logging.log-ips`, `logging.metrics-bind`, `bridge`, `managed-servers`, and the plugin host sections except `plugins.required-gates`) | `pumbo.proxy.reload` |
| `/prox plugins` | | Plugins, their versions and state | `pumbo.proxy.plugins` |
| `/prox plugins reload` | `<id>` | Reloads a plugin from its file | `pumbo.proxy.plugins.manage` |
| `/prox plugins load` | `<id>` | Loads an unloaded plugin again; a new file in `plugins/` needs a restart | `pumbo.proxy.plugins.manage` |
| `/prox plugins unload` | `<id>` | Stops a plugin until `load`; its gates deny meanwhile | `pumbo.proxy.plugins.manage` |
| `/prox bridge` | | Which servers run [PumboBridge](bridge.md) | `pumbo.proxy.bridge` |
| `/prox bridge key` | | Shows the bridge key | `pumbo.proxy.bridge.key` |
| `/prox debug perms` | `<player> <node> [server]` | How a permission is decided for a player | `pumbo.proxy.debug` |
| `/prox debug services` | | The services plugins offer | `pumbo.proxy.debug` |

## Operators

| Command | Arguments | What it does | Permission |
| --- | --- | --- | --- |
| `/prox op` | `<player\|uuid>` | Adds the player to `commands.operators` in `pumboprox.yml` (comments stay) and reloads. A name must be online; use a UUID for someone offline. Example: `/prox op Steve` | `pumbo.proxy.op` |
| `/prox deop` | `<player\|uuid>` | Removes the player from `commands.operators` | `pumbo.proxy.op` |
| `/prox claim` | `<code>` | Makes you an operator with the code from the console. Only on a new network, while it has no operator and no server; the code expires when the first server appears, and after five wrong tries | none |

Operators have every proxy permission. Without a permission plugin, everything above belongs to the operators and the console.

## The way in: `/prox route`

| Command | Arguments | What it does | Permission |
| --- | --- | --- | --- |
| `/prox route` | | The way into the network: domains → gates → start servers → fallback | `pumbo.proxy.route` |
| `/prox route servers add` | `<server> [position]` | Adds a start server; position 1 is the first one players try, without a position it goes last. Example: `/prox route servers add arena 1` | `pumbo.proxy.route.edit` |
| `/prox route servers remove` | `<server>` | Removes a start server; the last one stays | `pumbo.proxy.route.edit` |
| `/prox route servers move` | `<server> <position>` | Moves a start server. Example: `/prox route servers move lobby 1` | `pumbo.proxy.route.edit` |
| `/prox route host set` | `<domain> <server>` | A server for a domain. Example: `/prox route host set play.example.org lobby`; a domain never skips the gates | `pumbo.proxy.route.edit` |
| `/prox route host remove` | `<domain>` | The domain goes back to the start servers | `pumbo.proxy.route.edit` |
| `/prox route gates require` | `<gate>` | A login needs this gate; without the gate's plugin nobody can log in | `pumbo.proxy.route.edit` |
| `/prox route gates optional` | `<gate>` | A login goes on without this gate | `pumbo.proxy.route.edit` |

Every change is written to `pumboprox.yml` (only that key, comments stay) and applied at once.

## Server software: `/prox download`

| Command | Arguments | What it does | Permission |
| --- | --- | --- | --- |
| `/prox download` | | What you can download and the downloads running now | `pumbo.proxy.download` |
| `/prox download` | `<pumpkin\|paper>` | The releases, numbered, newest first, with `[download]` buttons | `pumbo.proxy.download` |
| `/prox download` | `<pumpkin\|paper> <#n\|version>` | Downloads a release, checked with SHA256, progress on a boss bar. Example: `/prox download pumpkin #1` | `pumbo.proxy.download` |
| `/prox download stop` | `[pumpkin\|paper] [#n\|version]` | Stops downloads; without arguments every running one | `pumbo.proxy.download` |

From the game too, unless `managed-servers.download-from-game: false`. `paper` answers "coming soon" for now.

## Servers run by the proxy: `/prox servers`

| Command | Arguments | What it does | Permission |
| --- | --- | --- | --- |
| `/prox servers` | | State, version, port, players and memory of each server | `pumbo.proxy.servers` |
| `/prox servers new` | `<name> [#n\|version] [template]` | Creates a server and starts it; the version must be downloaded, without one the newest downloaded. Example: `/prox servers new arena` | `pumbo.proxy.servers.create` |
| `/prox servers start` | `<name>` | Starts a server | `pumbo.proxy.servers.control` |
| `/prox servers stop` | `<name>` | `stop` on its console, a kill after the timeout | `pumbo.proxy.servers.control` |
| `/prox servers restart` | `<name>` | Restarts a server | `pumbo.proxy.servers.control` |
| `/prox servers logs` | `<name> [lines]` | The last console lines, 20 by default, up to 100 | `pumbo.proxy.servers.logs` |
| `/prox servers delete` | `<name> confirm` | Stops the server and moves its folder to `servers/.trash/`; nothing is deleted | `pumbo.proxy.servers.delete` |

More in [Servers from the proxy](servers.md).

## Console only

| Command | What it does |
| --- | --- |
| `reload` | Same as `prox reload` |
| `stop` or `end` | Stops the proxy and the servers it runs |
| `version` | Same as `prox version` |
| `pumbo` | The Pumbo plugins, their versions and state |

## Plugins

| Plugin | Command | Short |
| --- | --- | --- |
| [PumboFilter](https://github.com/PumboMC/PumboFilter) | `/pumbofilter` | `/pf` |
| [PumboAuth](https://github.com/PumboMC/PumboAuth) | `/pumboauth` | `/pa` |
| [PumboBans](https://github.com/PumboMC/PumboBans) | `/pumbobans` | `/pb` |
| [PumboPerms](https://github.com/PumboMC/PumboPerms) | `/pumboperms` | `/pp` |

`/pumbo` lists the Pumbo plugins; a click opens a plugin's help. Player commands such as `/login` or `/ban`, and their aliases, are listed in each plugin's README.

## Permissions

Operators (`commands.operators`, edited by `/prox op` and `/prox deop`) have every proxy permission. For anyone else, permissions live in `permissions.yml` next to the config, with server, group and global contexts. With PumboPerms you manage them from the game instead.

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
