//! Built-in proxy commands (§2.8): `/server`, `/glist`, `/send`, `/find`,
//! `/alert`, for players and the console. Permissions `pumbo.proxy.<name>`:
//! until the permission system (E5) `/server` is open to everyone and the
//! rest to `commands.operators` and the console.

use std::sync::Arc;

use pumbo_text::Component;
use uuid::Uuid;

use crate::server::{Proxy, Runtime};
use crate::status::parse_text;
use pumbo_text::template::escape_mini as esc;

mod help;
mod operators;
mod prox;

pub use crate::ops::{LOGIN_GATE, claim_code, claim_open};
pub(crate) use help::{DOWNLOAD_HELP, ROUTE_HELP, SERVERS_HELP, help};
use operators::claim;
pub use operators::{claim_hint, login_warnings};
use prox::prox;

pub const NAMES: &[&str] = &["server", "glist", "send", "find", "alert", "prox"];

/// `/prox` subcommands, for completion.
const PROX_SUBS: &[&str] = &[
    "help", "version", "reload", "plugins", "bridge", "route", "download", "servers", "op", "deop",
    "debug",
];

/// Who runs a command.
#[derive(Debug, Clone, Copy)]
pub enum Source<'a> {
    Player {
        id: Uuid,
        /// Current server, if any.
        server: Option<&'a str>,
    },
    Console,
}

/// Result of a command for the one who ran it.
#[derive(Debug, Default)]
pub struct Reply {
    pub lines: Vec<Component>,
    /// A player's own `/server <name>`: the session connects there.
    pub connect: Option<String>,
}

impl Reply {
    pub(crate) fn text(s: impl AsRef<str>) -> Self {
        Self {
            lines: vec![parse_text(s.as_ref())],
            connect: None,
        }
    }
}

pub fn allowed(proxy: &Proxy, rt: &Runtime, source: &Source<'_>, name: &str) -> bool {
    match source {
        Source::Console => true,
        Source::Player { id, server } => {
            // `/prox` (its help and version) is open like `/server`;
            // `pumbo.proxy.help` can close it.
            let default = name == "server" || name == "prox" || rt.operators.contains(id);
            let node = if name == "prox" { "help" } else { name };
            // With the plugin host (E5), `pumbo.proxy.<command>` decides
            // where it has an entry; `commands.operators` stays the default.
            proxy
                .plugins
                .get()
                .and_then(|p| p.permission(*id, &format!("pumbo.proxy.{node}"), *server))
                .unwrap_or(default)
        }
    }
}

/// `/prox op` and `/prox deop`.
pub(crate) const OP: &str = "pumbo.proxy.op";

/// Nodes as strong as an operator: `*` and `pumbo.proxy.*` do not grant
/// them, only an entry for exactly the node (else `commands.operators`);
/// a negated wildcard still takes them away. `pumbo.proxy.op` also needs an
/// operator: an entry can only take it away.
const EXACT_ONLY: &[&str] = pumbo_host::EXACT_ONLY;

/// A `pumbo.proxy.…` node of a `/prox` subcommand: the permission plugin
/// decides where it has an entry, else `commands.operators` and the console.
pub fn permitted(proxy: &Proxy, rt: &Runtime, source: &Source<'_>, node: &str) -> bool {
    match source {
        Source::Console => true,
        Source::Player { id, server } => {
            let operator = rt.operators.contains(id);
            if EXACT_ONLY.contains(&node) {
                let exact = proxy
                    .plugins
                    .get()
                    .and_then(|p| p.exact_permission(*id, node, *server));
                return match node {
                    OP => operator && exact != Some(false),
                    _ => exact.unwrap_or(operator),
                };
            }
            proxy
                .plugins
                .get()
                .and_then(|p| p.permission(*id, node, *server))
                .unwrap_or(operator)
        }
    }
}

/// The refusal for a node of [`EXACT_ONLY`].
pub(crate) fn exact_denied(node: &str) -> String {
    if node == OP {
        return "<err>Only an operator or the console may do that; * and pumbo.proxy.* do not include it.".into();
    }
    format!(
        "<err>{node} needs an exact entry or an operator; * and pumbo.proxy.* do not include it."
    )
}

/// Commands this source may run, for the command tree.
pub fn visible(proxy: &Proxy, rt: &Runtime, source: &Source<'_>) -> Vec<&'static str> {
    NAMES
        .iter()
        .copied()
        .filter(|n| allowed(proxy, rt, source, n) || (*n == "prox" && claim_open(proxy, rt)))
        .collect()
}

/// Name of the argument after a command in the client's command tree
/// (`/server <server>`); everything after the command is one argument whose
/// suggestions come from the proxy.
pub fn arg_name(command: &str) -> &'static str {
    match command {
        "server" => "server",
        "send" => "player server",
        "find" => "player",
        "alert" => "message",
        "prox" => "command",
        _ => "args",
    }
}

/// First word of a command line, lower case, without a namespace.
pub fn root(line: &str) -> String {
    let word = line.split(' ').next().unwrap_or_default();
    let word = word.rsplit(':').next().unwrap_or(word);
    word.to_ascii_lowercase()
}

/// Runs `line` (without the slash); `None` if it is not a proxy command or
/// the source may not run it (then it belongs to the backend).
pub fn run(proxy: &Arc<Proxy>, rt: &Runtime, source: &Source<'_>, line: &str) -> Option<Reply> {
    let name = root(line);
    let args: Vec<&str> = line.split(' ').skip(1).filter(|a| !a.is_empty()).collect();
    // Open to everyone: the way to the first operator.
    if name == "prox"
        && args
            .first()
            .is_some_and(|a| a.eq_ignore_ascii_case("claim"))
    {
        return Some(claim(proxy, rt, source, args.get(1).copied()));
    }
    if !NAMES.contains(&name.as_str()) || !allowed(proxy, rt, source, &name) {
        return None;
    }
    Some(match name.as_str() {
        "server" => server(rt, source, &args),
        "glist" => glist(proxy, rt),
        "send" => send(proxy, rt, source, &args),
        "find" => find(proxy, &args),
        "alert" => alert(proxy, line.split_once(' ').map_or("", |(_, t)| t.trim())),
        "prox" => prox(proxy, rt, source, line, &args),
        _ => return None,
    })
}

/// Gate timeouts longer than `virtual.gate-timeout-ms`: that one counts the
/// whole stay in the virtual world and ends it first.
pub fn gate_timeout_warnings(host: &pumbo_host::HostConfig, virtual_ms: u64) -> Vec<String> {
    let p = &host.plugins;
    let mut over: Vec<(String, u64)> = p
        .gate_timeouts
        .iter()
        .map(|(gate, ms)| (format!("plugins.gate-timeouts.{gate}"), *ms))
        .collect();
    over.push(("plugins.gate-timeout-ms".into(), p.gate_timeout_ms));
    over.into_iter()
        .filter(|(_, ms)| *ms > virtual_ms)
        .map(|(key, ms)| {
            format!(
                "{key} ({ms}) is longer than virtual.gate-timeout-ms ({virtual_ms}), which limits the whole stay at the gates; set virtual.gate-timeout-ms to at least the sum of the gates' timeouts"
            )
        })
        .collect()
}

fn server(rt: &Runtime, source: &Source<'_>, args: &[&str]) -> Reply {
    use crate::servers::{button, reply};
    let Source::Player { server, .. } = source else {
        return reply("<err>Only players can switch servers. <s>Use <c>send \\<player> \\<server>");
    };
    let names: Vec<&str> = rt.backends.iter().map(|b| b.name.as_str()).collect();
    match args.first() {
        None => reply(&format!(
            "<s>You are on <v>{}</v>. Servers:{}",
            esc(server.unwrap_or("-")),
            names
                .iter()
                .map(|n| button(false, n, &format!("/server {n}"), true))
                .collect::<String>()
        )),
        Some(name) => match rt
            .backends
            .iter()
            .find(|b| b.name.eq_ignore_ascii_case(name))
        {
            Some(b) => Reply {
                lines: Vec::new(),
                connect: Some(b.name.clone()),
            },
            // `/server stop arena` meant `/prox servers stop arena`.
            None if MANAGE_WORDS.contains(&name.to_ascii_lowercase().as_str()) => {
                let fixed = format!("/prox servers {}", args.join(" "));
                reply(&format!(
                    "<warn>Did you mean <c>{}</c>?{}",
                    esc(&fixed),
                    button(false, "use it", &fixed, false)
                ))
            }
            None => reply(&format!(
                "<err>There is no server named <v>{}</v>. <s>Servers:{}",
                esc(name),
                names
                    .iter()
                    .map(|n| button(false, n, &format!("/server {n}"), true))
                    .collect::<String>()
            )),
        },
    }
}

/// Words of `/prox servers …` that are no server names in `/server`.
const MANAGE_WORDS: &[&str] = &["list", "new", "start", "stop", "restart", "logs", "delete"];

fn glist(proxy: &Proxy, rt: &Runtime) -> Reply {
    let players = proxy.players();
    let mut lines = Vec::new();
    for b in &rt.backends {
        let here: Vec<&str> = players
            .iter()
            .filter(|p| p.server.as_deref() == Some(b.name.as_str()))
            .map(|p| p.name.as_str())
            .collect();
        if !here.is_empty() {
            lines.push(parse_text(&format!(
                "&a[{}] &e({}): &f{}",
                b.name,
                here.len(),
                here.join(", ")
            )));
        }
    }
    lines.push(parse_text(&format!(
        "&e{} player(s) online.",
        players.len()
    )));
    Reply {
        lines,
        connect: None,
    }
}

fn send(proxy: &Proxy, rt: &Runtime, source: &Source<'_>, args: &[&str]) -> Reply {
    use crate::ops::SendError;
    use crate::servers::{reply, usage};
    let (Some(who), Some(to)) = (args.first(), args.get(1)) else {
        return usage("/send <player|all|current> <server>");
    };
    let current = match source {
        Source::Player { server, .. } => *server,
        Source::Console => None,
    };
    match crate::ops::send_players(proxy, rt, who, current, to) {
        Ok(sent) => reply(&format!(
            "<ok>Sending <v>{}</v> player(s) to <v>{}</v>.",
            sent.players,
            esc(&sent.server)
        )),
        Err(SendError::NoServer(to)) => reply(&format!(
            "<err>There is no server named <v>{}</v>.",
            esc(&to)
        )),
        Err(SendError::NoCurrent) => {
            reply("<err>current needs a player on a server; <s>name the server instead.")
        }
        Err(SendError::NotOnline(who)) => {
            reply(&format!("<err><v>{}</v> is not online.", esc(&who)))
        }
    }
}

fn find(proxy: &Proxy, args: &[&str]) -> Reply {
    use crate::servers::{reply, usage};
    let Some(who) = args.first() else {
        return usage("/find <player>");
    };
    match proxy.find_player(who) {
        Some(p) => reply(&format!(
            "<v>{}</v> <s>is on <v>{}</v>.",
            esc(&p.name),
            esc(p.server.as_deref().unwrap_or("-"))
        )),
        None => reply(&format!("<err><v>{}</v> is not online.", esc(who))),
    }
}

fn alert(proxy: &Proxy, text: &str) -> Reply {
    use crate::servers::{reply, usage};
    if text.is_empty() {
        return usage("/alert <message>");
    }
    let n = crate::ops::alert(proxy, text);
    reply(&format!("<ok>Alert sent to <v>{n}</v> player(s)."))
}

/// Tab completion for a proxy command line (without the slash): byte offset
/// of the completed word and the matches. `None` if it is not a proxy command.
pub fn suggest(
    proxy: &Proxy,
    rt: &Runtime,
    source: &Source<'_>,
    line: &str,
) -> Option<(usize, Vec<String>)> {
    let name = root(line);
    let open = allowed(proxy, rt, source, &name);
    let claimable =
        name == "prox" && matches!(source, Source::Player { .. }) && claim_open(proxy, rt);
    if !NAMES.contains(&name.as_str()) || !(open || claimable) || !line.contains(' ') {
        return None;
    }
    let start = line.rfind(' ').map_or(0, |i| i + 1);
    let word = line.get(start..).unwrap_or_default().to_ascii_lowercase();
    let arg = line
        .get(..start)
        .unwrap_or_default()
        .split_whitespace()
        .count()
        .saturating_sub(1);
    let servers = || {
        rt.backends
            .iter()
            .map(|b| b.name.clone())
            .collect::<Vec<_>>()
    };
    let after_prox = || {
        line.get(..start)
            .unwrap_or_default()
            .split_whitespace()
            .skip(1)
            .collect::<Vec<_>>()
    };
    let players = || {
        proxy
            .players()
            .into_iter()
            .map(|p| p.name)
            .collect::<Vec<_>>()
    };
    let options = match (name.as_str(), arg) {
        ("server", 0) => servers(),
        ("find", 0) => players(),
        ("send", 0) => {
            let mut v = players();
            v.extend(["all".into(), "current".into()]);
            v.extend(servers());
            v
        }
        ("send", 1) => servers(),
        ("prox", 0) if !open => vec!["claim".into()],
        ("prox", 0) => {
            let mut v: Vec<String> = PROX_SUBS.iter().map(|s| s.to_string()).collect();
            if claimable {
                v.push("claim".into());
            }
            v
        }
        ("prox", _) if !open => Vec::new(),
        ("prox", n) => {
            let words = after_prox();
            let w = |i: usize| {
                words
                    .get(i)
                    .map(|s| s.to_ascii_lowercase())
                    .unwrap_or_default()
            };
            let plugin_ids = || {
                proxy
                    .plugins
                    .get()
                    .map(|p| p.host.plugins().iter().map(|s| s.id.clone()).collect())
                    .unwrap_or_default()
            };
            match (w(0).as_str(), n) {
                ("help", 1) => vec!["1".into(), "2".into()],
                ("plugins", 1) => vec!["reload".into(), "load".into(), "unload".into()],
                ("plugins", 2) => plugin_ids(),
                ("bridge", 1) => vec!["key".into()],
                ("op" | "deop", 1) => players(),
                ("debug", 1) => vec!["perms".into(), "services".into()],
                ("debug", 2) if w(1) == "perms" => {
                    let mut v = vec!["list".to_string()];
                    v.extend(players());
                    v
                }
                ("debug", 4) if w(1) == "perms" => servers(),
                ("route", n) => crate::route::suggest(proxy, rt, &words, n),
                ("download" | "servers", n) => crate::servers::suggest(proxy, &words, n),
                _ => Vec::new(),
            }
        }
        _ => Vec::new(),
    };
    let matches = options
        .into_iter()
        .filter(|o| o.to_ascii_lowercase().starts_with(&word))
        .collect();
    Some((start, matches))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roots() {
        assert_eq!(root("Server lobby"), "server");
        assert_eq!(root("pumbo:glist"), "glist");
        assert_eq!(root(""), "");
    }
}
