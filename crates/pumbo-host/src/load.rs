//! Plugin files in the plugin directory: manifests, duplicates, required
//! plugins and the permission provider.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use crate::{HostConfig, HostError, Manifest, PluginSlot, config, manifest};

/// A plugin file that did not load. The proxy starts without it,
/// unless it is required.
#[derive(Debug, Clone)]
pub struct LoadFailure {
    /// The file in the plugin directory.
    pub file: String,
    /// The manifest when it reads (also an invalid one).
    pub manifest: Option<Manifest>,
    pub reason: String,
    /// Another file has the same plugin id (D-STD-5: neither loads).
    pub duplicate: bool,
}

/// A plugin that loads: its manifest (and the text of it) and module.
type Loaded = (Manifest, String, std::path::PathBuf);

/// Plugins in the directory: any `*.wasm` with its manifest built in
/// (`pumbo_sdk::embed!`); the id comes from the manifest, not the file name
/// (D-STD-5). A `<id>.yml` next to it overrides that manifest (builds without
/// one need it, found by the module's file name then); a `.yml` without a
/// `.wasm` reserves its names and shows the plugin as missing. Two files
/// with one id both fail. A `<id>.toml` (the format before YAML) is
/// reported, not read. A plugin that does not load is returned as a
/// failure; only an unreadable directory is an error.
pub(crate) fn load_manifests(dir: &Path) -> Result<(Vec<Loaded>, Vec<LoadFailure>), HostError> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok((Vec::new(), Vec::new()));
        }
        Err(e) => return Err(HostError::Config(format!("{}: {e}", dir.display()))),
    };
    let mut wasms = std::collections::BTreeSet::new();
    let mut ymls = std::collections::BTreeSet::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let ext = path.extension().and_then(|e| e.to_str());
        if ext == Some("toml") && !path.with_extension("yml").exists() {
            tracing::warn!(
                "found {}, this version reads YAML manifests (built into the .wasm) - delete it (see the plugin's README)",
                path.display()
            );
        }
        if path.is_file() {
            match ext {
                Some("wasm") => wasms.insert(path),
                Some("yml") => ymls.insert(path),
                _ => false,
            };
        }
    }
    let name = |p: &Path| {
        p.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    };
    let stem = |p: &Path| {
        p.file_stem()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    };
    let mut failed = Vec::new();
    let mut by_id: BTreeMap<String, Vec<Loaded>> = BTreeMap::new();
    let mut add = |file: String, text: Result<String, String>, id: Option<String>, wasm| {
        let parsed = text
            .map_err(|e| (None, e))
            .and_then(|t| match Manifest::parse_partial(&t) {
                Ok(m) => Ok((m, t)),
                Err((m, e)) => Err((m, e.to_string())),
            });
        match parsed {
            Ok((m, _)) if id.as_ref().is_some_and(|id| *id != m.id) => failed.push(LoadFailure {
                reason: format!("{file}: manifest id {} does not match the file name", m.id),
                file,
                manifest: Some(m),
                duplicate: false,
            }),
            Ok((m, t)) => by_id.entry(m.id.clone()).or_default().push((m, t, wasm)),
            Err((manifest, e)) => failed.push(LoadFailure {
                reason: format!("{file}: {e}"),
                file,
                manifest,
                duplicate: false,
            }),
        }
    };
    for wasm in wasms {
        let bytes = std::fs::read(&wasm).unwrap_or_default();
        let built_in = manifest::embedded(&bytes, manifest::MANIFEST_SECTION).map(|b| {
            String::from_utf8(b.to_vec()).map_err(|_| "the built-in manifest is not UTF-8".into())
        });
        // The override is `<id>.yml` by the built-in id, else by the file name.
        let id = match &built_in {
            Some(Ok(t)) => match Manifest::parse_partial(t) {
                Ok(m) | Err((Some(m), _)) => Some(m.id),
                Err(_) => None,
            },
            _ => None,
        }
        .unwrap_or_else(|| stem(&wasm));
        let file = dir.join(format!("{id}.yml"));
        if ymls.remove(&file) {
            if built_in.is_some() {
                tracing::warn!(
                    "{}: the manifest file overrides the one built into {}",
                    file.display(),
                    wasm.display()
                );
            }
            let text = std::fs::read_to_string(&file).map_err(|e| e.to_string());
            add(name(&file), text, Some(id), wasm);
        } else {
            let text = built_in.unwrap_or_else(|| {
                Err(format!(
                    "no manifest built in (pumbo_sdk::embed!) and no {id}.yml next to it"
                ))
            });
            add(name(&wasm), text, None, wasm);
        }
    }
    for file in ymls {
        let id = stem(&file);
        let wasm = dir.join(format!("{id}.wasm"));
        // A module of that name was taken by the id built into it.
        let text = match wasm.exists() {
            true => Err(format!("{id}.wasm next to it is another plugin")),
            false => std::fs::read_to_string(&file).map_err(|e| e.to_string()),
        };
        add(name(&file), text, Some(id), wasm);
    }
    let mut out = Vec::new();
    for (id, mut found) in by_id {
        if found.len() == 1 {
            out.extend(found.pop());
            continue;
        }
        let files: Vec<String> = found.iter().map(|(_, _, w)| name(w)).collect();
        for (m, _, w) in found {
            failed.push(LoadFailure {
                file: name(&w),
                manifest: Some(m),
                reason: format!("duplicate plugin id {id} in {}", files.join(" and ")),
                duplicate: true,
            });
        }
    }
    Ok((out, failed))
}

/// Whether `plugins.required-plugins` or `required-gates` names the plugin.
pub(crate) fn required(cfg: &config::PluginsConfig, m: &Manifest) -> bool {
    cfg.required_plugins.contains(&m.id)
        || m.gate
            .as_ref()
            .is_some_and(|g| cfg.required_gates.contains(&g.name))
}

/// The start error of a required plugin that did not load (fail-closed: the
/// proxy does not run with logins closed for good).
pub(crate) fn required_error(id: &str, reason: &str) -> HostError {
    HostError::Plugin {
        id: id.to_string(),
        message: format!(
            "{reason} - it is required (plugins.required-gates / required-plugins in pumboprox.yml), so the proxy does not start"
        ),
    }
}

/// A failure that is (or, without a readable manifest, may be) a required
/// gate or plugin stops the start; the others are logged and skipped.
pub(crate) fn check_failures(
    cfg: &config::PluginsConfig,
    plugins: &BTreeMap<String, Arc<PluginSlot>>,
    failed: &[LoadFailure],
) -> Result<(), HostError> {
    let unmet: Vec<String> = cfg
        .required_plugins
        .iter()
        .filter(|id| !plugins.contains_key(*id))
        .map(|id| format!("plugin {id}"))
        .chain(
            cfg.required_gates
                .iter()
                .filter(|g| {
                    !plugins
                        .values()
                        .any(|s| s.manifest.gate.as_ref().is_some_and(|d| &d.name == *g))
                })
                .map(|g| format!("gate {g}")),
        )
        .collect();
    // Failures with a manifest first: they name themselves.
    let known = failed.iter().filter(|f| f.manifest.is_some());
    for f in known.chain(failed.iter().filter(|f| f.manifest.is_none())) {
        let hit = match &f.manifest {
            Some(m) => required(cfg, m),
            None => !unmet.is_empty(),
        };
        if hit {
            let reason = match &f.manifest {
                Some(_) => f.reason.clone(),
                None => format!("{} (it may be the missing {})", f.reason, unmet.join(", ")),
            };
            let id = f.manifest.as_ref().map_or(&f.file, |m| &m.id);
            return Err(required_error(id, &reason));
        }
    }
    for f in failed {
        tracing::error!(
            "plugin not loaded, the proxy starts without it: {}",
            f.reason
        );
    }
    Ok(())
}

/// The plugin `permissions.provider: auto` takes (the id of PumboPerms).
/// Not the owner of `pumbo:permissions`: any plugin named in
/// `permissions.provider` may provide it.
const DEFAULT_PERMISSION_PROVIDER: &str = "pumbo-perms";

/// The permission provider (`permissions.provider`): `auto` takes
/// pumbo-perms when it is installed, else the file; another plugin with
/// `permission-provider: true` that provides `pumbo:permissions` is used
/// only when its id is named there. `file` uses no plugin. A named plugin
/// that is not installed or provides no permissions stops the start
/// instead of leaving an old `permissions.yml` in charge, and so does `auto`
/// with only another provider installed. The services registry offers
/// `pumbo:permissions` only from the plugin chosen here.
pub(crate) fn permission_provider(
    cfg: &HostConfig,
    plugins: &BTreeMap<String, Arc<PluginSlot>>,
    failed: &[LoadFailure],
) -> Result<Option<String>, HostError> {
    let provides = |m: &Manifest| {
        m.permission_provider
            && m.provides
                .iter()
                .any(|p| p.service == pumbo_contracts::PERMISSIONS.name)
    };
    let able: Vec<&String> = plugins
        .values()
        .filter(|s| provides(&s.manifest))
        .map(|s| &s.id)
        .collect();
    let named = cfg.permissions.provider.as_str();
    // Starting without the provider would hand the network to the file and
    // take the backends out of proxy-rules, so two copies stop the start.
    let wanted = |id: &str| named == id || (named == "auto" && id == DEFAULT_PERMISSION_PROVIDER);
    let twins: Vec<&LoadFailure> = failed
        .iter()
        .filter(|f| {
            f.duplicate
                && f.manifest
                    .as_ref()
                    .is_some_and(|m| provides(m) && wanted(&m.id))
        })
        .collect();
    if let Some(m) = twins.first().and_then(|f| f.manifest.as_ref()) {
        let files: Vec<&str> = twins.iter().map(|f| f.file.as_str()).collect();
        return Err(HostError::Plugin {
            id: m.id.clone(),
            message: format!(
                "the permission provider is in two files ({}): delete the older one, then start the proxy again",
                files.join(" and ")
            ),
        });
    }
    let chosen = if named == "auto" {
        DEFAULT_PERMISSION_PROVIDER
    } else {
        named
    };
    // A file whose manifest does not read is known only by its name
    // (`pumbo-perms.wasm`, `PumboPerms.wasm`, `pumbo-perms.yml`).
    let squash = |s: &str| -> String {
        s.chars()
            .filter(char::is_ascii_alphanumeric)
            .map(|c| c.to_ascii_lowercase())
            .collect()
    };
    let is_chosen = |f: &LoadFailure| match &f.manifest {
        Some(m) => m.id == chosen,
        None => std::path::Path::new(&f.file)
            .file_stem()
            .and_then(|s| s.to_str())
            .is_some_and(|stem| squash(stem) == squash(chosen)),
    };
    if named != "file"
        && let Some(f) = failed.iter().find(|f| is_chosen(f))
    {
        return Err(HostError::Plugin {
            id: chosen.to_string(),
            message: format!(
                "the permission provider did not load ({}): fix or replace {}, or set permissions.provider: file to run on {} without it, then start the proxy again",
                f.reason,
                f.file,
                cfg.permissions.file.display()
            ),
        });
    }
    let provider = match named {
        "file" => None,
        "auto" => {
            let others: Vec<&str> = able
                .iter()
                .map(|s| s.as_str())
                .filter(|id| *id != DEFAULT_PERMISSION_PROVIDER)
                .collect();
            if able.len() == others.len() {
                if !others.is_empty() {
                    return Err(HostError::Config(format!(
                        "permissions.provider is auto, which picks only {DEFAULT_PERMISSION_PROVIDER}; {} also provides permissions: set permissions.provider: {} to use it, or permissions.provider: file to use {}",
                        others.join(", "),
                        others.first().copied().unwrap_or_default(),
                        cfg.permissions.file.display()
                    )));
                }
                None
            } else {
                for id in others {
                    tracing::warn!(
                        "{id} also provides permissions and is not used: permissions.provider is auto, which picks only {DEFAULT_PERMISSION_PROVIDER}; set permissions.provider: {id} to use it instead"
                    );
                }
                Some(DEFAULT_PERMISSION_PROVIDER.to_string())
            }
        }
        id if able.iter().any(|a| *a == id) => Some(id.to_string()),
        id if plugins.contains_key(id) => {
            return Err(HostError::Config(format!(
                "permissions.provider: {id} provides no permissions (its manifest needs permission-provider: true and pumbo:permissions under provides): name another plugin, or set permissions.provider: file"
            )));
        }
        id => {
            return Err(HostError::Config(format!(
                "permissions.provider: {id} is not installed (no plugin with this id in {}): put its .wasm back, or set permissions.provider: file",
                cfg.plugins.dir.display()
            )));
        }
    };
    // `services.providers` cannot pick another plugin behind its back.
    if let Some(other) = cfg
        .services
        .providers
        .get(pumbo_contracts::PERMISSIONS.name)
        .filter(|id| provider.as_deref() != Some(id.as_str()))
    {
        return Err(HostError::Config(format!(
            "services.providers chooses {other} for {}, but permissions.provider picks {}: the permission plugin is chosen in permissions.provider (set permissions.provider: {other}), remove it from services.providers",
            pumbo_contracts::PERMISSIONS.name,
            provider.as_deref().unwrap_or("file")
        )));
    }
    Ok(provider)
}

/// Start errors for names two plugins cannot share.
pub(crate) fn check_unique(plugins: &BTreeMap<String, Arc<PluginSlot>>) -> Result<(), HostError> {
    let mut gates = BTreeMap::new();
    let mut short = BTreeMap::new();
    let mut prefixes = BTreeMap::new();
    for s in plugins.values() {
        // `bans` and `pumbo-bans` would share the `pumbo.bans.` nodes and their defaults.
        for p in s.manifest.permission_prefixes() {
            if let Some(other) = prefixes.insert(p.clone(), s.id.clone()) {
                return Err(HostError::Config(format!(
                    "permission prefix \"{p}\" claimed by {other} and {}",
                    s.id
                )));
            }
        }
        if let Some(g) = &s.manifest.gate
            && let Some(other) = gates.insert(g.name.clone(), s.id.clone())
        {
            return Err(HostError::Config(format!(
                "gate \"{}\" declared by {other} and {}",
                g.name, s.id
            )));
        }
        if let Some(n) = &s.manifest.short_name
            && let Some(other) = short.insert(n.clone(), s.id.clone())
        {
            return Err(HostError::Config(format!(
                "short-name \"{n}\" declared by {other} and {}",
                s.id
            )));
        }
        if let Some(a) = &s.manifest.short_alias
            && let Some(other) = short.insert(format!("alias {a}"), s.id.clone())
        {
            return Err(HostError::Config(format!(
                "short-alias \"{a}\" declared by {other} and {}",
                s.id
            )));
        }
    }
    Ok(())
}
