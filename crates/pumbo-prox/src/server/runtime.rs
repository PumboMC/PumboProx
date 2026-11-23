use std::collections::HashSet;
use std::sync::Arc;

use pumbo_core::routing::BackendInfo;
use pumbo_protocol::ProtocolVersion;
use pumbo_text::Component;
use tokio::sync::Semaphore;
use tracing::{info, warn};
use uuid::Uuid;

use super::{Proxy, off_worker};
use crate::config::{Config, is_loopback_host, split_host_port};
use crate::modules::{self, Modules};

/// Everything that a reload replaces. Sessions in play follow the newest one
/// (`Play::follow_runtime`); a login finishes with the one it started with.
#[derive(Debug)]
pub struct Runtime {
    pub config: Config,
    pub modules: Modules,
    pub backends: Vec<BackendInfo>,
    pub motd: Component,
    /// `status.favicon` as a `data:` URL, read at start and reload.
    pub favicon: Option<String>,
    pub version_name: String,
    /// `commands.operators` parsed.
    pub operators: HashSet<Uuid>,
    /// `commands.sensitive` in lower case.
    pub sensitive: HashSet<String>,
    /// Servers of `managed-servers` added to `config.servers`.
    pub managed: Vec<String>,
    /// Whether a server takes each client version (`versions::way`).
    pub(crate) ways: crate::versions::Ways,
}

impl Runtime {
    pub fn build(config: Config) -> Result<Self, String> {
        let registry = modules::builtin().map_err(|e| e.to_string())?;
        let modules = modules::build(&registry, &config).map_err(|e| e.to_string())?;
        for (name, s) in &config.servers {
            let (host, _) = split_host_port(&s.address).unwrap_or(("", 0));
            if modules.forwarding.requires_local_backend() && !is_loopback_host(host) {
                return Err(format!(
                    "server {name}: forwarding \"{}\" is only allowed for backends on 127.0.0.1",
                    modules.forwarding.name()
                ));
            }
            if let Some(e) = &s.engine
                && let Err(unknown) = registry.engines.get(&e.name)
            {
                return Err(format!("server {name}: {unknown}"));
            }
        }
        let backends = config
            .servers
            .iter()
            .map(|(name, s)| BackendInfo {
                name: name.clone(),
                address: s.address.clone(),
                protocol: s.protocol.fixed().map(ProtocolVersion),
                engine: s.engine.clone(),
                online: true,
                players: 0,
            })
            .collect();
        let version_name = if config.status.version_name.is_empty() {
            format!("PumboProx {}", config.versions.clients.names("-"))
        } else {
            config.status.version_name.clone()
        };
        let operators = config
            .commands
            .operators
            .iter()
            .filter_map(|s| Uuid::parse_str(s).ok())
            .collect();
        let sensitive = config
            .commands
            .sensitive
            .iter()
            .map(|s| s.to_ascii_lowercase())
            .collect();
        Ok(Self {
            operators,
            sensitive,
            motd: crate::status::parse_text(&config.status.motd),
            favicon: crate::status::load_favicon(&config.status.favicon),
            config,
            modules,
            backends,
            version_name,
            managed: Vec::new(),
            ways: crate::versions::Ways::default(),
        })
    }
}

/// A semaphore of `from` permits to `to`. Permits held now stay valid; a
/// smaller limit takes free permits at once and the rest as they come back.
fn resize(sem: &Arc<Semaphore>, from: usize, to: usize) {
    if to >= from {
        sem.add_permits(to - from);
        return;
    }
    let rest = (from - to).saturating_sub(sem.forget_permits(from - to));
    if rest == 0 {
        return;
    }
    let (sem, rest) = (sem.clone(), u32::try_from(rest).unwrap_or(u32::MAX));
    if let Ok(rt) = tokio::runtime::Handle::try_current() {
        rt.spawn(async move {
            if let Ok(p) = sem.acquire_many(rest).await {
                p.forget();
            }
        });
    }
}

/// Whether two parsed settings differ (the config types have no `PartialEq`).
fn differs(a: &impl std::fmt::Debug, b: &impl std::fmt::Debug) -> bool {
    format!("{a:?}") != format!("{b:?}")
}

/// Settings of the proxy read once at start: a reload that changes them
/// names them. Everything else applies with the reload.
fn restart_needed(old: &Config, new: &Config) -> Vec<String> {
    let (o, n) = (&old.logging, &new.logging);
    [
        ("listener", differs(&old.listener, &new.listener)),
        ("logging.format", o.format != n.format),
        ("logging.log-ips", o.log_ips != n.log_ips),
        ("logging.metrics-bind", o.metrics_bind != n.metrics_bind),
        (
            "logging.metrics-allow-remote",
            o.metrics_allow_remote != n.metrics_allow_remote,
        ),
        (
            "logging.metrics-token-file",
            o.metrics_token_file != n.metrics_token_file,
        ),
        ("bridge", differs(&old.bridge, &new.bridge)),
        (
            "managed-servers",
            differs(&old.managed_servers, &new.managed_servers),
        ),
    ]
    .into_iter()
    .filter(|(_, changed)| *changed)
    .map(|(name, _)| name.to_string())
    .collect()
}

/// The same for the host sections, which the plugin host reads at start;
/// only `plugins.required-gates` applies with a reload.
fn host_restart_needed(old: &pumbo_host::HostConfig, new: &pumbo_host::HostConfig) -> Vec<String> {
    let plugins = |c: &pumbo_host::HostConfig| {
        let mut p = c.plugins.clone();
        p.required_gates.clear();
        p
    };
    [
        ("plugins", differs(&plugins(old), &plugins(new))),
        ("services", differs(&old.services, &new.services)),
        ("permissions", differs(&old.permissions, &new.permissions)),
        (
            "placeholders",
            differs(&old.placeholders, &new.placeholders),
        ),
        (
            "server-group",
            differs(&old.server_groups, &new.server_groups),
        ),
        ("style", differs(&old.style, &new.style)),
    ]
    .into_iter()
    .filter(|(_, changed)| *changed)
    .map(|(name, _)| name.to_string())
    .collect()
}

impl Proxy {
    /// Re-reads the config file. The whole file, the host sections too, is
    /// read and checked before anything changes; any error keeps the old
    /// config. Players keep their sessions and see the new server list and
    /// commands at once; `logging.level`, `logging.filters` and `plugins.required-gates` apply
    /// at once too. Returns the changed settings that wait for a restart.
    pub fn reload(&self) -> Result<Vec<String>, String> {
        off_worker(|| {
            let _one = self.config_lock();
            self.reload_locked()
        })
    }

    /// Holds off other reloads, refreshes and edits of the config file.
    pub(crate) fn config_lock(&self) -> std::sync::MutexGuard<'_, ()> {
        self.config_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// [`Proxy::reload`] for a caller that holds [`Proxy::config_lock`].
    pub(crate) fn reload_locked(&self) -> Result<Vec<String>, String> {
        let path = self
            .config_path
            .as_ref()
            .ok_or("no config file to reload")?;
        let loaded = crate::config::load(path)?;
        let config = loaded.config;
        let at = |e: &dyn std::fmt::Display| format!("{}: {e}", path.display());
        let unresolved = config.check_backend_names().map_err(|e| at(&e))?;
        let host = loaded.host.map_err(|e| at(&e))?;
        let old = self.runtime();
        let mut restart = restart_needed(&old.config, &config);
        let logging = config.logging.clone();
        let relog = (&logging.level, &logging.filters)
            != (&old.config.logging.level, &old.config.logging.filters);
        let runtime = self.prepare(config)?;
        if let Some(p) = self.plugins.get() {
            restart.extend(host_restart_needed(p.host.config(), &host));
            // Last check: it applies the new permissions only when they load.
            p.host.reload_permissions()?;
            p.host.set_required_gates(host.plugins.required_gates);
        }
        let unreachable = crate::versions::unreachable(&runtime);
        self.commit(runtime);
        // The software under an address may have changed: the next brand counts.
        self.detect.rearm();
        for w in loaded
            .unknown
            .iter()
            .chain(&loaded.warnings)
            .chain(&unresolved)
            .chain(&unreachable)
        {
            warn!("{w}");
        }
        if relog && let Err(e) = crate::logging::set_filter(&logging) {
            warn!("{e}");
        }
        Ok(restart)
    }

    /// The config file the proxy runs with (`/prox reload`, `/prox route`).
    pub fn config_path(&self) -> Option<&std::path::Path> {
        self.config_path.as_deref()
    }

    /// The server list again after a server of `managed-servers` was
    /// created or deleted; the rest of the config stays.
    pub fn refresh(&self) -> Result<(), String> {
        off_worker(|| {
            let _one = self.config_lock();
            let old = self.runtime();
            let mut config = old.config.clone();
            for name in &old.managed {
                config.servers.remove(name);
            }
            self.replace(config)
        })
    }

    /// A new runtime from `config` plus the servers of `managed-servers`
    /// (a server in the config file wins over one of the same name in any
    /// letter case).
    fn replace(&self, config: Config) -> Result<(), String> {
        let runtime = self.prepare(config)?;
        // A server of the proxy can be the one that leaves no listed version
        // a way: said when it happens, not at every later change.
        if let Some(w) = crate::versions::unreachable(&runtime)
            && crate::versions::unreachable(&self.runtime()).is_none()
        {
            warn!("{w}");
        }
        self.commit(runtime);
        Ok(())
    }

    /// The runtime of [`Proxy::replace`], built and checked; nothing changes yet.
    fn prepare(&self, mut config: Config) -> Result<Runtime, String> {
        let mut managed = Vec::new();
        if let Some(m) = self.servers.get() {
            for (name, s) in crate::servers::backends(self, m) {
                if let Some(own) = config
                    .servers
                    .keys()
                    .find(|k| k.eq_ignore_ascii_case(&name))
                {
                    if *own != name {
                        warn!("server {name} of managed-servers is left out: the config has {own}");
                    }
                    continue;
                }
                config.servers.insert(name.clone(), s);
                managed.push(name);
            }
        }
        // A server the proxy has not created yet is skipped until it exists.
        if config.managed_servers.enabled
            && let Err(e) = config.check_server_names()
        {
            warn!("{e}; players skip it until it exists");
        }
        let mut runtime = Runtime::build(config)?;
        runtime.managed = managed;
        runtime.backends = self.detect.backends(&runtime);
        Ok(runtime)
    }

    fn commit(&self, runtime: Runtime) {
        // The operator code is for a fresh network only.
        if !runtime.backends.is_empty() && self.claim.lock().is_ok_and(|mut c| c.take().is_some()) {
            info!("The operator code expired: the network has a server now.");
        }
        let (old, new) = (&self.runtime().config.limits, &runtime.config.limits);
        resize(
            &self.pending,
            old.max_pending_logins,
            new.max_pending_logins,
        );
        resize(
            &self.auth_slots,
            old.max_concurrent_has_joined,
            new.max_concurrent_has_joined,
        );
        self.runtime.send_replace(Arc::new(runtime));
        if let Some(p) = self.plugins.get() {
            p.servers_changed(self);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::time::Duration;

    use super::*;

    /// A refresh that waits for the config lock (held by a reload that
    /// resolves server names) leaves the other tasks of its worker running.
    #[test]
    fn waiting_for_the_config_lock_keeps_the_worker_running() {
        let cfg = Config::parse(
            "listener:\n  - bind: \"127.0.0.1:0\"\nlogin:\n  online-mode: false\nforwarding:\n  mode: none\n",
        )
        .unwrap();
        let proxy = Proxy::new(cfg, None).unwrap();
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let held = proxy.config_lock();
        let p = proxy.clone();
        let refresh = rt.spawn(async move { p.refresh() });
        std::thread::sleep(Duration::from_millis(200));
        let (tx, rx) = std::sync::mpsc::channel();
        rt.spawn(async move {
            let _ = tx.send(());
        });
        let ran = rx.recv_timeout(Duration::from_secs(5)).is_ok();
        drop(held);
        assert!(ran, "the only worker is stuck waiting for the lock");
        rt.block_on(refresh).unwrap().unwrap();
    }

    /// A new list of the proxy's own servers that no listed version reaches
    /// warns when that happens, not at every later change of the list.
    #[test]
    fn a_server_list_no_listed_version_reaches_warns_once() {
        #[derive(Clone, Default)]
        struct Log(Arc<Mutex<Vec<u8>>>);
        impl std::io::Write for Log {
            fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(b);
                Ok(b.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let log = Log::default();
        let out = log.clone();
        let sub = tracing_subscriber::fmt()
            .with_ansi(false)
            .with_writer(move || out.clone())
            .finish();
        let _logs = tracing::subscriber::set_default(sub);
        let cfg = |servers: &str| {
            Config::parse(&format!(
                "forwarding:\n  mode: none\nversions:\n  clients: [\"26.3\"]\nservers: {servers}\n"
            ))
            .unwrap()
        };
        let proxy = Proxy::new(cfg("{}"), None).unwrap();
        let old = "{ old: { address: \"127.0.0.1:2\", protocol: 776 } }";
        proxy.replace(cfg(old)).unwrap();
        proxy.replace(cfg(old)).unwrap();
        let text = String::from_utf8(log.0.lock().unwrap().clone()).unwrap();
        let warned = text.matches("no listed version reaches any server");
        assert_eq!(warned.count(), 1, "{text}");
    }
}
