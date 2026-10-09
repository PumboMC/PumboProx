//! Downloads from Mojang's launcher metadata with SHA-1 verification.
//!
//! Uses the system `curl` with a neutral User-Agent; nothing about the user is
//! sent. Everything lands in the cache directory outside the repository.

use std::path::{Path, PathBuf};
use std::process::Command;

use aws_lc_rs::digest;
use serde_json::Value;

use crate::Error;

pub const USER_AGENT: &str = "pumbo-datagen/0.1";
pub const MANIFEST_URL: &str = "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json";

/// A release entry from the version manifest.
#[derive(Debug, Clone)]
pub struct ManifestRelease {
    pub id: String,
    pub url: String,
    pub sha1: String,
}

/// Hex SHA-1 of a file.
pub fn sha1_file(path: &Path) -> Result<String, Error> {
    let data = std::fs::read(path).map_err(|e| Error::io(path, e))?;
    let d = digest::digest(&digest::SHA1_FOR_LEGACY_USE_ONLY, &data);
    Ok(d.as_ref().iter().map(|b| format!("{b:02x}")).collect())
}

/// Downloads `url` to `dest` (via a `.part` file), checking SHA-1 if given.
pub fn download(url: &str, dest: &Path, sha1: Option<&str>) -> Result<(), Error> {
    let part = PathBuf::from(format!("{}.part", dest.display()));
    let status = Command::new("curl")
        .args(["-fsSL", "--retry", "2", "-A", USER_AGENT, "-o"])
        .arg(&part)
        .arg(url)
        .status()
        .map_err(|e| Error::Tool(format!("curl: {e}")))?;
    if !status.success() {
        return Err(Error::Tool(format!("curl {url}: {status}")));
    }
    if let Some(expected) = sha1 {
        let got = sha1_file(&part)?;
        if got != expected {
            return Err(Error::Checksum {
                url: url.to_string(),
                expected: expected.to_string(),
                got,
            });
        }
    }
    std::fs::rename(&part, dest).map_err(|e| Error::io(dest, e))
}

/// Keeps `dest` if its SHA-1 matches, otherwise downloads it again.
pub fn ensure(url: &str, dest: &Path, sha1: &str) -> Result<(), Error> {
    if dest.is_file() && sha1_file(dest)? == sha1 {
        return Ok(());
    }
    download(url, dest, Some(sha1))
}

fn read_json(path: &Path) -> Result<Value, Error> {
    let text = std::fs::read_to_string(path).map_err(|e| Error::io(path, e))?;
    serde_json::from_str(&text).map_err(|e| Error::Json(format!("{}: {e}", path.display())))
}

/// Fetches the manifest (always fresh unless `offline`) and returns releases
/// from `from` up to the newest, oldest first. Snapshots are skipped.
pub fn releases_since(
    cache: &Path,
    from: &str,
    offline: bool,
) -> Result<Vec<ManifestRelease>, Error> {
    let path = cache.join("version_manifest_v2.json");
    if !offline || !path.is_file() {
        download(MANIFEST_URL, &path, None)?;
    }
    let manifest = read_json(&path)?;
    let versions = manifest
        .get("versions")
        .and_then(Value::as_array)
        .ok_or_else(|| Error::Json("manifest without versions".into()))?;
    let mut out = Vec::new();
    let mut found = false;
    // The manifest lists newest first.
    for v in versions {
        let field = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or_default();
        if field("type") != "release" {
            continue;
        }
        out.push(ManifestRelease {
            id: field("id").to_string(),
            url: field("url").to_string(),
            sha1: field("sha1").to_string(),
        });
        if field("id") == from {
            found = true;
            break;
        }
    }
    if !found {
        return Err(Error::Json(format!("release {from} not in the manifest")));
    }
    out.reverse();
    Ok(out)
}

/// Server jar of a release (downloaded and verified into `dir`).
pub fn server_jar(release: &ManifestRelease, dir: &Path) -> Result<PathBuf, Error> {
    std::fs::create_dir_all(dir).map_err(|e| Error::io(dir, e))?;
    let version_json = dir.join("version.json");
    ensure(&release.url, &version_json, &release.sha1)?;
    let meta = read_json(&version_json)?;
    let server = meta
        .get("downloads")
        .and_then(|d| d.get("server"))
        .ok_or_else(|| Error::Json(format!("{}: no server download", release.id)))?;
    let url = server
        .get("url")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let sha1 = server
        .get("sha1")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let jar = dir.join("server.jar");
    ensure(url, &jar, sha1)?;
    Ok(jar)
}
