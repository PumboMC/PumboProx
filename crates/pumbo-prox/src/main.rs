//! `pumboprox`: `run [config]` (default), `check-config [config]`, `version`,
//! `describe <plugin> [--json] [config]`.
//! Console commands while running: `reload`, `stop`, `version`, the
//! built-in proxy commands `glist`, `find`, `send`, `alert`, `pumbo …` and
//! `plugin reload|unload|load <id>` (short for `pumbo proxy plugin …`).

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use pumbo_prox::config::Config;
use pumbo_prox::modules;
use pumbo_prox::server::{Proxy, Runtime};
use tokio::io::AsyncBufReadExt;
use tracing::{error, info, warn};

const DEFAULT_CONFIG: &str = "pumboprox.yml";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("version") => version(),
        Some("check-config") => check_config(args.get(1).map_or(DEFAULT_CONFIG, String::as_str)),
        Some("run") => run(args.get(1).map_or(DEFAULT_CONFIG, String::as_str)),
        Some("describe") => describe(&args),
        None => run(DEFAULT_CONFIG),
        _ => {
            eprintln!(
                "usage: pumboprox [run [file]] | check-config [file] | describe <plugin> [--json] [file] | version"
            );
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

fn load(path: &str) -> Result<Config, String> {
    let text = read(path)?;
    Config::parse(&text).map_err(|e| format!("{path}: {e}"))
}

/// The config file; a missing one next to a `.toml` of the same name (the
/// format before YAML) says so instead of only "not found".
fn read(path: &str) -> Result<String, String> {
    std::fs::read_to_string(path).map_err(|e| {
        let old = std::path::Path::new(path).with_extension("toml");
        if e.kind() == std::io::ErrorKind::NotFound && old.is_file() {
            format!(
                "{path} not found; found {}, this version reads YAML - convert it to YAML with the same keys (see https://github.com/PumboMC/PumboProx)",
                old.display()
            )
        } else {
            format!("{path}: {e}")
        }
    })
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
        .and_then(|text| pumbo_host::HostConfig::parse(&text).map_err(|e| format!("{path}: {e}")))
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
    let result = load(path).and_then(|cfg| {
        // Warnings of the checks (e.g. a backend outside private networks) go to stderr.
        pumbo_prox::logging::init(&cfg.logging);
        Runtime::build(cfg, &(String::new(), String::new()))
    });
    match result {
        Ok(rt) => {
            let cfg = &rt.config;
            for l in &cfg.listener {
                let pp = if l.proxy_protocol {
                    ", PROXY protocol"
                } else {
                    ""
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
            match check_plugins(path) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("config error: {e}");
                    ExitCode::FAILURE
                }
            }
        }
        Err(e) => {
            eprintln!("config error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(path: &str) -> ExitCode {
    let cfg = match load(path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("config error: {e}");
            return ExitCode::FAILURE;
        }
    };
    pumbo_prox::logging::init(&cfg.logging);
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
        let proxy = match Proxy::new(cfg, Some(PathBuf::from(path))) {
            Ok(p) => p,
            Err(e) => {
                error!("cannot start: {e}");
                return ExitCode::FAILURE;
            }
        };
        // PumboBridge (before the plugin host: it provides pumbo:bridge).
        let bridge_cfg = proxy.runtime().config.bridge.clone();
        let mut natives = Vec::new();
        if bridge_cfg.enabled {
            match pumbo_prox::bridge::Bridge::new(&proxy, bridge_cfg) {
                Ok(b) => {
                    natives.push(b.native());
                    let _ = proxy.bridge.set(b);
                }
                Err(e) => {
                    error!("cannot start the bridge: {e}");
                    return ExitCode::FAILURE;
                }
            }
        }
        // Plugin host (E5), from the same file; skipped without a plugin directory.
        let text = std::fs::read_to_string(path).unwrap_or_default();
        match pumbo_prox::plugins::Plugins::start(&text, &proxy, natives).await {
            Ok(Some(p)) => {
                let _ = proxy.plugins.set(p);
            }
            Ok(None) => info!("no plugin directory, running without plugins"),
            Err(e) => {
                error!("cannot start plugins: {e}");
                return ExitCode::FAILURE;
            }
        }
        if let Some(b) = proxy.bridge.get()
            && let Err(e) = b.start().await
        {
            error!("cannot start: {e}");
            return ExitCode::FAILURE;
        }
        let listeners = match proxy.bind().await {
            Ok(l) => l,
            Err(e) => {
                error!("cannot start: {e}");
                return ExitCode::FAILURE;
            }
        };
        info!("PumboProx {} started", env!("CARGO_PKG_VERSION"));
        tokio::spawn(console(proxy.clone()));
        tokio::spawn(signals(proxy.clone()));
        let plugins = proxy.plugins.get().cloned();
        proxy.serve(listeners).await;
        if let Some(p) = plugins {
            let _ = tokio::time::timeout(Duration::from_secs(4), p.host.shutdown()).await;
        }
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
/// ends the console, not the proxy.
async fn console(proxy: std::sync::Arc<Proxy>) {
    let mut lines = tokio::io::BufReader::new(tokio::io::stdin()).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let line = match line.trim() {
            l if l.starts_with("plugin ") => format!("pumbo proxy {l}"),
            // `prox <sub>` as the console command it stands for; the help stays.
            l if l.starts_with("prox ") => {
                pumbo_prox::commands::prox_target(l).unwrap_or_else(|| l.to_string())
            }
            l if l == "bridge" || l.starts_with("bridge ") => format!("pumbo {l}"),
            l => l.to_string(),
        };
        // `/pumbo bridge` works without the plugin host too.
        if let Some(rest) = line
            .strip_prefix("pumbo bridge")
            .filter(|r| r.is_empty() || r.starts_with(' '))
        {
            let args: Vec<String> = rest.split_whitespace().map(str::to_string).collect();
            match proxy.bridge.get() {
                // Printed, not logged: the key must not land in log files.
                Some(b) => b
                    .admin(&args, true, true)
                    .iter()
                    .for_each(|l| println!("{}", pumbo_prox::bridge::plain(l))),
                None => warn!("the bridge is off (bridge.enabled in the proxy config)"),
            }
            continue;
        }
        match line.as_str() {
            "" => {}
            "reload" => match proxy.reload() {
                Ok(()) => info!("config reloaded"),
                Err(e) => warn!("reload failed, keeping the old config: {e}"),
            },
            "stop" | "end" => {
                info!("stopping");
                proxy.stop();
                return;
            }
            "version" => info!(
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
                        warn!("gate needs virtual.test-gate: true (tests only)");
                    }
                    Some(p) => pumbo_prox::world::enter_test_gate(&proxy, p.id),
                    None => warn!("{name} is not online"),
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
                            info!("{}", line.plain_text());
                        }
                    }
                    None => match proxy.plugins.get().and_then(|p| p.console(other)) {
                        Some(lines) => {
                            for line in lines {
                                info!("{}", line.plain_text());
                            }
                        }
                        None => warn!(
                            "unknown command \"{other}\" (prox, reload, stop, version, glist, find, send, alert, pumbo, plugin, bridge, gate)"
                        ),
                    },
                }
            }
        }
    }
}

async fn signals(proxy: std::sync::Arc<Proxy>) {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let (Ok(mut term), Ok(mut int)) = (
            signal(SignalKind::terminate()),
            signal(SignalKind::interrupt()),
        ) else {
            return;
        };
        tokio::select! {
            _ = term.recv() => {}
            _ = int.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
    info!("stopping (signal)");
    proxy.stop();
}
