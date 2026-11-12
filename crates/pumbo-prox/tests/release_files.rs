//! Files of the repository that releases depend on.
#![allow(clippy::panic)]

use std::path::Path;

fn read(rel: &str) -> String {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    std::fs::read_to_string(root.join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
}

/// The quoted strings of the list `key = [ ... ]` in a TOML file.
fn list(text: &str, key: &str) -> Vec<String> {
    let start = text
        .lines()
        .position(|l| l.trim_start().starts_with(&format!("{key} = [")))
        .unwrap_or_else(|| panic!("no {key} list"));
    let mut out = Vec::new();
    for line in text.lines().skip(start + 1) {
        let line = line.trim();
        if line.starts_with(']') {
            return out;
        }
        if let Some(s) = line.strip_prefix('"').and_then(|l| l.split('"').next()) {
            out.push(s.to_string());
        }
    }
    panic!("{key} list not closed");
}

/// The release lists third-party licenses with the licenses `cargo deny` allows.
#[test]
fn third_party_licenses_accept_what_deny_allows() {
    let deny = list(&read("deny.toml"), "allow");
    let about = list(&read("about.toml"), "accepted");
    assert!(!deny.is_empty());
    assert_eq!(
        about, deny,
        "about.toml accepted and deny.toml allow differ"
    );
}

/// Release archives and the Docker image carry NOTICE and the license files.
#[test]
fn releases_ship_notice_and_licenses() {
    let release = read(".github/workflows/release.yml");
    for needed in [
        "crates/pumbo-translate-mv/NOTICE",
        "crates/pumbo-translate-mv/licenses",
        "THIRD-PARTY-LICENSES.txt",
    ] {
        assert!(
            release.contains(needed),
            "release.yml does not ship {needed}"
        );
    }
    let docker = read("Dockerfile");
    assert!(docker.contains("/usr/share/doc/pumboprox/"));
    for file in ["NOTICE", "THIRD-PARTY-LICENSES.txt", "licenses"] {
        assert!(docker.contains(file), "the Docker image lacks {file}");
    }
}
