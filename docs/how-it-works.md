[PumboProx](../README.md) · [Get started](getting-started.md) · [Servers from the proxy](servers.md) · [Commands](commands.md) · [PumboBridge](bridge.md) · **How it works** · [Writing plugins](writing-plugins.md)

---

# How it works

<img src="../assets/diagram.webp" alt="PumboProx: the core, PumboAPI and plugins/ on the proxy; forwarding and the bridge to lobby, survival and skyblock, each with PumboBridge" width="100%">

Players connect to the proxy. It checks them, logs them in and sends them to a server with Velocity forwarding, so every server sees the real UUID, IP and skin. Moving players between servers needs nothing more. Every server also runs [PumboBridge](bridge.md), so the proxy can act inside it as well: teleport players within the world, change game modes and apply its ranks there.

Plugins from the Pumpkin market keep working on your servers as usual. The proxy only adds what a network needs on top.

```
Player (Minecraft Java 1.21 – 26.3)
   │
   ▼
PumboProx ──────────────────────────────── one program (Rust), one config
├── core
│   ├── protocol 1.21 – 26.3
│   ├── premium and offline logins       encryption, Mojang sessions
│   ├── version translation              older clients on 26.3 servers
│   └── forwarding                       name, UUID, IP, skin → server
│
├── PumboAPI ─────────────────────────── what every plugin builds on
│   ├── services, placeholders, permissions, events
│   ├── contexts: server > group > everywhere
│   ├── per-server and per-group plugin config
│   └── PumboVir: virtual worlds, checks before any server
│
└── plugins/ (.wasm) ─────────────────── the logic, the data, the config
    ├── PumboFilter    anti-bot checks in a virtual world
    ├── PumboAuth      accounts, premium auto-login, 2FA
    ├── PumboBans      bans, mutes, warnings
    ├── PumboPerms     ranks and permissions for the whole network
    ├── PumboPM        plugin manager                           (soon)
    ├── PumboTabasco   tab list, scoreboard, boss bars, chat    (soon)
    ├── PumboDB        one database for all Pumbo plugins       (soon)
    ├── PumboCore      /home, /spawn, /tp …                     (soon)
    └── PumboGuard     region protection                        (soon)
   │
   │  forwarding + bridge (signed with a key)
   ├──────────────────────┬────────────────────────┐
   ▼                      ▼                        ▼
lobby                  survival-1, survival-2    skyblock
(plain Pumpkin)        (group "survival")        (plain Pumpkin)
└─ PumboBridge.wasm    └─ PumboBridge.wasm       ├─ PumboBridge.wasm
                                                 └─ any other plugins
```

## How a player gets in

PumboFilter and PumboAuth hold a new player in a virtual world on the proxy until the checks and the login are done.

```
Player joins
   │
   ▼
Login on the proxy ──────────────────────── before encryption
   ├── PumboFilter   counts connections, attack mode, auto-ban of addresses that keep failing
   └── PumboAuth     refuses bad nicknames, locked accounts, new players while registrations are closed
   │
   ▼
Gates in a virtual world (PumboVir) ─────── no server sees the player yet
   │
   ├── 1. PumboFilter                       gate "filter"
   │      ├── let through: premium (outside an attack), verified in the last 12 h,
   │      │                whitelist, pumbo.filter.bypass
   │      ├── gravity check      falls for 6.4 s, compared with vanilla physics
   │      ├── client check       brand and settings of the game
   │      ├── CAPTCHA if needed  a code on a map in hand → /captcha <code>
   │      └── passed → next gate in the same connection, no reconnect screen
   │
   └── 2. PumboAuth                         gate "auth"
          ├── premium            logged in by Mojang, no password
          ├── session            joined from here in the last 60 min
          ├── new player         /register <password> <password>
          ├── known player       /login <password>   (+ /2fa <code> with 2FA on)
          ├── countdown bar, lockout after wrong passwords, argon2id
          └── logged in → a server
   │
   ▼
lobby (Pumpkin) ─────────────────────────── /changepassword, /logout, /2fa, /premium
                                            are taken by the proxy and never reach a server
```

Which gates a player passes, and in which order, is set with `/prox route gates` (see [Servers from the proxy](servers.md#the-way-into-the-network)).

## Plugins work together

You don't wire anything up. Each Pumbo plugin looks for the others when it starts and uses what it finds. Take one away and the rest keep running on their own.

| When you have | What happens |
| --- | --- |
| **PumboFilter + PumboAuth** | A new player passes the bot check and goes straight to the login, in the same connection, with no reconnect screen. |
| **PumboFilter + PumboBans** | Addresses that keep failing the bot check get a temporary IP ban through PumboBans (`auto-ban` in the config). |
| **PumboBans + PumboAuth** | A ban ends the player's login sessions, so after the ban is lifted they type their password again. |
| **PumboPerms on the proxy + PumboBridge** | Ranks you set on the proxy with `/pp` apply on every server right away. A PumboPerms that also runs on a server steps back and lets the proxy decide. |
| **Any plugin + PumboBridge** | The plugin can teleport players within a server's world, change game modes or open inventories on any server, and read `%server_tps:lobby%` and other placeholders. |
