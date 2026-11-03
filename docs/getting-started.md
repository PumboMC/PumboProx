[PumboProx](../README.md) · **Get started** · [Configuration](configuration.md) · [Servers from the proxy](servers.md) · [Commands](commands.md) · [PumboBridge](bridge.md) · [How it works](how-it-works.md) · [Writing plugins](writing-plugins.md)

---

# Get started

## 1. Get PumboProx

Pick one:

- **Download** the file for your system from [Releases](https://github.com/PumboMC/PumboProx/releases/latest): Linux (x86_64 and ARM64, also static builds), Windows (x86_64 and ARM64) or macOS. Unpack it and you have the `pumboprox` program.
- **Docker**: an image for `linux/amd64` and `linux/arm64`, `ghcr.io/pumbomc/pumboprox:<version>`. It holds only the program and runs as a non-root user. `latest` is the newest stable release and `beta` the newest pre-release; there is no stable release yet, so `latest` stays on an older beta and `beta` is the one to use. Mount a directory as `/data`: the proxy writes its config, secrets, data and `plugins/` there on the first start.

  ```sh
  mkdir pumbo && cd pumbo        # pumboprox.yml goes here
  docker run -dit --name pumboprox -p 25565:25565 \
    -v "$PWD:/data" --user "$(id -u):$(id -g)" \
    ghcr.io/pumbomc/pumboprox:beta
  ```

  Inside the container `127.0.0.1` is the container itself: give the servers in `pumboprox.yml` addresses the container can reach (another container's name on the same Docker network, or `--network host` on Linux). A server name has to resolve to a private address, as it does on a Docker network. Console: `docker attach pumboprox`, leave it with Ctrl+P Ctrl+Q. `docker stop pumboprox` shuts the proxy down cleanly.
- **Build from source** (Rust stable):

  ```sh
  git clone https://github.com/PumboMC/PumboProx
  cd PumboProx
  cargo build --release -p pumbo-prox
  ```

## 2. Start it

On macOS, unblock the downloaded files once with `xattr -dr com.apple.quarantine .` in the unpacked folder.

```sh
./pumboprox          # Windows: pumboprox.exe; Docker starts it for you
```

On the first start the proxy writes everything it needs next to itself: `pumboprox.yml` with the default settings, `forwarding.secret` and an empty `plugins/`. The console then works right away.

## 3. Become an operator

A new network has no operator. The console shows a one-time code:

```
No operator yet: join and type /prox claim 482193 to become one
```

Join (`localhost`, or the machine's address) with a premium account and type `/prox claim 482193`. You are added to `commands.operators` in `pumboprox.yml`. The code works only while the network has no operator and no server, and expires when the first server appears. Later, `prox op <player>` and `prox deop <player>` in the console (or the game) add and remove operators.

> [!WARNING]
> With `login.online-mode: per-player` or `false`, players without a premium account join under any name they type, with no password. Use these modes only with [PumboAuth](https://github.com/PumboMC/PumboAuth) and `auth` in `plugins.required-gates`. Until then the proxy warns at every start, and an account without premium (an offline UUID) can become an operator only from the console (`prox op <uuid>`): anyone who joins under that name would get its permissions.

## 4. Get a server

Without a server, players wait in the **Pumbox**, an empty world on the proxy. From the game or the console:

```
/prox download pumpkin          # the releases, newest first
/prox download pumpkin #1       # downloads one, SHA256 checked
/prox servers new lobby         # creates and starts it
```

When `lobby` is ready, it becomes the server players join, and everyone in the Pumbox moves there. More servers: `/prox servers new arena`, then `/server arena`. All of it in [Servers from the proxy](servers.md).

## 5. Add plugins

Put the `.wasm` files into `plugins/` and restart the proxy. Each plugin creates its folder with a commented `config.yml` on its first start.

<img src="../assets/pumbo-plugins.png" alt="/pumbo in the game: pumbo-auth, pumbo-bans, pumbo-filter and pumbo-perms 0.1.0, all running" width="100%">

## The config

The file written on the first start:

```yaml
# PumboProx, written on the first start; the proxy never overwrites it.
# Change it and type `reload` in the console; it names the changes that need
# a restart, such as listeners. A line "# key: value" shows a setting with
# its default: remove the "# " to change it. All settings and their
# defaults are in docs/configuration.md of the PumboProx repository.

listener:
  - bind: "0.0.0.0:25565"
    # Behind DDoS protection or a load balancer that sends the PROXY header:
    # proxy-protocol: true             # only trusted-proxies may connect
    # trusted-proxies: ["10.0.0.0/8"]  # optional instead of true also lets players in directly

# The entry in the server list. The MOTD takes & colour codes or text tags.
# status:
#   motd: "&aMy network"
#   max-players: 500            # only the number shown; see limits.max-players
#   favicon: server-icon.png    # a 64x64 PNG next to this file

# Players log in with a Mojang account. "per-player" or false let players
# without one in too, under any name and without a password: use them only
# with PumboAuth (gate auth in plugins.required-gates).
login:
  online-mode: true
  # accept-transfers: false     # true: players sent here by /transfer

# Servers you run yourself: replace {} with them, for example
# { lobby: { address: "127.0.0.1:25566" } }. Addresses outside private networks
# (and Tailscale) are refused unless the server has allow-public: true:
# forwarding is not encrypted.
servers: {}

# Servers the proxy downloads and runs itself: /prox download, /prox servers.
# The first one that is ready becomes the server players join.
managed-servers:
  enabled: true
  download-from-game: true
  # ports: 25700-25799          # ports of new servers

# Modern forwarding; servers need the secret from this file (Pumpkin:
# [networking.proxy.velocity] in pumpkin.toml, Paper: proxies.velocity).
forwarding:
  mode: modern
  secret-file: forwarding.secret
  # secret-env: PUMBO_FORWARDING_SECRET   # the secret from this variable instead

plugins:
  dir: plugins

# PumboBridge: the plugin on your Pumpkin servers connects here, so ranks and
# commands of the proxy reach them. Servers from /prox servers new get it
# installed and set up; on servers you run yourself put PumboBridge into
# plugins/ and copy the key from /prox bridge key.
bridge:
  enabled: true
  listen: "127.0.0.1:25578"
  key-file: bridge.key
  # key-env: PUMBO_BRIDGE_KEY   # the key from this variable instead
  # allow-public: false         # true accepts 0.0.0.0 or a public address

# Limits against floods, per address and for the whole proxy. max-players:
# players online at most (0: no limit; operators join anyway); max-in-gates:
# players the login gates (PumboAuth, PumboFilter) hold at once.
# limits:
#   max-players: 0
#   max-in-gates: 500
#   connections-per-ip-per-second: 3
#   concurrent-per-ip: 5
#   status-per-ip-per-second: 10
#   max-pending-logins: 2000

# Logs: error, warn, info, debug, trace or off, and levels per module. With
# log-ips false addresses show as a hash; metrics-bind serves Prometheus
# metrics on a loopback address.
# logging:
#   level: info
#   filters:
#     pumbo_host: debug
#   log-ips: true
#   metrics-bind: "127.0.0.1:9100"

# Players wait in an empty world while no server is available, and join one
# as soon as it is ready.
pumbox:
  enabled: true

# Player UUIDs with every proxy permission. /prox op and /prox deop edit it.
# On a new network, without operators or servers, the console shows a code:
# the first player to type /prox claim <code> is added here.
commands:
  operators: []
```

Change it and type `reload` in the console. Every key, its default and whether `reload` applies it: [Configuration](configuration.md). Every command is in [Commands](commands.md), with permissions.

## Your own Pumpkin servers

Servers you run yourself go into `servers:`:

```yaml
servers:
  lobby: { address: "127.0.0.1:25566", protocol: 777 }
```

A server cannot send players to another address with a `transfer` (the `/transfer` command): one to another server of the network becomes a normal switch, any other is dropped. Add `allow-transfers: true` to a server to let its transfers through.

> [!TIP]
> Run your servers on Pumpkin 0.2.0 (Minecraft 26.3): then players on every version from 1.21 to 26.3 can join.

In each server's `pumpkin.toml`, turn on modern forwarding with the secret from `forwarding.secret`, and let the server listen on `127.0.0.1` only:

```toml
[networking.proxy]
enabled = true

[networking.proxy.velocity]
enabled = true
secret = "<contents of forwarding.secret>"
```

On Pumpkin 0.2.0, read the [warning under Important](../README.md#important) first. Servers the proxy runs for you get all of this on their own.
