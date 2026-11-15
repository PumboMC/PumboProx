//! What the admin commands do, as functions with typed results. The
//! commands of the console and the game parse their words and turn these
//! results into text; anything else that drives the proxy calls the same
//! functions.

pub(crate) mod operators;
mod players;
mod route;

pub use operators::{
    Caller, ClaimError, LOGIN_GATE, OperatorError, claim, claim_code, claim_open, operators_set,
};
pub use players::{SendError, alert, kick, send_players};
pub use route::{RouteDone, RouteEdit, route_set};

use crate::server::Proxy;

/// Reloads `pumboprox.yml`: the changed settings that apply only after a
/// restart, or why the old config stays.
pub fn reload(proxy: &Proxy) -> Result<Vec<String>, String> {
    proxy.reload()
}
