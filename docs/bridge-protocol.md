[PumboProx](../README.md) · [Get started](getting-started.md) · [Configuration](configuration.md) · [Servers from the proxy](servers.md) · [Commands](commands.md) · [PumboBridge](bridge.md) · **Bridge protocol** · [How it works](how-it-works.md) · [Writing plugins](writing-plugins.md) · [Plugin reference](plugins.md)

---

# Bridge protocol

The wire protocol between PumboProx and a bridge on a server (the [PumboBridge](bridge.md) plugin, or your own for another server engine). This page describes protocol **1.0**, the version in [`crates/pumbo-bridge-proto`](../crates/pumbo-bridge-proto) (`PROTO = [1, 0]`). That crate is MIT/Apache-2.0 and holds the message types, the frame codec and the handshake; a bridge written in Rust can use it as it is.

## Connection

The bridge opens one TCP connection to the proxy's `bridge.listen` address (default `127.0.0.1:25578`) and keeps it open; when the connection drops, the bridge connects again. Frames are signed, not encrypted: keep the address on loopback or a private network (the proxy refuses a public one unless `bridge.allow-public: true`, see [Configuration](configuration.md#bridge)).

A newer session paired with the same server replaces the older one.

## Frames

Every message is one CBOR map (RFC 8949) in a frame. All integers in frame headers are big-endian.

Before the handshake ends:

```
u32 length | CBOR
```

`length` counts the CBOR bytes and is at most 4096.

After the handshake:

```
u32 length | u64 seq | CBOR | tag (16 bytes)
```

`length` counts `seq`, the CBOR and the tag, at most 1049600 bytes (1 MiB + 1 KiB). `seq` is 0 for the first frame in each direction and grows by one with every frame. `tag` is the first 16 bytes of

```
HMAC-SHA256(Ks, direction | seq | CBOR)
```

where `direction` is the byte `P` (0x50) for frames the proxy sends and `B` (0x42) for frames the bridge sends, and `seq` the same 8 bytes as in the frame. A frame with a wrong tag or an unexpected `seq` ends the session. So a frame cannot be replayed, reordered or reflected back to its sender, and a frame from another session fails its tag.

A frame that is too long ends the session too.

As an example, the `ping` message before the handshake is these 12 bytes:

```
00 00 00 08  a1 61 74 64 70 69 6e 67
```

## Messages

The map key `t` names the message; every other key is a field. Names are kebab-case. Fields marked optional may be missing. A receiver skips map keys it does not know, and a message with an unknown `t` is skipped, so a minor version can add messages and optional fields.

Encodings:

- UUID: a byte string of 16 bytes.
- Key, nonce, proof (32 bytes) and instance (16 bytes): an array of integers 0-255, one per byte.
- Text shown to players: a JSON text component, as a CBOR text string.
- Worlds: dimension names such as `minecraft:overworld`.

| `t` | Direction | Fields |
| --- | --- | --- |
| `hello` | B→P | `proto` [major, minor], `bridge` (bridge version), `pumpkin` (server engine version), `mc` (Minecraft version), `instance` (16 random bytes, fixed while the server runs), `nb` (32-byte nonce) |
| `challenge` | P→B | `np` (32-byte nonce), `proof` (32 bytes) |
| `auth` | B→P | `proof` (32 bytes) |
| `info` | B→P | `caps` (methods this bridge can run), `catalog` (permission nodes of the server's built-in commands) |
| `seen` | B→P | `nonce` (32 hex digits from a pairing ping) |
| `welcome` | P→B | `server` (its name in the proxy), `groups` (its server groups), `stats-ms`, `player-stats-ms` (how often to send stats) |
| `sync` | B→P | `players` (each `player`, `name`, `pos`, `gamemode`), `worlds` (each `name`, `dimension`, `players`) |
| `cmd` | P→B | `id` (unsigned), `method`, `args` (a CBOR value, see [Methods](#methods)) |
| `res` | B→P | `id`, and either `ok` (a CBOR value) or `err` (an [error code](#errors)) with optional `detail` |
| `ev` | B→P | `ev` (an [event](#events)) |
| `stats-server` | B→P | `tps`, `mspt` (floats), `worlds` |
| `stats-player` | B→P | `players`: per player `player` and only the changed fields of `health`, `food`, `level`, `gamemode`, `world` |
| `ping`, `pong` | both | none |

A position (`pos`) is a map of `world`, `x`, `y`, `z` (floats) and optional `yaw` and `pitch`. A game mode is `survival`, `creative`, `adventure` or `spectator`.

## Handshake

The bridge key `K` is 32 bytes. The proxy creates it on the first start in `bridge.key-file` (64 hex digits) and shows it with `/prox bridge key`. Each bridge gets the same key in its config.

`HMAC` below is HMAC-SHA256, and `|` joins byte strings.

1. B→P `hello`. The proxy closes the connection when `proto[0]` is not its own major version (1).
2. P→B `challenge` with a fresh nonce `np` and `proof = HMAC(K, "P" | nb | np | instance)`.
3. The bridge checks the proof before it sends anything else. With a wrong proof it hangs up: the other end does not have the key. Otherwise B→P `auth` with `proof = HMAC(K, "B" | np | nb | instance)`.
4. The proxy checks that proof. Both ends then switch to signed frames with the session key `Ks = HMAC(K, "S" | nb | np)`, `seq` starting at 0 in each direction.

The whole handshake has to finish within 5 seconds. Compare proofs in constant time.

Test vectors, with `K` = 32 bytes `0x11`, `nb` = 32 bytes `0x22`, `np` = 32 bytes `0x33`, `instance` = 16 bytes `0x44`:

```
proxy proof   623e37cc00224135bc40f7f6dbee1d5fb06b57d4253af57ca52cc7883feab203
bridge proof  ab3443c70252ea827d53977c33fbe2713227475539e52af5cedb5ffdfb1bb081
Ks            00e6e624ea0c231bc693898def7cbb739a3bb358b87980504b65c6fec21e96dd
```

## Pairing

After `auth` the proxy does not know yet which of its `servers` the bridge runs on. It finds out with a status ping:

1. The bridge sends `info` right after `auth`.
2. While a session waits, the proxy sends every server in `servers` a status request (a Minecraft handshake with next state 1) whose server address is `pumbo-bridge.<nonce>`, with a fresh 32-hex-digit nonce per server, at once and then every 10 seconds.
3. A bridge that sees such a handshake on its server sends `seen` with the nonce. The proxy matches the nonce to the server it pinged and answers `welcome`.
4. The bridge sends `sync` with the players and worlds it has. The proxy then sends the permissions of those players (`perm-set`) and its permission table (`perms-export`).

A session that matches no server after three rounds is closed. One server engine answering for two server names is not supported: the second name stays without a bridge.

## Keeping the session

The proxy sends `ping` every 5 seconds; the bridge answers `pong` (and the other way round, if the bridge pings). Any frame counts as a sign of life. After 15 seconds without a frame from the bridge the proxy closes the session. A write that makes no progress for 10 seconds closes it too.

After `welcome` the bridge sends `stats-server` every `stats-ms` and `stats-player` every `player-stats-ms` milliseconds.

## Methods

The proxy sends `cmd` and waits for the `res` with the same `id`, at most `bridge.command-timeout-ms` (default 5 s); several commands can be waiting at once. A bridge that does not have a method answers `unsupported`.

Argument maps (field names as written; `?` marks an optional field):

| Method | `args` | `ok` |
| --- | --- | --- |
| `teleport` | `player`, `to`: one of `{pos: <pos>}`, `{player: <uuid>}`, `{spawn: <dimension or null>}` | none |
| `set-gamemode` | `player`, `mode` | none |
| `heal` | `player`, `health?` (default: maximum), `food?`, `saturation?`, `extinguish?` (default `false`) | none |
| `effect` | `player`, `op` (`add`, `remove`, `clear`), `id?` (`minecraft:speed`; for `add` and `remove`), `amplifier?`, `seconds?`, `particles?` | none |
| `fly` | `player`, `allow`, `flying?`, `speed?` | none |
| `inv-get` | `player`, `part` (`main` 0-35, `armor` 0-3, `offhand` 0, `ender` 0-26) | list of `{slot, item}` |
| `inv-set` | `player`, `part`, `slot`, `item?` (missing: empty the slot) | none |
| `inv-give` | `player`, `item` | `{leftover}` |
| `inv-clear` | `player`, `part?` (missing: all parts) | none |
| `show-items` | `viewer`, `title` (JSON text), `rows` (1-6), `items` (list of `{slot, item}`) | none |
| `perm-set` | `player`, `nodes` (node → bool), `extra` (node → bool, kept by the bridge for itself), `info?` (JSON text of the permission plugin) | none |
| `perms-export` | `fingerprint` (SHA-256 hex of `data`), `data` (the permission table as JSON text) | none |
| `q-player` | `player` | `pos`, `gamemode`, `health`, `max-health`, `food`, `saturation`, `level`, `xp`, `flying` |
| `q-server` | none | `tps`, `mspt`, `players`, `worlds` |
| `q-spawn` | `world?` (missing: the overworld) | a position |
| `q-entities` | `world?` | dimension → number of entities; at most once a second, else `busy` |

An item is a map of `id` (`minecraft:...`), `count`, and optional `name` (JSON text), `lore` (JSON texts), `ench` (list of `{id, lvl}`), `dmg` and `raw` (`{fmt, data}`: the item's components in the format of one server engine release, such as `pumpkin-0.2.0`; a bridge with the same `fmt` restores the item exactly, another one uses the other fields).

Plugins on the proxy reach these methods through the `pumbo:bridge@1.0` service. Any field `server` in their arguments picks the server; the bridge ignores it. `perm-set` and `perms-export` come only from the proxy itself. `guard-rules` is reserved and not sent in 1.0.

## Events

`ev` carries a map whose key `kind` names the event:

| `kind` | Fields |
| --- | --- |
| `join` | `player`, `name`, `pos` |
| `leave` | `player`, `pos` (the last position) |
| `death` | `player`, `pos`, `message` (JSON text), `killer?` |
| `respawn` | `player`, `pos` |
| `world` | `player`, `from`, `to`, `pos` |
| `perms-mode` | `mode`: `bridge` (the bridge applies the proxy's permissions) or `local` (a permission plugin on the server decides; the proxy stops sending `perm-set`) |

`denied` is reserved and not sent in 1.0. An unknown `kind` is skipped.

## Errors

`err` in a `res`, also seen by plugins:

| Code | Meaning |
| --- | --- |
| `no-player` | The player is not on that server |
| `bad-args` | The arguments do not fit the method |
| `unknown-id` | An item, effect or world the server does not know |
| `unsupported` | This bridge, or this server version, does not have the method |
| `busy` | Asked again too soon |
| `expired` | A held teleport waited too long for the previous one (proxy) |
| `superseded` | A newer teleport for the same player replaced a held one (proxy) |
| `failed` | The server could not do it |
| `no-bridge`, `timeout`, `disconnected`, `not-allowed`, `offline` | Set by the proxy: no session for that server, no answer in time, the session ended, the plugin may not call this method, the player is not online |

## Versions

`proto` is [major, minor]. The proxy accepts any minor of its major and closes the connection on another major, with a line in its log and `/prox bridge` showing why. A minor version only adds messages, methods and optional fields, which older receivers skip. A change that older ends cannot skip gets a new major.

The plugin service `pumbo:bridge@1.0` has its own version, 1.0.

## Plugin messages

The bridge does not use plugin message channels. The proxy never relays a channel that starts with `pumbo:bridge`, from a client or from a server.
