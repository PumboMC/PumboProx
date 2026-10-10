[PumboProx](../README.md) · [Get started](getting-started.md) · **Servers from the proxy** · [Commands](commands.md) · [PumboBridge](bridge.md) · [How it works](how-it-works.md) · [Writing plugins](writing-plugins.md)

---

# Servers from the proxy

PumboProx can download Pumpkin, create servers, run them and add them to the network without a restart. It is meant for the owner of a network on one machine: no customer accounts, no resource quotas.

https://github.com/user-attachments/assets/6ae651c3-0782-4c79-ac05-6018fe869001

## Turn it on

It is off by default. Add this to `pumboprox.yml` and restart the proxy:

```yaml
managed-servers:
  enabled: true
```

That's all. Everything else has a default, listed under [Options](#options). Downloads work from the console right away; to download from the game as well, as in the video, add `download-from-game: true`.

## Download Pumpkin and create a server

In the console, or in the game with the [permissions](#permissions) below:

```
prox download pumpkin              # numbered list of releases, newest first
prox download pumpkin #3           # download one (by number or tag), SHA256 checked
prox servers new arena             # created, started and added to the network
```

Then `/server arena` in the game, and you are on it.

## Manage your servers

```
prox servers                       # state, version, port, players, memory, uptime
prox servers logs arena 30         # the last 30 lines of its console
prox servers stop|start|restart arena
prox servers delete arena confirm  # stops it and moves the folder to servers/.trash/
prox download                      # what can be downloaded, what is downloading
prox download stop [name]          # stop one download or all of them
```

## Options

Only `enabled` has to be set. Change the rest when you need to:

| Option | Default | What it does |
| --- | --- | --- |
| `enabled` | `false` | Turns servers from the proxy on |
| `dir` | `servers` | Server folders, `servers.yml`, `templates/`, `.trash/` |
| `versions-dir` | `versions` | Downloads, in `versions/<software>/<tag>/` |
| `ports` | `25700-25799` | Ports for new servers, on `127.0.0.1` |
| `download-from-game` | `false` | Allow `/prox download` from the game; the console always can |
| `allow-unverified` | `false` | Accept releases without a SHA256 checksum (canary, nightly) |
| `stop-timeout-secs` | `30` | How long a server gets after `stop` before it is killed |
| `templates` | `default` | How new servers are made, see below |

### Templates

Without `templates:` every new server is made from this one:

```yaml
managed-servers:
  templates:
    default:
      source: pumpkin
      version: latest         # the newest downloaded release, or a tag
      autostart: true         # start right after `new` and with the proxy
      restart-on-crash: 3     # restarts within 10 minutes, then it stays down
```

Add your own next to it (for example `minigames` on another version) and pick it with `prox servers new <name> [#n|tag|latest] [template]`. Files in `servers/templates/<template>/`, such as `plugins/PumboBridge.wasm` or configs, are copied into every new server made from it.

## How it works

- Downloads come only from the Pumpkin-MC/Pumpkin GitHub releases, with the file for this machine, and are checked against the release's `checksums.sha256`. Development builds (canary, nightly) have no checksums and are refused unless `allow-unverified: true`.
- Each new server gets its own `pumpkin.toml`: a random seed, `127.0.0.1` and a free port of `ports`, offline mode behind the proxy, Velocity forwarding with the proxy's secret. Telemetry stays at Pumpkin's default; the file says how to turn it off. With the bridge on, PumboBridge gets its `config.yml` (address and key).
- A server is `ready` when its port answers. A crashed server is restarted after 1, 5 and 15 seconds, at most `restart-on-crash` times in 10 minutes.
- When the proxy stops, it stops its servers (`stop` on their console, a kill after `stop-timeout-secs`). If the proxy itself dies, the servers keep running and the next start takes them over (their PID is in `servers/servers.yml`).
- The player who starts a download sees its progress on a boss bar (percent, speed, size); the console logs it every 10%. A stopped download leaves no partial file.
- Servers in `servers:` of the config win over servers from the proxy with the same name. `routing.try` and `forced-hosts` may name servers from the proxy. Changes to `managed-servers` itself need a restart.

## The way into the network

`/prox route` shows the way a player takes: domains with servers of their own (`forced-hosts`), the gates of plugins in order (required or optional), the servers of `routing.try` and the fallback. Every step can be changed from the game or the console; the change is written to `pumboprox.yml` (only that key, comments stay) and applied at once:

```
prox route servers add arena 1           # try arena first
prox route servers move lobby 1
prox route host set play.example.org arena
prox route host remove play.example.org
prox route gates require auth            # no login without the auth gate
```

A domain never skips the gates.

## Permissions

| Command | Permission |
| --- | --- |
| `/prox download [pumpkin\|paper] [#n\|version]`, `/prox download stop …` | `pumbo.proxy.download` |
| `/prox servers` | `pumbo.proxy.servers` |
| `/prox servers new <name> [#n\|tag\|latest] [template]` | `pumbo.proxy.servers.create` |
| `/prox servers start\|stop\|restart <name>` | `pumbo.proxy.servers.control` |
| `/prox servers logs <name> [lines]` | `pumbo.proxy.servers.logs` |
| `/prox servers delete <name> confirm` | `pumbo.proxy.servers.delete` |
| `/prox route` | `pumbo.proxy.route` |
| `/prox route servers\|host\|gates …` | `pumbo.proxy.route.edit` |

Without a permission plugin these belong to `commands.operators` and the console.
