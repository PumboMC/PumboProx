//! Shared foundation of the Pumbo plugins.
//!
//! Nothing here depends on a platform: the crate builds natively (where it is
//! tested) and for `wasm32-wasip2`, and is used by every `*-core` crate and every
//! platform layer. Loading never panics: bad input becomes a [`config::Warning`]
//! or an error value.
//!
//! - [`config`]: `config.yml` with per-option validation and defaults
//! - [`lang`]: messages from `lang/<code>.yml` with built-in fallbacks
//! - [`text`]: the shared text format (`&` codes, `{0}` / `{name}` placeholders)
//! - [`rich`]: styled segments with click and hover, which platform layers turn
//!   into their text components (plain text for the console)
//! - [`style`]: the shared look: colours, prefix, replies with a tone, values,
//!   durations and numbers in words
//! - [`help`]: help pages with aligned descriptions, tooltips and page arrows
//! - [`time`]: durations such as `1d2h30m` and `perm`
//! - [`store`]: key-value storage with transactions (redb, memory)
//! - [`command`]: subcommands, permissions `pumbo.<plugin>.<action>`, `/pumbo <plugin>`
//! - [`id`]: nicknames, UUIDs and IP addresses (IPv6 grouped by /64)
//! - [`clock`]: injectable wall clock
//! - [`gate`]: the hand-off contract of gate plugins (PumboFilter, PumboAuth)
//! - [`bans`]: what other plugins may ask PumboBans (Pumpkin `ipc`, PumboProx service)
//! - [`random`], [`map`], [`util`]: randomness, map images, small helpers

pub mod bans;
pub mod clock;
pub mod command;
pub mod config;
pub mod gate;
pub mod help;
pub mod id;
pub mod lang;
pub mod map;
pub mod pumpkin;
pub mod random;
pub mod rich;
pub mod store;
pub mod style;
pub mod text;
pub mod time;
pub mod util;
