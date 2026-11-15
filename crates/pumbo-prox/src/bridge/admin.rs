//! Bridge status and the `/pumbo bridge` admin lines.

use std::net::IpAddr;
use std::sync::atomic::Ordering;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use pumbo_bridge_proto::api;
use pumbo_bridge_proto::wire;

use super::{Bridge, REJECTED_SHOWN, now_ms};

impl Bridge {
    /// `status` of the service and `/pumbo bridge`.
    pub fn status(&self, only: Option<&str>) -> Vec<api::ServerStatus> {
        let Some(proxy) = self.proxy() else {
            return Vec::new();
        };
        let rt = proxy.runtime();
        let st = self.lock();
        let host_ip = |address: &str| {
            let (host, _) = crate::config::split_host_port(address)?;
            if host.eq_ignore_ascii_case("localhost") {
                return Some(IpAddr::from([127, 0, 0, 1]));
            }
            host.parse::<IpAddr>().ok()
        };
        let same = |a: IpAddr, b: IpAddr| a == b || (a.is_loopback() && b.is_loopback());
        // Servers without a session, by address: a rejection from that
        // address is theirs only when it is the only such server there.
        let without: Vec<(&String, Option<IpAddr>)> = rt
            .config
            .servers
            .iter()
            .filter(|(n, _)| st.servers.get(*n).is_none_or(|s| s.link.is_none()))
            .map(|(n, s)| (n, host_ip(&s.address)))
            .collect();
        let rejection = |name: &str| {
            let ip = without.iter().find(|(n, _)| n.as_str() == name)?.1?;
            let (_, (_, why)) = st
                .rejected
                .iter()
                .filter(|(_, (t, _))| t.elapsed() < REJECTED_SHOWN)
                .find(|(peer, _)| same(**peer, ip))?;
            let candidates = without
                .iter()
                .filter(|(_, i)| i.is_some_and(|i| same(i, ip)))
                .count();
            (candidates == 1).then(|| why.clone())
        };
        rt.config
            .servers
            .keys()
            .filter(|n| only.is_none_or(|o| o == n.as_str()))
            .map(|name| {
                let slot = st.servers.get(name);
                let link = slot.and_then(|s| s.link.clone());
                let rejected = if link.is_none() {
                    rejection(name)
                } else {
                    None
                };
                let state = match (&link, &rejected, slot.is_some_and(|s| s.ever)) {
                    (Some(_), _, _) => api::State::Connected,
                    (None, Some(_), _) => api::State::Rejected,
                    (None, None, true) => api::State::Lost,
                    (None, None, false) => api::State::None,
                };
                let (bridge, pumpkin, mc) =
                    link.as_ref().map(|l| l.version.clone()).unwrap_or_default();
                let rtt = link
                    .as_ref()
                    .map(|l| l.rtt_ms.load(Ordering::Relaxed))
                    .filter(|ms| *ms != u32::MAX);
                api::ServerStatus {
                    server: name.clone(),
                    state,
                    since: slot.and_then(|s| s.since).map(now_ms),
                    bridge,
                    pumpkin,
                    mc,
                    caps: link
                        .as_ref()
                        .and_then(|l| l.caps.lock().ok().map(|c| c.clone()))
                        .unwrap_or_default(),
                    perms_mode: link.as_ref().map(|l| l.mode()).unwrap_or_default(),
                    proto: link
                        .as_ref()
                        .map(|l| format!("{}.{}", l.proto[0], l.proto[1]))
                        .unwrap_or_default(),
                    latency_ms: rtt,
                    detail: rejected,
                }
            })
            .collect()
    }

    /// `/pumbo bridge [status|key]` as lines with text tags (plain columns for
    /// the console). `secrets`: the console or `pumbo.proxy.bridge.key`.
    pub fn admin(&self, args: &[String], console: bool, secrets: bool) -> Vec<String> {
        let esc = |s: &str| pumbo_text::template::escape_mini(s);
        match args.first().map(String::as_str) {
            Some("key") if secrets => vec![
                "<p>Bridge key</p> <s>(config.yml of PumboBridge on every server):".into(),
                format!("<ok>{}", wire::key_hex(&self.key)),
            ],
            Some("key") => vec![crate::commands::exact_denied("pumbo.proxy.bridge.key")],
            None | Some("status") => {
                let now = SystemTime::now();
                let mut rows = vec![[
                    ("server".to_string(), "s"),
                    ("bridge".into(), "s"),
                    ("version / contract".into(), "s"),
                    ("ping".into(), "s"),
                    ("since".into(), "s"),
                ]];
                for s in self.status(None) {
                    let (state, style) = match s.state {
                        api::State::Connected => ("connected", "ok"),
                        api::State::Lost => ("lost", "warn"),
                        api::State::None => ("none", "muted"),
                        api::State::Rejected => ("rejected", "err"),
                    };
                    let connected = s.state == api::State::Connected;
                    let since = match (s.since, connected || s.state == api::State::Lost) {
                        (Some(ms), true) => {
                            let t = UNIX_EPOCH + Duration::from_millis(ms);
                            ago(now.duration_since(t).unwrap_or_default())
                        }
                        _ => "-".into(),
                    };
                    rows.push([
                        (s.server.clone(), "p"),
                        (state.into(), style),
                        (
                            if connected {
                                format!("{} / {}", s.bridge, s.proto)
                            } else {
                                s.detail.clone().unwrap_or_else(|| "-".into())
                            },
                            if connected { "s" } else { style },
                        ),
                        (
                            s.latency_ms.map_or("-".into(), |ms| format!("{ms} ms")),
                            "s",
                        ),
                        (since, "s"),
                    ]);
                }
                let mut lines = vec![format!(
                    "<p>PumboBridge</p> <s>listening on {}",
                    esc(&self.cfg.listen)
                )];
                lines.extend(columns(&rows, console));
                lines
            }
            Some(_) => vec!["<p>/pumbo bridge</p> <s>[status | key]".into()],
        }
    }
}

/// A line of [`Bridge::admin`] without tags (console).
pub fn plain(line: &str) -> String {
    let none =
        pumbo_text::StyleSheet::new(["p", "s", "ok", "warn", "err", "muted"].map(|t| (t, "")));
    pumbo_text::parse_mini_styled(line, &none).plain_text()
}

/// `5m 03s`.
fn ago(d: Duration) -> String {
    let s = d.as_secs();
    match s {
        0..60 => format!("{s}s"),
        60..3600 => format!("{}m {:02}s", s / 60, s % 60),
        _ => format!("{}h {:02}m", s / 3600, s % 3600 / 60),
    }
}

/// Rows of styled cells as aligned lines: pixels in chat, characters in the
/// console (styles are tags of the host style sheet).
fn columns<const N: usize>(rows: &[[(String, &str); N]], console: bool) -> Vec<String> {
    let width = |t: &str| {
        if console {
            t.chars().count() as u32
        } else {
            pumbo_common::rich::text_width(t, false)
        }
    };
    let mut widths = [0u32; N];
    for row in rows {
        for (w, (t, _)) in widths.iter_mut().zip(row) {
            *w = (*w).max(width(t));
        }
    }
    rows.iter()
        .map(|row| {
            let mut line = String::new();
            for (i, ((t, style), w)) in row.iter().zip(widths).enumerate() {
                let text = pumbo_text::template::escape_mini(t);
                line.push_str(&format!("<{style}>{text}</{style}>"));
                if i + 1 < N {
                    let gap = w - width(t);
                    if console {
                        line.push_str(&" ".repeat(gap as usize + 2));
                    } else {
                        line.push_str(&pumbo_host::pad(gap + 8));
                    }
                }
            }
            line
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::columns;

    /// Chat columns measure with the shared font table, `ł` included.
    #[test]
    fn chat_columns_line_up_in_pixels() {
        let rows = [
            [("łódź".to_string(), "p"), ("on".to_string(), "ok")],
            [("lobby".to_string(), "p"), ("off".to_string(), "err")],
        ];
        // "łódź" is 4 + 6 + 6 + 6 = 22 px, "lobby" 3 + 6 + 6 + 6 + 6 = 27 px:
        // 5 px more before the second column, so one of its spaces is bold.
        assert_eq!(
            columns(&rows, false),
            [
                "<p>łódź</p>  <b> </b><ok>on</ok>",
                "<p>lobby</p>  <err>off</err>"
            ]
        );
    }
}
