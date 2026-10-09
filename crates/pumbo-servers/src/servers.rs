//! Server folders and `servers.yml`: names, ports, the files a new server
//! gets, the trash.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// What the servers are told about the proxy's network.
#[derive(Debug, Clone, Default)]
pub struct Network {
    /// Velocity modern forwarding secret; `None`: no forwarding.
    pub velocity_secret: Option<String>,
    /// PumboBridge: address the bridges connect to and the key.
    pub bridge: Option<(String, String)>,
}

/// One server in `servers.yml`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Entry {
    pub source: String,
    pub version: String,
    pub template: String,
    pub port: u16,
    #[serde(default)]
    pub autostart: bool,
    /// The last process, for taking it over after a proxy crash.
    #[serde(default)]
    pub pid: Option<u32>,
}

#[derive(Deserialize, Default)]
struct StateFile {
    #[serde(default)]
    servers: BTreeMap<String, Entry>,
}

/// `servers.yml`: state written by the proxy, not a config.
#[derive(Debug)]
pub struct ServerStore {
    path: PathBuf,
    pub entries: BTreeMap<String, Entry>,
}

impl ServerStore {
    /// Reads `<dir>/servers.yml`; a missing file is an empty list.
    pub fn load(dir: &Path) -> Result<Self, String> {
        let path = dir.join("servers.yml");
        let entries = match std::fs::read_to_string(&path) {
            Ok(text) => {
                let options = serde_saphyr::options! { strict_booleans: true, with_snippet: false };
                let file: StateFile = serde_saphyr::from_str_with_options(&text, options)
                    .map_err(|e| format!("{}: {e}", path.display()))?;
                for name in file.servers.keys() {
                    valid_name(name).map_err(|e| format!("{}: {e}", path.display()))?;
                }
                file.servers
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
            Err(e) => return Err(format!("{}: {e}", path.display())),
        };
        Ok(Self { path, entries })
    }

    /// Writes the file atomically (a temporary file, then a rename).
    pub fn save(&self) -> Result<(), String> {
        let tmp = self.path.with_extension("yml.tmp");
        std::fs::write(&tmp, render(&self.entries))
            .and_then(|()| std::fs::rename(&tmp, &self.path))
            .map_err(|e| format!("{}: {e}", self.path.display()))
    }
}

fn render(entries: &BTreeMap<String, Entry>) -> String {
    // JSON strings are valid YAML double-quoted strings.
    let q = |s: &str| serde_json::to_string(s).unwrap_or_default();
    let mut out = String::from(
        "# Servers created by PumboProx. The proxy writes this file; edit it only\n# while the proxy is stopped.\n",
    );
    if entries.is_empty() {
        out.push_str("servers: {}\n");
        return out;
    }
    out.push_str("servers:\n");
    for (name, e) in entries {
        out.push_str(&format!(
            "  {name}:\n    source: {}\n    version: {}\n    template: {}\n    port: {}\n    autostart: {}\n",
            q(&e.source),
            q(&e.version),
            q(&e.template),
            e.port,
            e.autostart
        ));
        if let Some(pid) = e.pid {
            out.push_str(&format!("    pid: {pid}\n"));
        }
    }
    out
}

/// Folder names under `dir` that are not servers.
const RESERVED: &[&str] = &["templates"];

/// `[a-z0-9-]{1,32}`, not a folder of the manager.
pub fn valid_name(name: &str) -> Result<(), String> {
    let ok = (1..=32).contains(&name.len())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !RESERVED.contains(&name);
    if ok {
        Ok(())
    } else {
        Err(format!(
            "invalid server name {name:?}: 1 to 32 characters a-z, 0-9 and -, not \"templates\""
        ))
    }
}

/// `25700-25799` → (25700, 25799).
pub fn parse_ports(range: &str) -> Result<(u16, u16), String> {
    let bad = || format!("ports {range:?} must look like 25700-25799");
    let (a, b) = range.split_once('-').ok_or_else(bad)?;
    let (a, b) = (
        a.trim().parse::<u16>().map_err(|_| bad())?,
        b.trim().parse::<u16>().map_err(|_| bad())?,
    );
    if a == 0 || a > b {
        return Err(bad());
    }
    Ok((a, b))
}

/// The lowest port of the range that no server has and nothing listens on.
pub fn free_port(range: (u16, u16), taken: impl Fn(u16) -> bool) -> Option<u16> {
    (range.0..=range.1)
        .find(|p| !taken(*p) && std::net::TcpListener::bind(("127.0.0.1", *p)).is_ok())
}

/// A world seed of up to 18 digits (Pumpkin reads it as an i64).
pub fn random_seed() -> Result<String, String> {
    let mut b = [0u8; 8];
    aws_lc_rs::rand::fill(&mut b).map_err(|_| "random generator failed".to_string())?;
    Ok((u64::from_le_bytes(b) % 1_000_000_000_000_000_000).to_string())
}

/// `pumpkin.toml` of a new server: loopback only, offline behind the proxy,
/// Velocity forwarding with the proxy's secret, no console TTY.
pub fn pumpkin_toml(name: &str, port: u16, secret: Option<&str>, seed: &str) -> String {
    let velocity = match secret {
        Some(s) => format!(
            "[networking.proxy]\nenabled = true\n\n[networking.proxy.velocity]\nenabled = true\nsecret = \"{s}\"\n"
        ),
        None => "[networking.proxy]\nenabled = false\n".to_string(),
    };
    format!(
        "# Written by PumboProx for the server \"{name}\". Pumpkin fills in the options missing here.\n\
         seed = \"{seed}\"\n\
         \n\
         [networking.java]\n\
         address = \"127.0.0.1:{port}\"\n\
         online_mode = false\n\
         encryption = false\n\
         motd = \"{name}\"\n\
         \n\
         [networking.bedrock]\n\
         enabled = false\n\
         \n\
         [networking.lan_broadcast]\n\
         enabled = false\n\
         \n\
         {velocity}\
         \n\
         [commands]\n\
         use_tty = false\n\
         \n\
         [plugins]\n\
         ask_permission_confirmation = false\n\
         \n\
         # Pumpkin's telemetry keeps Pumpkin's default. To turn it off:\n\
         # [telemetry]\n\
         # enabled = false\n"
    )
}

/// `plugins/data/pumbobridge/config.yml` of a new server.
pub fn bridge_config(addr: &str, key: &str) -> String {
    format!("# Written by PumboProx.\nproxy: {addr}\nkey: \"{key}\"\n")
}

/// Copies the files of a template folder into a new server's folder.
pub fn copy_dir(from: &Path, to: &Path) -> Result<(), String> {
    let err = |p: &Path, e: std::io::Error| format!("{}: {e}", p.display());
    std::fs::create_dir_all(to).map_err(|e| err(to, e))?;
    for entry in std::fs::read_dir(from).map_err(|e| err(from, e))? {
        let entry = entry.map_err(|e| err(from, e))?;
        let (src, dst) = (entry.path(), to.join(entry.file_name()));
        let kind = entry.file_type().map_err(|e| err(&src, e))?;
        if kind.is_dir() {
            copy_dir(&src, &dst)?;
        } else if kind.is_file() {
            std::fs::copy(&src, &dst).map_err(|e| err(&src, e))?;
        }
    }
    Ok(())
}

/// Moves `<dir>/<name>` to `<dir>/.trash/<name>-<seconds>`; nothing is deleted.
pub fn trash(dir: &Path, name: &str) -> Result<PathBuf, String> {
    let bin = dir.join(".trash");
    std::fs::create_dir_all(&bin).map_err(|e| format!("{}: {e}", bin.display()))?;
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let to = bin.join(format!("{name}-{secs}"));
    let from = dir.join(name);
    std::fs::rename(&from, &to).map_err(|e| format!("{}: {e}", from.display()))?;
    Ok(to)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::indexing_slicing)]
    use super::*;

    #[test]
    fn names() {
        assert!(valid_name("arena").is_ok());
        assert!(valid_name("bed-wars-2").is_ok());
        for bad in [
            "",
            "Arena",
            "a_b",
            "../x",
            "a.b",
            "templates",
            &"x".repeat(33),
        ] {
            assert!(valid_name(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn ports() {
        assert_eq!(parse_ports("25700-25799"), Ok((25700, 25799)));
        assert!(parse_ports("25799-25700").is_err());
        assert!(parse_ports("25700").is_err());
        let range = parse_ports("25750-25799").unwrap();
        let taken = |p: u16| p == range.0;
        let got = free_port(range, taken).unwrap();
        assert!(got > range.0, "{got}");
    }

    #[test]
    fn seeds_fit_in_18_digits() {
        for _ in 0..100 {
            let s = random_seed().unwrap();
            assert!(s.len() <= 18 && s.parse::<i64>().is_ok(), "{s}");
        }
        assert_ne!(random_seed().unwrap(), random_seed().unwrap());
    }

    #[test]
    fn pumpkin_toml_golden() {
        let got = pumpkin_toml("arena", 25700, Some("abc123"), "42");
        let want = r#"# Written by PumboProx for the server "arena". Pumpkin fills in the options missing here.
seed = "42"

[networking.java]
address = "127.0.0.1:25700"
online_mode = false
encryption = false
motd = "arena"

[networking.bedrock]
enabled = false

[networking.lan_broadcast]
enabled = false

[networking.proxy]
enabled = true

[networking.proxy.velocity]
enabled = true
secret = "abc123"

[commands]
use_tty = false

[plugins]
ask_permission_confirmation = false

# Pumpkin's telemetry keeps Pumpkin's default. To turn it off:
# [telemetry]
# enabled = false
"#;
        assert_eq!(got, want);
        let none = pumpkin_toml("arena", 25700, None, "42");
        assert!(none.contains("[networking.proxy]\nenabled = false\n"));
        assert!(!none.contains("velocity"));
    }

    #[test]
    fn state_file_round_trip() {
        let dir = std::env::temp_dir().join(format!("pumbo-servers-yml-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut store = ServerStore::load(&dir).unwrap();
        assert!(store.entries.is_empty());
        store.save().unwrap();
        assert!(ServerStore::load(&dir).unwrap().entries.is_empty());
        let entry = Entry {
            source: "pumpkin".into(),
            version: "0.2.0+26.3-26.51".into(),
            template: "default".into(),
            port: 25700,
            autostart: true,
            pid: Some(4242),
        };
        store.entries.insert("arena".into(), entry.clone());
        store.entries.insert(
            "lobby-2".into(),
            Entry {
                pid: None,
                autostart: false,
                ..entry.clone()
            },
        );
        store.save().unwrap();
        let back = ServerStore::load(&dir).unwrap();
        assert_eq!(back.entries, store.entries);
        assert!(!dir.join("servers.yml.tmp").exists());
        std::fs::write(
            dir.join("servers.yml"),
            "servers:\n  \"../x\": { source: pumpkin, version: a, template: b, port: 1 }\n",
        )
        .unwrap();
        assert!(ServerStore::load(&dir).is_err());
    }
}
