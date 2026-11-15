//! The way into the network: start servers, domains and required gates,
//! changed in `pumboprox.yml` (only the changed key) and applied by a reload.

use crate::config_edit::{flow_list, save, set_key};
use crate::server::{Proxy, Runtime};

/// A change of the route; positions are the words as typed (`1` is first).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteEdit<'a> {
    /// A start server, at `at` or last.
    ServerAdd {
        name: &'a str,
        at: Option<&'a str>,
    },
    ServerRemove {
        name: &'a str,
    },
    /// A start server to `at`, or last.
    ServerMove {
        name: &'a str,
        at: Option<&'a str>,
    },
    /// The server a domain tries first.
    HostSet {
        domain: &'a str,
        server: Option<&'a str>,
    },
    /// The domain uses the server list again.
    HostRemove {
        domain: &'a str,
    },
    /// Nobody logs in without the gate.
    GateRequire {
        name: &'a str,
    },
    /// Logins go on without the gate.
    GateOptional {
        name: &'a str,
    },
}

/// What [`route_set`] changed; positions count from 1.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouteDone {
    ServerAdded { name: String, position: usize },
    ServerRemoved { name: String },
    ServerMoved { name: String, position: usize },
    HostSet { domain: String, server: String },
    HostRemoved { domain: String },
    GateRequired { name: String },
    GateOptional { name: String },
}

/// Why [`route_set`] changed nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouteError {
    NoServer(String),
    AlreadyListed(String),
    /// A position as typed and the last one allowed.
    Position(String, usize),
    NotListed(String),
    /// The last start server stays.
    LastServer,
    NotADomain(String),
    NoHost(String),
    /// `host set` without a server.
    NoHostServer,
    PluginsOff,
    NoGate(String),
    AlreadyRequired(String),
    NotRequired(String),
    /// The config file was not changed or the reload refused it.
    Save(String),
}

impl std::fmt::Display for RouteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoServer(name) => write!(f, "there is no server named {name}"),
            Self::AlreadyListed(name) => write!(f, "{name} is in the list already"),
            Self::Position(p, max) => write!(f, "position {p} is not 1 to {max}"),
            Self::NotListed(name) => write!(f, "{name} is not in the list"),
            Self::LastServer => f.write_str("the last server stays: players need one to join"),
            Self::NotADomain(domain) => write!(f, "{domain} is not a domain"),
            Self::NoHost(domain) => write!(f, "{domain} has no servers of its own"),
            Self::NoHostServer => {
                f.write_str("name the server: /prox route host set <domain> <server>")
            }
            Self::PluginsOff => f.write_str("the plugin host is off"),
            Self::NoGate(name) => write!(f, "no plugin has the gate {name}"),
            Self::AlreadyRequired(name) => write!(f, "{name} is required already"),
            Self::NotRequired(name) => write!(f, "{name} is not required"),
            Self::Save(e) => f.write_str(e),
        }
    }
}

/// Changes the route in the config file and reloads.
pub fn route_set(
    proxy: &Proxy,
    rt: &Runtime,
    edit: RouteEdit<'_>,
) -> Result<RouteDone, RouteError> {
    match edit {
        RouteEdit::ServerAdd { name, at } => servers(proxy, rt, "add", name, at),
        RouteEdit::ServerRemove { name } => servers(proxy, rt, "remove", name, None),
        RouteEdit::ServerMove { name, at } => servers(proxy, rt, "move", name, at),
        RouteEdit::HostSet { domain, server } => host(proxy, rt, "set", domain, server),
        RouteEdit::HostRemove { domain } => host(proxy, rt, "remove", domain, None),
        RouteEdit::GateRequire { name } => gate(proxy, "require", name),
        RouteEdit::GateOptional { name } => gate(proxy, "optional", name),
    }
}

fn servers(
    proxy: &Proxy,
    rt: &Runtime,
    op: &str,
    name: &str,
    at: Option<&str>,
) -> Result<RouteDone, RouteError> {
    let mut order = rt.config.try_order();
    let known = |n: &str| rt.backends.iter().any(|b| b.name == n);
    let pos = |order: &[String]| -> Result<usize, RouteError> {
        match at {
            None => Ok(order.len()),
            Some(p) => p
                .parse::<usize>()
                .ok()
                .filter(|p| (1..=order.len() + 1).contains(p))
                .map(|p| p - 1)
                .ok_or_else(|| RouteError::Position(p.to_string(), order.len() + 1)),
        }
    };
    let done = match op {
        "add" => {
            if !known(name) {
                return Err(RouteError::NoServer(name.to_string()));
            }
            if order.iter().any(|n| n == name) {
                return Err(RouteError::AlreadyListed(name.to_string()));
            }
            let p = pos(&order)?;
            order.insert(p, name.to_string());
            RouteDone::ServerAdded {
                name: name.to_string(),
                position: p + 1,
            }
        }
        "remove" => {
            let before = order.len();
            order.retain(|n| n != name);
            if order.len() == before {
                return Err(RouteError::NotListed(name.to_string()));
            }
            if order.is_empty() {
                return Err(RouteError::LastServer);
            }
            RouteDone::ServerRemoved {
                name: name.to_string(),
            }
        }
        _ => {
            if !order.iter().any(|n| n == name) {
                return Err(RouteError::NotListed(name.to_string()));
            }
            order.retain(|n| n != name);
            let p = pos(&order)?;
            order.insert(p, name.to_string());
            RouteDone::ServerMoved {
                name: name.to_string(),
                position: p + 1,
            }
        }
    };
    save(proxy, |text| {
        set_key(text, "routing", "try", Some(&flow_list(&order)))
    })
    .map_err(RouteError::Save)?;
    Ok(done)
}

fn host(
    proxy: &Proxy,
    rt: &Runtime,
    op: &str,
    domain: &str,
    server: Option<&str>,
) -> Result<RouteDone, RouteError> {
    let ok = !domain.is_empty()
        && domain.len() <= 253
        && domain
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-'));
    if !ok {
        return Err(RouteError::NotADomain(domain.to_string()));
    }
    if op == "remove" {
        if !rt.config.forced_hosts.contains_key(domain) {
            return Err(RouteError::NoHost(domain.to_string()));
        }
        save(proxy, |text| set_key(text, "forced-hosts", domain, None))
            .map_err(RouteError::Save)?;
        return Ok(RouteDone::HostRemoved {
            domain: domain.to_string(),
        });
    }
    let Some(server) = server else {
        return Err(RouteError::NoHostServer);
    };
    if !rt.backends.iter().any(|b| b.name == server) {
        return Err(RouteError::NoServer(server.to_string()));
    }
    save(proxy, |text| {
        set_key(
            text,
            "forced-hosts",
            domain,
            Some(&flow_list(&[server.to_string()])),
        )
    })
    .map_err(RouteError::Save)?;
    Ok(RouteDone::HostSet {
        domain: domain.to_string(),
        server: server.to_string(),
    })
}

fn gate(proxy: &Proxy, op: &str, name: &str) -> Result<RouteDone, RouteError> {
    let host = proxy
        .plugins
        .get()
        .map(|p| p.host.clone())
        .ok_or(RouteError::PluginsOff)?;
    let mut required = host.required_gates();
    if op == "require" {
        // A gate nobody provides would close the network for everyone.
        let loaded = host
            .plugins()
            .iter()
            .any(|s| s.manifest.gate.as_ref().is_some_and(|g| g.name == name));
        if !loaded {
            return Err(RouteError::NoGate(name.to_string()));
        }
        if required.iter().any(|g| g == name) {
            return Err(RouteError::AlreadyRequired(name.to_string()));
        }
        required.push(name.to_string());
    } else {
        let before = required.len();
        required.retain(|g| g != name);
        if required.len() == before {
            return Err(RouteError::NotRequired(name.to_string()));
        }
    }
    save(proxy, |text| {
        set_key(
            text,
            "plugins",
            "required-gates",
            Some(&flow_list(&required)),
        )
    })
    .map_err(RouteError::Save)?;
    let name = name.to_string();
    Ok(if op == "require" {
        RouteDone::GateRequired { name }
    } else {
        RouteDone::GateOptional { name }
    })
}
