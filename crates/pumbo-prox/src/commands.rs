//! Built-in proxy commands (§2.8): `/server`, `/glist`, `/send`, `/find`,
//! `/alert`, for players and the console. Permissions `pumbo.proxy.<name>`:
//! until the permission system (E5) `/server` is open to everyone and the
//! rest to `commands.operators` and the console.

use std::sync::Arc;

use pumbo_host::HelpEntry;
use pumbo_text::Component;
use tracing::{info, warn};
use uuid::Uuid;

use crate::server::{Proxy, Runtime, SessionCmd};
use crate::status::parse_text;
use pumbo_text::template::escape_mini as esc;

pub const NAMES: &[&str] = &["server", "glist", "send", "find", "alert", "prox"];

/// `/prox` subcommands, for completion.
const PROX_SUBS: &[&str] = &[
    "help", "version", "reload", "plugins", "bridge", "route", "download", "servers", "op", "deop",
    "debug",
];

/// The main `/prox` help: commands for players and staff, then the proxy's
/// own, with its groups (`route`, `download`, `servers`) on pages of their own.
/// `/prox debug` is left out (developer tools).
const PROX_HELP: &[HelpEntry] = &[
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

/// A `pumbo.proxy.…` node of a `/prox` subcommand: the permission plugin
/// decides where it has an entry, else `commands.operators` and the console.
pub fn permitted(proxy: &Proxy, rt: &Runtime, source: &Source<'_>, node: &str) -> bool {
    match source {
        Source::Console => true,
        Source::Player { id, server } => proxy
            .plugins
            .get()
            .and_then(|p| p.permission(*id, node, *server))
            .unwrap_or_else(|| rt.operators.contains(id)),
    }
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

/// `/prox`: the help, the version, a config reload, the
/// plugins, the bridge, servers from the proxy, the route, developer tools.
fn prox(
    proxy: &Arc<Proxy>,
    rt: &Runtime,
    source: &Source<'_>,
    _line: &str,
    args: &[&str],
) -> Reply {
    let version = env!("CARGO_PKG_VERSION");
    let sub = args
        .first()
        .map(|a| a.to_ascii_lowercase())
        .unwrap_or_default();
    let rest: Vec<String> = args.iter().skip(1).map(|a| a.to_string()).collect();
    let player = match source {
        Source::Player { id, .. } => Some(*id),
        Source::Console => None,
    };
    match sub.as_str() {
        "download" | "servers" => crate::servers::command(proxy, rt, source, args),
        "route" => crate::route::command(proxy, rt, source, args.get(1..).unwrap_or_default()),
        "version" => Reply::text(format!(
            "<#F28C28><b>PumboProx</b></#F28C28> <white>{version}</white> <dark_gray>· protocols {}",
            proxy
                .versions
                .versions()
                .map(|v| v.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        )),
        "op" | "deop" => op(proxy, rt, source, sub == "op", args.get(1).copied()),
        "reload" if !allowed(proxy, rt, source, "reload") => denied(),
        "reload" => match proxy.reload() {
            Ok(()) => crate::servers::reply("<ok>Config reloaded."),
            Err(e) => crate::servers::reply(&format!(
                "<err>Reload failed, keeping the old config: <s>{}",
                pumbo_text::template::escape_mini(&e)
            )),
        },
        "plugins" | "debug" => {
            let Some(p) = proxy.plugins.get() else {
                return crate::servers::reply(
                    "<err>The plugin host is off <s>(plugins.dir in pumboprox.yml)",
                );
            };
            let host_args: Vec<String> = match (
                sub.as_str(),
                rest.first().map(|r| r.to_ascii_lowercase()).as_deref(),
            ) {
                ("plugins", None) => {
                    if !permitted(proxy, rt, source, "pumbo.proxy.plugins") {
                        return denied();
                    }
                    return Reply {
                        lines: p.console("pumbo").unwrap_or_default(),
                        connect: None,
                    };
                }
                ("plugins", Some("reload" | "load" | "unload")) if rest.len() >= 2 => {
                    ["plugin".to_string()]
                        .into_iter()
                        .chain(rest.iter().take(2).cloned())
                        .collect()
                }
                ("plugins", _) => {
                    return crate::servers::usage("/prox plugins [reload|load|unload <id>]");
                }
                ("debug", Some("services")) => vec!["services".into()],
                ("debug", Some("perms")) if rest.get(1).is_some_and(|w| w == "list") => {
                    vec!["perms".into(), "list".into()]
                }
                ("debug", Some("perms")) if rest.len() >= 3 => {
                    ["perms".to_string(), "check".to_string()]
                        .into_iter()
                        .chain(rest.iter().skip(1).cloned())
                        .collect()
                }
                _ => {
                    return crate::servers::usage(
                        "/prox debug perms <player> <node> [server] | perms list | services",
                    );
                }
            };
            Reply {
                lines: p.admin(player, &host_args, &|node| {
                    permitted(proxy, rt, source, node)
                }),
                connect: None,
            }
        }
        "bridge" => {
            if !permitted(proxy, rt, source, "pumbo.proxy.bridge") {
                return denied();
            }
            let Some(b) = proxy.bridge.get() else {
                return crate::servers::reply(
                    "<err>The bridge is off <s>(bridge.enabled in pumboprox.yml)",
                );
            };
            let console = player.is_none();
            let secrets = permitted(proxy, rt, source, "pumbo.proxy.bridge.key");
            let styles = pumbo_text::StyleSheet::new([
                ("p", "<aqua>"),
                ("s", "<gray>"),
                ("ok", "<green>"),
                ("warn", "<yellow>"),
                ("err", "<red>"),
                ("muted", "<dark_gray>"),
            ]);
            let lines = b
                .admin(&rest, console, secrets)
                .iter()
                .map(|l| pumbo_text::parse_mini_styled(l, &styles))
                .collect();
            Reply {
                lines,
                connect: None,
            }
        }
        _ => {
            let page = match source {
                Source::Console => 0,
                Source::Player { .. } => {
                    args.iter().find_map(|a| a.parse().ok()).unwrap_or(1).max(1)
                }
            };
            help(
                proxy,
                rt,
                source,
                "Proxy",
                "/prox",
                "/prox help",
                PROX_HELP,
                page,
            )
        }
    }
}

fn denied() -> Reply {
    crate::servers::reply("<err>You do not have permission to do that.")
}

/// Wrong codes for `/prox claim` before the code stops working.
const CLAIM_TRIES: u8 = 5;

/// A new one-time code for `/prox claim` on a fresh network: no operator
/// and no server yet. Kept in memory only, so every start has its own; the
/// first server ends it (`Proxy::replace`).
pub fn claim_code(proxy: &Proxy) -> Option<String> {
    let rt = proxy.runtime();
    if !rt.operators.is_empty() || !rt.backends.is_empty() {
        return None;
    }
    let n = u32::from_be_bytes(crate::session::random_bytes::<4>().ok()?) % 1_000_000;
    let code = format!("{n:06}");
    *proxy.claim.lock().ok()? = Some((code.clone(), 0));
    Some(code)
}

/// The login gate that proves who a player without a premium account is:
/// PumboAuth's. Other gates (the bot filter) do not count.
pub const LOGIN_GATE: &str = "auth";

/// Whether `plugins.required-gates` names [`LOGIN_GATE`].
fn login_gate_required(proxy: &Proxy) -> bool {
    proxy
        .plugins
        .get()
        .is_some_and(|p| p.host.required_gates().iter().any(|g| g == LOGIN_GATE))
}

/// [`LOGIN_GATE`] is required and its plugin runs: an offline UUID then
/// belongs to whoever passed the login.
fn login_gate_up(proxy: &Proxy) -> bool {
    login_gate_required(proxy)
        && proxy.plugins.get().is_some_and(|p| {
            p.host.plugins().iter().any(|s| {
                s.manifest
                    .gate
                    .as_ref()
                    .is_some_and(|g| g.name == LOGIN_GATE)
                    && s.status() == pumbo_host::Status::Running
            })
        })
}

/// Whether a player may become an operator: an offline UUID (version 3)
/// names anyone who joins under that name, unless the login gate checks it.
fn offline_without_login(proxy: &Proxy, id: Uuid) -> bool {
    id.get_version_num() == 3 && !login_gate_up(proxy)
}

const OFFLINE_OP: &str = "<s>Anyone who joins under that name would get it. Install PumboAuth and add auth to plugins.required-gates first, or type prox op \\<uuid> in the console.";

/// Warnings for the start: logins that let anyone in under any name.
pub fn login_warnings(proxy: &Proxy) -> Vec<String> {
    let rt = proxy.runtime();
    let login = &rt.config.login;
    let mut out = Vec::new();
    if login.online_mode.is_none() {
        out.push(
            "login.online-mode is not set in pumboprox.yml, so it is per-player; new installs get online-mode: true. Set it to keep this choice".into(),
        );
    }
    if login.online_mode() == crate::config::OnlineMode::Always || login_gate_required(proxy) {
        return out;
    }
    out.push(format!(
        "online-mode is not true and plugins.required-gates has no {LOGIN_GATE} gate: players without a premium account join under any name with no password, an operator's name too. Install PumboAuth and add {LOGIN_GATE} to plugins.required-gates, or set login.online-mode: true"
    ));
    let offline: Vec<String> = rt
        .operators
        .iter()
        .filter(|id| id.get_version_num() == 3)
        .map(Uuid::to_string)
        .collect();
    if !offline.is_empty() {
        out.push(format!(
            "commands.operators has offline UUIDs ({}): anyone who joins under those names gets every proxy permission",
            offline.join(", ")
        ));
    }
    out
}

/// What the console says about the claim code at the start.
pub fn claim_hint(proxy: &Proxy, code: &str) -> String {
    let mode = proxy.runtime().config.login.online_mode();
    if mode == crate::config::OnlineMode::Always || login_gate_required(proxy) {
        return format!("No operator yet: join and type /prox claim {code} to become one");
    }
    if mode == crate::config::OnlineMode::Never {
        return "No operator yet: type prox op <uuid> here; /prox claim needs online mode or PumboAuth".into();
    }
    format!(
        "No operator yet: join with a premium account and type /prox claim {code}, or type prox op <uuid> here"
    )
}

/// Whether `/prox claim` takes a code now.
pub fn claim_open(proxy: &Proxy, rt: &Runtime) -> bool {
    rt.operators.is_empty()
        && proxy
            .claim
            .lock()
            .is_ok_and(|c| c.as_ref().is_some_and(|(_, wrong)| *wrong < CLAIM_TRIES))
}

/// `/prox claim <code>`: the right code puts the player in
/// `commands.operators` (in the file, comments stay) and is used up.
fn claim(proxy: &Arc<Proxy>, rt: &Runtime, source: &Source<'_>, code: Option<&str>) -> Reply {
    use crate::servers::{reply, usage};
    let Source::Player { id, .. } = source else {
        return reply("<err>Only players can claim. <s>Put a UUID in commands.operators instead.");
    };
    if offline_without_login(proxy, *id) && claim_open(proxy, rt) {
        return reply(&format!(
            "<err>Your account is not premium, so you cannot claim the proxy. {OFFLINE_OP}"
        ));
    }
    let mut claim = proxy
        .claim
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some((right, wrong)) = claim.as_mut().filter(|_| rt.operators.is_empty()) else {
        return reply("<s>There is nothing to claim.");
    };
    if *wrong >= CLAIM_TRIES {
        return reply(
            "<err>The code no longer works after too many wrong tries. <s>A restart of the proxy makes a new one.",
        );
    }
    let Some(code) = code else {
        return usage("/prox claim <code>");
    };
    if code != right.as_str() {
        *wrong += 1;
        if *wrong >= CLAIM_TRIES {
            warn!(
                "the operator code no longer works after {CLAIM_TRIES} wrong tries; restart the proxy for a new one"
            );
        }
        return reply("<err>Wrong code.");
    }
    // Out of the lock: the save reloads, and a reload may end the code.
    let taken = claim.take();
    drop(claim);
    if let Err(e) = save_operator(proxy, *id, true) {
        if proxy.runtime().backends.is_empty()
            && let Ok(mut c) = proxy.claim.lock()
        {
            *c = taken;
        }
        return reply(&format!("<err>Not saved: <s>{}", esc(&e)));
    }
    let (name, ip) = proxy
        .players()
        .into_iter()
        .find(|p| p.id == *id)
        .map(|p| (p.name, proxy.ip(p.address)))
        .unwrap_or_default();
    info!("{name} ({id}, {ip}) is now an operator");
    reply("<ok>You are now an operator of this proxy.")
}

/// Adds `id` to or removes it from `commands.operators` (in the file,
/// comments stay; a failed reload puts the old file back).
fn save_operator(proxy: &Proxy, id: Uuid, add: bool) -> Result<(), String> {
    crate::route::save(proxy, |text| {
        // Keeps an operator someone wrote in the file without a reload.
        let mut ops = crate::config::Config::parse(text)
            .map(|c| c.commands.operators)
            .unwrap_or_default();
        ops.retain(|o| Uuid::parse_str(o).ok() != Some(id));
        if add {
            ops.push(id.to_string());
        }
        crate::route::set_key(
            text,
            "commands",
            "operators",
            Some(&crate::route::flow_list(&ops)),
        )
    })
}

/// `/prox op|deop <player|uuid>`: an online player's name or a UUID.
fn op(proxy: &Proxy, rt: &Runtime, source: &Source<'_>, add: bool, who: Option<&str>) -> Reply {
    use crate::servers::{reply, usage};
    if !permitted(proxy, rt, source, "pumbo.proxy.op") {
        return denied();
    }
    let Some(who) = who else {
        return usage(if add {
            "/prox op <player|uuid>"
        } else {
            "/prox deop <player|uuid>"
        });
    };
    let (id, name) = match Uuid::parse_str(who) {
        Ok(id) => match proxy.players().into_iter().find(|p| p.id == id) {
            Some(p) => (id, p.name),
            None => (id, id.to_string()),
        },
        Err(_) => match proxy.find_player(who) {
            Some(p) => (p.id, p.name),
            None => {
                return reply(&format!(
                    "<err>There is no player <v>{}</v> online. <s>Use a UUID for someone offline.",
                    esc(who)
                ));
            }
        },
    };
    if add && matches!(source, Source::Player { .. }) && offline_without_login(proxy, id) {
        return reply(&format!(
            "<err><v>{}</v> has no premium account. {OFFLINE_OP}",
            esc(&name)
        ));
    }
    if rt.operators.contains(&id) == add {
        let state = if add { "already" } else { "not" };
        return reply(&format!("<s><v>{}</v> is {state} an operator.", esc(&name)));
    }
    if let Err(e) = save_operator(proxy, id, add) {
        return reply(&format!("<err>Not saved: <s>{}", esc(&e)));
    }
    if add {
        info!("{name} ({id}) is now an operator");
        reply(&format!("<ok><v>{}</v> is now an operator.", esc(&name)))
    } else {
        info!("{name} ({id}) is no longer an operator");
        reply(&format!(
            "<ok><v>{}</v> is no longer an operator.",
            esc(&name)
        ))
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
    use crate::servers::{reply, usage};
    let (Some(who), Some(to)) = (args.first(), args.get(1)) else {
        return usage("/send <player|all|current> <server>");
    };
    let Some(target) = rt.backends.iter().find(|b| b.name.eq_ignore_ascii_case(to)) else {
        return reply(&format!(
            "<err>There is no server named <v>{}</v>.",
            esc(to)
        ));
    };
    let players = proxy.players();
    let current = match source {
        Source::Player { server, .. } => *server,
        Source::Console => None,
    };
    let chosen: Vec<_> = if who.eq_ignore_ascii_case("all") {
        players
    } else if who.eq_ignore_ascii_case("current") {
        let Some(here) = current else {
            return reply("<err>current needs a player on a server; <s>name the server instead.");
        };
        players
            .into_iter()
            .filter(|p| p.server.as_deref() == Some(here))
            .collect()
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
            None => return reply(&format!("<err><v>{}</v> is not online.", esc(who))),
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
    reply(&format!(
        "<ok>Sending <v>{n}</v> player(s) to <v>{}</v>.",
        esc(&target.name)
    ))
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
    let message = parse_text("&8[&cAlert&8] &r").append(parse_text(text));
    let players = proxy.players();
    for p in &players {
        proxy.send_to(p.id, SessionCmd::Message(Box::new(message.clone())));
    }
    reply(&format!(
        "<ok>Alert sent to <v>{}</v> player(s).",
        players.len()
    ))
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
