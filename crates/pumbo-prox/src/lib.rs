//! The PumboProx proxy. `modules` is the only place that knows the concrete
//! implementations of extension points; the rest of the code sees only the
//! traits from `pumbo-core`.

pub mod bridge;
pub mod bungee;
pub mod commands;
pub mod config;
pub mod conn;
pub mod limits;
pub mod logging;
pub mod metrics;
pub mod modules;
pub mod net;
mod play;
pub mod plugins;
pub mod route;
pub mod server;
pub mod servers;
pub mod session;
pub mod status;
pub mod world;
