//! Servers run by the proxy: server software downloads ([`versions`]),
//! server folders and `servers.yml` ([`servers`]), processes
//! ([`supervisor`], [`runtime`]). Nothing here knows the proxy, an HTTP API
//! or WIT: adapters call [`Manager`].

pub mod runtime;
pub mod servers;
pub mod supervisor;
pub mod versions;

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

use serde::Deserialize;

pub use runtime::{Launch, ProcessRunner, Runner};
pub use servers::{Entry, Network};
pub use supervisor::{ServerInfo, State};
pub use versions::{Progress, Pumpkin, Release, Source, VersionStore};

/// `managed-servers` in `pumboprox.yml`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case", default)]
pub struct Config {
    pub enabled: bool,
    /// Server folders, `servers.yml`, `templates/`, `.trash/`.
    pub dir: PathBuf,
    /// Downloaded software, `<source>/<tag>/<file>`.
    pub versions_dir: PathBuf,
    /// Ports of new servers, `first-last`.
    pub ports: String,
    /// `/prox download` from the game; the console can always.
    pub download_from_game: bool,
    /// Accept releases without a SHA256 checksum (development builds).
    pub allow_unverified: bool,
    /// After `stop` on the console, then a kill.
    pub stop_timeout_secs: u64,
    pub templates: BTreeMap<String, Template>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: false,
            dir: "servers".into(),
            versions_dir: "versions".into(),
            ports: "25700-25799".into(),
            download_from_game: false,
            allow_unverified: false,
            stop_timeout_secs: 30,
            templates: BTreeMap::new(),
        }
    }
}

impl Config {
    pub fn validate(&self) -> Result<(), String> {
        servers::parse_ports(&self.ports).map(|_| ())
    }

    /// The templates; `default` when none are configured.
    pub fn templates(&self) -> BTreeMap<String, Template> {
        if self.templates.is_empty() {
            BTreeMap::from([("default".to_string(), Template::default())])
        } else {
            self.templates.clone()
        }
    }
}

/// How new servers are made. Files in `<dir>/templates/<name>/` (plugins,
/// configs) are copied into each new server.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case", default)]
pub struct Template {
    /// Server software: `pumpkin`.
    pub source: String,
    /// `latest` (the newest downloaded release) or a tag.
    pub version: String,
    /// Start with the proxy, and right after `new`.
    pub autostart: bool,
    /// Restarts after crashes within 10 minutes, then the server stays down.
    pub restart_on_crash: u32,
    /// Options of the server software, read by its [`Source`].
    #[serde(flatten)]
    pub options: BTreeMap<String, serde_json::Value>,
}

impl Default for Template {
    fn default() -> Self {
        Self {
            source: "pumpkin".into(),
            version: "latest".into(),
            autostart: true,
            restart_on_crash: 3,
            options: BTreeMap::new(),
        }
    }
}

/// Everything about the servers of the proxy, for commands (and later the
/// plugin API and an HTTP API).
pub struct Manager {
    pub config: Config,
    pub versions: VersionStore,
    network: Network,
    ports: (u16, u16),
    store: Mutex<servers::ServerStore>,
    runner: Arc<dyn Runner>,
    slots: Mutex<HashMap<String, supervisor::Slot>>,
    closing: std::sync::atomic::AtomicBool,
    on_change: OnceLock<Box<dyn Fn() + Send + Sync>>,
}

impl std::fmt::Debug for Manager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Manager")
            .field("dir", &self.config.dir)
            .finish_non_exhaustive()
    }
}

impl Manager {
    /// Reads `servers.yml`; starts nothing (see [`Manager::boot`]).
    pub fn new(
        config: Config,
        network: Network,
        sources: Vec<Arc<dyn Source>>,
        runner: Arc<dyn Runner>,
    ) -> Result<Arc<Self>, String> {
        let ports = servers::parse_ports(&config.ports)?;
        for name in config.templates().keys() {
            let plugins = config.dir.join("templates").join(name).join("plugins");
            std::fs::create_dir_all(&plugins).map_err(|e| format!("{}: {e}", plugins.display()))?;
        }
        let store = servers::ServerStore::load(&config.dir)?;
        let versions = VersionStore::new(
            config.versions_dir.clone(),
            sources,
            config.allow_unverified,
        )?;
        Ok(Arc::new(Self {
            versions,
            network,
            ports,
            store: Mutex::new(store),
            runner,
            slots: Mutex::new(HashMap::new()),
            closing: std::sync::atomic::AtomicBool::new(false),
            on_change: OnceLock::new(),
            config,
        }))
    }

    /// Called after a server was created or deleted (the proxy adds it to
    /// or removes it from its server list).
    pub fn on_change(&self, f: impl Fn() + Send + Sync + 'static) {
        let _ = self.on_change.set(Box::new(f));
    }

    fn changed(&self) {
        if let Some(f) = self.on_change.get() {
            f();
        }
    }

    pub fn entries(&self) -> BTreeMap<String, Entry> {
        self.store
            .lock()
            .map(|s| s.entries.clone())
            .unwrap_or_default()
    }

    pub fn entry(&self, name: &str) -> Option<Entry> {
        self.store.lock().ok()?.entries.get(name).cloned()
    }

    fn edit<T>(&self, f: impl FnOnce(&mut servers::ServerStore) -> T) -> Result<T, String> {
        let mut store = self.store.lock().map_err(|_| "servers.yml lock")?;
        let out = f(&mut store);
        store.save()?;
        Ok(out)
    }

    pub fn template_names(&self) -> Vec<String> {
        self.config.templates().into_keys().collect()
    }

    /// Minecraft release of a server's software (`26.3`), for its protocol.
    pub fn minecraft(&self, entry: &Entry) -> Option<String> {
        self.versions
            .source(&entry.source)
            .ok()?
            .minecraft(&entry.version)
    }

    /// `new <name> [#n|tag|latest] [template]`. `taken` names servers the
    /// proxy already has from its config.
    pub async fn create(
        self: &Arc<Self>,
        name: &str,
        version: Option<&str>,
        template: Option<&str>,
        taken: &[String],
    ) -> Result<Entry, String> {
        servers::valid_name(name)?;
        if taken.iter().any(|t| t.eq_ignore_ascii_case(name)) || self.entry(name).is_some() {
            return Err(format!("a server named {name} exists"));
        }
        let template_name = template.unwrap_or("default");
        let tpl = self
            .config
            .templates()
            .remove(template_name)
            .ok_or_else(|| format!("no template {template_name}"))?;
        let source = self.versions.source(&tpl.source)?.clone();
        let src = source.name();
        let tag = match version.unwrap_or(&tpl.version) {
            "latest" => self.versions.latest_installed(src).ok_or_else(|| {
                format!("no {src} release downloaded yet, see /prox download {src} list")
            })?,
            w if w.starts_with('#') => self.versions.resolve(src, w).await?.tag,
            tag => tag.to_string(),
        };
        if self.versions.file(src, &tag).is_none() {
            return Err(format!(
                "{src} {tag} is not downloaded, use /prox download {src} {tag}"
            ));
        }
        let dir = self.config.dir.join(name);
        if dir.exists() {
            return Err(format!("{} exists already", dir.display()));
        }
        let entries = self.entries();
        let port = servers::free_port(self.ports, |p| entries.values().any(|e| e.port == p))
            .ok_or_else(|| format!("no free port in {}", self.config.ports))?;
        let from = self.config.dir.join("templates").join(template_name);
        if from.is_dir() {
            servers::copy_dir(&from, &dir)?;
        } else {
            std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        source.prepare(
            &dir,
            &versions::Setup {
                name,
                port,
                network: &self.network,
                template: &tpl,
            },
        )?;
        let entry = Entry {
            source: src.to_string(),
            version: tag,
            template: template_name.to_string(),
            port,
            autostart: tpl.autostart,
            pid: None,
        };
        self.edit(|s| s.entries.insert(name.to_string(), entry.clone()))?;
        tracing::info!(
            "created server {name} ({src} {}, port {port})",
            entry.version
        );
        self.changed();
        if tpl.autostart {
            self.start(name)?;
        }
        Ok(entry)
    }

    /// Stops the server and moves its folder to `<dir>/.trash/`.
    pub async fn delete(&self, name: &str) -> Result<PathBuf, String> {
        if self.entry(name).is_none() {
            return Err(format!("no server named {name}"));
        }
        self.stop(name).await?;
        if let Ok(mut slots) = self.slots.lock() {
            slots.remove(name);
        }
        self.edit(|s| s.entries.remove(name))?;
        self.changed();
        let to = servers::trash(&self.config.dir, name)?;
        tracing::info!("deleted server {name}, its folder is now {}", to.display());
        Ok(to)
    }
}
