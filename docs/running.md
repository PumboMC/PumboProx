[PumboProx](../README.md) · [Get started](getting-started.md) · [Configuration](configuration.md) · [Servers from the proxy](servers.md) · **Running it** · [Upgrading](upgrading.md) · [Commands](commands.md) · [PumboBridge](bridge.md) · [Bridge protocol](bridge-protocol.md) · [How it works](how-it-works.md) · [Writing plugins](writing-plugins.md) · [Plugin reference](plugins.md)

---

# Running it

How to keep PumboProx running on a server: the servers behind it, a system service or Docker, stopping, and what to back up. For the first start, see [Get started](getting-started.md).

## Servers behind the proxy

Every server behind the proxy needs modern forwarding with the secret from `forwarding.secret`, and should listen on `127.0.0.1` or a private address only. Anyone who reaches the server directly can join it under any name.

### Pumpkin

See [Your own Pumpkin servers](getting-started.md#your-own-pumpkin-servers). Servers the proxy runs for you are set up on their own.

### Paper

You run Paper yourself; the proxy cannot download it yet. In `config/paper-global.yml`:

```yaml
proxies:
  velocity:
    enabled: true
    online-mode: true
    secret: "<contents of forwarding.secret>"
```

In `server.properties`:

```properties
online-mode=false
server-ip=127.0.0.1
```

`online-mode=false` is right here: the proxy checks the players, and the server trusts the forwarded data. If players without a premium account join (with `login.online-mode: per-player` or `false`), also set `enforce-secure-profile=false`, or they cannot chat there. The proxy says so in the log when it finds a server that enforces it.

In `pumboprox.yml`, give the server its protocol number only when it runs 26.3 (`protocol: 777`). On another version, leave `protocol` out: players then join it with their own client version, and only clients of that version get in.

### Vanilla

The vanilla server has no modern forwarding. You need a server mod that adds it (several exist for the common mod loaders) and the same secret in its config, plus `online-mode=false` in `server.properties`.

Servers older than Minecraft 1.21 are not supported.

## As a service on Linux

A systemd unit, with the proxy in `/opt/pumboprox` and its own user:

```ini
[Unit]
Description=PumboProx
After=network-online.target
Wants=network-online.target

[Service]
User=pumboprox
WorkingDirectory=/opt/pumboprox
ExecStart=/opt/pumboprox/pumboprox run /opt/pumboprox/pumboprox.yml
Restart=on-failure
# SIGTERM stops the proxy cleanly. With the default
# managed-servers.stop-timeout-secs (30 s) it needs up to 60 s.
TimeoutStopSec=60

[Install]
WantedBy=multi-user.target
```

```sh
sudo useradd --system --home /opt/pumboprox pumboprox
sudo chown -R pumboprox:pumboprox /opt/pumboprox
sudo systemctl enable --now pumboprox
journalctl -u pumboprox -f     # the log
```

Under systemd the proxy has no console: its standard input is closed, and it keeps running without one. To type console commands, run it in `screen` or `tmux` instead, or use the commands from the game.

## On Windows

`pumboprox.exe` is a console program. To start it with the machine, add a task in Task Scheduler that runs it at startup, with **Start in** set to its folder. Stop it with `stop` in its console or Ctrl+C: ending the task kills it at once, without stopping the servers it runs.

## In Docker

The image keeps its config and data in `/data` and runs as uid 65532, so that directory has to belong to that user:

```yaml
services:
  proxy:
    image: ghcr.io/pumbomc/pumboprox:beta
    ports:
      - "25565:25565"
    volumes:
      - ./data:/data
    # Enough for the default managed-servers.stop-timeout-secs (30 s).
    stop_grace_period: 60s
    # Reaps the processes of servers the proxy runs for you.
    init: true
    stdin_open: true
    tty: true
```

```sh
mkdir data && sudo chown -R 65532:65532 ./data
docker compose up -d
docker compose attach proxy    # the console; leave it with Ctrl+P Ctrl+Q
```

Run the proxy's own commands inside the container, so that the files it writes keep their owner, for example:

```sh
docker compose run --rm proxy check-config /data/pumboprox.yml
docker compose run --rm proxy migrate-config /data/pumboprox.yml
```

The image has no `HEALTHCHECK`. If your orchestrator checks over HTTP, the metrics listener answers health and readiness checks: see [Metrics and health](configuration.md#metrics-and-health). An address outside loopback needs `logging.metrics-allow-remote`.

## Stopping

`stop` in the console, SIGTERM or Ctrl+C stop the proxy cleanly: plugins shut down, and every server the proxy runs gets `stop` and `managed-servers.stop-timeout-secs` to finish before the proxy kills it. A second signal while the proxy stops ends it at once, with exit code 130. With the default timeout, give your service manager 60 seconds. When the proxy starts stopping with servers to stop, it logs the time it needs, which grows with `stop-timeout-secs`.

## Logs

The proxy logs to its standard error, as do the replies to console commands (`pumboprox 2> proxy.log` writes the log to a file, `> proxy.log 2>&1` everything); `logging` in [Configuration](configuration.md#logging) sets the level, the format and filters. Under systemd that is the journal, in Docker `docker compose logs proxy`.

## Backups

Back up these, all next to `pumboprox.yml` unless you moved them:

| File | What it holds |
| --- | --- |
| `pumboprox.yml` | The config |
| `forwarding.secret` | The secret every server checks |
| `bridge.key` | The key every PumboBridge has |
| `permissions.yml` | Permissions, when the proxy keeps them itself |
| `plugins/` | The plugins, their configs and `plugins/data/` with their data (accounts, bans, ranks) |
| `servers/`, `versions/` | Servers the proxy runs for you, with their worlds, and the downloaded server software |
| `server-icon.png` | The server icon, if you have one |

Stop the proxy before you copy `plugins/data/` and `servers/`, or copy them from a file system snapshot, so that no file is half written.

Keep `forwarding.secret` and `bridge.key`. A restored proxy with a new `forwarding.secret` locks every player out of every server until you put the new secret into each one. A new `bridge.key` disconnects every PumboBridge until each server gets the new key.
