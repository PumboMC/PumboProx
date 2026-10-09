//! Built-in proxy commands (§2.8): `/server`, `/glist`, `/send`, `/find`,
//! `/alert`, for players and the console. Permissions `pumbo.proxy.<name>`:
//! until the permission system (E5) `/server` is open to everyone and the
//! rest to `commands.operators` and the console.

use pumbo_host::HelpEntry;
use pumbo_text::Component;
use uuid::Uuid;

use crate::server::{Proxy, Runtime, SessionCmd};
use crate::status::parse_text;

pub const NAMES: &[&str] = &["server", "glist", "send", "find", "alert", "prox"];

/// `/prox` subcommands, for completion.
const PROX_SUBS: &[&str] = &[
    "help", "version", "reload", "plugins", "plugin", "bridge", "perms", "services",
];

/// Lines of the `/prox` help; `pumbo.proxy.*` nodes as for the command each
/// one stands for.
const PROX_HELP: &[HelpEntry] = &[
    entry(
        "/server",
        "[name]",
        "Shows or switches your server",
        Some("pumbo.proxy.server"),
    ),
    entry(
        "/glist",
        "",
        "Lists players on each server",
        Some("pumbo.proxy.glist"),
    ),
    entry(
        "/send",
        "<player|all> <server>",
        "Sends players to a server",
        Some("pumbo.proxy.send"),
    ),
    entry(
        "/find",
        "<player>",
        "Shows a player's server",
        Some("pumbo.proxy.find"),
    ),
    entry(
        "/alert",
        "<message>",
        "Broadcasts a message",
        Some("pumbo.proxy.alert"),
    ),
    entry("/prox version", "", "Proxy version", None),
    entry(
        "/prox reload",
        "",
        "Reloads pumboprox.yml",
        Some("pumbo.proxy.reload"),
    ),
    entry(
        "/prox plugins",
        "",
        "Lists plugins",
        Some("pumbo.proxy.plugins"),
    ),
    entry(
        "/prox plugin",
        "reload|load|unload <id>",
        "Reloads, loads or unloads a plugin",
        Some("pumbo.proxy.plugin"),
    ),
    entry(
        "/prox bridge",
        "[status|key]",
        "PumboBridge state and key",
        Some("pumbo.proxy.bridge"),
    ),
    entry(
        "/prox perms",
        "list|check <player> <node> [server]",
        "Permission nodes and checks",
        Some("pumbo.proxy.perms"),
    ),
    entry(
        "/prox services",
        "",
        "Plugin services",
        Some("pumbo.proxy.services"),
    ),
];

const fn entry(
    command: &'static str,
    args: &'static str,
    summary: &'static str,
    permission: Option<&'static str>,
) -> HelpEntry {
    HelpEntry {
        command,
        args,
        summary,
        permission,
    }
}

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
    fn text(s: impl AsRef<str>) -> Self {
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

/// Commands this source may run, for the command tree.
pub fn visible(proxy: &Proxy, rt: &Runtime, source: &Source<'_>) -> Vec<&'static str> {
    NAMES
        .iter()
        .copied()
        .filter(|n| allowed(proxy, rt, source, n))
        .collect()
}

/// First word of a command line, lower case, without a namespace.
pub fn root(line: &str) -> String {
    let word = line.split(' ').next().unwrap_or_default();
    let word = word.rsplit(':').next().unwrap_or(word);
    word.to_ascii_lowercase()
}

/// Runs `line` (without the slash); `None` if it is not a proxy command or
/// the source may not run it (then it belongs to the backend).
pub fn run(proxy: &Proxy, rt: &Runtime, source: &Source<'_>, line: &str) -> Option<Reply> {
    let name = root(line);
    if !NAMES.contains(&name.as_str()) || !allowed(proxy, rt, source, &name) {
        return None;
    }
    let args: Vec<&str> = line.split(' ').skip(1).filter(|a| !a.is_empty()).collect();
    Some(match name.as_str() {
        "server" => server(rt, source, &args),
        "glist" => glist(proxy, rt),
        "send" => send(proxy, rt, &args),
        "find" => find(proxy, &args),
        "alert" => alert(proxy, line.split_once(' ').map_or("", |(_, t)| t.trim())),
        "prox" => prox(proxy, rt, source, line, &args),
        _ => return None,
    })
}

/// The line `/prox <sub>` stands for: `/pumbo proxy …`, `/pumbo bridge …`,
/// `/pumbo` or the console's `reload` and `version`. `None`: the help.
pub fn prox_target(line: &str) -> Option<String> {
    let rest = line.split_once(' ')?.1.trim();
    let (sub, tail) = rest.split_once(' ').unwrap_or((rest, ""));
    let sub = sub.to_ascii_lowercase();
    Some(match sub.as_str() {
        "plugins" => "pumbo".into(),
        "plugin" | "perms" | "services" => format!("pumbo proxy {sub} {tail}").trim_end().into(),
        "bridge" => format!("pumbo bridge {tail}").trim_end().into(),
        "reload" | "version" => sub,
        _ => return None,
    })
}

/// `/prox` (like `/velocity`): the help, the version, a config reload, and
/// the host's `/pumbo` admin commands under shorter names.
fn prox(proxy: &Proxy, rt: &Runtime, source: &Source<'_>, line: &str, args: &[&str]) -> Reply {
    let version = env!("CARGO_PKG_VERSION");
    let Some(target) = prox_target(line) else {
        let page = match source {
            Source::Console => 0,
            Source::Player { .. } => args.get(1).and_then(|p| p.parse().ok()).unwrap_or(1).max(1),
        };
        let can = |node: &str| match source {
            Source::Console => true,
            Source::Player { id, server } => match node.strip_prefix("pumbo.proxy.") {
                Some(n) if NAMES.contains(&n) || n == "reload" => allowed(proxy, rt, source, n),
                // Host commands answer by the host's own decision.
                _ => proxy
                    .plugins
                    .get()
                    .and_then(|p| p.permission(*id, node, *server))
                    .unwrap_or(false),
            },
        };
        let lines =
            pumbo_host::help_page("PumboProx", version, "Proxy", "/prox", PROX_HELP, page, can);
        return Reply {
            lines,
            connect: None,
        };
    };
    match (target.as_str(), source) {
        ("version", _) => Reply::text(format!(
            "<#F28C28><b>PumboProx</b></#F28C28> <white>{version}</white> <dark_gray>· protocols {}",
            proxy
                .versions
                .versions()
                .map(|v| v.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        )),
        ("reload", _) if !allowed(proxy, rt, source, "reload") => {
            Reply::text("&cYou do not have permission to do that.")
        }
        ("reload", _) => match proxy.reload() {
            Ok(()) => Reply::text("&aConfig reloaded."),
            Err(e) => Reply::text(format!("&cReload failed, keeping the old config: {e}")),
        },
        (t, Source::Player { id, .. }) => match proxy.plugins.get() {
            Some(p) => Reply {
                lines: p.command(*id, t).unwrap_or_default(),
                connect: None,
            },
            None => Reply::text("&cThe plugin host is off."),
        },
        (t, Source::Console) => match proxy.plugins.get() {
            Some(p) => Reply {
                lines: p.console(t).unwrap_or_default(),
                connect: None,
            },
            None => Reply::text("&cThe plugin host is off."),
        },
    }
}

fn server(rt: &Runtime, source: &Source<'_>, args: &[&str]) -> Reply {
    let Source::Player { server, .. } = source else {
        return Reply::text("Only players can switch servers. Use send <player> <server>.");
    };
    match args.first() {
        None => {
            let all: Vec<&str> = rt.backends.iter().map(|b| b.name.as_str()).collect();
            Reply::text(format!(
                "&eYou are on &f{}&e. Servers: &f{}",
                server.unwrap_or("-"),
                all.join(", ")
            ))
        }
        Some(name) => match rt
            .backends
            .iter()
            .find(|b| b.name.eq_ignore_ascii_case(name))
        {
            Some(b) => Reply {
                lines: Vec::new(),
                connect: Some(b.name.clone()),
            },
            None => Reply::text(format!("&cThere is no server named {name}.")),
        },
    }
}

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

fn send(proxy: &Proxy, rt: &Runtime, args: &[&str]) -> Reply {
    let (Some(who), Some(to)) = (args.first(), args.get(1)) else {
        return Reply::text("&cUsage: send <player|all|server> <server>");
    };
    let Some(target) = rt.backends.iter().find(|b| b.name.eq_ignore_ascii_case(to)) else {
        return Reply::text(format!("&cThere is no server named {to}."));
    };
    let players = proxy.players();
    let chosen: Vec<_> = if who.eq_ignore_ascii_case("all") {
        players
    } else if let Some(from) = rt
        .backends
        .iter()
        .find(|b| b.name.eq_ignore_ascii_case(who))
    {
        players
            .into_iter()
            .filter(|p| p.server.as_deref() == Some(from.name.as_str()))
            .collect()
    } else {
        match proxy.find_player(who) {
            Some(p) => vec![p],
            None => return Reply::text(format!("&c{who} is not online.")),
        }
    };
    let n = chosen
        .iter()
        .filter(|p| {
            proxy.send_to(
                p.id,
                SessionCmd::Connect {
                    server: target.name.clone(),
                    quiet: true,
                },
            )
        })
        .count();
    Reply::text(format!("&eSending {n} player(s) to {}.", target.name))
}

fn find(proxy: &Proxy, args: &[&str]) -> Reply {
    let Some(who) = args.first() else {
        return Reply::text("&cUsage: find <player>");
    };
    match proxy.find_player(who) {
        Some(p) => Reply::text(format!(
            "&f{} &eis on &f{}&e.",
            p.name,
            p.server.as_deref().unwrap_or("-")
        )),
        None => Reply::text(format!("&c{who} is not online.")),
    }
}

fn alert(proxy: &Proxy, text: &str) -> Reply {
    if text.is_empty() {
        return Reply::text("&cUsage: alert <message>");
    }
    let message = parse_text("&8[&cAlert&8] &r").append(parse_text(text));
    let players = proxy.players();
    for p in &players {
        proxy.send_to(p.id, SessionCmd::Message(Box::new(message.clone())));
    }
    Reply::text(format!("&eAlert sent to {} player(s).", players.len()))
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
    if !NAMES.contains(&name.as_str()) || !allowed(proxy, rt, source, &name) || !line.contains(' ')
    {
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
            v.push("all".into());
            v.extend(servers());
            v
        }
        ("send", 1) => servers(),
        ("prox", 0) => PROX_SUBS.iter().map(|s| s.to_string()).collect(),
        ("prox", 1) => match line
            .split_whitespace()
            .nth(1)
            .map(str::to_ascii_lowercase)
            .as_deref()
        {
            Some("plugin") => vec!["reload".into(), "load".into(), "unload".into()],
            Some("bridge") => vec!["status".into(), "key".into()],
            Some("perms") => vec!["list".into(), "check".into()],
            _ => Vec::new(),
        },
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
