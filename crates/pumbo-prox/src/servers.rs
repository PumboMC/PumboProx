//! Servers from the proxy (`managed-servers`): `pumbo-servers` wired into
//! the proxy. Its servers join the server list, and `/prox download` and
//! `/prox server` drive it. Permissions `pumbo.proxy.servers.<node>`,
//! by default for `commands.operators` and the console.

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant};

use pumbo_servers::versions::CANCELLED;
use pumbo_servers::{Manager, Network, ProcessRunner, Progress, Pumpkin, Source, State};
use pumbo_text::template::escape_mini as esc;
use pumbo_text::{Component, StyleSheet};
use tracing::{info, warn};
use uuid::Uuid;

use crate::commands::{Reply, Source as Who};
use crate::config::{Config, ServerConfig, SignedChatCancel};
use crate::server::{Proxy, Runtime, SessionCmd};
use crate::world::BossbarOp;

/// The server software the proxy can download and run.
fn sources() -> Vec<Arc<dyn Source>> {
    vec![Arc::new(Pumpkin::default())]
}

const SERVER_SUBS: &[&str] = &["help", "new", "start", "stop", "restart", "logs", "delete"];

// ------------------------------------------------------------------ texts

/// Every message of this feature starts like this.
const PREFIX: &str = "<#F28C28>PumboProx</#F28C28> <dark_gray>»</dark_gray> ";

/// Style tags of the messages: `ok`, `err`, `warn`, `s` (secondary), `v` (a
/// value: name, version, port), `c` (a command), `muted`.
fn styles() -> &'static StyleSheet {
    static STYLES: OnceLock<StyleSheet> = OnceLock::new();
    STYLES.get_or_init(|| {
        StyleSheet::new([
            ("ok", "<green>"),
            ("err", "<red>"),
            ("warn", "<yellow>"),
            ("s", "<gray>"),
            ("v", "<white>"),
            ("c", "<#FFC27A>"),
            ("muted", "<dark_gray>"),
        ])
    })
}

/// A message with the prefix; `mini` uses the tags of [`styles`], values in
/// it are escaped with [`esc`].
pub(crate) fn line(mini: &str) -> Component {
    pumbo_text::parse_mini_styled(&format!("{PREFIX}{mini}"), styles())
}

/// A line under a message (lists), without the prefix.
pub(crate) fn row(mini: &str) -> Component {
    pumbo_text::parse_mini_styled(mini, styles())
}

pub(crate) fn reply(mini: &str) -> Reply {
    Reply {
        lines: vec![line(mini)],
        connect: None,
    }
}

/// `Usage: /send <player> <server>`; a click types the command.
pub(crate) fn usage(syntax: &str) -> Reply {
    let typed = syntax
        .find(['<', '[', '|'])
        .map_or(syntax, |i| syntax.get(..i).unwrap_or(syntax));
    let typed = typed.rsplit_once(' ').map_or(typed, |(a, _)| a);
    reply(&format!(
        "<s>Usage: <click:suggest_command:'{} '><hover:show_text:'<gray>Click to type it'><c>{}</c></hover></click>",
        typed.trim_end(),
        esc(syntax)
    ))
}

fn denied() -> Reply {
    reply("<err>You do not have permission to do that.")
}

/// A clickable command in brackets; the console gets nothing.
pub(crate) fn button(console: bool, label: &str, command: &str, run: bool) -> String {
    if console {
        return String::new();
    }
    let action = if run {
        "run_command"
    } else {
        "suggest_command"
    };
    let hover = if run { "Click to run" } else { "Click to type" };
    format!(
        " <click:{action}:'{command}'><hover:show_text:'<gray>{hover}: <#FFC27A>{}'><c>[{label}]</c></hover></click>",
        esc(command)
    )
}

/// `pumpkin` → `Pumpkin`.
fn title_case(s: &str) -> String {
    let mut c = s.chars();
    c.next()
        .map(|f| f.to_uppercase().chain(c).collect())
        .unwrap_or_default()
}

// ------------------------------------------------------------------ setup

/// With `managed-servers.enabled`: reads `servers.yml`, puts the servers in
/// the server list, takes over the ones a crashed proxy left running and
/// starts the `autostart` ones. Needs the bridge key, so after the bridge.
pub fn start(proxy: &Arc<Proxy>) -> Result<(), String> {
    start_with(proxy, sources())
}

/// [`start`] with other server software (tests: a local release server).
pub fn start_with(proxy: &Arc<Proxy>, sources: Vec<Arc<dyn Source>>) -> Result<(), String> {
    let config = proxy.runtime().config.clone();
    if !config.managed_servers.enabled {
        return Ok(());
    }
    let manager = Manager::new(
        config.managed_servers.clone(),
        network(&config)?,
        sources,
        Arc::new(ProcessRunner::default()),
    )
    .map_err(|e| format!("managed-servers: {e}"))?;
    let weak = Arc::downgrade(proxy);
    manager.on_change(move || {
        if let Some(p) = weak.upgrade()
            && let Err(e) = p.refresh()
        {
            warn!("server list not updated: {e}");
        }
    });
    let _ = proxy.servers.set(manager.clone());
    proxy.refresh()?;
    manager.boot();
    Ok(())
}

/// The forwarding secret and the bridge for the files of new servers.
fn network(config: &Config) -> Result<Network, String> {
    let velocity_secret = if config.forwarding.mode == "modern" {
        let get = |k: &str| config.forwarding.rest.get(k).and_then(|v| v.as_str());
        let secret = match get("secret-env") {
            Some(var) => {
                std::env::var(var).map_err(|_| format!("environment variable {var} is not set"))?
            }
            None => {
                let path = get("secret-file").unwrap_or("forwarding.secret");
                std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?
            }
        };
        Some(secret.trim().to_string())
    } else {
        None
    };
    let bridge = if config.bridge.enabled {
        let path = &config.bridge.key_file;
        let key = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
        Some((config.bridge.listen.clone(), key.trim().to_string()))
    } else {
        None
    };
    Ok(Network {
        velocity_secret,
        bridge,
    })
}

/// The servers of the manager as `servers:` entries: loopback, the protocol
/// of their Minecraft release.
pub fn backends(proxy: &Proxy, m: &Manager) -> BTreeMap<String, ServerConfig> {
    m.entries()
        .into_iter()
        .map(|(name, e)| {
            let protocol = m.minecraft(&e).and_then(|release| {
                proxy
                    .versions
                    .versions()
                    .find(|v| {
                        proxy
                            .versions
                            .get(*v)
                            .is_some_and(|m| m.release_names().contains(&release))
                    })
                    .map(|v| v.0)
            });
            let backend = ServerConfig {
                address: format!("127.0.0.1:{}", e.port),
                protocol,
                chat_session_forwarding: true,
                signed_chat_cancel: SignedChatCancel::default(),
            };
            (name, backend)
        })
        .collect()
}

/// `pumbo.proxy.servers.<node>`; operators and the console by default.
pub fn can(proxy: &Proxy, rt: &Runtime, who: &Who<'_>, node: &str) -> bool {
    crate::commands::permitted(proxy, rt, who, &format!("pumbo.proxy.servers.{node}"))
}

/// `pumbo.proxy.download`.
fn may(proxy: &Proxy, rt: &Runtime, who: &Who<'_>) -> bool {
    crate::commands::permitted(proxy, rt, who, "pumbo.proxy.download")
}

/// Sends the result of a command that finishes later to whoever ran it.
type Say = Arc<dyn Fn(&str) + Send + Sync>;

fn say(proxy: &Arc<Proxy>, who: &Who<'_>) -> Say {
    match who {
        Who::Console => Arc::new(|t: &str| info!("{}", line(t).plain_text())),
        Who::Player { id, .. } => {
            let (p, id) = (Arc::downgrade(proxy), *id);
            Arc::new(move |t: &str| {
                if let Some(p) = p.upgrade() {
                    p.send_to(id, SessionCmd::Message(Box::new(line(t))));
                }
            })
        }
    }
}

/// `/prox download …` and `/prox servers …`; `args` are the words after `prox`.
pub fn command(proxy: &Arc<Proxy>, rt: &Runtime, who: &Who<'_>, args: &[&str]) -> Reply {
    let Some(m) = proxy.servers.get().cloned() else {
        return reply(
            "<err>Servers from the proxy are off <s>(managed-servers.enabled in pumboprox.yml)",
        );
    };
    let rest = args.get(1..).unwrap_or_default();
    match args.first().map(|s| s.to_ascii_lowercase()).as_deref() {
        Some("download") => download(proxy, rt, who, m, rest),
        _ => server(proxy, rt, who, m, rest),
    }
}

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

fn download(
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

/// Bar colours (vanilla IDs).
const YELLOW: i32 = 4;
const BYTES_PER_MB: f64 = 1_000_000.0;
const SPEED_WINDOW: Duration = Duration::from_secs(3);
const DRAW_EVERY: Duration = Duration::from_millis(250);

/// One download as its starter sees it: a boss bar for a player (it ends
/// with the player), the log every 10 % for everyone.
struct Watch {
    proxy: Weak<Proxy>,
    player: Option<Uuid>,
    bar: Uuid,
    software: String,
    st: Mutex<WatchState>,
}

#[derive(Default)]
struct WatchState {
    tag: Option<String>,
    shown: bool,
    drawn: Option<Instant>,
    samples: VecDeque<(Instant, u64)>,
    tenths: u64,
    retry: Option<(u32, u32)>,
    unverified: bool,
}

impl Watch {
    fn new(proxy: &Arc<Proxy>, player: Option<Uuid>, src: &str) -> Self {
        Self {
            proxy: Arc::downgrade(proxy),
            player,
            bar: {
                let mut b = [0u8; 16];
                let _ = aws_lc_rs::rand::fill(&mut b);
                Uuid::from_bytes(b)
            },
            software: src.to_string(),
            st: Mutex::new(WatchState::default()),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, WatchState> {
        self.st
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn unverified(&self) -> bool {
        self.lock().unverified
    }

    /// The tag once known, else what was asked for (`#2`).
    fn what(&self, which: &str) -> String {
        self.lock().tag.clone().unwrap_or_else(|| esc(which))
    }

    fn send(&self, op: BossbarOp) {
        if let (Some(id), Some(p)) = (self.player, self.proxy.upgrade()) {
            // A player who left: nothing to show, the download goes on.
            p.send_to(
                id,
                SessionCmd::Bossbar {
                    id: self.bar,
                    op: Box::new(op),
                },
            );
        }
    }

    fn on(&self, p: Progress) {
        let mut st = self.lock();
        let (done, total) = match p {
            Progress::Started { tag } => {
                st.tag = Some(tag);
                return;
            }
            Progress::Unverified => {
                st.unverified = true;
                return;
            }
            Progress::Retry { attempt, of } => {
                st.retry = Some((attempt, of));
                st.samples.clear();
                st.tenths = 0;
                st.drawn = None;
                info!(
                    "downloading {} {}: retrying ({attempt}/{of})",
                    self.software,
                    st.tag.as_deref().unwrap_or("")
                );
                (0, None)
            }
            Progress::Bytes { done, total } => (done, total),
        };
        let now = Instant::now();
        st.samples.push_back((now, done));
        while st.samples.len() > 2
            && st
                .samples
                .front()
                .is_some_and(|(t, _)| now.duration_since(*t) > SPEED_WINDOW)
        {
            st.samples.pop_front();
        }
        let speed = match st.samples.front() {
            Some((t, d)) if now.duration_since(*t) > Duration::from_millis(200) => {
                done.saturating_sub(*d) as f64 / now.duration_since(*t).as_secs_f64()
            }
            _ => 0.0,
        };
        let mb = |b: u64| b as f64 / BYTES_PER_MB;
        let tag = st.tag.clone().unwrap_or_default();
        if let Some(t) = total.filter(|t| *t > 0) {
            let tenths = done * 10 / t;
            if tenths > st.tenths {
                st.tenths = tenths;
                info!(
                    "downloading {} {tag}: {}% ({:.0}/{:.0} MB, {:.1} MB/s)",
                    self.software,
                    tenths * 10,
                    mb(done),
                    mb(t),
                    speed / BYTES_PER_MB
                );
            }
        }
        let last = total == Some(done);
        if self.player.is_none() || (!last && st.drawn.is_some_and(|d| d.elapsed() < DRAW_EVERY)) {
            return;
        }
        st.drawn = Some(now);
        let (progress, size) = match total.filter(|t| *t > 0) {
            Some(t) => (
                done as f32 / t as f32,
                format!(
                    " · {}% · {:.1} MB/s · {:.0}/{:.0} MB",
                    done * 100 / t,
                    speed / BYTES_PER_MB,
                    mb(done),
                    mb(t)
                ),
            ),
            None => (
                0.0,
                format!(" · {:.1} MB/s · {:.0} MB", speed / BYTES_PER_MB, mb(done)),
            ),
        };
        let retry = st
            .retry
            .map(|(a, of)| format!(" <yellow>· retrying ({a}/{of})"))
            .unwrap_or_default();
        let title = row(&format!(
            "<white>Downloading {} {tag}<gray>{size}{retry}",
            title_case(&self.software)
        ));
        if st.shown {
            self.send(BossbarOp::Title(title));
            self.send(BossbarOp::Progress(progress));
        } else {
            st.shown = true;
            self.send(BossbarOp::Show {
                title,
                progress,
                color: YELLOW,
                overlay: 0,
            });
        }
    }

    /// The bar in its end colour (a new bar: colours do not change in place),
    /// gone after `linger`.
    fn finish(self: &Arc<Self>, mini: &str, color: i32, linger: Duration) {
        if self.player.is_none() {
            return;
        }
        if self.lock().shown {
            self.send(BossbarOp::Hide);
        }
        self.send(BossbarOp::Show {
            title: row(mini),
            progress: 1.0,
            color,
            overlay: 0,
        });
        let me = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(linger).await;
            me.send(BossbarOp::Hide);
        });
    }
}

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

fn server(
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
    if sub != "new" && m.entry(&name).is_none() && rt.config.servers.contains_key(&name) {
        return reply(&format!(
            "<err><v>{}</v> is not run by the proxy; <s>it comes from servers: in pumboprox.yml, change it there.",
            esc(&name)
        ));
    }
    if sub != "new" && m.entry(&name).is_none() {
        return reply(&format!(
            "<err>No server named <v>{}</v>.{}",
            esc(&name),
            button(console, "list", "/prox servers list", true)
        ));
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
        "start" => match m.start(&name) {
            Ok(()) => {
                when_ready(m, name.clone(), say);
                reply(&format!("<s>Starting <v>{name}</v>…"))
            }
            Err(e) => reply(&format!("<err>{}", esc(&e))),
        },
        "stop" => {
            let n = name.clone();
            tokio::spawn(async move {
                match m.stop(&n).await {
                    Ok(()) => say(&format!("<ok>Stopped <v>{n}</v>.")),
                    Err(e) => say(&format!("<err>{}", esc(&e))),
                }
            });
            reply(&format!("<s>Stopping <v>{name}</v>…"))
        }
        "restart" => {
            let n = name.clone();
            tokio::spawn(async move {
                match m.restart(&n).await {
                    Ok(()) => when_ready(m, n, say),
                    Err(e) => say(&format!("<err>{}", esc(&e))),
                }
            });
            reply(&format!("<s>Restarting <v>{name}</v>…"))
        }
        "logs" => {
            let n = args
                .get(2)
                .and_then(|n| n.parse::<usize>().ok())
                .unwrap_or(20)
                .clamp(1, 100);
            let lines = m.logs(&name, n).unwrap_or_default();
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

fn list(proxy: &Proxy, m: &Manager, console: bool) -> Reply {
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

/// Completion after `prox download|server`: `words` are the finished words
/// after `prox`, `arg` the index of the word being typed among them.
pub fn suggest(proxy: &Proxy, words: &[&str], arg: usize) -> Vec<String> {
    let Some(m) = proxy.servers.get() else {
        return Vec::new();
    };
    let word = |i: usize| {
        words
            .get(i)
            .map(|s| s.to_ascii_lowercase())
            .unwrap_or_default()
    };
    let numbered = |src: &str| {
        let list = m.versions.cached(src).unwrap_or_default();
        (1..=list.len())
            .map(|n| format!("#{n}"))
            .chain(list.into_iter().map(|r| r.tag))
            .collect::<Vec<_>>()
    };
    let names = || m.entries().into_keys().collect::<Vec<_>>();
    let active = m.versions.downloads();
    match (word(0).as_str(), arg) {
        ("download", 1) => {
            let mut v: Vec<String> = m
                .versions
                .source_names()
                .into_iter()
                .map(String::from)
                .collect();
            v.extend(["paper".into(), "stop".into(), "help".into()]);
            v
        }
        ("download", 2) if word(1) == "stop" => {
            let mut v: Vec<String> = active.iter().map(|(s, _)| s.to_string()).collect();
            v.extend(active.iter().map(|(_, t)| t.clone()));
            v.dedup();
            v
        }
        ("download", 3) if word(1) == "stop" => active
            .iter()
            .filter(|(s, _)| *s == word(2))
            .map(|(_, t)| t.clone())
            .collect(),
        ("download", 2) => {
            let mut v = vec!["list".to_string()];
            v.extend(numbered(&word(1)));
            v
        }
        ("servers", 1) => SERVER_SUBS.iter().map(|s| s.to_string()).collect(),
        ("servers", 2)
            if matches!(
                word(1).as_str(),
                "start" | "stop" | "restart" | "logs" | "delete"
            ) =>
        {
            names()
        }
        ("servers", 3) if word(1) == "new" => {
            let mut v = vec!["latest".to_string()];
            for src in m.versions.source_names() {
                v.extend(m.versions.installed(src));
                v.extend(numbered(src).into_iter().filter(|s| s.starts_with('#')));
            }
            v.dedup();
            v
        }
        ("servers", 3) if word(1) == "delete" => vec!["confirm".into()],
        ("servers", 3) if word(1) == "logs" => vec!["20".into(), "50".into(), "100".into()],
        ("servers", 4) if word(1) == "new" => m.template_names(),
        _ => Vec::new(),
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

    #[test]
    fn messages_render_without_raw_codes() {
        let all = [
            line("<err>Usage: <c>/prox servers <v>x"),
            line(&format!(
                "<ok>Downloaded <v>pumpkin 0.2.0</v>, <s>SHA256 checked.{}",
                button(
                    false,
                    "create a server",
                    "/prox servers new <name> 0.2.0",
                    false
                )
            )),
            usage("/prox download <pumpkin> [list|#n|tag]")
                .lines
                .remove(0),
            row("<muted>#1</muted> <v>canary</v> <warn>development</warn>"),
        ];
        for c in all {
            let text = c.plain_text();
            assert!(
                text.starts_with("PumboProx » ") || text.starts_with("#1"),
                "{text}"
            );
            assert!(
                !text.contains('&') && !text.contains("<ok>") && !text.contains("<v>"),
                "{text}"
            );
        }
        assert!(
            usage("/prox servers new <name>").lines[0]
                .plain_text()
                .ends_with("/prox servers new <name>")
        );
        assert_eq!(title_case("pumpkin"), "Pumpkin");
    }
}
