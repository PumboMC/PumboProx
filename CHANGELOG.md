# Changelog

All notable changes to PumboProx, one section per release. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [0.2.0-beta] - 2026-10-09

### Added

- Servers from the proxy (`managed-servers`, off by default): `/prox download pumpkin` downloads Pumpkin with a SHA256 check and a progress bar, `/prox servers` creates and runs servers that join the network at once.
- `/prox route`: domains, plugin gates, start servers and fallback, changed from the game into `pumboprox.yml`.
- `/send current <server>` moves everyone on your server.

### Changed

- Every proxy command lives under `/prox`; `/pumbo` only lists the plugins.
- `/prox help` lists groups with pages of their own; lines fit the chat, details are in the tooltip.
- Tab completion and a clickable usage line for every proxy command; `/server stop arena` points to `/prox servers stop arena`.

### Removed

| Removed | Use instead |
| --- | --- |
| `/prox plugin …`, `/pumbo proxy plugin …` | `/prox plugins reload\|load\|unload <id>` |
| `/prox perms …`, `/prox services`, `/pumbo proxy perms\|services` | `/prox debug perms\|services` |
| `/pumbo bridge [key]` | `/prox bridge [key]` |
| `/pumbo <plugin> <command>` | the plugin's command, e.g. `/pb history Steve` |
| `/prox server …` | `/prox servers …` |
| permissions `pumbo.proxy.plugin`, `.perms`, `.services` | `pumbo.proxy.plugins.manage`, `.debug` |

### Fixed

- Players already online see servers added by `/prox reload` or `/prox servers new` at once.

## [0.1.1-beta] - 2026-10-09

### Fixed

- Plugin help pages: the page number after `help` works again (shared plugin library).
- A failed plugin reload shows the failure in `/pumbo` right away.

## [0.1.0-beta.1] - 2026-10-09

First public beta.

[0.2.0-beta]: https://github.com/PumboMC/PumboProx/compare/v0.1.1-beta...v0.2.0-beta
[0.1.1-beta]: https://github.com/PumboMC/PumboProx/compare/v0.1.0-beta.1...v0.1.1-beta
[0.1.0-beta.1]: https://github.com/PumboMC/PumboProx/releases/tag/v0.1.0-beta.1
