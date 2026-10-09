//! Machine-readable plugin descriptions (plan §6.6.5): declared metrics in
//! Prometheus text, config validation and redaction, the description as
//! JSON for `pumboprox describe`, and the umbrella command `/pumbo`.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use pumbo_text::Component;
use pumbo_text::template::escape_mini;
use serde_json::{Value, json};

use crate::HostInner;
use crate::actor::{PluginSlot, Status};
use crate::commands::{self, CommandOutcome, CommandSender};
use crate::schema;
use crate::wit::admin::{Actor, MetricKind, ParamKind, PluginDescription};
use crate::wit::events::CommandEvent;
use crate::wit::types::Text;

/// Series per plugin (label combinations).
const MAX_SERIES: usize = 1000;

#[derive(Debug, Clone, Copy)]
enum Series {
    Counter(f64),
    Gauge(f64),
    Histogram { count: u64, sum: f64 },
}

type SeriesKey = (String, String, Vec<(String, String)>);

#[derive(Debug, Default)]
pub(crate) struct Metrics {
    series: Mutex<BTreeMap<SeriesKey, Series>>,
}

fn prom_name(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect()
}

fn prom_label(v: &str) -> String {
    v.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

impl HostInner {
    /// A metric update from a plugin; only metrics from its description.
    pub(crate) fn metric(
        &self,
        slot: &PluginSlot,
        name: &str,
        kind: MetricKind,
        value: f64,
        mut labels: Vec<(String, String)>,
    ) -> Result<(), String> {
        let desc = slot.description().ok_or("no description yet")?;
        let m = desc
            .metrics
            .iter()
            .find(|m| m.name == name)
            .ok_or_else(|| format!("metric {name} is not declared in the description"))?;
        if m.kind != kind {
            return Err(format!("metric {name} is a {:?}", m.kind));
        }
        if let Some((k, _)) = labels.iter().find(|(k, _)| !m.labels.contains(k)) {
            return Err(format!("label {k} is not declared for {name}"));
        }
        if !value.is_finite() || (kind == MetricKind::Counter && value < 0.0) {
            return Err("invalid value".into());
        }
        labels.sort();
        let key = (slot.id.clone(), name.to_string(), labels);
        let mut series = self.metrics.series.lock().map_err(|_| "metrics poisoned")?;
        if !series.contains_key(&key)
            && series.keys().filter(|(p, _, _)| *p == slot.id).count() >= MAX_SERIES
        {
            return Err(format!("more than {MAX_SERIES} series"));
        }
        let e = series.entry(key).or_insert(match kind {
            MetricKind::Counter => Series::Counter(0.0),
            MetricKind::Gauge => Series::Gauge(0.0),
            MetricKind::Histogram => Series::Histogram { count: 0, sum: 0.0 },
        });
        match e {
            Series::Counter(c) => *c += value,
            Series::Gauge(g) => *g = value,
            Series::Histogram { count, sum } => {
                *count += 1;
                *sum += value;
            }
        }
        Ok(())
    }

    /// Plugin metrics in Prometheus text format, `pumbo_plugin_<id>_<name>`;
    /// histograms as count and sum.
    pub(crate) fn render_metrics(&self) -> String {
        let mut out = String::new();
        let Ok(series) = self.metrics.series.lock() else {
            return out;
        };
        let mut last = String::new();
        for ((plugin, name, labels), s) in series.iter() {
            let base = format!("pumbo_plugin_{}_{}", prom_name(plugin), prom_name(name));
            let l: Vec<String> = labels
                .iter()
                .map(|(k, v)| format!("{}=\"{}\"", prom_name(k), prom_label(v)))
                .collect();
            let l = if l.is_empty() {
                String::new()
            } else {
                format!("{{{}}}", l.join(","))
            };
            let (kind, lines) = match s {
                Series::Counter(v) => ("counter", vec![format!("{base}{l} {v}")]),
                Series::Gauge(v) => ("gauge", vec![format!("{base}{l} {v}")]),
                Series::Histogram { count, sum } => (
                    "summary",
                    vec![
                        format!("{base}_count{l} {count}"),
                        format!("{base}_sum{l} {sum}"),
                    ],
                ),
            };
            if base != last {
                out.push_str(&format!("# TYPE {base} {kind}\n"));
                last = base;
            }
            for line in lines {
                out.push_str(&line);
                out.push('\n');
            }
        }
        let dropped = self
            .services
            .bus_dropped
            .load(std::sync::atomic::Ordering::Relaxed);
        out.push_str(&format!(
            "# TYPE pumbo_bus_dropped_total counter\npumbo_bus_dropped_total {dropped}\n"
        ));
        out
    }

    /// Validates the config files of a plugin against its schema.
    pub(crate) fn validate_config(&self, slot: &PluginSlot) -> Result<(), String> {
        let schema = slot
            .description()
            .map(|d| d.config_schema.clone())
            .unwrap_or_default();
        schema::validate_dir(&schema, &self.cfg.plugins.config_dir(&slot.id))
    }

    /// The global config of a plugin with secret fields hidden.
    pub(crate) fn config_view(&self, slot: &PluginSlot) -> Result<Value, String> {
        let path = self.cfg.plugins.config_dir(&slot.id).join("config.yml");
        let mut v = Value::Object(schema::read_table(&path)?.unwrap_or_default());
        let s = slot
            .description()
            .map(|d| d.config_schema.clone())
            .unwrap_or_default();
        schema::redact(&s, &mut v);
        Ok(v)
    }
}

fn kind_name(k: ParamKind) -> &'static str {
    match k {
        ParamKind::Text => "text",
        ParamKind::Integer => "integer",
        ParamKind::Number => "number",
        ParamKind::Boolean => "boolean",
        ParamKind::Duration => "duration",
        ParamKind::Player => "player",
        ParamKind::Uuid => "uuid",
        ParamKind::Server => "server",
        ParamKind::Ip => "ip",
        ParamKind::Choice => "choice",
    }
}

/// The description as JSON (for `pumboprox describe <id> --json` and tools).
pub fn description_json(slot: &PluginSlot, d: &PluginDescription) -> Value {
    let m = &slot.manifest;
    json!({
        "id": m.id,
        "version": m.version,
        "api": m.api,
        "short-name": m.short_name,
        "permissions": m.permissions.iter().map(|p| json!({"node": p.node, "default": p.default, "description": p.description})).collect::<Vec<_>>(),
        "config-schema": serde_json::from_str::<Value>(&d.config_schema).unwrap_or(Value::Null),
        "actions": d.actions.iter().map(|a| json!({
            "name": a.name,
            "permission": a.permission,
            "dangerous": a.dangerous,
            "sensitive": a.sensitive,
            "description-key": a.description_key,
            "params": a.params.iter().map(|p| json!({
                "name": p.name, "kind": kind_name(p.kind), "required": p.required,
                "choices": p.choices, "description-key": p.description_key,
            })).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
        "metrics": d.metrics.iter().map(|x| json!({
            "name": x.name,
            "kind": match x.kind { MetricKind::Counter => "counter", MetricKind::Gauge => "gauge", MetricKind::Histogram => "histogram" },
            "unit": x.unit, "labels": x.labels, "description-key": x.description_key,
        })).collect::<Vec<_>>(),
        "topics": d.topics.iter().map(|t| json!({
            "topic": t.topic,
            "payload-schema": serde_json::from_str::<Value>(&t.payload_schema).unwrap_or(Value::Null),
            "description-key": t.description_key,
        })).collect::<Vec<_>>(),
    })
}

/// Standard subcommands every plugin with a short name has.
pub const STANDARD: &[&str] = &["reload", "version", "debug"];

/// Colours of the Pumbo plugins (`pumbo_common::style`): name and commands.
const BRAND: &str = "#F28C28";
const COMMAND: &str = "#FFC27A";

fn reply(host: &HostInner, lines: Vec<String>) -> CommandOutcome {
    CommandOutcome::Reply(lines.iter().map(|l| host.message(l)).collect())
}

fn text_of(host: &HostInner, t: &Text) -> Component {
    crate::text::component(host, t, None, None)
}

/// One line of a host help page (`/prox`).
#[derive(Debug, Clone, Copy)]
pub struct HelpEntry {
    /// The command as typed, without arguments: `/prox plugin`.
    pub command: &'static str,
    /// `<required>`, `[optional]`, other words as typed.
    pub args: &'static str,
    pub summary: &'static str,
    /// `None`: everyone who sees the page.
    pub permission: Option<&'static str>,
}

/// Entries on one chat page of a help.
pub const HELP_PER_PAGE: usize = 8;

/// `/send <player>` coloured like `pumbo_common::style::syntax`.
fn syntax(line: &str) -> String {
    line.split_whitespace()
        .map(|t| {
            let color = match t.chars().next() {
                Some('<') => "white",
                Some('[') => "gray",
                _ => COMMAND,
            };
            format!("<{color}>{}</{color}>", escape_mini(t))
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// A help page in the look of the plugins' help (`pumbo_common::help`,
/// D-HELP-1): header, `command <args>` and its description, a click types the
/// command, hovering shows it with its permission, [`HELP_PER_PAGE`] per page
/// with arrows running `{root} help <n>`. Only entries `allowed` lets through.
/// `page == 0`: every entry as plain lines (the console).
// ponytail: no pixel-aligned description column (the host has no font widths); add one if a host help grows long.
pub fn help_page(
    title: &str,
    version: &str,
    section: &str,
    root: &str,
    entries: &[HelpEntry],
    page: usize,
    allowed: impl Fn(&str) -> bool,
) -> Vec<Component> {
    let visible: Vec<&HelpEntry> = entries
        .iter()
        .filter(|e| e.permission.is_none_or(&allowed))
        .collect();
    let full = |e: &HelpEntry| format!("{} {}", e.command, e.args).trim_end().to_string();
    if page == 0 {
        let width = visible.iter().map(|e| full(e).len()).max().unwrap_or(0) + 3;
        let mut out = vec![format!("{title} {version} · {section}   {root}")];
        out.extend(
            visible
                .iter()
                .map(|e| format!("  {:<width$}{}", full(e), e.summary)),
        );
        return out
            .iter()
            .map(|l| pumbo_text::parse_mini(&escape_mini(l)))
            .collect();
    }
    let pages = visible.len().div_ceil(HELP_PER_PAGE).max(1);
    let page = page.min(pages);
    let mut lines = vec![format!(
        "<{BRAND}><b>{}</b></{BRAND}><dark_gray> {} · </dark_gray><white>{}</white>   <click:suggest_command:'{root} '><{COMMAND}>{root}</{COMMAND}></click>",
        escape_mini(title),
        escape_mini(version),
        escape_mini(section)
    )];
    for e in visible
        .iter()
        .skip((page - 1) * HELP_PER_PAGE)
        .take(HELP_PER_PAGE)
    {
        let all = full(e);
        // Under the root the command is listed without it, like the plugins do.
        let listed = all
            .strip_prefix(root)
            .and_then(|r| r.strip_prefix(' '))
            .unwrap_or(&all);
        let suggest = if e.args.is_empty() {
            e.command.to_string()
        } else {
            format!("{} ", e.command)
        };
        let permission = e.permission.map_or(String::new(), |p| {
            format!(
                "<newline><dark_gray>Permission: {}</dark_gray>",
                escape_mini(p)
            )
        });
        let summary = escape_mini(e.summary);
        let tooltip = format!(
            "{}<newline><white>{summary}</white><newline>{permission}<newline><dark_gray><i>Click to type it in chat",
            syntax(&all)
        )
        .replace('\'', "\\'");
        lines.push(format!(
            "<click:suggest_command:'{suggest}'><hover:show_text:'{tooltip}'>{}   <gray>{summary}</gray></hover></click>",
            syntax(listed)
        ));
    }
    let arrow = |symbol: &str, target: Option<usize>, hover: &str| match target {
        Some(n) => format!(
            "<click:run_command:'{root} help {n}'><hover:show_text:'<gray>{hover}'><{BRAND}><b>{symbol}</b></{BRAND}></hover></click>"
        ),
        None => format!("<dark_gray>{symbol}</dark_gray>"),
    };
    let hint = "<dark_gray>Hover a command for details, click to type it.";
    lines.push(if pages > 1 {
        format!(
            "{} <gray>{page}/{pages}</gray> {}   {hint}",
            arrow("◀", (page > 1).then(|| page - 1), "Previous page"),
            arrow("▶", (page < pages).then_some(page + 1), "Next page"),
        )
    } else {
        hint.to_string()
    });
    lines.iter().map(|l| pumbo_text::parse_mini(l)).collect()
}

/// `/pumbo …` and `/pumbo<short-name> …` (plan §6.6.1).
pub(crate) fn umbrella(
    host: &Arc<HostInner>,
    sender: CommandSender,
    root: &str,
    args: &[String],
) -> Option<CommandOutcome> {
    let (short, rest): (Option<String>, &[String]) = if root == "pumbo" {
        (
            args.first().map(|s| s.to_ascii_lowercase()),
            args.get(1..).unwrap_or_default(),
        )
    } else {
        // `/pumbo<short-name>` or the plugin's `short-alias` (`/pf`).
        let m = &host
            .plugins
            .values()
            .find(|p| {
                let m = &p.manifest;
                m.short_alias.as_deref() == Some(root)
                    || root
                        .strip_prefix("pumbo")
                        .is_some_and(|s| m.short_name.as_deref() == Some(s))
            })?
            .manifest;
        (m.short_name.clone(), args)
    };
    let can = |node: &str| match sender {
        CommandSender::Console => true,
        CommandSender::Player(id) => host.has(id, node, &host.player_levels(id)),
    };
    let denied = || CommandOutcome::Refused(host.message(&host.cfg.plugins.messages.no_permission));
    let Some(short) = short else {
        if !can("pumbo.proxy.plugins") {
            return Some(denied());
        }
        let mut lines = vec![format!(
            "<{BRAND}><b>PumboProx</b></{BRAND}> <white>{}</white> <dark_gray>·</dark_gray> <gray>plugins",
            env!("CARGO_PKG_VERSION")
        )];
        for s in host.plugins.values() {
            let m = &s.manifest;
            let row = format!(
                "<{COMMAND}>{}</{COMMAND}> <white>{}</white> <dark_gray>{}",
                escape_mini(&s.id),
                escape_mini(&m.version),
                escape_mini(&s.status_text())
            );
            // A click types the plugin's help command; hovering names it.
            lines.push(match &m.short_name {
                Some(short) => {
                    let cmd = m
                        .short_alias
                        .as_ref()
                        .map_or(format!("/pumbo{short}"), |a| format!("/{a}"));
                    format!(
                        "<click:suggest_command:'{cmd} '><hover:show_text:'<gray>Help: <{COMMAND}>{cmd}</{COMMAND}> (/pumbo {short})'>{row}</hover></click>"
                    )
                }
                None => row,
            });
        }
        for f in &host.failed {
            let (id, version) = f.manifest.as_ref().map_or((f.file.as_str(), "?"), |m| {
                (m.id.as_str(), m.version.as_str())
            });
            lines.push(format!(
                "<s>{}</s> {} <muted>{}",
                escape_mini(id),
                escape_mini(version),
                escape_mini(&format!("{:?}", Status::Failed(f.reason.clone())))
            ));
        }
        if host.services.native(pumbo_contracts::BRIDGE.name).is_some() {
            lines.push("<s>/pumbo bridge</s> <muted>PumboBridge on the servers: state, key".into());
        }
        return Some(reply(host, lines));
    };
    if short == "bridge"
        && let Some(native) = host.services.native(pumbo_contracts::BRIDGE.name)
    {
        // `/pumbo bridge [status|key]` (the native module renders it).
        if !can("pumbo.proxy.bridge") {
            return Some(denied());
        }
        let console = matches!(sender, CommandSender::Console);
        let lines = native.admin(rest, console, console || can("pumbo.proxy.bridge.key"));
        return Some(reply(host, lines));
    }
    if short == "proxy" {
        return Some(proxy_command(host, sender, rest, &can));
    }
    let slot = host
        .plugins
        .values()
        .find(|p| p.manifest.short_name.as_deref() == Some(short.as_str()))?
        .clone();
    let sub = match rest.first() {
        Some(s) => s.to_ascii_lowercase(),
        // The plugin's own paged help (`help` under its short-name), like `/lp`.
        None if host.commands.umbrella(&short, "help").is_some() => "help".to_string(),
        None => {
            let mut lines = vec![format!(
                "<{BRAND}><b>{}</b></{BRAND}> <white>{}</white>   <{COMMAND}>/pumbo {}",
                escape_mini(&slot.id),
                escape_mini(&slot.manifest.version),
                escape_mini(&short)
            )];
            let mut names: Vec<String> = STANDARD.iter().map(|s| s.to_string()).collect();
            if let Some(d) = slot.description() {
                names.extend(d.actions.iter().map(|a| a.name.clone()));
            }
            names.extend(
                host.commands
                    .umbrella_of(&short)
                    .iter()
                    .map(|e| e.spec.name.clone()),
            );
            lines.extend(names.iter().map(|n| {
                let n = escape_mini(n);
                format!(
                    "<click:suggest_command:'/pumbo {short} {n} '><hover:show_text:'<{COMMAND}>/pumbo {short} {n}'><{COMMAND}>{n}"
                )
            }));
            return Some(reply(host, lines));
        }
    };
    let params = rest.get(1..).unwrap_or_default().to_vec();
    if STANDARD.contains(&sub.as_str()) {
        if !can(&format!("pumbo.{short}.{sub}")) {
            return Some(denied());
        }
        return Some(match sub.as_str() {
            "version" => reply(
                host,
                vec![format!(
                    "<p>{} <s>{}</s> <muted>{}",
                    escape_mini(&slot.id),
                    escape_mini(&slot.manifest.version),
                    escape_mini(&slot.status_text())
                )],
            ),
            "reload" => {
                if let Err(e) = host.validate_config(&slot) {
                    return Some(CommandOutcome::Refused(pumbo_text::parse_mini(&format!(
                        "<red>Config not reloaded: {}",
                        escape_mini(&e)
                    ))));
                }
                admin_action(host, &slot, sender, "reload", Vec::new())
            }
            _ => admin_action(host, &slot, sender, &sub, Vec::new()),
        });
    }
    if let Some(action) = slot
        .description()
        .and_then(|d| d.actions.iter().find(|a| a.name == sub).cloned())
    {
        if !can(&action.permission) {
            return Some(denied());
        }
        let required = action.params.iter().filter(|p| p.required).count();
        if params.len() < required {
            let usage: Vec<String> = action
                .params
                .iter()
                .map(|p| {
                    if p.required {
                        format!("<{}>", p.name)
                    } else {
                        format!("[{}]", p.name)
                    }
                })
                .collect();
            return Some(reply(
                host,
                vec![escape_mini(&format!(
                    "/pumbo {short} {sub} {}",
                    usage.join(" ")
                ))],
            ));
        }
        let mut named: Vec<(String, String)> = action
            .params
            .iter()
            .zip(params.iter())
            .map(|(p, v)| (p.name.clone(), v.clone()))
            .collect();
        if let (Some(last), true) = (action.params.last(), params.len() > action.params.len()) {
            // The last parameter takes the rest of the line (reasons, messages).
            let tail = params
                .get(action.params.len() - 1..)
                .unwrap_or_default()
                .join(" ");
            if let Some(entry) = named.iter_mut().find(|(n, _)| *n == last.name) {
                entry.1 = tail;
            }
        }
        if action.sensitive {
            tracing::info!(plugin = %slot.id, "/pumbo {short} {sub} (arguments hidden)");
        }
        return Some(admin_action(host, &slot, sender, &sub, named));
    }
    if let Some(entry) = host.commands.umbrella(&short, &sub) {
        if let Err(commands::Denied::NoPermission) = commands::allowed(host, &slot, &entry, sender)
        {
            return Some(denied());
        }
        let event = CommandEvent {
            player: match sender {
                CommandSender::Player(id) => Some(id),
                CommandSender::Console => None,
            },
            name: entry.spec.name.clone(),
            args: params,
        };
        let job = crate::actor::job(move |acc, g| {
            Box::pin(async move {
                let _ = g.call_on_command(acc, event).await;
            })
        });
        return Some(match slot.submit(job) {
            Ok(()) => CommandOutcome::Handled,
            Err(_) => CommandOutcome::Refused(
                host.message(&host.cfg.plugins.messages.command_unavailable),
            ),
        });
    }
    Some(reply(
        host,
        vec![format!("<red>Unknown subcommand {}", escape_mini(&sub))],
    ))
}

/// Runs an admin action; the answer goes to the sender when it arrives.
fn admin_action(
    host: &Arc<HostInner>,
    slot: &Arc<PluginSlot>,
    sender: CommandSender,
    action: &str,
    args: Vec<(String, String)>,
) -> CommandOutcome {
    if slot.status() != Status::Running {
        return CommandOutcome::Refused(
            host.message(&host.cfg.plugins.messages.command_unavailable),
        );
    }
    let by = match sender {
        CommandSender::Console => Actor::Console,
        CommandSender::Player(id) => Actor::Player(id),
    };
    let (h, s, a) = (Arc::clone(host), Arc::clone(slot), action.to_string());
    tokio::spawn(async move {
        let deadline = std::time::Duration::from_secs(30);
        let r = s
            .call(
                std::time::Duration::from_secs(5),
                deadline,
                move |acc, g| {
                    Box::pin(async move { g.call_on_admin_action(acc, a, args, by).await })
                },
            )
            .await;
        let text = match r {
            Ok(Ok(t)) | Ok(Err(t)) => text_of(&h, &t),
            Err(e) => pumbo_text::parse_mini(&format!("<red>{}", escape_mini(&e.to_string()))),
        };
        tell(&h, sender, &s.id, text);
    });
    CommandOutcome::Handled
}

/// A late answer to a command: a message to the player or a console log line.
fn tell(host: &HostInner, sender: CommandSender, plugin: &str, text: Component) {
    match sender {
        CommandSender::Player(id) => host
            .bridge
            .send(id, crate::bridge::PlayerCommand::Message(text)),
        CommandSender::Console => tracing::info!(plugin = %plugin, "{}", text.plain_text()),
    }
}

/// `/pumbo proxy plugin reload|unload|load <id>`. The answer comes when the
/// instance runs or is down. While a gate is unloaded it denies, and a
/// player it holds is kicked, never let through (plan §4.4 item 8).
fn plugin_command(host: &Arc<HostInner>, sender: CommandSender, rest: &[String]) -> CommandOutcome {
    let action = rest
        .get(1)
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();
    let (Some(id), true) = (
        rest.get(2),
        ["reload", "unload", "load"].contains(&action.as_str()),
    ) else {
        return reply(
            host,
            vec![escape_mini("/pumbo proxy plugin reload|unload|load <id>")],
        );
    };
    let Some(slot) = host.plugins.get(id.as_str()).cloned() else {
        return reply(
            host,
            vec![format!("<red>Unknown plugin {}", escape_mini(id))],
        );
    };
    if action == "load" && slot.status().is_coming() {
        return reply(host, vec![format!("{} is loaded", escape_mini(id))]);
    }
    tracing::info!(plugin = %slot.id, by = %commands::sender_name(host, sender), "plugin {action}");
    let h = Arc::clone(host);
    tokio::spawn(async move {
        let r = if action == "unload" {
            slot.unload().await
        } else {
            slot.reload().await
        };
        let restart = slot.restart.lock().ok().and_then(|r| r.clone());
        let line = match (r, restart) {
            (Ok(()), Some(changes)) => format!(
                "<p>{} {action}ed, <warn>restart the proxy to apply the new manifest ({})",
                escape_mini(&slot.id),
                escape_mini(&changes)
            ),
            (Ok(()), None) => format!("<p>{} {action}ed", escape_mini(&slot.id)),
            (Err(e), _) => format!(
                "<red>{} not {action}ed: {}",
                escape_mini(&slot.id),
                escape_mini(&e)
            ),
        };
        tell(&h, sender, &slot.id, h.message(&line));
    });
    CommandOutcome::Handled
}

fn proxy_command(
    host: &Arc<HostInner>,
    sender: CommandSender,
    rest: &[String],
    can: &dyn Fn(&str) -> bool,
) -> CommandOutcome {
    let sub = rest.first().map(String::as_str).unwrap_or_default();
    match sub {
        "services" if can("pumbo.proxy.services") => {
            let mut lines = vec!["<p>Services:".to_string()];
            for slot in host.plugins.values() {
                for p in &slot.manifest.provides {
                    let up = slot.status() == Status::Running;
                    lines.push(format!(
                        "<s>{}@{}</s> by {} <muted>{}",
                        escape_mini(&p.service),
                        escape_mini(&p.version),
                        escape_mini(&slot.id),
                        if up { "available" } else { "unavailable" }
                    ));
                }
                for u in &slot.manifest.uses {
                    let active = host.service_lookup(slot, &u.service).is_some();
                    lines.push(format!(
                        "<muted>{} uses {}@{}: {}",
                        escape_mini(&slot.id),
                        escape_mini(&u.service),
                        escape_mini(&u.version),
                        if active { "active" } else { "inactive" }
                    ));
                }
            }
            reply(host, lines)
        }
        "perms" if can("pumbo.proxy.perms") => match rest.get(1).map(String::as_str) {
            Some("list") => {
                let mut lines = vec!["<p>Permission nodes:".to_string()];
                for slot in host.plugins.values() {
                    for p in &slot.manifest.permissions {
                        lines.push(format!(
                            "<s>{}</s> <muted>{}{}",
                            escape_mini(&p.node),
                            escape_mini(&p.description),
                            if p.default { " (default)" } else { "" }
                        ));
                    }
                }
                reply(host, lines)
            }
            Some("check") => {
                let (Some(name), Some(node)) = (rest.get(2), rest.get(3)) else {
                    return reply(
                        host,
                        vec![escape_mini(
                            "/pumbo proxy perms check <player> <node> [server]",
                        )],
                    );
                };
                let Some(p) = host.players.read().ok().and_then(|ps| {
                    ps.values()
                        .find(|p| p.profile.name.eq_ignore_ascii_case(name))
                        .cloned()
                }) else {
                    return reply(
                        host,
                        vec![format!("<red>Unknown player {}", escape_mini(name))],
                    );
                };
                let lv = match rest.get(4) {
                    Some(s) => crate::permissions::levels(Some(s), &host.cfg.groups_of(s)),
                    None => host.player_levels(p.id),
                };
                let line = match host.perms.decide(p.id, node, &lv) {
                    Some(d) => format!(
                        "{} = {} <muted>({} in {:?}, {:?})",
                        escape_mini(node),
                        d.value,
                        escape_mini(&d.entry.node),
                        d.entry.context,
                        d.layer
                    ),
                    // Namespaced nodes are the servers': without an entry
                    // none is sent and the server decides (operators too).
                    None if node.contains(':') => format!(
                        "{} = not set <muted>(no entry: the server decides, e.g. by its ops.json)",
                        escape_mini(node)
                    ),
                    None => format!(
                        "{} = {} <muted>(no entry, default)",
                        escape_mini(node),
                        host.declared_default(node)
                    ),
                };
                reply(host, vec![line])
            }
            _ => reply(
                host,
                vec![escape_mini(
                    "/pumbo proxy perms list | check <player> <node> [server]",
                )],
            ),
        },
        "plugin" if can("pumbo.proxy.plugin") => plugin_command(host, sender, rest),
        "services" | "perms" | "plugin" => {
            CommandOutcome::Refused(host.message(&host.cfg.plugins.messages.no_permission))
        }
        _ => reply(
            host,
            vec![escape_mini(
                "/pumbo proxy services | perms list | perms check <player> <node> [server] | plugin reload|unload|load <id>",
            )],
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn e(command: &'static str, args: &'static str, node: Option<&'static str>) -> HelpEntry {
        HelpEntry {
            command,
            args,
            summary: "Does it",
            permission: node,
        }
    }

    const ENTRIES: &[HelpEntry] = &[
        e("/server", "[name]", Some("p.server")),
        HelpEntry {
            command: "/find",
            args: "<player>",
            summary: "Shows a player's server",
            permission: Some("p.find"),
        },
        e("/send", "<player|all> <server>", Some("p.send")),
        e("/prox version", "", None),
        e("/prox a", "", Some("p.a")),
        e("/prox b", "", Some("p.b")),
        e("/prox c", "", Some("p.c")),
        e("/prox d", "", Some("p.d")),
        e("/prox e", "", Some("p.e")),
        e("/prox f", "", Some("p.f")),
    ];

    #[test]
    fn help_header_permissions_and_pages() {
        let all = help_page("PumboProx", "1.2.3", "Proxy", "/prox", ENTRIES, 1, |_| true);
        assert_eq!(all[0].plain_text(), "PumboProx 1.2.3 · Proxy   /prox");
        assert_eq!(all.len(), 1 + HELP_PER_PAGE + 1);
        assert!(all[1].plain_text().starts_with("/server [name]   Does it"));
        // A quote in a description does not break the tooltip.
        assert_eq!(
            all[2].plain_text(),
            "/find <player>   Shows a player's server"
        );
        // Under the root the command is listed without it; a click types it.
        assert!(all[4].plain_text().starts_with("version   "));
        let send = format!("{:?}", all[3]);
        assert!(send.contains("SuggestCommand(\"/send \")") && send.contains("p.send"));
        let footer = all.last().unwrap();
        assert!(footer.plain_text().starts_with("◀ 1/2 ▶"));
        assert!(format!("{footer:?}").contains("RunCommand(\"/prox help 2\")"));
        let second = help_page("PumboProx", "1.2.3", "Proxy", "/prox", ENTRIES, 9, |_| true);
        assert_eq!(second.len(), 1 + 2 + 1);
        assert!(second.last().unwrap().plain_text().starts_with("◀ 2/2 ▶"));

        // Only what the sender may use; one page, no arrows.
        let guest = help_page("PumboProx", "1.2.3", "Proxy", "/prox", ENTRIES, 1, |n| {
            n == "p.server"
        });
        let text: Vec<String> = guest.iter().map(Component::plain_text).collect();
        assert_eq!(text.len(), 4, "{text:?}");
        assert!(text[1].starts_with("/server") && text[2].starts_with("version"));
        assert!(!text[3].contains('▶'));

        // The console: every entry, plain, no pages.
        let console = help_page("PumboProx", "1.2.3", "Proxy", "/prox", ENTRIES, 0, |_| true);
        assert_eq!(console.len(), 1 + ENTRIES.len());
        assert!(
            console[3]
                .plain_text()
                .starts_with("  /send <player|all> <server>   Does it")
        );
    }
}
