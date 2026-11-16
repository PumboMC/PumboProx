//! `/prox servers new|start|stop|restart|logs|delete`.

use std::sync::Arc;
use std::time::Duration;

use pumbo_servers::{Manager, State};
use pumbo_text::Component;
use pumbo_text::template::escape_mini as esc;

use super::view::list;
use super::{SERVER_SUBS, Say, can, say};
use crate::commands::{Reply, Source as Who};
use crate::ops::{Action, ServerError};
use crate::server::{Proxy, Runtime, SessionCmd};
use crate::ui::{button, denied, line, reply, usage};

// ------------------------------------------------------------------ servers

/// Tells `say` when a starting server is ready or did not start.
fn when_ready(m: Arc<Manager>, name: String, say: Say) {
    tokio::spawn(async move {
        for _ in 0..240 {
            tokio::time::sleep(Duration::from_millis(500)).await;
            match m.state(&name) {
                State::Ready => return say(&format!("<ok><v>{name}</v> is ready.")),
                State::Crashed => {
                    return say(&format!(
                        "<err><v>{name}</v> did not start, see <c>/prox servers logs {name}"
                    ));
                }
                State::Stopped => return,
                State::Starting | State::Stopping => {}
            }
        }
        say(&format!(
            "<warn><v>{name}</v> is not ready after 2 minutes."
        ));
    });
}

pub(super) fn server(
    proxy: &Arc<Proxy>,
    rt: &Runtime,
    who: &Who<'_>,
    m: Arc<Manager>,
    args: &[&str],
) -> Reply {
    let console = matches!(who, Who::Console);
    let sub = args
        .first()
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();
    let node = match sub.as_str() {
        "" | "list" => "list",
        "help" => {
            let page = usize::from(!console);
            return crate::commands::help(
                proxy,
                rt,
                who,
                "Servers",
                "/prox servers",
                "/prox servers help",
                crate::commands::SERVERS_HELP,
                page,
            );
        }
        "new" => "create",
        "start" | "stop" | "restart" => "control",
        "logs" => "logs",
        "delete" => "delete",
        _ => {
            return reply(&format!(
                "<err>Unknown command <v>{}</v>. <s>Use one of: <c>{}{}",
                esc(&sub),
                SERVER_SUBS.join(", "),
                button(console, "help", "/prox servers help", true)
            ));
        }
    };
    let allowed = if node == "list" {
        crate::commands::permitted(proxy, rt, who, "pumbo.proxy.servers")
    } else {
        can(proxy, rt, who, node)
    };
    if !allowed {
        return denied();
    }
    if node == "list" {
        return list(proxy, &m, console);
    }
    let Some(name) = args.get(1).map(|s| s.to_ascii_lowercase()) else {
        return usage(&match sub.as_str() {
            "new" => "/prox servers new <name> [#n|tag|latest] [template]".to_string(),
            "logs" => "/prox servers logs <name> [lines]".to_string(),
            "delete" => "/prox servers delete <name> confirm".to_string(),
            s => format!("/prox servers {s} <name>"),
        });
    };
    let known = if sub == "new" {
        Ok(())
    } else {
        crate::ops::managed_server(&m, rt, &name)
    };
    match known {
        Ok(()) => {}
        Err(ServerError::NotManaged(name)) => {
            return reply(&format!(
                "<err><v>{}</v> is not run by the proxy; <s>it comes from servers: in pumboprox.yml, change it there.",
                esc(&name)
            ));
        }
        Err(ServerError::Unknown(name)) => {
            return reply(&format!(
                "<err>No server named <v>{}</v>.{}",
                esc(&name),
                button(console, "list", "/prox servers list", true)
            ));
        }
    }
    let say = say(proxy, who);
    match sub.as_str() {
        "new" => {
            let version = args.get(2).map(|s| s.to_string());
            let template = args.get(3).map(|s| s.to_string());
            let taken: Vec<String> = rt.config.servers.keys().cloned().collect();
            let n = name.clone();
            tokio::spawn(async move {
                match m
                    .create(&n, version.as_deref(), template.as_deref(), &taken)
                    .await
                {
                    Ok(e) if e.autostart => {
                        say(&format!(
                            "<ok>Created <v>{n}</v>: <v>{} {}</v>, port <v>{}</v>. <s>Starting it…",
                            e.source, e.version, e.port
                        ));
                        when_ready(m, n, say);
                    }
                    Ok(e) => say(&format!(
                        "<ok>Created <v>{n}</v>: <v>{} {}</v>, port <v>{}</v>.{}",
                        e.source,
                        e.version,
                        e.port,
                        button(console, "start", &format!("/prox servers start {n}"), true)
                    )),
                    Err(e) => say(&format!(
                        "<err>Server <v>{}</v> not created: <s>{}",
                        esc(&n),
                        esc(&e)
                    )),
                }
            });
            reply(&format!("<s>Creating <v>{}</v>…", esc(&name)))
        }
        "start" => match crate::ops::server_control(&m, &name, Action::Start) {
            Ok(_) => {
                when_ready(m, name.clone(), say);
                reply(&format!("<s>Starting <v>{name}</v>…"))
            }
            Err(e) => reply(&format!("<err>{}", esc(&e))),
        },
        "stop" => match crate::ops::server_control(&m, &name, Action::Stop) {
            Ok(done) => {
                let n = name.clone();
                tokio::spawn(async move {
                    match done.await {
                        Ok(()) => say(&format!("<ok>Stopped <v>{n}</v>.")),
                        Err(e) => say(&format!("<err>{}", esc(&e))),
                    }
                });
                reply(&format!("<s>Stopping <v>{name}</v>…"))
            }
            Err(e) => reply(&format!("<err>{}", esc(&e))),
        },
        "restart" => match crate::ops::server_control(&m, &name, Action::Restart) {
            Ok(done) => {
                let n = name.clone();
                tokio::spawn(async move {
                    match done.await {
                        Ok(()) => when_ready(m, n, say),
                        Err(e) => say(&format!("<err>{}", esc(&e))),
                    }
                });
                reply(&format!("<s>Restarting <v>{name}</v>…"))
            }
            Err(e) => reply(&format!("<err>{}", esc(&e))),
        },
        "logs" => {
            let n = args
                .get(2)
                .and_then(|n| n.parse::<usize>().ok())
                .unwrap_or(20);
            let lines = crate::ops::managed_logs(&m, &name, n);
            let mut out = vec![line(&format!(
                "<v>{name}</v><s>: last {} console line(s)",
                lines.len()
            ))];
            // Plain text: a console line is not markup.
            out.extend(lines.into_iter().map(Component::text));
            Reply {
                lines: out,
                connect: None,
            }
        }
        _ if !args
            .get(2)
            .is_some_and(|c| c.eq_ignore_ascii_case("confirm")) =>
        {
            reply(&format!(
                "<warn>This stops <v>{name}</v> and moves its folder to <v>{}</v>.{}",
                esc(&m.config.dir.join(".trash").display().to_string()),
                if console {
                    format!(" <s>Confirm: <c>prox server delete {name} confirm")
                } else {
                    button(
                        console,
                        "confirm",
                        &format!("/prox servers delete {name} confirm"),
                        true,
                    )
                }
            ))
        }
        _ => {
            let moved = move_players(proxy, rt, &name);
            let n = name.clone();
            tokio::spawn(async move {
                if moved > 0 {
                    tokio::time::sleep(Duration::from_secs(2)).await;
                }
                match m.delete(&n).await {
                    Ok(to) => say(&format!(
                        "<ok>Deleted <v>{n}</v>; its folder is now <v>{}</v>.",
                        esc(&to.display().to_string())
                    )),
                    Err(e) => say(&format!("<err><v>{n}</v> not deleted: <s>{}", esc(&e))),
                }
            });
            reply(&format!("<s>Deleting <v>{name}</v>…"))
        }
    }
}

/// Players on a server that goes away move to the first other server of
/// `routing.try`; returns how many.
fn move_players(proxy: &Proxy, rt: &Runtime, name: &str) -> usize {
    let target = rt
        .config
        .routing
        .rest
        .get("try")
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str())
        .find(|s| *s != name);
    let Some(target) = target else {
        return 0;
    };
    proxy
        .players()
        .into_iter()
        .filter(|p| p.server.as_deref() == Some(name))
        .filter(|p| {
            proxy.send_to(
                p.id,
                SessionCmd::Connect {
                    server: target.to_string(),
                    quiet: true,
                },
            )
        })
        .count()
}
