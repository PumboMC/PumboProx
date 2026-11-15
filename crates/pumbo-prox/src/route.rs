//! `/prox route`: the way a player takes into the network (domain, gates,
//! servers, fallback), shown and changed from the game or the console.
//! Changes go into `pumboprox.yml`, only the changed key (the admin's
//! comments and layout stay), and apply through a reload.

use std::sync::Arc;

use pumbo_text::template::escape_mini as esc;

use crate::commands::{Reply, Source as Who, permitted};
use crate::ops::{RouteDone, RouteEdit};
use crate::server::{Proxy, Runtime};
use crate::servers::{button, line, reply, row, usage};

const VIEW: &str = "pumbo.proxy.route";
const EDIT: &str = "pumbo.proxy.route.edit";

/// `/prox route …`; `args` are the words after `route`.
pub fn command(proxy: &Arc<Proxy>, rt: &Runtime, who: &Who<'_>, args: &[&str]) -> Reply {
    let console = matches!(who, Who::Console);
    if !permitted(proxy, rt, who, VIEW) {
        return reply("<err>You do not have permission to do that.");
    }
    let words: Vec<String> = args.iter().map(|a| a.to_string()).collect();
    let w = |i: usize| {
        words
            .get(i)
            .map(|s| s.to_ascii_lowercase())
            .unwrap_or_default()
    };
    let edit = match (w(0).as_str(), w(1).as_str()) {
        ("", _) => return show(proxy, rt, console),
        ("help", _) => {
            let page = usize::from(!console);
            return crate::commands::help(
                proxy,
                rt,
                who,
                "Way into the network",
                "/prox route",
                "/prox route help",
                crate::commands::ROUTE_HELP,
                page,
            );
        }
        ("gates", "") => return gates(proxy, console),
        ("servers", "add" | "remove" | "move")
        | ("host", "set" | "remove")
        | ("gates", "require" | "optional") => true,
        _ => false,
    };
    if !edit {
        return usage(
            "/prox route [servers add|remove|move <server> [position] | host set|remove <domain> [server] | gates [require|optional <gate>]]",
        );
    }
    if !permitted(proxy, rt, who, EDIT) {
        return reply(&crate::commands::exact_denied(EDIT));
    }
    let Some(target) = words.get(2).map(|s| s.to_string()) else {
        return usage(&format!("/prox route {} {} <name>", w(0), w(1)));
    };
    let domain = target.to_ascii_lowercase();
    let at = words.get(3).map(String::as_str);
    let edit = match (w(0).as_str(), w(1).as_str()) {
        ("servers", "add") => RouteEdit::ServerAdd { name: &target, at },
        ("servers", "remove") => RouteEdit::ServerRemove { name: &target },
        ("servers", _) => RouteEdit::ServerMove { name: &target, at },
        ("host", "remove") => RouteEdit::HostRemove { domain: &domain },
        ("host", _) => RouteEdit::HostSet {
            domain: &domain,
            server: at,
        },
        (_, "require") => RouteEdit::GateRequire { name: &target },
        _ => RouteEdit::GateOptional { name: &target },
    };
    match crate::ops::route_set(proxy, rt, edit) {
        Ok(done) => {
            let done = match done {
                RouteDone::ServerAdded { name, position } => {
                    format!("Players now try {name} as server {position}.")
                }
                RouteDone::ServerRemoved { name } => format!("{name} is off the list."),
                RouteDone::ServerMoved { name, position } => {
                    format!("{name} is now server {position}.")
                }
                RouteDone::HostRemoved { domain } => {
                    format!("{domain} uses the server list again.")
                }
                RouteDone::HostSet { domain, server } => {
                    format!("Players joining with {domain} go to {server} first.")
                }
                RouteDone::GateRequired { name } => {
                    format!("{name} is required: without it nobody can log in.")
                }
                RouteDone::GateOptional { name } => {
                    format!("{name} is optional: logins go on when it is missing.")
                }
            };
            let mut r = reply(&format!("<ok>{done}"));
            r.lines.extend(show(proxy, &proxy.runtime(), console).lines);
            r
        }
        Err(e) => reply(&format!("<err>{}", esc(&e.to_string()))),
    }
}

// ------------------------------------------------------------------ view

fn show(proxy: &Proxy, rt: &Runtime, console: bool) -> Reply {
    let cfg = &rt.config;
    let mut lines = vec![line("<s>The way into the network, step by step:")];
    // 1. Domains.
    if cfg.forced_hosts.is_empty() {
        lines.push(row(&format!(
            "<v>1 Domain</v>  <s>no domain has servers of its own{}",
            button(
                console,
                "add",
                "/prox route host set <domain> <server>",
                false
            )
        )));
    } else {
        lines.push(row(
            "<v>1 Domain</v>  <s>these domains try their servers first:",
        ));
        for (domain, order) in &cfg.forced_hosts {
            lines.push(row(&format!(
                "   <v>{}</v> <muted>→</muted> <v>{}</v>{}",
                esc(domain),
                esc(&order.join(", ")),
                button(
                    console,
                    "remove",
                    &format!("/prox route host remove {domain}"),
                    true
                )
            )));
        }
    }
    // 2. Gates.
    lines.extend(gate_rows(proxy, console, "2 Gates"));
    lines.push(row("   <muted>A domain never skips the gates."));
    // 3. Servers.
    let order = cfg.try_order();
    lines.push(row(&format!(
        "<v>3 Servers</v>  <s>tried in this order:{}",
        button(console, "add", "/prox route servers add <server>", false)
    )));
    for (i, name) in order.iter().enumerate() {
        let n = i + 1;
        let mut text = format!("   <muted>{n}.</muted> <v>{}</v>", esc(name));
        if n > 1 {
            text.push_str(&button(
                console,
                "↑",
                &format!("/prox route servers move {name} {}", n - 1),
                true,
            ));
        }
        if n < order.len() {
            text.push_str(&button(
                console,
                "↓",
                &format!("/prox route servers move {name} {}", n + 1),
                true,
            ));
        }
        if order.len() > 1 {
            text.push_str(&button(
                console,
                "remove",
                &format!("/prox route servers remove {name}"),
                true,
            ));
        }
        lines.push(row(&text));
    }
    // 4. Fallback.
    let none_left = if cfg.pumbox.enabled {
        "they wait in the Pumbox".to_string()
    } else {
        format!("<v>{}", esc(&cfg.messages.no_server))
    };
    lines.push(row(&format!(
        "<v>4 Fallback</v>  <s>a server that stops or restarts sends its players to the next one; with none left: {none_left}"
    )));
    lines.push(row(&format!(
        "<s>Commands:{}",
        if console {
            " <c>prox route help".to_string()
        } else {
            button(false, "help", "/prox route help", true)
        }
    )));
    Reply {
        lines,
        connect: None,
    }
}

fn gates(proxy: &Proxy, console: bool) -> Reply {
    let mut r = reply("<s>Gates of the plugins, in the order players pass them:");
    r.lines.extend(gate_rows(proxy, console, "Gates"));
    r
}

fn gate_rows(proxy: &Proxy, console: bool, title: &str) -> Vec<pumbo_text::Component> {
    let Some(p) = proxy.plugins.get() else {
        return vec![row(&format!("<v>{title}</v>  <s>none (no plugins)"))];
    };
    let required = p.host.required_gates();
    let mut found: Vec<(i32, String, String, bool)> = p
        .host
        .plugins()
        .iter()
        .filter_map(|s| {
            let g = s.manifest.gate.as_ref()?;
            Some((
                g.priority,
                g.name.clone(),
                s.id.clone(),
                s.status().is_coming(),
            ))
        })
        .collect();
    found.sort();
    let mut out = vec![row(&format!(
        "<v>{title}</v>  <s>{}",
        if found.is_empty() {
            "none"
        } else {
            "every player passes these:"
        }
    ))];
    for (priority, name, id, up) in &found {
        let need = required.contains(name);
        let (state, toggle) = if need {
            ("<warn>required</warn>", ("make optional", "optional"))
        } else {
            ("<s>optional</s>", ("require", "require"))
        };
        out.push(row(&format!(
            "   <v>{}</v> <muted>({}, priority {priority}{})</muted> {state}{}",
            esc(name),
            esc(id),
            if *up { "" } else { ", not running" },
            button(
                console,
                toggle.0,
                &format!("/prox route gates {} {name}", toggle.1),
                true
            )
        )));
    }
    for name in required
        .iter()
        .filter(|g| !found.iter().any(|f| &f.1 == *g))
    {
        out.push(row(&format!(
            "   <v>{}</v> <err>required, but no plugin has it: nobody can log in</err>{}",
            esc(name),
            button(
                console,
                "make optional",
                &format!("/prox route gates optional {name}"),
                true
            )
        )));
    }
    out
}

/// Completion after `prox route`: `words` are the finished words after
/// `prox`, `arg` the index of the word being typed among them.
pub fn suggest(proxy: &Proxy, rt: &Runtime, words: &[&str], arg: usize) -> Vec<String> {
    let w = |i: usize| {
        words
            .get(i)
            .map(|s| s.to_ascii_lowercase())
            .unwrap_or_default()
    };
    let gate_names = || -> Vec<String> {
        proxy
            .plugins
            .get()
            .map(|p| {
                p.host
                    .plugins()
                    .iter()
                    .filter_map(|s| s.manifest.gate.as_ref().map(|g| g.name.clone()))
                    .collect()
            })
            .unwrap_or_default()
    };
    let servers = || rt.backends.iter().map(|b| b.name.clone()).collect();
    match (w(1).as_str(), arg) {
        (_, 1) => vec![
            "help".into(),
            "servers".into(),
            "host".into(),
            "gates".into(),
        ],
        ("servers", 2) => vec!["add".into(), "remove".into(), "move".into()],
        ("host", 2) => vec!["set".into(), "remove".into()],
        ("gates", 2) => vec!["require".into(), "optional".into()],
        ("servers", 3) if w(2) == "add" => servers(),
        ("servers", 3) => rt.config.try_order(),
        ("host", 3) => rt.config.forced_hosts.keys().cloned().collect(),
        ("host", 4) if w(2) == "set" => servers(),
        ("gates", 3) => gate_names(),
        ("servers", 4) => (1..=rt.config.try_order().len() + 1)
            .map(|n| n.to_string())
            .collect(),
        _ => Vec::new(),
    }
}
