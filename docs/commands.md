[PumboProx](../README.md) · [Get started](getting-started.md) · [Servers from the proxy](servers.md) · **Commands** · [PumboBridge](bridge.md) · [How it works](how-it-works.md) · [Writing plugins](writing-plugins.md)

---

# Commands

In the proxy console the same commands work without the slash. `/prox` shows only the commands you may use, with pages and tooltips.

## Players and staff

| Command | What it does | Permission |
| --- | --- | --- |
| `/server [server]` | Go to another server, or list them | `pumbo.proxy.server` |
| `/glist` | Players on every server | `pumbo.proxy.glist` |
| `/send <player\|all\|current> <server>` | Move players to a server | `pumbo.proxy.send` |
| `/find <player>` | Which server a player is on | `pumbo.proxy.find` |
| `/alert <message>` | A message to the whole network | `pumbo.proxy.alert` |

## The proxy: `/prox`

| Command | What it does | Permission |
| --- | --- | --- |
| `/prox [help] [page]` | The proxy commands you may use | |
| `/prox version` | The proxy's version and the protocols it speaks | |
| `/prox reload` | Reload `pumboprox.yml` | `pumbo.proxy.reload` |
| `/prox plugins [reload\|load\|unload <id>]` | Plugins, their versions and state; manage them while the proxy runs | `pumbo.proxy.plugins`, changes: `.plugins.manage` |
| `/prox bridge [key]` | Which servers run [PumboBridge](bridge.md), and the bridge key | `pumbo.proxy.bridge`, key: `.bridge.key` |
| `/prox route …` | The way into the network: domains, gates, servers | `pumbo.proxy.route`, changes: `.route.edit` |
| `/prox download …` | Download server software | `pumbo.proxy.download` |
| `/prox servers …` | Create and run servers | `pumbo.proxy.servers.*` |
| `/prox debug perms\|services` | How a permission is decided; plugin services | `pumbo.proxy.debug` |

`/prox route`, `/prox download` and `/prox servers` in detail: [Servers from the proxy](servers.md).

`/pumbo` lists the Pumbo plugins; a click opens a plugin's help.

## Plugins

| Plugin | Command | Short |
| --- | --- | --- |
| [PumboFilter](https://github.com/PumboMC/PumboFilter) | `/pumbofilter` | `/pf` |
| [PumboAuth](https://github.com/PumboMC/PumboAuth) | `/pumboauth` | `/pa` |
| [PumboBans](https://github.com/PumboMC/PumboBans) | `/pumbobans` | `/pb` |
| [PumboPerms](https://github.com/PumboMC/PumboPerms) | `/pumboperms` | `/pp` |

Player commands such as `/login` or `/ban`, and their aliases, are listed in each plugin's README.

## Permissions

Permissions live in `permissions.yml`, with server, group and global contexts. With PumboPerms you manage them from the game instead.

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
