//! Every page under docs/ starts with a line that links all the others.
#![allow(clippy::panic)]

use std::path::Path;

/// Pages outside the navigation: code signing (linked from the README only)
/// and the log tools/bench.sh writes.
const OUTSIDE: &[&str] = &["code-signing.md", "test-results.md"];

#[test]
fn every_page_links_every_page() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs");
    let mut pages: Vec<String> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter(|n| n.ends_with(".md") && !OUTSIDE.contains(&n.as_str()))
        .collect();
    pages.sort();
    assert!(pages.len() > 1);
    for page in &pages {
        let text = std::fs::read_to_string(dir.join(page)).unwrap_or_default();
        let nav = text.lines().next().unwrap_or_default();
        assert!(
            nav.starts_with("[PumboProx](../README.md) · "),
            "{page}: the first line is not the navigation"
        );
        for other in pages.iter().filter(|p| *p != page) {
            assert!(
                nav.contains(&format!("]({other})")),
                "{page}: the navigation lacks {other}"
            );
        }
        assert!(
            !nav.contains(&format!("]({page})")) && nav.contains(" · **"),
            "{page}: the navigation should name this page in bold, not link it"
        );
    }
}
