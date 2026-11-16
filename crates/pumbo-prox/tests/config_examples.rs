//! Every example of `pumboprox.yml` in docs/ and the config
//! tools/dev-network.sh writes loads cleanly: no error, no unknown key, no
//! warning. A YAML example of another file names that file on its first line
//! (`# compose.yml`).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::path::{Path, PathBuf};

use pumbo_prox::config::Config;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// What is wrong with `text` as `pumboprox.yml`, if anything.
fn problems(text: &str) -> Option<String> {
    let remarks = match Config::parse_checked(text) {
        Ok((_, remarks)) => remarks,
        Err(e) => return Some(e.to_string()),
    };
    if let Err(e) = pumbo_host::HostConfig::parse(text) {
        return Some(e);
    }
    let notes: Vec<&str> = remarks
        .unknown
        .iter()
        .chain(&remarks.warnings)
        .map(|(_, m)| m.as_str())
        .collect();
    (!notes.is_empty()).then(|| notes.join("; "))
}

/// A fence line: its character (`` ` `` or `~`), how many of them, and the
/// info string after them.
fn fence(line: &str) -> Option<(char, usize, &str)> {
    let t = line.trim_start();
    let c = t.chars().next().filter(|c| *c == '`' || *c == '~')?;
    let n = t.chars().take_while(|x| *x == c).count();
    (n >= 3).then(|| (c, n, t[n..].trim()))
}

/// A fenced block being read: its fence, its indent, whether it is YAML,
/// the line it starts on and its text.
struct Block {
    fence: (char, usize),
    indent: String,
    yaml: bool,
    at: usize,
    text: String,
}

/// The fenced blocks of a page whose info string starts with `yaml` or `yml`
/// in any case, with the line each starts on, without the indent of their
/// opening fence.
fn yaml_blocks(page: &str) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    let mut open: Option<Block> = None;
    for (i, line) in page.lines().enumerate() {
        match (open.as_mut(), fence(line)) {
            (None, Some((c, n, info))) => {
                let lang = info.split_whitespace().next().unwrap_or_default();
                open = Some(Block {
                    fence: (c, n),
                    indent: line[..line.len() - line.trim_start().len()].to_string(),
                    yaml: lang.eq_ignore_ascii_case("yaml") || lang.eq_ignore_ascii_case("yml"),
                    at: i + 2,
                    text: String::new(),
                });
            }
            (Some(b), Some((c, n, rest)))
                if c == b.fence.0 && n >= b.fence.1 && rest.is_empty() =>
            {
                if let Some(b) = open.take().filter(|b| b.yaml) {
                    out.push((b.at, b.text));
                }
            }
            (Some(b), _) => {
                b.text
                    .push_str(line.strip_prefix(b.indent.as_str()).unwrap_or(line));
                b.text.push('\n');
            }
            (None, None) => {}
        }
    }
    out
}

/// `#`, then a name ending in `.yml` or `.yaml` whose last path component is
/// not `pumboprox.yml`, then anything: an example of another file.
fn names_another_file(block: &str) -> bool {
    let first = block.lines().next().unwrap_or_default();
    let Some(name) = first
        .strip_prefix('#')
        .and_then(|r| r.split_whitespace().next())
    else {
        return false;
    };
    let file = name.rsplit('/').next().unwrap_or(name);
    (file.ends_with(".yml") || file.ends_with(".yaml")) && file != "pumboprox.yml"
}

/// Every YAML block in docs/, with the page and line it starts on.
fn docs_blocks() -> Vec<(String, String)> {
    let mut pages: Vec<PathBuf> = std::fs::read_dir(root().join("docs"))
        .unwrap()
        .filter_map(|e| Some(e.ok()?.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "md"))
        .collect();
    pages.sort();
    let mut out = Vec::new();
    for page in &pages {
        let name = page.file_name().unwrap().to_string_lossy().into_owned();
        for (line, block) in yaml_blocks(&std::fs::read_to_string(page).unwrap()) {
            out.push((format!("docs/{name}:{line}"), block));
        }
    }
    out
}

#[test]
fn yaml_blocks_follow_the_fences() {
    let page = "text\n```YAML\na: 1\n```\n~~~ yml extra\nb: 2\n~~~\n````sh\n```yaml\nc: 3\n````\n  ```yaml\n  d: 4\n  ```\n";
    let blocks: Vec<String> = yaml_blocks(page).into_iter().map(|(_, t)| t).collect();
    assert_eq!(blocks, ["a: 1\n", "b: 2\n", "d: 4\n"]);
}

#[test]
fn the_examples_in_the_docs_load() {
    let mut fragments = 0;
    for (at, block) in docs_blocks() {
        if names_another_file(&block) {
            // A label must not hide an example of pumboprox.yml.
            assert!(
                problems(&block).is_some(),
                "{at}: names another file but loads as pumboprox.yml"
            );
        } else {
            fragments += 1;
            if let Some(p) = problems(&block) {
                panic!("{at}: {p}");
            }
        }
    }
    assert!(
        fragments >= 10,
        "only {fragments} examples of pumboprox.yml in docs/"
    );
}

/// The config tools/dev-network.sh writes, its shell variables set.
fn dev_network_config() -> String {
    let script = std::fs::read_to_string(root().join("tools/dev-network.sh")).unwrap();
    let start = "cat > \"$DIR/proxy/pumboprox.yml\" <<EOF\n";
    let from = script.find(start).expect("the script writes pumboprox.yml") + start.len();
    let body = &script[from..];
    let text = &body[..=body.find("\nEOF\n").expect("the config ends with EOF")];
    [
        ("$((PORT + 1))", "25711"),
        ("$((PORT + 2))", "25712"),
        ("$PORT", "25710"),
        ("$ONLINE_MODE", "per-player"),
        ("$PROTOCOL", "777"),
        ("$BRIDGE_PORT", "25718"),
    ]
    .iter()
    .fold(text.to_string(), |t, (var, value)| t.replace(var, value))
}

#[test]
fn the_dev_network_config_loads() {
    let text = dev_network_config();
    assert!(!text.contains('$'), "a shell variable is left:\n{text}");
    assert_eq!(problems(&text), None, "{text}");
    // tools/bench.sh turns the managed servers off.
    let bench = format!("{text}managed-servers:\n  enabled: false\n");
    assert_eq!(problems(&bench), None, "{bench}");
}

/// The top-level keys of a config text.
fn top_keys(text: &str) -> Vec<String> {
    match pumbo_core::yaml::from_str::<serde_json::Value>(text) {
        Ok(serde_json::Value::Object(map)) => map.keys().cloned().collect(),
        _ => Vec::new(),
    }
}

/// Every section `Config` reads in the first-start file, the examples in
/// docs/ and the dev network is registered.
#[test]
fn every_section_the_examples_use_is_registered() {
    let registered: Vec<&str> = pumbo_prox::modules::sections()
        .iter()
        .map(|s| s.key)
        .collect();
    let mut texts = vec![
        pumbo_prox::config::DEFAULT_FILE.to_string(),
        dev_network_config(),
    ];
    texts.extend(
        docs_blocks()
            .into_iter()
            .map(|(_, block)| block)
            .filter(|block| !names_another_file(block)),
    );
    for key in texts.iter().flat_map(|text| top_keys(text)) {
        // Config takes or refuses a value under a key of its own; any other
        // key lands in `sections`.
        let read = match serde_json::from_value::<Config>(serde_json::json!({ key.as_str(): "x" }))
        {
            Ok(cfg) => !cfg.sections.contains_key(&key),
            Err(_) => true,
        };
        assert!(
            !read || registered.contains(&key.as_str()),
            "Config reads {key}, but no section is registered for it"
        );
    }
}
