//! Embeds every table directory under `tables/` (one per protocol). Adding a
//! protocol is a generator run; no code changes.
//!
//! Full registry data (never committed) is embedded too when it exists in
//! `$PUMBO_DATA_FULL/<protocol>/registries-full.rec` or, by default, in
//! `target/pumbo-data-full/<protocol>/registries-full.rec` of the workspace.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

const FILES: [&str; 5] = [
    "meta.txt",
    "packets.txt",
    "registries.txt",
    "blocks.txt",
    "synced.txt",
];

fn main() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let root = manifest.join("tables");
    println!("cargo:rerun-if-changed={}", root.display());
    println!("cargo:rerun-if-env-changed=PUMBO_DATA_FULL");
    let full_root = std::env::var_os("PUMBO_DATA_FULL")
        .map(PathBuf::from)
        .unwrap_or_else(|| manifest.join("../../target/pumbo-data-full"));
    let mut protocols: Vec<(i32, PathBuf)> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&root) {
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if let (true, Ok(p)) = (path.is_dir(), name.parse::<i32>()) {
                for f in FILES {
                    println!("cargo:rerun-if-changed={}", path.join(f).display());
                }
                protocols.push((p, path));
            }
        }
    }
    protocols.sort();
    let mut out = String::from("pub(crate) static EMBEDDED: &[Embedded] = &[\n");
    for (p, dir) in &protocols {
        let file = |f: &str| format!("include_str!({:?})", dir.join(f).display().to_string());
        let synced = dir.join("synced.txt");
        let synced = if synced.is_file() {
            format!("Some({})", file("synced.txt"))
        } else {
            "None".into()
        };
        let full_path = full_root.join(p.to_string()).join("registries-full.rec");
        println!("cargo:rerun-if-changed={}", full_path.display());
        let full = if full_path.is_file() {
            format!(
                "Some(include_bytes!({:?}))",
                full_path.display().to_string()
            )
        } else {
            "None".into()
        };
        let _ = writeln!(
            out,
            "    Embedded {{ protocol: {p}, meta: {}, packets: {}, registries: {}, blocks: {}, synced: {synced}, full: {full} }},",
            file("meta.txt"),
            file("packets.txt"),
            file("registries.txt"),
            file("blocks.txt"),
        );
    }
    out.push_str("];\n");
    let n = protocols.len();
    let _ = writeln!(
        out,
        "pub(crate) static PARSED: [std::sync::OnceLock<Result<crate::Tables, crate::DataError>>; {n}] = [const {{ std::sync::OnceLock::new() }}; {n}];"
    );
    let _ = writeln!(
        out,
        "pub(crate) static SYNCED: [std::sync::OnceLock<Result<crate::Synced, crate::DataError>>; {n}] = [const {{ std::sync::OnceLock::new() }}; {n}];"
    );
    let dest = Path::new(&std::env::var("OUT_DIR").unwrap_or_default()).join("embedded.rs");
    if let Err(e) = std::fs::write(&dest, out) {
        println!("cargo:warning=cannot write {}: {e}", dest.display());
        std::process::exit(1);
    }
}
