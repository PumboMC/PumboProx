//! `/prox route`: the way a player takes into the network (domain, gates,
//! servers, fallback), shown and changed from the game or the console.
//! Changes go into `pumboprox.yml`, only the changed key (the admin's
//! comments and layout stay), and apply through a reload.

use std::sync::Arc;

use pumbo_text::template::escape_mini as esc;

use crate::commands::{Reply, Source as Who, permitted};
use crate::config_edit::{flow_list, save, set_key};
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
    let result = match w(0).as_str() {
        "servers" => servers(proxy, rt, &w(1), &target, words.get(3)),
        "host" => host(proxy, rt, &w(1), &target.to_ascii_lowercase(), words.get(3)),
        _ => gate(proxy, rt, &w(1), &target),
    };
    match result {
        Ok(done) => {
            let mut r = reply(&format!("<ok>{done}"));
            r.lines.extend(show(proxy, &proxy.runtime(), console).lines);
            r
        }
        Err(e) => reply(&format!("<err>{}", esc(&e))),
    }
}

fn servers(
    proxy: &Proxy,
    rt: &Runtime,
    op: &str,
    name: &str,
    at: Option<&String>,
) -> Result<String, String> {
    let mut order = rt.config.try_order();
    let known = |n: &str| rt.backends.iter().any(|b| b.name == n);
    let pos = |order: &[String]| -> Result<usize, String> {
        match at {
            None => Ok(order.len()),
            Some(p) => p
                .parse::<usize>()
                .ok()
                .filter(|p| (1..=order.len() + 1).contains(p))
                .map(|p| p - 1)
                .ok_or_else(|| format!("position {p} is not 1 to {}", order.len() + 1)),
        }
    };
    let done = match op {
        "add" => {
            if !known(name) {
                return Err(format!("there is no server named {name}"));
            }
            if order.iter().any(|n| n == name) {
                return Err(format!("{name} is in the list already"));
            }
            let p = pos(&order)?;
            order.insert(p, name.to_string());
            format!("Players now try {name} as server {}.", p + 1)
        }
        "remove" => {
            let before = order.len();
            order.retain(|n| n != name);
            if order.len() == before {
                return Err(format!("{name} is not in the list"));
            }
            if order.is_empty() {
                return Err("the last server stays: players need one to join".into());
            }
            format!("{name} is off the list.")
        }
        _ => {
            if !order.iter().any(|n| n == name) {
                return Err(format!("{name} is not in the list"));
            }
            order.retain(|n| n != name);
            let p = pos(&order)?;
            order.insert(p, name.to_string());
            format!("{name} is now server {}.", p + 1)
        }
    };
    save(proxy, |text| {
        set_key(text, "routing", "try", Some(&flow_list(&order)))
    })?;
    Ok(done)
}

fn host(
    proxy: &Proxy,
    rt: &Runtime,
    op: &str,
    domain: &str,
    server: Option<&String>,
) -> Result<String, String> {
    let ok = !domain.is_empty()
        && domain.len() <= 253
        && domain
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-'));
    if !ok {
        return Err(format!("{domain} is not a domain"));
    }
    if op == "remove" {
        if !rt.config.forced_hosts.contains_key(domain) {
            return Err(format!("{domain} has no servers of its own"));
        }
        save(proxy, |text| set_key(text, "forced-hosts", domain, None))?;
        return Ok(format!("{domain} uses the server list again."));
    }
    let Some(server) = server else {
        return Err("name the server: /prox route host set <domain> <server>".into());
    };
    if !rt.backends.iter().any(|b| &b.name == server) {
        return Err(format!("there is no server named {server}"));
    }
    save(proxy, |text| {
        set_key(
            text,
            "forced-hosts",
            domain,
            Some(&flow_list(std::slice::from_ref(server))),
        )
    })?;
    Ok(format!(
        "Players joining with {domain} go to {server} first."
    ))
}

fn gate(proxy: &Proxy, _rt: &Runtime, op: &str, name: &str) -> Result<String, String> {
    let host = proxy
        .plugins
        .get()
        .map(|p| p.host.clone())
        .ok_or("the plugin host is off")?;
    let mut required = host.required_gates();
    if op == "require" {
        // A gate nobody provides would close the network for everyone.
        let loaded = host
            .plugins()
            .iter()
            .any(|s| s.manifest.gate.as_ref().is_some_and(|g| g.name == name));
        if !loaded {
            return Err(format!("no plugin has the gate {name}"));
        }
        if required.iter().any(|g| g == name) {
            return Err(format!("{name} is required already"));
        }
        required.push(name.to_string());
    } else {
        let before = required.len();
        required.retain(|g| g != name);
        if required.len() == before {
            return Err(format!("{name} is not required"));
        }
    }
    save(proxy, |text| {
        set_key(
            text,
            "plugins",
            "required-gates",
            Some(&flow_list(&required)),
        )
    })?;
    Ok(if op == "require" {
        format!("{name} is required: without it nobody can log in.")
    } else {
        format!("{name} is optional: logins go on when it is missing.")
    })
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
