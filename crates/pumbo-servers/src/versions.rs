//! Server software: where it comes from ([`Source`], one per software),
//! release lists, downloads checked with SHA256 and the local copies
//! ([`VersionStore`]).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures::future::BoxFuture;
use serde::Deserialize;

use crate::Template;
use crate::runtime::Launch;
use crate::servers::{self, Network};

pub type Fut<'a, T> = BoxFuture<'a, Result<T, String>>;

/// Release lists are asked for at most this often (GitHub: 60 an hour).
const LIST_TTL: Duration = Duration::from_secs(600);
/// Largest release list or checksum file read.
const MAX_META_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub tag: String,
    /// A release, not a development build (canary, nightly).
    pub official: bool,
    /// File name → download URL, when the list has them.
    pub files: Vec<(String, String)>,
}

/// The file of a release for this machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Artifact {
    pub url: String,
    /// Lower-case hex; `None`: the release publishes no checksum.
    pub sha256: Option<String>,
}

/// What a download reports while it runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Progress {
    /// The release is known, its file is about to be fetched.
    Started { tag: String },
    /// Bytes written so far and the size, when the server sends it; at most
    /// every 100 ms and once at the end.
    Bytes { done: u64, total: Option<u64> },
    /// The connection dropped: attempt `attempt` of `of` starts from zero.
    Retry { attempt: u32, of: u32 },
    /// The release has no checksum and `allow-unverified` lets it through.
    Unverified,
}

/// Receives [`Progress`] of a download.
pub type OnProgress<'a> = &'a (dyn Fn(Progress) + Send + Sync);

/// How often [`Progress::Bytes`] is reported at most.
const PROGRESS_EVERY: Duration = Duration::from_millis(100);

/// Attempts of a download.
const ATTEMPTS: u32 = 3;

/// What a new server is told about the network.
#[derive(Debug)]
pub struct Setup<'a> {
    pub name: &'a str,
    pub port: u16,
    pub network: &'a Network,
    pub template: &'a Template,
}

/// One kind of server software (`pumpkin`, later `paper`): its releases,
/// its files and how a server of it is set up and started.
pub trait Source: Send + Sync {
    /// The name in commands and in `versions/<name>/`.
    fn name(&self) -> &'static str;
    /// Releases, newest first.
    fn releases<'a>(&'a self, http: &'a Http) -> Fut<'a, Vec<Release>>;
    /// The file of `release` for this machine.
    fn artifact<'a>(&'a self, http: &'a Http, release: &'a Release) -> Fut<'a, Artifact>;
    /// Name of the downloaded file in `versions/<source>/<tag>/`.
    fn file_name(&self) -> &'static str;
    /// The Minecraft release a version runs (`26.3`), for the proxy's protocol.
    fn minecraft(&self, tag: &str) -> Option<String>;
    /// Writes the config files of a new server into `dir`.
    fn prepare(&self, dir: &Path, setup: &Setup<'_>) -> Result<(), String>;
    /// How to start a server whose software is `file`.
    fn launch(&self, file: &Path, template: &Template) -> Result<Launch, String>;
}

/// HTTP for release lists and downloads. The User-Agent names only the
/// proxy and its version.
#[derive(Debug, Clone)]
pub struct Http {
    client: reqwest::Client,
}

impl Http {
    pub fn new() -> Result<Self, String> {
        let client = reqwest::Client::builder()
            .user_agent(concat!("PumboProx/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(10))
            .read_timeout(Duration::from_secs(60))
            .build()
            .map_err(|e| format!("HTTP client: {e}"))?;
        Ok(Self { client })
    }

    /// A small document (a list, a checksum file).
    pub async fn get(&self, url: &str) -> Result<Vec<u8>, String> {
        let mut r = self.send(url).await?;
        let mut body = Vec::new();
        while let Some(chunk) = r
            .chunk()
            .await
            .map_err(|e| format!("{url}: {}", causes(&e)))?
        {
            if body.len() + chunk.len() > MAX_META_BYTES {
                return Err(format!("{url}: answer larger than {MAX_META_BYTES} bytes"));
            }
            body.extend_from_slice(&chunk);
        }
        Ok(body)
    }

    /// Streams `url` into `to`; returns the SHA256 of what was written. A
    /// dropped connection is retried twice, from the start.
    pub async fn download(
        &self,
        url: &str,
        to: &Path,
        on: OnProgress<'_>,
    ) -> Result<String, String> {
        let mut attempt = 1;
        loop {
            match self.download_once(url, to, on).await {
                Err(e) if attempt < ATTEMPTS => {
                    tracing::warn!(
                        "download failed (attempt {attempt} of {ATTEMPTS}), retrying: {e}"
                    );
                    attempt += 1;
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    on(Progress::Retry {
                        attempt,
                        of: ATTEMPTS,
                    });
                }
                done => return done,
            }
        }
    }

    async fn download_once(
        &self,
        url: &str,
        to: &Path,
        on: OnProgress<'_>,
    ) -> Result<String, String> {
        use tokio::io::AsyncWriteExt as _;
        let mut r = self.send(url).await?;
        let total = r.content_length();
        let mut file = tokio::fs::File::create(to)
            .await
            .map_err(|e| format!("{}: {e}", to.display()))?;
        let mut sha = aws_lc_rs::digest::Context::new(&aws_lc_rs::digest::SHA256);
        let (mut done, mut told) = (0u64, Instant::now());
        on(Progress::Bytes { done, total });
        while let Some(chunk) = r
            .chunk()
            .await
            .map_err(|e| format!("{url}: {}", causes(&e)))?
        {
            sha.update(&chunk);
            file.write_all(&chunk)
                .await
                .map_err(|e| format!("{}: {e}", to.display()))?;
            done += chunk.len() as u64;
            if told.elapsed() >= PROGRESS_EVERY {
                told = Instant::now();
                on(Progress::Bytes { done, total });
            }
        }
        on(Progress::Bytes { done, total });
        file.flush()
            .await
            .map_err(|e| format!("{}: {e}", to.display()))?;
        Ok(hex(sha.finish().as_ref()))
    }

    async fn send(&self, url: &str) -> Result<reqwest::Response, String> {
        let r = self
            .client
            .get(url)
            .send()
            .await
            .map_err(|e| format!("{url}: {}", causes(&e)))?;
        if !r.status().is_success() {
            return Err(format!("{url}: HTTP {}", r.status()));
        }
        Ok(r)
    }
}

/// The error and its causes: reqwest says only "error decoding response body"
/// for any failure while reading, the cause is underneath.
fn causes(e: &dyn std::error::Error) -> String {
    let mut text = e.to_string();
    let mut source = e.source();
    while let Some(s) = source {
        text.push_str(": ");
        text.push_str(&s.to_string());
        source = s.source();
    }
    text
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// A tag usable as a folder name: letters, digits, `.`, `_`, `+`, `-`.
pub fn valid_tag(tag: &str) -> bool {
    !tag.is_empty()
        && tag.len() <= 64
        && !tag.starts_with('.')
        && tag
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '+' | '-'))
}

/// Sort key: the numbers of a tag before `+` (`0.2.0+26.3` → 0, 2, 0).
pub fn version_key(tag: &str) -> Vec<u64> {
    tag.split('+')
        .next()
        .unwrap_or(tag)
        .split(|c: char| !c.is_ascii_digit())
        .filter_map(|n| n.parse().ok())
        .collect()
}

// ------------------------------------------------------------------ Pumpkin

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

#[derive(Deserialize)]
struct GhRelease {
    tag_name: String,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    assets: Vec<GhAsset>,
}

#[derive(Deserialize)]
struct GhAsset {
    name: String,
    browser_download_url: String,
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

/// The checksum of `file` in a `sha256sum` listing (`<hex>  <name>`).
pub fn checksum_of(listing: &str, file: &str) -> Option<String> {
    listing.lines().find_map(|l| {
        let (sum, name) = l.trim().split_once(char::is_whitespace)?;
        let name = name.trim().trim_start_matches('*');
        (name == file && sum.len() == 64 && sum.chars().all(|c| c.is_ascii_hexdigit()))
            .then(|| sum.to_ascii_lowercase())
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
        let toml = servers::pumpkin_toml(
            setup.name,
            setup.port,
            setup.network.velocity_secret.as_deref(),
            &servers::random_seed()?,
        );
        let write = |path: PathBuf, text: String| {
            std::fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))
        };
        write(dir.join("pumpkin.toml"), toml)?;
        if let Some((addr, key)) = &setup.network.bridge {
            let bridge = dir.join("plugins/data/pumbobridge");
            std::fs::create_dir_all(&bridge).map_err(|e| format!("{}: {e}", bridge.display()))?;
            write(bridge.join("config.yml"), servers::bridge_config(addr, key))?;
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
}

// ------------------------------------------------------------------ store

/// The error of a cancelled download.
pub const CANCELLED: &str = "cancelled";

/// Removes an unfinished download and its folder when nothing else is in it.
fn discard(part: &Path) {
    let _ = std::fs::remove_file(part);
    if let Some(dir) = part.parent() {
        // Fails on a folder with files: a downloaded version stays.
        let _ = std::fs::remove_dir(dir);
    }
}

/// Downloaded software in `versions/<source>/<tag>/<file>`.
pub struct VersionStore {
    dir: PathBuf,
    http: Http,
    sources: Vec<Arc<dyn Source>>,
    allow_unverified: bool,
    lists: Mutex<HashMap<&'static str, (Instant, Vec<Release>)>>,
    /// Downloads running now, with their cancel switch.
    downloading: Mutex<HashMap<(&'static str, String), tokio::sync::watch::Sender<bool>>>,
}

impl std::fmt::Debug for VersionStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VersionStore")
            .field("dir", &self.dir)
            .finish_non_exhaustive()
    }
}

impl VersionStore {
    pub fn new(
        dir: PathBuf,
        sources: Vec<Arc<dyn Source>>,
        allow_unverified: bool,
    ) -> Result<Self, String> {
        Ok(Self {
            dir,
            http: Http::new()?,
            sources,
            allow_unverified,
            lists: Mutex::new(HashMap::new()),
            downloading: Mutex::new(HashMap::new()),
        })
    }

    pub fn source(&self, name: &str) -> Result<&Arc<dyn Source>, String> {
        self.sources
            .iter()
            .find(|s| s.name().eq_ignore_ascii_case(name))
            .ok_or_else(|| {
                format!(
                    "unknown server software {name} (known: {})",
                    self.source_names().join(", ")
                )
            })
    }

    pub fn source_names(&self) -> Vec<&'static str> {
        self.sources.iter().map(|s| s.name()).collect()
    }

    /// Releases of a source, at most one request per 10 minutes.
    pub async fn releases(&self, source: &str) -> Result<Vec<Release>, String> {
        let src = self.source(source)?.clone();
        if let Some(list) = self.cached(src.name()) {
            return Ok(list);
        }
        let list = src.releases(&self.http).await?;
        if let Ok(mut l) = self.lists.lock() {
            l.insert(src.name(), (Instant::now(), list.clone()));
        }
        Ok(list)
    }

    /// The list from the last 10 minutes, without a request (completion).
    pub fn cached(&self, source: &str) -> Option<Vec<Release>> {
        let lists = self.lists.lock().ok()?;
        let (at, list) = lists.get(source)?;
        (at.elapsed() < LIST_TTL).then(|| list.clone())
    }

    /// Downloaded tags, newest first.
    pub fn installed(&self, source: &str) -> Vec<String> {
        let Ok(src) = self.source(source) else {
            return Vec::new();
        };
        let dir = self.dir.join(src.name());
        let mut tags: Vec<String> = std::fs::read_dir(&dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|t| valid_tag(t) && dir.join(t).join(src.file_name()).is_file())
            .collect();
        tags.sort_by_key(|t| std::cmp::Reverse(version_key(t)));
        tags
    }

    /// The newest downloaded release (tags that start with a digit).
    pub fn latest_installed(&self, source: &str) -> Option<String> {
        self.installed(source)
            .into_iter()
            .find(|t| t.starts_with(|c: char| c.is_ascii_digit()))
    }

    /// The downloaded file of a tag.
    pub fn file(&self, source: &str, tag: &str) -> Option<PathBuf> {
        let src = self.source(source).ok()?;
        let path = self.dir.join(src.name()).join(tag).join(src.file_name());
        (valid_tag(tag) && path.is_file()).then_some(path)
    }

    /// `#n` (from the list) or a tag.
    pub async fn resolve(&self, source: &str, which: &str) -> Result<Release, String> {
        let list = self.releases(source).await?;
        let found = match which.strip_prefix('#') {
            Some(n) => n
                .parse::<usize>()
                .ok()
                .and_then(|n| n.checked_sub(1))
                .and_then(|i| list.get(i)),
            None => list.iter().find(|r| r.tag == which),
        };
        found
            .cloned()
            .ok_or_else(|| format!("{source} has no release {which}"))
    }

    /// Downloads `#n` or a tag, checks its SHA256 and keeps it; returns the tag.
    pub async fn download(
        &self,
        source: &str,
        which: &str,
        on: OnProgress<'_>,
    ) -> Result<String, String> {
        let src = self.source(source)?.clone();
        let release = self.resolve(source, which).await?;
        let key = (src.name(), release.tag.clone());
        let (cancel, mut cancelled) = tokio::sync::watch::channel(false);
        let free = self.downloading.lock().is_ok_and(|mut d| {
            let free = !d.contains_key(&key);
            if free {
                d.insert(key.clone(), cancel);
            }
            free
        });
        if !free {
            return Err(format!(
                "{} {} is being downloaded",
                src.name(),
                release.tag
            ));
        }
        on(Progress::Started {
            tag: release.tag.clone(),
        });
        let result = tokio::select! {
            r = self.fetch(&*src, &release, on) => r,
            _ = cancelled.wait_for(|c| *c) => {
                let dir = self.dir.join(src.name()).join(&release.tag);
                discard(&dir.join(format!("{}.part", src.file_name())));
                Err(CANCELLED.into())
            }
        };
        if let Ok(mut d) = self.downloading.lock() {
            d.remove(&key);
        }
        result.map(|()| release.tag)
    }

    /// Downloads running now: (source, tag), sorted.
    pub fn downloads(&self) -> Vec<(&'static str, String)> {
        let mut v: Vec<_> = self
            .downloading
            .lock()
            .map(|d| d.keys().cloned().collect())
            .unwrap_or_default();
        v.sort();
        v
    }

    /// Cancels a running download; its [`VersionStore::download`] ends with
    /// [`CANCELLED`] and leaves no file behind.
    pub fn cancel(&self, source: &str, tag: &str) -> bool {
        self.downloading
            .lock()
            .ok()
            .and_then(|d| {
                d.iter()
                    .find(|((s, t), _)| *s == source && t == tag)
                    .map(|(_, c)| c.send(true).is_ok())
            })
            .unwrap_or(false)
    }

    async fn fetch(
        &self,
        src: &dyn Source,
        release: &Release,
        on: OnProgress<'_>,
    ) -> Result<(), String> {
        let artifact = src.artifact(&self.http, release).await?;
        if artifact.sha256.is_none() && !self.allow_unverified {
            return Err(format!(
                "{} {} publishes no SHA256 checksum, refused (allow-unverified: true in managed-servers allows it)",
                src.name(),
                release.tag
            ));
        }
        if artifact.sha256.is_none() {
            on(Progress::Unverified);
        }
        let dir = self.dir.join(src.name()).join(&release.tag);
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let part = dir.join(format!("{}.part", src.file_name()));
        let sha = self.http.download(&artifact.url, &part, on).await;
        let checked = match (sha, &artifact.sha256) {
            (Ok(got), Some(want)) if &got != want => Err(format!(
                "SHA256 mismatch for {} {}: expected {want}, got {got}",
                src.name(),
                release.tag
            )),
            (Ok(_), _) => Ok(()),
            (Err(e), _) => Err(e),
        };
        if let Err(e) = checked {
            discard(&part);
            return Err(e);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&part, std::fs::Permissions::from_mode(0o755))
                .map_err(|e| format!("{}: {e}", part.display()))?;
        }
        let file = dir.join(src.file_name());
        std::fs::rename(&part, &file).map_err(|e| format!("{}: {e}", file.display()))
    }
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
    fn checksum_listing() {
        let a = "a".repeat(64);
        let b = "B".repeat(64);
        let listing = format!("{a}  pumpkin-X64-Linux\n{b} *pumpkin-ARM64-macOS\nbad  x\n");
        assert_eq!(checksum_of(&listing, "pumpkin-X64-Linux"), Some(a));
        assert_eq!(
            checksum_of(&listing, "pumpkin-ARM64-macOS"),
            Some("b".repeat(64))
        );
        assert_eq!(checksum_of(&listing, "pumpkin-X64-Linux-musl"), None);
        assert_eq!(checksum_of(&listing, "x"), None);
    }

    #[test]
    fn tags() {
        assert!(valid_tag("0.2.0+26.3-26.51"));
        assert!(valid_tag("canary"));
        assert!(!valid_tag("../x"));
        assert!(!valid_tag(".hidden"));
        assert!(!valid_tag("a/b"));
        assert!(!valid_tag(""));
        let p = Pumpkin::default();
        assert_eq!(p.minecraft("0.2.0+26.3-26.51").as_deref(), Some("26.3"));
        assert_eq!(p.minecraft("0.1.0-dev+26.2-26.45").as_deref(), Some("26.2"));
        assert_eq!(p.minecraft("canary"), None);
        assert!(version_key("0.10.0+26.4") > version_key("0.2.0+26.3-26.51"));
        assert!(version_key("0.2.0+26.3") > version_key("0.1.0-dev+26.2-26.45"));
    }
}
