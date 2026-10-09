//! `pumbo-datagen tables [--from 1.21] [--cache DIR] [--out DIR] [--jobs N] [--java PATH] [--offline]`
//! `pumbo-datagen record [--work DIR] [--full DIR] [--port N] [--parallel N] [--only P,P] [--java PATH]`
//! `pumbo-datagen resync` (rewrites `synced.txt` from existing recordings)

use std::path::PathBuf;
use std::process::ExitCode;

use pumbo_datagen::{Options, default_cache, group, release_tables, write};

const USAGE: &str = "usage: pumbo-datagen tables [--from 1.21] [--cache DIR] [--out DIR] [--jobs N] [--java PATH] [--offline]\n       pumbo-datagen record [--work DIR] [--full DIR] [--port N] [--parallel N] [--only P,P] [--java PATH]\n       pumbo-datagen resync";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut it = args.iter();
    match it.next().map(String::as_str) {
        Some("tables") => {}
        Some("record") => return record(it.cloned().collect()),
        Some("resync") => return resync(),
        _ => {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    }
    let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..");
    let mut opts = Options {
        cache: default_cache(),
        out: workspace.join("crates").join("pumbo-data").join("tables"),
        from: "1.21".into(),
        java: std::env::var("JAVA").unwrap_or_else(|_| "java".into()),
        jobs: 4,
        offline: false,
    };
    while let Some(flag) = it.next() {
        if flag == "--offline" {
            opts.offline = true;
            continue;
        }
        let Some(v) = it.next().cloned() else {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        };
        match flag.as_str() {
            "--from" => opts.from = v,
            "--cache" => opts.cache = v.into(),
            "--out" => opts.out = v.into(),
            "--java" => opts.java = v,
            "--jobs" => match v.parse() {
                Ok(n) => opts.jobs = n,
                Err(_) => {
                    eprintln!("{USAGE}");
                    return ExitCode::from(2);
                }
            },
            _ => {
                eprintln!("{USAGE}");
                return ExitCode::from(2);
            }
        }
    }
    let result = release_tables(&opts).and_then(group).and_then(|protocols| {
        write(&opts.out, &protocols)?;
        Ok(protocols)
    });
    match result {
        Ok(protocols) => {
            for t in &protocols {
                eprintln!(
                    "protocol {}: {} ({} packets, {} blocks)",
                    t.protocol,
                    t.release_names().join(", "),
                    t.packets.len(),
                    t.blocks.len()
                );
            }
            eprintln!("tables written to {}", opts.out.display());
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn record(args: Vec<String>) -> ExitCode {
    let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..");
    let mut opts = pumbo_datagen::record::RecordOptions {
        cache: default_cache(),
        work: workspace.join("target").join("pumbo-datagen-servers"),
        full: std::env::var_os("PUMBO_DATA_FULL")
            .map(PathBuf::from)
            .unwrap_or_else(|| workspace.join("target").join("pumbo-data-full")),
        tables: workspace.join("crates").join("pumbo-data").join("tables"),
        java: std::env::var("JAVA").unwrap_or_else(|_| "java".into()),
        base_port: 25600,
        parallel: 2,
        only: Vec::new(),
    };
    let mut it = args.into_iter();
    while let Some(flag) = it.next() {
        let Some(v) = it.next() else {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        };
        let ok = match flag.as_str() {
            "--work" => {
                opts.work = v.into();
                true
            }
            "--full" => {
                opts.full = v.into();
                true
            }
            "--java" => {
                opts.java = v;
                true
            }
            "--port" => v.parse().map(|p| opts.base_port = p).is_ok(),
            "--parallel" => v.parse().map(|p| opts.parallel = p).is_ok(),
            "--only" => v
                .split(',')
                .map(str::parse)
                .collect::<Result<Vec<i32>, _>>()
                .map(|o| opts.only = o)
                .is_ok(),
            _ => false,
        };
        if !ok {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    }
    let mut failed = false;
    for (protocol, result) in pumbo_datagen::record::record(&opts) {
        match result {
            Ok(r) => {
                let ok = pumbo_datagen::record::script_completed(&r);
                failed |= !ok;
                eprintln!(
                    "protocol {protocol} ({}): {} frames, known packs: {:?}, no packs: {:?}{}",
                    r.release,
                    r.frames,
                    r.known,
                    r.none,
                    if ok {
                        ""
                    } else {
                        "  <-- script did not complete"
                    }
                );
                for n in &r.notes {
                    eprintln!("    {n}");
                }
            }
            Err(e) => {
                failed = true;
                eprintln!("protocol {protocol}: error: {e}");
            }
        }
    }
    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn resync() -> ExitCode {
    let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..");
    let full = std::env::var_os("PUMBO_DATA_FULL")
        .map(PathBuf::from)
        .unwrap_or_else(|| workspace.join("target").join("pumbo-data-full"));
    let tables = workspace.join("crates").join("pumbo-data").join("tables");
    match pumbo_datagen::record::resync(&full, &tables) {
        Ok(done) => {
            eprintln!("synced.txt rewritten for {done:?}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
