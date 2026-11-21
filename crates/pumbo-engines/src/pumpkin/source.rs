//! Pumpkin as server software the proxy downloads and runs: its GitHub
//! releases, its file for this machine and the files of a new server.

use std::path::Path;

use pumbo_servers::versions::{
    Artifact, BridgeArtifact, Fut, GhRelease, Http, Release, Setup, Source, checksum_of, valid_tag,
};
use pumbo_servers::{Launch, Template, servers};

/// Pumpkin from its GitHub releases. Only this repository.
pub const PUMPKIN_API: &str = "https://api.github.com/repos/Pumpkin-MC/Pumpkin";

#[derive(Debug, Clone)]
pub struct Pumpkin {
    api: String,
}

impl Default for Pumpkin {
    fn default() -> Self {
        Self::with_api(PUMPKIN_API)
    }
}

impl Pumpkin {
    /// Another API base, for tests with a local server.
    pub fn with_api(api: &str) -> Self {
        Self {
            api: api.trim_end_matches('/').to_string(),
        }
    }
}

/// The Pumpkin file for a machine (names of the release assets).
pub fn pumpkin_asset(arch: &str, os: &str, musl: bool) -> Option<&'static str> {
    Some(match (arch, os, musl) {
        ("x86_64", "linux", false) => "pumpkin-X64-Linux",
        ("x86_64", "linux", true) => "pumpkin-X64-Linux-musl",
        ("aarch64", "linux", false) => "pumpkin-ARM64-Linux",
        ("aarch64", "linux", true) => "pumpkin-ARM64-Linux-musl",
        ("aarch64", "macos", _) => "pumpkin-ARM64-macOS",
        ("x86_64", "macos", _) => "pumpkin-X64-macOS",
        ("x86_64", "windows", _) => "pumpkin-X64-Windows.exe",
        ("aarch64", "windows", _) => "pumpkin-ARM64-Windows.exe",
        _ => return None,
    })
}

impl Source for Pumpkin {
    fn name(&self) -> &'static str {
        "pumpkin"
    }

    fn releases<'a>(&'a self, http: &'a Http) -> Fut<'a, Vec<Release>> {
        Box::pin(async move {
            let body = http
                .get(&format!("{}/releases?per_page=30", self.api))
                .await?;
            let list: Vec<GhRelease> =
                serde_json::from_slice(&body).map_err(|e| format!("release list: {e}"))?;
            Ok(list
                .into_iter()
                .filter(|r| !r.draft && valid_tag(&r.tag_name))
                .map(|r| Release {
                    official: !r.prerelease,
                    files: r
                        .assets
                        .into_iter()
                        .map(|a| (a.name, a.browser_download_url))
                        .collect(),
                    tag: r.tag_name,
                })
                .collect())
        })
    }

    fn artifact<'a>(&'a self, http: &'a Http, release: &'a Release) -> Fut<'a, Artifact> {
        Box::pin(async move {
            let os = std::env::consts::OS;
            let arch = std::env::consts::ARCH;
            let want = pumpkin_asset(arch, os, cfg!(target_env = "musl"))
                .ok_or_else(|| format!("Pumpkin has no build for {arch} {os}"))?;
            let file = |name: &str| {
                release
                    .files
                    .iter()
                    .find(|(n, _)| n == name)
                    .map(|(_, u)| u.clone())
            };
            let url = file(want).ok_or_else(|| format!("release {} has no {want}", release.tag))?;
            let sha256 = match file("checksums.sha256") {
                Some(sums) => {
                    let text = String::from_utf8_lossy(&http.get(&sums).await?).to_string();
                    Some(
                        checksum_of(&text, want)
                            .ok_or_else(|| format!("checksums.sha256 lists no {want}"))?,
                    )
                }
                None => None,
            };
            Ok(Artifact { url, sha256 })
        })
    }

    fn file_name(&self) -> &'static str {
        if cfg!(windows) {
            "pumpkin.exe"
        } else {
            "pumpkin"
        }
    }

    /// `0.2.0+26.3-26.51` runs 26.3.
    fn minecraft(&self, tag: &str) -> Option<String> {
        let mc = tag.split_once('+')?.1;
        let mc = mc.split('-').next().unwrap_or(mc);
        (!mc.is_empty()).then(|| mc.to_string())
    }

    fn prepare(&self, dir: &Path, setup: &Setup<'_>) -> Result<(), String> {
        let toml = pumpkin_toml(
            setup.name,
            setup.port,
            setup.network.forwarding_secret.as_deref(),
            &servers::random_seed()?,
        );
        servers::write_secret(&dir.join("pumpkin.toml"), &toml)?;
        if let Some((addr, key)) = &setup.network.bridge {
            let bridge = dir.join("plugins/data/pumbobridge");
            std::fs::create_dir_all(&bridge).map_err(|e| format!("{}: {e}", bridge.display()))?;
            servers::write_secret(
                &bridge.join("config.yml"),
                &servers::bridge_config(addr, key),
            )?;
        }
        Ok(())
    }

    /// Pumpkin runs without arguments in its folder.
    fn launch(&self, file: &Path, _: &Template) -> Result<Launch, String> {
        Ok(Launch {
            program: file.to_path_buf(),
            args: Vec::new(),
        })
    }

    /// The plugin for the Minecraft release of `tag`.
    fn bridge_artifact(&self, tag: &str) -> Result<Option<BridgeArtifact>, String> {
        let mc = self
            .minecraft(tag)
            .ok_or("the Minecraft release of its Pumpkin is unknown")?;
        Ok(Some(BridgeArtifact {
            asset_prefix: format!("PumboBridge-Pumpkin-{mc}-"),
            asset_suffix: ".wasm",
            file_name: format!("PumboBridge-{mc}.wasm"),
            minecraft: mc,
        }))
    }
}

/// `pumpkin.toml` of a new server: loopback only, offline behind the proxy,
/// Modern forwarding with the proxy's secret, no console TTY.
pub fn pumpkin_toml(name: &str, port: u16, secret: Option<&str>, seed: &str) -> String {
    let forwarding = match secret {
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
         {forwarding}\
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_files() {
        assert_eq!(
            pumpkin_asset("x86_64", "linux", false),
            Some("pumpkin-X64-Linux")
        );
        assert_eq!(
            pumpkin_asset("aarch64", "linux", true),
            Some("pumpkin-ARM64-Linux-musl")
        );
        assert_eq!(
            pumpkin_asset("aarch64", "macos", false),
            Some("pumpkin-ARM64-macOS")
        );
        assert_eq!(
            pumpkin_asset("aarch64", "windows", false),
            Some("pumpkin-ARM64-Windows.exe")
        );
        assert_eq!(pumpkin_asset("riscv64", "linux", false), None);
    }

    #[test]
    fn tags() {
        let p = Pumpkin::default();
        assert_eq!(p.minecraft("0.2.0+26.3-26.51").as_deref(), Some("26.3"));
        assert_eq!(p.minecraft("0.1.0-dev+26.2-26.45").as_deref(), Some("26.2"));
        assert_eq!(p.minecraft("canary"), None);
    }

    #[test]
    fn pumpkin_names_its_bridge_plugin() {
        let p = Pumpkin::default();
        let want = BridgeArtifact {
            minecraft: "26.3".into(),
            asset_prefix: "PumboBridge-Pumpkin-26.3-".into(),
            asset_suffix: ".wasm",
            file_name: "PumboBridge-26.3.wasm".into(),
        };
        assert_eq!(p.bridge_artifact("0.2.0+26.3-26.51"), Ok(Some(want)));
        assert_eq!(
            p.bridge_artifact("canary"),
            Err("the Minecraft release of its Pumpkin is unknown".into())
        );
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
}
