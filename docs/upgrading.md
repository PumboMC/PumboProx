[PumboProx](../README.md) · [Get started](getting-started.md) · [Configuration](configuration.md) · [Servers from the proxy](servers.md) · [Running it](running.md) · **Upgrading** · [Commands](commands.md) · [PumboBridge](bridge.md) · [Bridge protocol](bridge-protocol.md) · [How it works](how-it-works.md) · [Writing plugins](writing-plugins.md) · [Plugin reference](plugins.md)

---

# Upgrading

How to move to a new release of PumboProx, go back to an older one, or move a network to the proxy or to another machine.

## A new release

1. Read the [changelog](../CHANGELOG.md) from your version up. Lines under **Changed** and **Security** can need a change in your config.
2. Make a [backup](running.md#backups).
3. Stop the proxy.
4. Replace the program (or pull the new image). Keep everything else.
5. Run `pumboprox check-config`. It reads the config and the plugins' configs the way the new version does and fails on keys it does not know.
6. Start the proxy and read the first lines of the log: renamed and unknown keys, plugin configs that lack new settings and plugins that did not load are named there. New proxy settings are in the changelog.

Plugins: a plugin built for another plugin API version does not load, and the log says so. Get a build of each plugin for the new release (see [API version](plugins.md#api-version)).

## Config versions

`pumboprox.yml` can carry a `config-version`; a file without one, such as the one a new install writes, is version 1. When the proxy reads an older file, it converts it in memory and says so in the log; the file itself stays as it is. `pumboprox migrate-config [file]` writes the converted file and keeps the old one as `pumboprox.yml.bak` next to it. Run it with the proxy stopped. In Docker, run it inside the container so that the file keeps its owner:

```sh
docker compose run --rm proxy migrate-config /data/pumboprox.yml
```

## Going back

An older release may refuse a newer config. 0.2.5-beta refuses any file with `config-version`, so after `migrate-config` go back with `pumboprox.yml.bak` (or your backup), not with the migrated file. Plugin data written by a newer plugin may not read in an older one: restore `plugins/` from the backup too.

## Notes for the next release

- Keys the proxy does not know now give a warning with the line at start and on `reload`, and fail `check-config`. Fix or remove them.
- Relative paths in the config (secrets, `plugins/`, `servers/`, the icon) are read next to the config file, not in the folder the proxy starts in. When only the old place exists, the proxy uses it and warns: move the files next to the config.
- A misspelled `logging.level` stops the start. `warning` still works.
- A metrics address outside loopback needs `logging.metrics-allow-remote: true`.
- `bungeecord-channel` is now `messaging-channel`. The old name still works, with a warning; `migrate-config` renames it.

## Moving a network to PumboProx

From another proxy, the servers stay as they are. Per server:

1. Turn on modern forwarding with the secret from PumboProx's `forwarding.secret`, as in [Servers behind the proxy](running.md#servers-behind-the-proxy). A server that used another forwarding method needs this switch, or it refuses the players.
2. Add it to `servers:` with its address, and its `protocol` if it runs 26.3.

Then in `pumboprox.yml`:

| You had | Here |
| --- | --- |
| The order servers are tried in | `routing.try` |
| Host names that lead to one server | `forced-hosts` |
| A server list text and icon | `status` and `server-icon.png` |
| Permissions in a file | `permissions.yml` ([Commands](commands.md#permissions)), or PumboPerms, which imports existing permission data |
| Staff with every permission | `commands.operators`, edited with `/prox op` |
| Bans and logins of a plugin | PumboBans and PumboAuth; see their pages for imports |

Point players' DNS (or the port) at PumboProx last, after a test with your own account.

## Moving PumboProx to another machine

1. Stop the proxy and copy everything in the [backup list](running.md#backups), with `forwarding.secret` and `bridge.key`.
2. Put the program next to the files and start it.
3. Change the addresses of servers in `servers:` and `bridge.listen` if they moved, and the proxy address in each server's PumboBridge `config.yml`.

With the same `forwarding.secret` and `bridge.key`, the servers and their bridges need no other change.
