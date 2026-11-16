//! The help pages of `/prox` and its groups.

use pumbo_host::HelpEntry;

use super::{NAMES, Reply, Source, allowed, permitted};
use crate::ops::claim_open;
use crate::server::{Proxy, Runtime};

/// The main `/prox` help: commands for players and staff, then the proxy's
/// own, with its groups (`route`, `download`, `servers`) on pages of their own.
/// `/prox debug` is left out (developer tools).
pub(super) const PROX_HELP: &[HelpEntry] = &[
    entry(
        "/server",
        "[server]",
        "Shows or switches your server",
        "Without a name: your server and the list.\nExample: /server lobby",
        Some("pumbo.proxy.server"),
    ),
    entry(
        "/glist",
        "",
        "Players on each server",
        "",
        Some("pumbo.proxy.glist"),
    ),
    entry(
        "/send",
        "<player|all|current> <server>",
        "Sends players to a server",
        "current: everyone on your server.\nExample: /send Steve lobby",
        Some("pumbo.proxy.send"),
    ),
    entry(
        "/find",
        "<player>",
        "Shows a player's server",
        "",
        Some("pumbo.proxy.find"),
    ),
    entry(
        "/alert",
        "<message>",
        "Message to the whole network",
        "Colours with & codes or text tags, e.g. <red> or <bold>.",
        Some("pumbo.proxy.alert"),
    ),
    entry(
        "/prox version",
        "",
        "Proxy version",
        "The version and the protocols it speaks.",
        None,
    ),
    entry(
        "/prox reload",
        "",
        "Reloads pumboprox.yml",
        "Players stay; listeners need a restart.",
        Some("pumbo.proxy.reload"),
    ),
    entry(
        "/prox plugins",
        "[reload|load|unload <id>]",
        "Plugins and their state",
        "Without arguments: the list.\nChanges need pumbo.proxy.plugins.manage.",
        Some("pumbo.proxy.plugins"),
    ),
    entry(
        "/prox bridge",
        "[key]",
        "PumboBridge on the servers",
        "key shows the bridge key (pumbo.proxy.bridge.key).",
        Some("pumbo.proxy.bridge"),
    ),
    entry(
        "/prox route",
        "…",
        "Way into the network",
        "Domains, gates, start and fallback servers.\nClick, then Enter: the commands of this group.",
        Some("pumbo.proxy.route"),
    ),
    entry(
        "/prox download",
        "…",
        "Server software",
        "Pumpkin releases with a SHA256 check.\nClick, then Enter: the commands of this group.",
        Some("pumbo.proxy.download"),
    ),
    entry(
        "/prox servers",
        "…",
        "Servers run by the proxy",
        "Create, start, stop, logs.\nClick, then Enter: the commands of this group.",
        Some("pumbo.proxy.servers"),
    ),
    entry(
        "/prox op",
        "<player|uuid>",
        "Makes a player an operator",
        "Adds the UUID to commands.operators.\nExample: /prox op Steve",
        Some("pumbo.proxy.op"),
    ),
    entry(
        "/prox deop",
        "<player|uuid>",
        "Takes operator away",
        "Removes the UUID from commands.operators.",
        Some("pumbo.proxy.op"),
    ),
    entry(
        "/prox claim",
        "<code>",
        "Become an operator",
        "With the code from the console, while the network has no operator and no server.",
        Some("pumbo.proxy.claim"),
    ),
];

/// `/prox route help`.
pub(crate) const ROUTE_HELP: &[HelpEntry] = &[
    entry(
        "/prox route",
        "",
        "Shows the way into the network",
        "Domains → gates → start servers → fallback.",
        Some("pumbo.proxy.route"),
    ),
    entry(
        "/prox route servers add",
        "<server> [position]",
        "Adds a start server",
        "Example: /prox route servers add arena 1",
        Some("pumbo.proxy.route.edit"),
    ),
    entry(
        "/prox route servers remove",
        "<server>",
        "Removes a start server",
        "The last one stays.",
        Some("pumbo.proxy.route.edit"),
    ),
    entry(
        "/prox route servers move",
        "<server> <position>",
        "Moves a start server",
        "Example: /prox route servers move lobby 1",
        Some("pumbo.proxy.route.edit"),
    ),
    entry(
        "/prox route host set",
        "<domain> <server>",
        "Server for a domain",
        "Example: /prox route host set play.example.org lobby\nA domain never skips the gates.",
        Some("pumbo.proxy.route.edit"),
    ),
    entry(
        "/prox route host remove",
        "<domain>",
        "Domain back to the list",
        "",
        Some("pumbo.proxy.route.edit"),
    ),
    entry(
        "/prox route gates require",
        "<gate>",
        "Login needs this gate",
        "Without the gate's plugin nobody can log in.",
        Some("pumbo.proxy.route.edit"),
    ),
    entry(
        "/prox route gates optional",
        "<gate>",
        "Login goes on without it",
        "",
        Some("pumbo.proxy.route.edit"),
    ),
];

/// `/prox download help`.
pub(crate) const DOWNLOAD_HELP: &[HelpEntry] = &[
    entry(
        "/prox download",
        "",
        "What you can download",
        "Server software and running downloads.",
        Some("pumbo.proxy.download"),
    ),
    entry(
        "/prox download",
        "<pumpkin|paper>",
        "Lists the releases",
        "Numbered, newest first, with [download] buttons.",
        Some("pumbo.proxy.download"),
    ),
    entry(
        "/prox download",
        "<pumpkin|paper> <#n|version>",
        "Downloads a release",
        "Checked with SHA256, progress on a boss bar.\nExample: /prox download pumpkin #1",
        Some("pumbo.proxy.download"),
    ),
    entry(
        "/prox download stop",
        "[pumpkin|paper] [#n|version]",
        "Stops downloads",
        "Without arguments: every running download.",
        Some("pumbo.proxy.download"),
    ),
];

/// `/prox servers help`.
pub(crate) const SERVERS_HELP: &[HelpEntry] = &[
    entry(
        "/prox servers",
        "",
        "Servers run by the proxy",
        "State, version, port, players, memory.",
        Some("pumbo.proxy.servers"),
    ),
    entry(
        "/prox servers new",
        "<name> [#n|version] [template]",
        "Creates a server",
        "Example: /prox servers new arena\nThe version must be downloaded.",
        Some("pumbo.proxy.servers.create"),
    ),
    entry(
        "/prox servers start",
        "<name>",
        "Starts a server",
        "",
        Some("pumbo.proxy.servers.control"),
    ),
    entry(
        "/prox servers stop",
        "<name>",
        "Stops a server",
        "stop on its console, a kill after the timeout.",
        Some("pumbo.proxy.servers.control"),
    ),
    entry(
        "/prox servers restart",
        "<name>",
        "Restarts a server",
        "",
        Some("pumbo.proxy.servers.control"),
    ),
    entry(
        "/prox servers logs",
        "<name> [lines]",
        "Last console lines",
        "Up to 100 lines, 20 by default.",
        Some("pumbo.proxy.servers.logs"),
    ),
    entry(
        "/prox servers delete",
        "<name> confirm",
        "Moves a server to the trash",
        "The folder goes to servers/.trash, nothing is deleted.",
        Some("pumbo.proxy.servers.delete"),
    ),
];

const fn entry(
    command: &'static str,
    args: &'static str,
    summary: &'static str,
    details: &'static str,
    permission: Option<&'static str>,
) -> HelpEntry {
    HelpEntry {
        command,
        args,
        summary,
        details,
        permission,
    }
}

/// A help page of `/prox` or one of its groups, with the entries the source
/// may use (servers from the proxy only while they are on).
#[allow(clippy::too_many_arguments)]
pub(crate) fn help(
    proxy: &Proxy,
    rt: &Runtime,
    source: &Source<'_>,
    section: &str,
    root: &str,
    help_command: &str,
    entries: &[HelpEntry],
    page: usize,
) -> Reply {
    let can = |node: &str| match node.strip_prefix("pumbo.proxy.") {
        Some("claim") => matches!(source, Source::Player { .. }) && claim_open(proxy, rt),
        Some(n) if n.starts_with("servers") => {
            proxy.servers.get().is_some() && permitted(proxy, rt, source, node)
        }
        Some(n) if NAMES.contains(&n) => allowed(proxy, rt, source, n),
        _ => permitted(proxy, rt, source, node),
    };
    let head = pumbo_host::HelpHeader {
        title: "PumboProx",
        version: env!("CARGO_PKG_VERSION"),
        section,
        root,
        help_command,
    };
    Reply {
        lines: pumbo_host::help_page(&head, entries, page, can),
        connect: None,
    }
}
