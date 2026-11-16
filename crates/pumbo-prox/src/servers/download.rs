//! `/prox download`: releases of the server software, downloads and stopping them.

use std::sync::Arc;
use std::time::Duration;

use pumbo_servers::versions::CANCELLED;
use pumbo_servers::{Manager, Progress};
use pumbo_text::Component;
use pumbo_text::template::escape_mini as esc;
use tracing::{info, warn};
use uuid::Uuid;

use super::watch::Watch;
use super::{may, say};
use crate::commands::{Reply, Source as Who};
use crate::server::{Proxy, Runtime, SessionCmd};
use crate::ui::{button, denied, line, reply, row, title_case};

// ------------------------------------------------------------------ downloads

/// `Downloading now: pumpkin 0.2.0 [stop]` lines.
fn running(m: &Manager, console: bool) -> Vec<Component> {
    m.versions
        .downloads()
        .into_iter()
        .map(|(src, tag)| {
            row(&format!(
                "<warn>Downloading now: <v>{src} {tag}</v>{}",
                button(
                    console,
                    "stop",
                    &format!("/prox download stop {src} {tag}"),
                    true
                )
            ))
        })
        .collect()
}

pub(super) fn download(
    proxy: &Arc<Proxy>,
    rt: &Runtime,
    who: &Who<'_>,
    m: Arc<Manager>,
    args: &[&str],
) -> Reply {
    let console = matches!(who, Who::Console);
    let names = m.versions.source_names();
    let src = args
        .first()
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();
    if !may(proxy, rt, who) {
        return denied();
    }
    match src.as_str() {
        "help" => {
            let page = usize::from(!console);
            return crate::commands::help(
                proxy,
                rt,
                who,
                "Server software",
                "/prox download",
                "/prox download help",
                crate::commands::DOWNLOAD_HELP,
                page,
            );
        }
        "stop" => return stop_downloads(&m, args.get(1..).unwrap_or_default()),
        "" => {
            let mut r = reply(&format!(
                "<s>Server software:{}",
                names
                    .iter()
                    .map(|n| format!(
                        " <v>{n}</v>{}",
                        button(console, "releases", &format!("/prox download {n}"), true)
                    ))
                    .collect::<String>()
            ));
            r.lines.push(row("<muted>paper: coming soon"));
            r.lines.extend(running(&m, console));
            r.lines.push(row(&format!(
                "<s>Commands:{}",
                if console {
                    " <c>prox download help".to_string()
                } else {
                    button(false, "help", "/prox download help", true)
                }
            )));
            return r;
        }
        "paper" => {
            return reply(
                "<warn>Paper support is coming soon. <s>For now: <c>/prox download pumpkin",
            );
        }
        _ => {}
    }
    if !names.contains(&src.as_str()) {
        return reply(&format!(
            "<err>Unknown server software <v>{}</v>. <s>Known: <v>{}</v>, <v>paper</v> (coming soon).",
            esc(&src),
            names.join(", ")
        ));
    }
    let which = args.get(1).copied().unwrap_or("list").to_string();
    let may_download = console || m.config.download_from_game;
    let say = say(proxy, who);
    if which.eq_ignore_ascii_case("list") {
        let now = running(&m, console);
        let (weak, player) = (Arc::downgrade(proxy), player_of(who));
        tokio::spawn(async move {
            let lines = match m.versions.releases(&src).await {
                Ok(list) => release_list(&m, &src, &list, console, may_download),
                Err(e) => vec![line(&format!("<err>No {src} release list: <s>{}", esc(&e)))],
            };
            for l in now.into_iter().chain(lines) {
                match (player, weak.upgrade()) {
                    (Some(id), Some(p)) => {
                        p.send_to(id, SessionCmd::Message(Box::new(l)));
                    }
                    _ => info!("{}", l.plain_text()),
                }
            }
        });
        return Reply::default();
    }
    if !console && !m.config.download_from_game {
        return reply(
            "<err>Downloads from the game are off <s>(managed-servers.download-from-game); use the console.",
        );
    }
    let watch = Arc::new(Watch::new(proxy, player_of(who), &src));
    let started = format!("<s>Downloading <v>{src} {}</v>…", esc(&which));
    tokio::spawn(async move {
        let on = |p: Progress| watch.on(p);
        match m.versions.download(&src, &which, &on).await {
            Ok(tag) => {
                let checked = if watch.unverified() {
                    "<warn>no checksum to check"
                } else {
                    "<s>SHA256 checked"
                };
                watch.finish(
                    &format!(
                        "<green>{} {tag} ready · {}",
                        title_case(&src),
                        if watch.unverified() {
                            "not verified"
                        } else {
                            "SHA256 OK"
                        }
                    ),
                    3,
                    Duration::from_secs(5),
                );
                info!("downloaded {src} {tag}");
                say(&format!(
                    "<ok>Downloaded <v>{src} {tag}</v>, {checked}.{}",
                    button(
                        console,
                        "create a server",
                        &format!("/prox servers new <name> {tag}"),
                        false
                    )
                ));
            }
            Err(e) if e == CANCELLED => {
                let what = watch.what(&which);
                watch.finish(
                    &format!("<red>{} {what} cancelled", title_case(&src)),
                    2,
                    Duration::from_secs(5),
                );
                info!("download of {src} {what} cancelled");
                say(&format!("<err>Download of <v>{src} {what}</v> cancelled."));
            }
            Err(e) => {
                let what = watch.what(&which);
                let short: String = e.chars().take(90).collect();
                watch.finish(
                    &format!("<red>{} {what} failed: {}", title_case(&src), esc(&short)),
                    2,
                    Duration::from_secs(10),
                );
                warn!("download of {src} {what} failed: {e}");
                say(&format!(
                    "<err>Download of <v>{src} {what}</v> failed: <s>{}",
                    esc(&e)
                ));
            }
        }
    });
    reply(&started)
}

fn player_of(who: &Who<'_>) -> Option<Uuid> {
    match who {
        Who::Player { id, .. } => Some(*id),
        Who::Console => None,
    }
}

fn release_list(
    m: &Manager,
    src: &str,
    list: &[pumbo_servers::Release],
    console: bool,
    may_download: bool,
) -> Vec<Component> {
    let installed = m.versions.installed(src);
    let mut out = vec![line(&format!(
        "<v>{}</v> <s>releases, newest first",
        title_case(src)
    ))];
    for (i, r) in list.iter().enumerate().take(15) {
        let tag = &r.tag;
        let kind = if r.official {
            "<ok>release</ok>"
        } else {
            "<warn>development</warn>"
        };
        let state = if installed.contains(tag) {
            format!(
                " <muted>downloaded</muted>{}",
                button(
                    console,
                    "create a server",
                    &format!("/prox servers new <name> {tag}"),
                    false
                )
            )
        } else if may_download {
            button(
                console,
                "download",
                &format!("/prox download {src} {tag}"),
                true,
            )
        } else {
            String::new()
        };
        out.push(row(&format!(
            "<muted>#{}</muted> <v>{tag}</v> {kind}{state}",
            i + 1
        )));
    }
    if may_download && console {
        out.push(row(&format!(
            "<s>Download one: <c>prox download {src} \\<#n|tag>"
        )));
    }
    out
}

/// `/prox download stop [pumpkin [tag] | tag | #n]`.
fn stop_downloads(m: &Manager, args: &[&str]) -> Reply {
    let active = m.versions.downloads();
    if active.is_empty() {
        return reply("<s>No downloads are running.");
    }
    let words: Vec<String> = args.iter().map(|a| a.to_ascii_lowercase()).collect();
    // `#n` is a number of the release list.
    let tag_of = |w: &str, src: &str| match w.strip_prefix('#') {
        Some(n) => n
            .parse::<usize>()
            .ok()
            .and_then(|n| m.versions.cached(src)?.get(n.checked_sub(1)?).cloned())
            .map(|r| r.tag.to_ascii_lowercase()),
        None => Some(w.to_string()),
    };
    let chosen: Vec<&(&str, String)> = active
        .iter()
        .filter(|(src, tag)| {
            let tag = tag.to_ascii_lowercase();
            match words.as_slice() {
                [] => true,
                [w] if w == src => true,
                [w] => tag_of(w, src).as_deref() == Some(tag.as_str()),
                [s, w, ..] => s == src && tag_of(w, src).as_deref() == Some(tag.as_str()),
            }
        })
        .collect();
    if chosen.is_empty() {
        let names: Vec<String> = active.iter().map(|(s, t)| format!("{s} {t}")).collect();
        return reply(&format!(
            "<err>No running download matches <v>{}</v>. <s>Running: <v>{}",
            esc(&words.join(" ")),
            esc(&names.join(", "))
        ));
    }
    let lines = chosen
        .into_iter()
        .filter(|(src, tag)| m.versions.cancel(src, tag))
        .map(|(src, tag)| line(&format!("<ok>Stopped the download of <v>{src} {tag}</v>.")))
        .collect();
    Reply {
        lines,
        connect: None,
    }
}
