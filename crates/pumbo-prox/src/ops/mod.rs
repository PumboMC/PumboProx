//! What the admin commands do, as functions with typed results. The
//! commands of the console and the game parse their words and turn these
//! results into text; anything else that drives the proxy calls the same
//! functions.

mod players;

pub use players::{SendError, alert, kick, send_players};

use crate::server::Proxy;

/// Reloads `pumboprox.yml`: the changed settings that apply only after a
/// restart, or why the old config stays.
pub fn reload(proxy: &Proxy) -> Result<Vec<String>, String> {
    proxy.reload()
}
