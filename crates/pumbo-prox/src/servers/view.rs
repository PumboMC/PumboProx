//! `/prox servers`: the list of the servers run by the proxy.

use std::time::Duration;

use pumbo_servers::{Manager, State};

use crate::commands::Reply;
use crate::server::Proxy;
use crate::ui::{button, line, reply, row};

pub(super) fn list(proxy: &Proxy, m: &Manager, console: bool) -> Reply {
    let servers = m.list();
    if servers.is_empty() {
        return reply(&format!(
            "<s>No servers yet.{}",
            if console {
                " Create one: <c>prox server new \\<name>".to_string()
            } else {
                button(
                    console,
                    "create a server",
                    "/prox servers new <name>",
                    false,
                )
            }
        ));
    }
    let players = proxy.players();
    let mut lines = vec![line(&format!(
        "<s>Servers run by the proxy (<v>{}</v>):",
        servers.len()
    ))];
    for s in servers {
        let colour = match s.state {
            State::Ready => "ok",
            State::Starting | State::Stopping => "warn",
            State::Stopped => "s",
            State::Crashed => "err",
        };
        let here = players
            .iter()
            .filter(|p| p.server.as_deref() == Some(s.name.as_str()))
            .count();
        let mut text = format!(
            "<v>{}</v> <muted>·</muted> <{colour}>{}</{colour}> <muted>·</muted> <s>{} {} <muted>·</muted> <s>port <v>{}</v> <muted>·</muted> <s>{here} player(s)",
            s.name,
            s.state.name(),
            s.entry.source,
            s.entry.version,
            s.entry.port
        );
        if let Some(kib) = s.rss_kib {
            text.push_str(&format!(" <muted>·</muted> <s>{} MB", kib / 1024));
        }
        if let Some(up) = s.uptime {
            text.push_str(&format!(" <muted>·</muted> <s>up {}", duration(up)));
        }
        let n = &s.name;
        if matches!(s.state, State::Stopped | State::Crashed) {
            text.push_str(&button(
                console,
                "start",
                &format!("/prox servers start {n}"),
                true,
            ));
        } else {
            text.push_str(&button(
                console,
                "stop",
                &format!("/prox servers stop {n}"),
                true,
            ));
        }
        text.push_str(&button(
            console,
            "logs",
            &format!("/prox servers logs {n}"),
            true,
        ));
        lines.push(row(&text));
    }
    lines.push(row(&format!(
        "<s>Commands:{}",
        if console {
            " <c>prox servers help".to_string()
        } else {
            button(false, "help", "/prox servers help", true)
        }
    )));
    Reply {
        lines,
        connect: None,
    }
}

/// `2d 3h`, `1h 5m`, `4m 10s`.
fn duration(d: Duration) -> String {
    let s = d.as_secs();
    match s {
        0..60 => format!("{s}s"),
        60..3600 => format!("{}m {}s", s / 60, s % 60),
        3600..86_400 => format!("{}h {}m", s / 3600, s % 3600 / 60),
        _ => format!("{}d {}h", s / 86_400, s % 86_400 / 3600),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations() {
        assert_eq!(duration(Duration::from_secs(42)), "42s");
        assert_eq!(duration(Duration::from_secs(250)), "4m 10s");
        assert_eq!(duration(Duration::from_secs(3900)), "1h 5m");
        assert_eq!(duration(Duration::from_secs(183_600)), "2d 3h");
    }
}
