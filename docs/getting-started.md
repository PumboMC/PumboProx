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

  Inside the container `127.0.0.1` is the container itself: give the servers in `pumboprox.yml` addresses the container can reach (another container's name on the same Docker network, or `--network host` on Linux). Console: `docker attach pumboprox`, leave it with Ctrl+P Ctrl+Q. `docker stop pumboprox` shuts the proxy down cleanly.
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
# PumboProx, written on the first start. Change it and type `reload` in the
# console; it names the changes that need a restart, such as listeners.
# Every other setting has a default.

listener:
  - bind: "0.0.0.0:25565"

# Players log in with a Mojang account. "per-player" or false let players
# without one in too, under any name and without a password: use them only
# with PumboAuth (gate auth in plugins.required-gates).
login:
  online-mode: true

# Servers you run yourself. For example:
#   lobby: { address: "127.0.0.1:25566" }
servers: {}

# Servers the proxy downloads and runs itself: /prox download, /prox servers.
# The first one that is ready becomes the server players join.
managed-servers:
  enabled: true
  download-from-game: true

# Modern forwarding; servers need the secret from this file (Pumpkin:
# [networking.proxy.velocity] in pumpkin.toml, Paper: proxies.velocity).
forwarding:
  mode: modern
  secret-file: forwarding.secret

plugins:
  dir: plugins

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
