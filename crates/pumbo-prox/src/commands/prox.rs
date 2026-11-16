//! `/prox`: version, reload, plugins, bridge, debug and the groups.

use std::sync::Arc;

use super::help::{PROX_HELP, help};
use super::operators::op;
use super::{Reply, Source, allowed, permitted};
use crate::server::{Proxy, Runtime};

/// `/prox`: the help, the version, a config reload, the
/// plugins, the bridge, servers from the proxy, the route, developer tools.
pub(super) fn prox(
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
        "reload" => match crate::ops::reload(proxy) {
            Ok(restart) => {
                let mut r = crate::servers::reply("<ok>Config reloaded.");
                if !restart.is_empty() {
                    r.lines.push(crate::servers::line(&format!(
                        "<warn>Changed, applied after a restart: <s>{}",
                        restart.join(", ")
                    )));
                }
                r
            }
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
