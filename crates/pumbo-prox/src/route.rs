//! `/prox route`: the way a player takes into the network (domain, gates,
//! servers, fallback), shown and changed from the game or the console.
//! Changes go into `pumboprox.yml`, only the changed key (the admin's
//! comments and layout stay), and apply through a reload.

use std::sync::Arc;

use pumbo_text::template::escape_mini as esc;

use crate::commands::{Reply, Source as Who, permitted};
use crate::config::Config;
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
        return reply("<err>You do not have permission to do that.");
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

/// Writes the edited config, reloads, and puts the old file back when the
/// reload refuses the new one.
fn save(proxy: &Proxy, edit: impl FnOnce(&str) -> Result<String, String>) -> Result<(), String> {
    let path = proxy.config_path().ok_or("no config file")?.to_path_buf();
    let old = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let new = edit(&old)?;
    Config::parse(&new).map_err(|e| format!("the change would break the config: {e}"))?;
    write(&path, &new)?;
    if let Err(e) = proxy.reload() {
        let _ = write(&path, &old);
        return Err(format!("not applied: {e}"));
    }
    Ok(())
}

/// A temporary file with the old file's permissions, then a rename.
fn write(path: &std::path::Path, text: &str) -> Result<(), String> {
    let tmp = path.with_extension("yml.tmp");
    let err = |e: std::io::Error| format!("{}: {e}", path.display());
    std::fs::write(&tmp, text).map_err(err)?;
    if let Ok(meta) = std::fs::metadata(path) {
        std::fs::set_permissions(&tmp, meta.permissions()).map_err(err)?;
    }
    std::fs::rename(&tmp, path).map_err(err)
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
    lines.push(row(&format!(
        "<v>4 Fallback</v>  <s>a server that stops or restarts sends its players to the next one; with none left: <v>{}",
        esc(&cfg.messages.no_server)
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

// ------------------------------------------------------------------ YAML

/// A YAML scalar: plain when that reads back as the same text, else quoted.
fn scalar(s: &str) -> String {
    let plain = !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        && !matches!(
            s.to_ascii_lowercase().as_str(),
            "true" | "false" | "null" | "yes" | "no" | "on" | "off" | "y" | "n"
        )
        && s.parse::<f64>().is_err();
    if plain {
        s.to_string()
    } else {
        serde_json::to_string(s).unwrap_or_default()
    }
}

/// `[a, b]`.
pub fn flow_list(items: &[String]) -> String {
    format!(
        "[{}]",
        items
            .iter()
            .map(|i| scalar(i))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

fn indent_of(l: &str) -> usize {
    l.len() - l.trim_start().len()
}

fn blank_or_comment(l: &str) -> bool {
    let t = l.trim();
    t.is_empty() || t.starts_with('#')
}

/// `key: rest` of a mapping line: the key without quotes, the key as
/// written, and what follows the colon.
fn split_key(line: &str) -> Option<(String, &str, &str)> {
    let t = line.trim_start();
    let (key, written, rest) = match t.chars().next()? {
        q @ ('"' | '\'') => {
            let end = t.get(1..)?.find(q)? + 1;
            (
                t.get(1..end)?.to_string(),
                t.get(..=end)?,
                t.get(end + 1..)?,
            )
        }
        _ => {
            let i = t.find(':')?;
            (t.get(..i)?.trim().to_string(), t.get(..i)?, t.get(i..)?)
        }
    };
    let rest = rest.strip_prefix(':')?;
    (rest.is_empty() || rest.starts_with([' ', '\t'])).then_some((key, written, rest))
}

/// ` # comment` at the end of a value outside quotes, with its spaces.
fn trailing_comment(rest: &str) -> &str {
    let mut quote = None;
    let bytes = rest.as_bytes();
    for (i, c) in rest.char_indices() {
        match (quote, c) {
            (None, '"' | '\'') => quote = Some(c),
            (Some(q), c) if c == q => quote = None,
            (None, '#') if i > 0 && bytes.get(i - 1).is_some_and(|b| *b == b' ') => {
                let start = rest[..i].trim_end().len();
                return rest.get(start..).unwrap_or_default();
            }
            _ => {}
        }
    }
    ""
}

/// Sets (`Some`, a one-line value) or removes (`None`) `parent.key` of a
/// top-level block mapping and leaves every other line as it is: comments,
/// order, the comment after the old value. A missing parent is added at the
/// end. A parent written on one line (`routing: {try: [a]}`) is refused,
/// except an empty `{}`.
pub fn set_key(text: &str, parent: &str, key: &str, value: Option<&str>) -> Result<String, String> {
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    let top = lines
        .iter()
        .position(|l| indent_of(l) == 0 && split_key(l).is_some_and(|(k, _, _)| k == parent));
    let Some(top) = top else {
        let Some(v) = value else {
            return Ok(text.to_string());
        };
        let mut out = text.to_string();
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(&format!("{parent}:\n  {}: {v}\n", scalar(key)));
        return Ok(out);
    };
    let inline = split_key(lines.get(top).map_or("", |l| l.as_str()))
        .map(|(_, _, r)| {
            let r = r.trim();
            r.get(..r.len() - trailing_comment(r).len())
                .unwrap_or(r)
                .trim()
                .to_string()
        })
        .unwrap_or_default();
    match inline.as_str() {
        "" => {}
        "{}" | "null" | "~" => {
            if value.is_none() {
                return Ok(text.to_string());
            }
            if let Some(l) = lines.get_mut(top) {
                *l = format!("{parent}:");
            }
        }
        _ => {
            return Err(format!(
                "{parent} is written on one line, change it by hand"
            ));
        }
    }
    let end = (top + 1..lines.len())
        .find(|&i| {
            lines
                .get(i)
                .is_some_and(|l| !blank_or_comment(l) && indent_of(l) == 0)
        })
        .unwrap_or(lines.len());
    let child = |i: usize| lines.get(i).filter(|l| !blank_or_comment(l));
    let indent = (top + 1..end)
        .find_map(|i| child(i).map(|l| indent_of(l)))
        .unwrap_or(2);
    let at = (top + 1..end).find(|&i| {
        child(i).is_some_and(|l| {
            indent_of(l) == indent && split_key(l).is_some_and(|(k, _, _)| k == key)
        })
    });
    match (at, value) {
        (Some(i), v) => {
            // The old value's own lines: deeper ones, `- ` items at the key's indent.
            let mut j = i + 1;
            while let Some(l) = child(j).filter(|_| j < end) {
                let deeper = indent_of(l) > indent
                    || (indent_of(l) == indent && l.trim_start().starts_with("- "));
                if !deeper {
                    break;
                }
                j += 1;
            }
            match v {
                Some(v) => {
                    let old = lines.get(i).cloned().unwrap_or_default();
                    let (_, written, rest) = split_key(&old).ok_or("unreadable line")?;
                    let comment = if j == i + 1 {
                        trailing_comment(rest)
                    } else {
                        ""
                    };
                    let new = format!("{}{written}: {v}{comment}", " ".repeat(indent));
                    lines.splice(i..j, [new]);
                }
                None => {
                    let left = (top + 1..end)
                        .filter(|k| !(i..j).contains(k))
                        .any(|k| child(k).is_some());
                    lines.drain(i..j);
                    if !left && let Some(l) = lines.get_mut(top) {
                        *l = format!("{parent}: {{}}");
                    }
                }
            }
        }
        (None, Some(v)) => {
            let last = (top + 1..end)
                .rev()
                .find(|&i| {
                    lines
                        .get(i)
                        .is_some_and(|l| !l.trim().is_empty() && indent_of(l) > 0)
                })
                .unwrap_or(top);
            lines.insert(
                last + 1,
                format!("{}{}: {v}", " ".repeat(indent), scalar(key)),
            );
        }
        (None, None) => {}
    }
    let mut out = lines.join("\n");
    out.push('\n');
    Ok(out)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    const FILE: &str = "# My network\nlistener:\n  - bind: \"0.0.0.0:25565\"   # public\nservers:\n  lobby: { address: \"127.0.0.1:25566\" }\n  survival: { address: \"127.0.0.1:25567\" }\n\nrouting:\n  # players start here\n  try: [lobby]   # keep lobby first\n\n# the end\nlogging:\n  level: info\n";

    #[test]
    fn a_value_changes_and_the_rest_stays() {
        let out = set_key(FILE, "routing", "try", Some("[lobby, survival]")).unwrap();
        assert_eq!(
            out,
            FILE.replace(
                "try: [lobby]   # keep lobby first",
                "try: [lobby, survival]   # keep lobby first"
            )
        );
    }

    #[test]
    fn block_lists_become_one_line() {
        let text = "routing:\n  try:\n    - lobby\n    - survival\n  selector: try\nother: 1\n";
        let out = set_key(text, "routing", "try", Some("[survival]")).unwrap();
        assert_eq!(
            out,
            "routing:\n  try: [survival]\n  selector: try\nother: 1\n"
        );
        let compact = "routing:\n  try:\n  - lobby\n  - survival\n";
        let out = set_key(compact, "routing", "try", Some("[lobby]")).unwrap();
        assert_eq!(out, "routing:\n  try: [lobby]\n");
    }

    #[test]
    fn missing_keys_and_parents_are_added() {
        let out = set_key(FILE, "forced-hosts", "play.example.org", Some("[lobby]")).unwrap();
        assert!(out.starts_with(FILE));
        assert!(out.ends_with("forced-hosts:\n  play.example.org: [lobby]\n"));
        let out = set_key(&out, "forced-hosts", "mc.example.org", Some("[survival]")).unwrap();
        assert!(out.ends_with(
            "forced-hosts:\n  play.example.org: [lobby]\n  mc.example.org: [survival]\n"
        ));
        let out = set_key(FILE, "plugins", "required-gates", Some("[auth]")).unwrap();
        assert!(out.ends_with("plugins:\n  required-gates: [auth]\n"));
        let text = "plugins:\n  dir: plugins\n\n# comment of the next section\nlogging: {}\n";
        let out = set_key(text, "plugins", "required-gates", Some("[]")).unwrap();
        assert_eq!(
            out,
            "plugins:\n  dir: plugins\n  required-gates: []\n\n# comment of the next section\nlogging: {}\n"
        );
    }

    #[test]
    fn removing_the_last_entry_leaves_an_empty_map() {
        let text = "forced-hosts:\n  \"play.example.org\": [lobby]\nrouting:\n  try: [lobby]\n";
        let out = set_key(text, "forced-hosts", "play.example.org", None).unwrap();
        assert_eq!(out, "forced-hosts: {}\nrouting:\n  try: [lobby]\n");
        let back = set_key(&out, "forced-hosts", "a.org", Some("[lobby]")).unwrap();
        assert_eq!(
            back,
            "forced-hosts:\n  a.org: [lobby]\nrouting:\n  try: [lobby]\n"
        );
        assert_eq!(set_key(&out, "forced-hosts", "x", None).unwrap(), out);
    }

    #[test]
    fn one_line_parents_are_refused() {
        assert!(set_key("routing: { try: [lobby] }\n", "routing", "try", Some("[a]")).is_err());
    }

    #[test]
    fn scalars() {
        assert_eq!(
            flow_list(&["lobby".into(), "yes".into(), "a b".into()]),
            "[lobby, \"yes\", \"a b\"]"
        );
        assert_eq!(scalar("25565"), "\"25565\"");
    }
}
