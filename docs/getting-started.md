[PumboProx](../README.md) · **Get started** · [Servers from the proxy](servers.md) · [Commands](commands.md) · [PumboBridge](bridge.md) · [How it works](how-it-works.md) · [Writing plugins](writing-plugins.md)

---

# Get started

## 1. Get PumboProx

Pick one:

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

## 2. Configure it

`pumboprox.yml`:

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
  mode: modern                 # modern | none (none: tests only)
  secret-file: forwarding.secret
plugins:
  dir: plugins
```

## 3. Point your Pumpkin servers at it

> [!TIP]
> Run your servers on Pumpkin 0.2.0 (Minecraft 26.3): then players on every version from 1.21 to 26.3 can join.

In each server's `pumpkin.toml`, turn on Velocity forwarding with the secret from `forwarding.secret`, and let the server listen on `127.0.0.1` only:

```toml
[networking.proxy]
enabled = true

[networking.proxy.velocity]
enabled = true
secret = "<contents of forwarding.secret>"
```

On Pumpkin 0.2.0, read the [warning under Important](../README.md#important) first.

Servers the proxy runs for you get all of this on their own: see [Servers from the proxy](servers.md).

## 4. Start it

```sh
./pumboprox run pumboprox.yml     # Docker starts it for you
```

## 5. Add plugins

Put the `.wasm` files into `plugins/` and restart the proxy, or load them with `/prox plugins load <id>`.

<img src="../assets/pumbo-plugins.png" alt="/pumbo in the game: pumbo-auth, pumbo-bans, pumbo-filter and pumbo-perms 0.1.0, all running" width="100%">

## 6. Give out permissions

Permissions live in `permissions.yml`, with server, group and global contexts. With PumboPerms you manage them from the game instead. Every command and its permission: [Commands](commands.md).

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
