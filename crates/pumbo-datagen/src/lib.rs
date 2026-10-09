//! `pumbo-datagen`: tables for every protocol from the official server jars
//! (plan §2.5).
//!
//! `pumbo-datagen tables` refreshes the version manifest, downloads every
//! release from 1.21 on (snapshots skipped) with SHA-1 checks into the cache
//! (`~/.cache/pumbo-datagen` or `PUMBO_DATAGEN_CACHE`), runs Mojang's
//! generator, checks that releases sharing a protocol give identical tables
//! and writes `crates/pumbo-data/tables/<protocol>/` plus `changes.txt`.

pub mod diff;
pub mod fetch;
pub mod record;
pub mod reports;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use pumbo_data::Tables;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{0}")]
    Tool(String),
    #[error("{0}")]
    Json(String),
    #[error("checksum of {url}: expected {expected}, got {got}")]
    Checksum {
        url: String,
        expected: String,
        got: String,
    },
    #[error("releases of protocol {protocol} differ ({release} vs {other}):\n{details}")]
    ReleasesDiffer {
        protocol: i32,
        release: String,
        other: String,
        details: String,
    },
    #[error(transparent)]
    Data(#[from] pumbo_data::DataError),
}

impl Error {
    pub fn io(path: &Path, source: std::io::Error) -> Self {
        Self::Io {
            path: path.to_path_buf(),
            source,
        }
    }
}

/// Options of the `tables` command.
#[derive(Debug, Clone)]
pub struct Options {
    pub cache: PathBuf,
    pub out: PathBuf,
    pub from: String,
    pub java: String,
    pub jobs: usize,
    pub offline: bool,
}

/// Default cache directory.
pub fn default_cache() -> PathBuf {
    if let Some(dir) = std::env::var_os("PUMBO_DATAGEN_CACHE") {
        return PathBuf::from(dir);
    }
    let home = std::env::var_os("HOME").unwrap_or_default();
    PathBuf::from(home).join(".cache").join("pumbo-datagen")
}

/// Tables of every release, oldest first.
pub fn release_tables(opts: &Options) -> Result<Vec<Tables>, Error> {
    std::fs::create_dir_all(&opts.cache).map_err(|e| Error::io(&opts.cache, e))?;
    let releases = fetch::releases_since(&opts.cache, &opts.from, opts.offline)?;
    eprintln!(
        "{} releases: {}",
        releases.len(),
        releases
            .iter()
            .map(|r| r.id.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    );
    // Downloads one by one, generator runs in parallel (`jobs` at a time).
    let mut prepared = Vec::new();
    for r in &releases {
        let dir = opts.cache.join(&r.id);
        let jar = fetch::server_jar(r, &dir)?;
        prepared.push((r.id.clone(), dir, jar));
    }
    let mut results: Vec<Option<Result<Tables, Error>>> =
        (0..prepared.len()).map(|_| None).collect();
    for chunk_start in (0..prepared.len()).step_by(opts.jobs.max(1)) {
        let chunk: Vec<_> = prepared
            .iter()
            .enumerate()
            .skip(chunk_start)
            .take(opts.jobs.max(1))
            .collect();
        std::thread::scope(|s| {
            let handles: Vec<_> = chunk
                .iter()
                .map(|(i, (id, dir, jar))| {
                    let java = opts.java.clone();
                    (
                        *i,
                        s.spawn(move || -> Result<Tables, Error> {
                            let version = reports::jar_version(jar)?;
                            if version.id != *id {
                                return Err(Error::Json(format!(
                                    "{id}: jar says it is {}",
                                    version.id
                                )));
                            }
                            let reports = reports::generate(dir, &java)?;
                            let sha1 = fetch::sha1_file(jar)?;
                            let t = reports::tables(&reports, &version, &sha1)?;
                            eprintln!("{id}: protocol {}", version.protocol);
                            Ok(t)
                        }),
                    )
                })
                .collect();
            for (i, h) in handles {
                let r = h
                    .join()
                    .unwrap_or_else(|_| Err(Error::Tool("generator thread panicked".into())));
                if let Some(slot) = results.get_mut(i) {
                    *slot = Some(r);
                }
            }
        });
    }
    results
        .into_iter()
        .map(|r| r.unwrap_or_else(|| Err(Error::Tool("missing result".into()))))
        .collect()
}

/// Groups releases by protocol; releases of one protocol must have identical
/// content. The newest release's tables represent the protocol.
pub fn group(releases: Vec<Tables>) -> Result<Vec<Tables>, Error> {
    let mut by_protocol: BTreeMap<i32, Tables> = BTreeMap::new();
    for t in releases {
        match by_protocol.get_mut(&t.protocol.0) {
            None => {
                by_protocol.insert(t.protocol.0, t);
            }
            Some(existing) => {
                if !diff::same_content(existing, &t) {
                    return Err(Error::ReleasesDiffer {
                        protocol: t.protocol.0,
                        release: existing.release_names().join(","),
                        other: t.release_names().join(","),
                        details: diff::describe(existing, &t).join("\n"),
                    });
                }
                existing.releases.extend(t.releases);
            }
        }
    }
    Ok(by_protocol.into_values().collect())
}

/// Writes the tables and the change report.
pub fn write(out: &Path, protocols: &[Tables]) -> Result<(), Error> {
    for t in protocols {
        let dir = out.join(t.protocol.0.to_string());
        std::fs::create_dir_all(&dir).map_err(|e| Error::io(&dir, e))?;
        for (name, content) in t.to_files() {
            // Check that what we write reads back the same.
            let path = dir.join(name);
            std::fs::write(&path, &content).map_err(|e| Error::io(&path, e))?;
        }
        let read = |f: &str| std::fs::read_to_string(dir.join(f)).map_err(|e| Error::io(&dir, e));
        let back = Tables::parse(
            &read("meta.txt")?,
            &read("packets.txt")?,
            &read("registries.txt")?,
            &read("blocks.txt")?,
        )?;
        if back != *t {
            return Err(Error::Tool(format!(
                "protocol {}: written tables do not read back the same",
                t.protocol
            )));
        }
    }
    let report = diff::report(protocols);
    let path = out.join("changes.txt");
    std::fs::write(&path, report).map_err(|e| Error::io(&path, e))?;
    if let Ok(entries) = std::fs::read_dir(out) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if let Ok(p) = name.parse::<i32>()
                && !protocols.iter().any(|t| t.protocol.0 == p)
            {
                eprintln!(
                    "warning: {name} is not produced by this run (stale protocol directory?)"
                );
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use pumbo_data::{Registry, Release};
    use pumbo_protocol::ProtocolVersion;

    fn release(p: i32, name: &str, items: &[&str]) -> Tables {
        Tables::new(
            ProtocolVersion(p),
            vec![Release {
                name: name.into(),
                world_version: 1,
                server_sha1: "0".repeat(40),
            }],
            vec![],
            vec![],
            vec![Registry::new(
                "item",
                items.iter().map(|s| s.to_string()).collect(),
            )],
            vec![],
        )
    }

    #[test]
    fn releases_of_one_protocol_merge_or_fail() {
        let merged = group(vec![
            release(767, "1.21", &["minecraft:a"]),
            release(767, "1.21.1", &["minecraft:a"]),
            release(768, "1.21.2", &["minecraft:a", "minecraft:b"]),
        ])
        .unwrap();
        assert_eq!(merged.len(), 2);
        assert_eq!(merged.first().unwrap().release_names(), ["1.21", "1.21.1"]);
        let err = group(vec![
            release(767, "1.21", &["minecraft:a"]),
            release(767, "1.21.1", &["minecraft:b"]),
        ])
        .unwrap_err();
        assert!(matches!(err, Error::ReleasesDiffer { protocol: 767, .. }));
        assert!(err.to_string().contains("registry minecraft:item"));
    }
}
