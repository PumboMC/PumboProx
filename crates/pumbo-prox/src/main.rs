//! `pumboprox`: `run [config]` (default; a missing config is written with
//! the defaults), `check-config [config]`, `version` (`--version`),
//! `describe <plugin> [--json] [config]`, `help` (`--help`).
//! Console commands while running: `help`, `reload`, `stop`, `version`, the
//! built-in proxy commands `glist`, `find`, `send`, `alert`, `pumbo` (the
//! plugin list), plugin commands and `prox …` (the same as in the game).

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use pumbo_core::listener::Listener;
use pumbo_prox::config::{Config, Loaded};
use pumbo_prox::modules;
use pumbo_prox::server::{ListenerSettings, Proxy, Runtime};
use tokio::io::AsyncBufReadExt;
use tracing::{error, info, warn};

const DEFAULT_CONFIG: &str = "pumboprox.yml";

const USAGE: &str = "\
usage: pumboprox [command]
  run [file]                         start the proxy (the default); a missing
                                     file is written with the defaults
  check-config [file]                check the file and the plugin configs
                                     without starting the proxy
  migrate-config [file]              rename old keys in the file (a copy of
                                     it stays as <file>.bak)
  describe <plugin> [--json] [file]  a plugin's description as JSON
  version, --version, -V             versions of PumboProx and the protocols
  help, --help, -h                   this text
The file is pumboprox.yml in the current directory unless given.";

/// Console commands for `help`; `prox help` lists the proxy commands.
const CONSOLE_HELP: &[&str] = &[
    "help: this list",
    "reload: read pumboprox.yml again; players stay online",
    "stop (end): stop the proxy and the servers it runs",
    "version: versions of PumboProx and the protocols",
    "glist, find, send, alert: players and servers (prox help for all)",
    "prox ...: the proxy commands, as /prox in the game",
    "pumbo: the plugins, their versions and state",
];

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // After a command, `--help` is no file name (`run --help` would write one).
    if args.iter().skip(1).any(|a| a == "--help" || a == "-h") {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    let describe_json = |a: &String| a == "--json" && args.first().is_some_and(|c| c == "describe");
    if let Some(flag) = args
        .iter()
        .skip(1)
        .find(|a| a.starts_with('-') && !describe_json(a))
    {
        eprintln!("pumboprox: unknown option \"{flag}\"; see pumboprox --help");
        return ExitCode::from(2);
    }
    match args.first().map(String::as_str) {
        Some("version" | "--version" | "-V") => version(),
        Some("help" | "--help" | "-h") => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        Some("check-config") => check_config(args.get(1).map_or(DEFAULT_CONFIG, String::as_str)),
        Some("migrate-config") => {
            migrate_config(args.get(1).map_or(DEFAULT_CONFIG, String::as_str))
        }
        Some("run") => run(args.get(1).map_or(DEFAULT_CONFIG, String::as_str)),
        Some("describe") => describe(&args),
        None => run(DEFAULT_CONFIG),
        Some(other) => {
            eprintln!("pumboprox: unknown command \"{other}\"; see pumboprox --help");
            ExitCode::from(2)
        }
    }
}

fn version() -> ExitCode {
    println!("PumboProx {}", env!("CARGO_PKG_VERSION"));
    match modules::versions() {
        Ok(reg) => {
            for v in reg.versions() {
                if let Some(m) = reg.get(v) {
                    println!("protocol {v}: {}", m.release_names().join(", "));
                }
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("protocol tables: {e}");
            ExitCode::FAILURE
        }
    }
}

fn load(path: &str) -> Result<Loaded, String> {
    pumbo_prox::config::load(std::path::Path::new(path))
}

fn read(path: &str) -> Result<String, String> {
    pumbo_prox::config::read(std::path::Path::new(path))
}

fn block_on<T>(f: impl std::future::Future<Output = T>) -> Result<T, String> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map(|rt| rt.block_on(f))
        .map_err(|e| format!("tokio: {e}"))
}

/// `describe <plugin> [--json] [file]`: the machine-readable description of
/// a plugin (plan §6.6.5) as JSON, without running its `init`.
fn describe(args: &[String]) -> ExitCode {
    let rest: Vec<&str> = args
        .iter()
        .skip(1)
        .map(String::as_str)
        .filter(|a| *a != "--json")
        .collect();
    let (Some(id), path) = (rest.first(), rest.get(1).copied().unwrap_or(DEFAULT_CONFIG)) else {
        eprintln!("usage: pumboprox describe <plugin> [--json] [file]");
        return ExitCode::from(2);
    };
    let result = read(path)
        .and_then(|text| {
            if let (_, Some(w)) = settle(path, None, &text) {
                eprintln!("{w}");
            }
            pumbo_host::HostConfig::parse(&text).map_err(|e| format!("{path}: {e}"))
        })
        .and_then(|cfg| block_on(pumbo_host::describe_offline(cfg, id)).and_then(|r| r));
    match result {
        Ok((json, check)) => {
            println!(
                "{}",
                serde_json::to_string_pretty(&json).unwrap_or_default()
            );
            if let Err(e) = check {
                eprintln!("config of {id}: {e}");
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("describe: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Plugin configs against the schemas of their descriptions.
fn check_plugins(path: &str) -> Result<(), String> {
    let text = read(path)?;
    let cfg = pumbo_host::HostConfig::parse(&text).map_err(|e| format!("{path}: {e}"))?;
    if !cfg.plugins.dir.is_dir() {
        return Ok(());
    }
    let results = block_on(pumbo_host::check_plugins(cfg))??;
    let mut failed = Vec::new();
    for (id, r) in results {
        match r {
            Ok(()) => println!("plugin {id}: config ok"),
            Err(e) => {
                println!("plugin {id}: {e}");
                failed.push(id);
            }
        }
    }
    if failed.is_empty() {
        Ok(())
    } else {
        Err(format!("invalid plugin config: {}", failed.join(", ")))
    }
}

fn check_config(path: &str) -> ExitCode {
    let mut unknown = Vec::new();
    let mut file = path.to_string();
    let result = load(path).and_then(|loaded| {
        let mut cfg = loaded.config;
        cfg.check_only = true;
        unknown = loaded.unknown;
        // Warnings of the checks (e.g. a backend outside private networks) go to stderr.
        pumbo_prox::logging::init(&cfg.logging);
        let (absolute, moved) = settle(path, Some(&cfg), &loaded.text);
        file = absolute;
        moved
            .iter()
            .chain(&loaded.warnings)
            .for_each(|w| warn!("{w}"));
        for w in cfg
            .check_backend_names()
            .map_err(|e| format!("{path}: {e}"))?
        {
            warn!("{w}");
        }
        if !cfg.logging.metrics_bind.is_empty() {
            pumbo_prox::metrics::read_token(&cfg.logging.metrics_token_file)
                .map_err(|e| format!("logging.metrics-token-file {e}"))?;
        }
        Runtime::build(cfg, &(String::new(), String::new()))
    });
    match result {
        Ok(rt) => {
            let cfg = &rt.config;
            for l in &cfg.listener {
                let pp = match l.proxy_protocol {
                    pumbo_prox::config::ProxyProtocol::Required => ", PROXY protocol (required)",
                    pumbo_prox::config::ProxyProtocol::Optional => ", PROXY protocol (optional)",
                    pumbo_prox::config::ProxyProtocol::Off => "",
                };
                println!("listener: {} on {}{pp}", l.transport, l.bind);
            }
            let m = &rt.modules;
            println!("forwarding: {}", m.forwarding.name());
            println!("authentication: {}", m.authenticator.name());
            println!("backend selection: {}", m.selector.name());
            let chain: Vec<&str> = m.translators.iter().map(|t| t.name()).collect();
            println!("translation: {}", chain.join(" -> "));
            println!(
                "servers: {}",
                cfg.servers.keys().cloned().collect::<Vec<_>>().join(", ")
            );
            let mut code = ExitCode::SUCCESS;
            // A key nothing reads is only a warning at start; here it fails.
            for u in &unknown {
                eprintln!("config error: {u}");
                code = ExitCode::FAILURE;
            }
            if let Err(e) = check_plugins(&file) {
                eprintln!("config error: {e}");
                code = ExitCode::FAILURE;
            }
            code
        }
        Err(e) => {
            eprintln!("config error: {e}");
            ExitCode::FAILURE
        }
    }
}

/// `migrate-config [file]`: old keys renamed in the file, a copy of the
/// old file first (never over an older copy).
fn migrate_config(path: &str) -> ExitCode {
    let file = std::path::Path::new(path);
    let result = read(path).and_then(|text| {
        let Some((new, changes)) = pumbo_prox::migrate::migrate_text(&text)? else {
            return Ok(None);
        };
        let backup = pumbo_prox::route::backup(file, &text)?;
        pumbo_prox::route::write(file, &new)?;
        Ok(Some((changes, backup)))
    });
    match result {
        Ok(None) => {
            println!(
                "{path} needs no changes (config-version {})",
                pumbo_prox::migrate::CONFIG_VERSION
            );
            ExitCode::SUCCESS
        }
        Ok(Some((changes, backup))) => {
            changes.iter().for_each(|c| println!("{path} {c}"));
            let backup = backup.display();
            println!(
                "{path} is at config-version {}; the old file is {backup}: to go back to an older PumboProx, restore {backup}",
                pumbo_prox::migrate::CONFIG_VERSION
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("migrate-config: {path}: {e}; the file is unchanged");
            ExitCode::FAILURE
        }
    }
}

/// The relative paths of the file that are in use, by the name a warning
/// shows (a directory ends in `/`).
fn relative_paths(
    cfg: Option<&Config>,
    host: Option<&pumbo_host::HostConfig>,
) -> Vec<(String, PathBuf)> {
    let mut out: Vec<(String, PathBuf)> = Vec::new();
    let mut file = |p: &str| out.push((p.to_string(), PathBuf::from(p)));
    if let Some(cfg) = cfg {
        let fwd = &cfg.forwarding;
        if fwd.mode == "modern" && !fwd.rest.contains_key("secret-env") {
            file(
                fwd.rest
                    .get("secret-file")
                    .and_then(|v| v.as_str())
                    .unwrap_or("forwarding.secret"),
            );
        }
        if cfg.bridge.enabled && cfg.bridge.key_env.is_empty() {
            file(&cfg.bridge.key_file);
        }
        if !cfg.status.favicon.is_empty() {
            file(&cfg.status.favicon);
        }
        if cfg.managed_servers.enabled {
            let m = &cfg.managed_servers;
            for d in [&m.dir, &m.versions_dir] {
                out.push((format!("{}/", d.display()), d.clone()));
            }
        }
    }
    if let Some(host) = host {
        // Plugin data lives under the plugin directory.
        let dir = &host.plugins.dir;
        out.push((format!("{}/", dir.display()), dir.clone()));
        let perms = &host.permissions.file;
        out.push((perms.display().to_string(), perms.clone()));
    }
    out.retain(|(_, p)| p.is_relative() && !p.as_os_str().is_empty());
    out
}

/// The relative paths found only under `cwd`, not next to the config in
/// `config_dir`: an install that ran from another directory. An empty
/// directory counts as missing: 0.2.5-beta made an empty `plugins/` next to
/// the config while it loaded the plugins from the working directory.
fn only_in_cwd(paths: &[(String, PathBuf)], config_dir: &Path, cwd: &Path) -> Vec<String> {
    let used = |p: PathBuf| match p.read_dir() {
        Ok(mut entries) => entries.next().is_some(),
        Err(_) => p.exists(),
    };
    paths
        .iter()
        .filter(|(_, p)| used(cwd.join(p)) && !used(config_dir.join(p)))
        .map(|(name, _)| name.clone())
        .collect()
}

/// Relative paths of the file are read next to the config: the process
/// moves there, once, before anything opens them. When a path the file
/// uses exists only in the working directory, the process stays there with
/// a warning, so a secret is never created anew next to the config while
/// the plugins still come from the old place. Returns the config path as
/// an absolute path (`reload` opens it again) and the warning.
fn settle(path: &str, cfg: Option<&Config>, text: &str) -> (String, Option<String>) {
    let Ok(absolute) = std::path::absolute(path) else {
        return (path.to_string(), None);
    };
    let file = absolute.display().to_string();
    let (Some(dir), Ok(cwd)) = (absolute.parent(), std::env::current_dir()) else {
        return (file, None);
    };
    if same_dir(dir, &cwd) {
        return (file, None);
    }
    let host = pumbo_host::HostConfig::parse(text).ok();
    let stay = only_in_cwd(&relative_paths(cfg, host.as_ref()), dir, &cwd);
    if !stay.is_empty() {
        let w = format!(
            "relative paths are read from the working directory {} because {} {} there and missing or empty next to {file}; move them next to the config",
            cwd.display(),
            stay.join(", "),
            if stay.len() == 1 { "is" } else { "are" }
        );
        return (file, Some(w));
    }
    match std::env::set_current_dir(dir) {
        Ok(()) => (file, None),
        Err(e) => {
            let w = format!(
                "relative paths are read from the working directory {}: cannot change to {} ({e})",
                cwd.display(),
                dir.display()
            );
            (file, Some(w))
        }
    }
}

fn same_dir(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// First start: the default config and an empty `plugins/` next to it.
/// A `.toml` of the same name is left to [`read`]'s message.
fn create_default(path: &str) -> Result<bool, String> {
    let file = std::path::Path::new(path);
    if file.exists() || file.with_extension("toml").is_file() {
        return Ok(false);
    }
    std::fs::write(file, pumbo_prox::config::DEFAULT_FILE).map_err(|e| format!("{path}: {e}"))?;
    let plugins = file.with_file_name("plugins");
    std::fs::create_dir_all(&plugins).map_err(|e| format!("{}: {e}", plugins.display()))?;
    Ok(true)
}

fn run(path: &str) -> ExitCode {
    let created = match create_default(path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("config error: {e}");
            return ExitCode::FAILURE;
        }
    };
    let Loaded {
        text,
        config: cfg,
        unknown,
        warnings,
    } = match load(path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("config error: {e}");
            return ExitCode::FAILURE;
        }
    };
    pumbo_prox::logging::init(&cfg.logging);
    let (file, moved) = settle(path, Some(&cfg), &text);
    unknown
        .iter()
        .chain(&warnings)
        .chain(&moved)
        .for_each(|u| warn!("{u}"));
    match cfg.check_backend_names() {
        Ok(warnings) => warnings.iter().for_each(|w| warn!("{w}")),
        Err(e) => {
            error!("config error: {path}: {e}");
            return ExitCode::FAILURE;
        }
    }
    if created {
        info!("created {path} with the default settings");
    }
    let rt = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("tokio: {e}");
            return ExitCode::FAILURE;
        }
    };
    let code = rt.block_on(async {
        // First, so a signal during the start stops what already runs.
        let mut signals = Signals::new();
        let proxy = match Proxy::new(cfg, Some(PathBuf::from(&file))) {
            Ok(p) => p,
            Err(e) => {
                error!("cannot start: {e}");
                return ExitCode::FAILURE;
            }
        };
        // Before the start, which can take minutes (a server download): `/health` answers meanwhile.
        proxy.start_metrics().await;
        let listeners = tokio::select! {
            r = start(&proxy, &text) => match r {
                Some(l) => l,
                None => return ExitCode::FAILURE,
            },
            () = signals.recv() => {
                info!("stopping (signal during the start)");
                log_stop_budget(&proxy);
                tokio::select! {
                    () = shutdown(&proxy) => {}
                    () = signals.recv() => forced_stop(),
                }
                info!("stopped");
                return ExitCode::SUCCESS;
            }
        };
        info!("PumboProx {} started", env!("CARGO_PKG_VERSION"));
        for w in pumbo_prox::commands::login_warnings(&proxy) {
            warn!("{w}");
        }
        if let Some(p) = proxy.plugins.get() {
            let virtual_ms = proxy.runtime().config.virtual_world.gate_timeout_ms;
            for w in pumbo_prox::commands::gate_timeout_warnings(p.host.config(), virtual_ms) {
                warn!("{w}");
            }
        }
        if let Some(code) = pumbo_prox::commands::claim_code(&proxy) {
            warn!("{}", pumbo_prox::commands::claim_hint(&proxy, &code));
        }
        tokio::spawn(console(proxy.clone()));
        let stopping = proxy.clone();
        tokio::spawn(async move {
            signals.recv().await;
            info!("stopping (signal)");
            log_stop_budget(&stopping);
            stopping.stop();
            signals.recv().await;
            forced_stop();
        });
        proxy.clone().serve(listeners).await;
        shutdown(&proxy).await;
        info!("stopped");
        ExitCode::SUCCESS
    });
    // The console reads stdin on a blocking thread that cannot be cancelled
    // (`tokio::io::stdin`): dropping the runtime would wait for the next line
    // forever when stdin never closes (a held FIFO, a TTY in `screen`).
    rt.shutdown_timeout(Duration::from_secs(1));
    code
}

/// Commands from stdin. End of input (e.g. under a service manager) only
/// ends the console, not the proxy. Replies go to stderr next to the log,
/// but not through it: they show at every `logging.level`, as plain text.
async fn console(proxy: std::sync::Arc<Proxy>) {
    let mut lines = tokio::io::BufReader::new(tokio::io::stdin()).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let line = line.trim().to_string();
        // `prox bridge` is printed, not logged: the key must not land in log files.
        if let Some(rest) = line
            .strip_prefix("prox bridge")
            .filter(|r| r.is_empty() || r.starts_with(' '))
        {
            let args: Vec<String> = rest.split_whitespace().map(str::to_string).collect();
            match proxy.bridge.get() {
                // Printed, not logged: the key must not land in log files.
                Some(b) => b
                    .admin(&args, true, true)
                    .iter()
                    .for_each(|l| println!("{}", pumbo_prox::bridge::plain(l))),
                None => eprintln!("the bridge is off (bridge.enabled in the proxy config)"),
            }
            continue;
        }
        match line.as_str() {
            "" => {}
            "help" => CONSOLE_HELP.iter().for_each(|l| eprintln!("{l}")),
            "reload" => match proxy.reload() {
                Ok(restart) => {
                    eprintln!("config reloaded");
                    if !restart.is_empty() {
                        eprintln!("changed, applied after a restart: {}", restart.join(", "));
                    }
                }
                Err(e) => eprintln!("reload failed, keeping the old config: {e}"),
            },
            "stop" | "end" => {
                info!("stopping");
                log_stop_budget(&proxy);
                proxy.stop();
                return;
            }
            "version" => eprintln!(
                "PumboProx {}, protocols {}",
                env!("CARGO_PKG_VERSION"),
                proxy
                    .versions
                    .versions()
                    .map(|v| v.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            other if other.starts_with("gate ") => {
                let name = other.trim_start_matches("gate ").trim();
                match proxy.find_player(name) {
                    _ if !proxy.runtime().config.virtual_world.test_gate => {
                        eprintln!("gate needs virtual.test-gate: true (tests only)");
                    }
                    Some(p) => pumbo_prox::world::enter_test_gate(&proxy, p.id),
                    None => eprintln!("{name} is not online"),
                }
            }
            other => {
                let rt = proxy.runtime();
                match pumbo_prox::commands::run(
                    &proxy,
                    &rt,
                    &pumbo_prox::commands::Source::Console,
                    other,
                ) {
                    Some(reply) => {
                        for line in reply.lines {
                            eprintln!("{}", line.plain_text());
                        }
                    }
                    None => match proxy.plugins.get().and_then(|p| p.console(other)) {
                        Some(lines) => {
                            for line in lines {
                                eprintln!("{}", line.plain_text());
                            }
                        }
                        None => eprintln!("unknown command \"{other}\"; type help"),
                    },
                }
            }
        }
    }
}

/// Everything before the listeners accept players; `None` after an error
/// (logged).
async fn start(
    proxy: &std::sync::Arc<Proxy>,
    text: &str,
) -> Option<Vec<(Box<dyn Listener>, std::sync::Arc<ListenerSettings>)>> {
    // PumboBridge (before the plugin host: it provides pumbo:bridge).
    let bridge_cfg = proxy.runtime().config.bridge.clone();
    let mut natives = Vec::new();
    if bridge_cfg.enabled {
        match pumbo_prox::bridge::Bridge::new(proxy, bridge_cfg) {
            Ok(b) => {
                natives.push(b.native());
                let _ = proxy.bridge.set(b);
            }
            Err(e) => {
                error!("cannot start the bridge: {e}");
                return None;
            }
        }
    }
    // Plugin host (E5), from the same file; a missing plugin directory is created.
    let started = match pumbo_prox::plugins::Plugins::launch(text, proxy, natives).await {
        Ok(Some(p)) => {
            let _ = proxy.plugins.set(p.clone());
            p.wait_started().await
        }
        Ok(None) => {
            warn!("no plugin directory, running without plugins");
            Ok(())
        }
        Err(e) => Err(e),
    };
    if let Err(e) = started {
        error!("cannot start plugins: {e}");
        return None;
    }
    if let Some(b) = proxy.bridge.get()
        && let Err(e) = b.start().await
    {
        error!("cannot start: {e}");
        return None;
    }
    // Servers from the proxy (after the bridge: new servers get its key).
    if let Err(e) = pumbo_prox::servers::start(proxy) {
        error!("cannot start: {e}");
        return None;
    }
    match proxy.bind().await {
        Ok(l) => Some(l),
        Err(e) => {
            error!("cannot start: {e}");
            None
        }
    }
}

/// Stops the servers run by the proxy and the plugins (bounded).
async fn shutdown(proxy: &Proxy) {
    if let Some(m) = proxy.servers.get() {
        info!("stopping the servers run by the proxy");
        m.shutdown().await;
    }
    if let Some(p) = proxy.plugins.get() {
        let _ = tokio::time::timeout(pumbo_prox::server::PLUGIN_SHUTDOWN, p.host.shutdown()).await;
    }
}

/// With managed servers: the time a container must give the proxy to stop.
fn log_stop_budget(proxy: &Proxy) {
    if proxy.servers.get().is_some_and(|m| !m.list().is_empty()) {
        let line = pumbo_prox::server::stop_budget(&proxy.runtime().config.managed_servers);
        info!("{line}");
    }
}

/// A second signal while stopping ends the process at once.
fn forced_stop() -> ! {
    warn!("forced stop: plugins and managed servers were not stopped");
    std::process::exit(130);
}

/// SIGTERM and SIGINT (Ctrl+C elsewhere), registered when created.
struct Signals {
    #[cfg(unix)]
    streams: Option<(tokio::signal::unix::Signal, tokio::signal::unix::Signal)>,
}

impl Signals {
    fn new() -> Signals {
        #[cfg(unix)]
        let streams = {
            use tokio::signal::unix::{SignalKind, signal};
            match (
                signal(SignalKind::terminate()),
                signal(SignalKind::interrupt()),
            ) {
                (Ok(term), Ok(int)) => Some((term, int)),
                _ => None,
            }
        };
        Signals {
            #[cfg(unix)]
            streams,
        }
    }

    /// The next signal (never, when they could not be registered).
    async fn recv(&mut self) {
        #[cfg(unix)]
        {
            if let Some((term, int)) = &mut self.streams {
                tokio::select! {
                    _ = term.recv() => {}
                    _ = int.recv() => {}
                }
                return;
            }
        }
        #[cfg(not(unix))]
        {
            if tokio::signal::ctrl_c().await.is_ok() {
                return;
            }
        }
        std::future::pending::<()>().await;
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn the_first_start_writes_the_default_config_once() {
        let dir = std::env::temp_dir().join(format!("pumbo-first-start-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("pumboprox.yml");
        let path = path.to_str().unwrap();
        assert!(create_default(path).unwrap());
        assert_eq!(
            std::fs::read_to_string(path).unwrap(),
            pumbo_prox::config::DEFAULT_FILE
        );
        assert!(dir.join("plugins").is_dir());
        assert!(!create_default(path).unwrap(), "an existing file stays");
        // Next to an old `.toml` nothing is written: `read` explains.
        let old = dir.join("old.yml");
        std::fs::write(dir.join("old.toml"), "").unwrap();
        assert!(!create_default(old.to_str().unwrap()).unwrap());
        assert!(
            read(old.to_str().unwrap())
                .unwrap_err()
                .contains("this version reads YAML")
        );
    }
}
