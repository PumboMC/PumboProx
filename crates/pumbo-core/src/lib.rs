//! PumboProx core: interfaces of pluggable modules and the factory registry.
//!
//! Rules (plan §1.1 "Modular architecture"):
//!
//! - the core knows only the traits of this crate, never concrete implementations,
//! - implementations live in separate crates (`pumbo-listener-java`,
//!   `pumbo-forwarding`, `pumbo-routing`, `pumbo-translate`, `pumbo-identity`,
//!   `pumbo-store`) and register factories in [`ModuleRegistry`],
//! - the `pumboprox` binary is the only place that assembles modules; the
//!   choice of implementation comes from the config (module name + its section),
//! - product features (filter, login, bans, skins) are WASM plugins only;
//!   the core has none of their logic.

pub mod forwarding;
pub mod identity;
pub mod listener;
pub mod permissions;
pub mod profile;
pub mod registry;
pub mod routing;
pub mod storage;
pub mod translate;
pub mod yaml;

pub use registry::{ModuleConfig, ModuleError, ModuleRegistry};

/// Future returned by traits used as `dyn`.
pub type BoxFuture<'a, T> = futures::future::BoxFuture<'a, T>;
