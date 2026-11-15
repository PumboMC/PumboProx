//! Servers run by the proxy (`managed-servers`).

use std::future::Future;
use std::sync::Arc;

use pumbo_servers::Manager;

use crate::server::Runtime;

/// What [`server_control`] does to a server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Start,
    Stop,
    Restart,
}

/// Why a name is not a server of the proxy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerError {
    /// The server comes from `servers:` in the config.
    NotManaged(String),
    Unknown(String),
}

/// Whether `name` is a server the proxy runs.
pub fn managed_server(m: &Manager, rt: &Runtime, name: &str) -> Result<(), ServerError> {
    if m.entry(name).is_none() && rt.config.servers.contains_key(name) {
        return Err(ServerError::NotManaged(name.to_string()));
    }
    if m.entry(name).is_none() {
        return Err(ServerError::Unknown(name.to_string()));
    }
    Ok(())
}

/// Starts, stops or restarts server `name` of the proxy. A start is under
/// way when this returns (ready comes later, `Manager::state`); the future
/// ends when a stop or a restart is done.
pub fn server_control(
    m: &Arc<Manager>,
    name: &str,
    action: Action,
) -> Result<impl Future<Output = Result<(), String>> + Send + 'static, String> {
    if action == Action::Start {
        m.start(name)?;
    }
    let (m, name) = (m.clone(), name.to_string());
    Ok(async move {
        match action {
            Action::Start => Ok(()),
            Action::Stop => m.stop(&name).await,
            Action::Restart => m.restart(&name).await,
        }
    })
}

/// The last `lines` console lines of server `name`, 1 to 100.
pub fn managed_logs(m: &Manager, name: &str, lines: usize) -> Vec<String> {
    m.logs(name, lines.clamp(1, 100)).unwrap_or_default()
}
