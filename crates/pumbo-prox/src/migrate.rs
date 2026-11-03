//! `config-version` of `pumboprox.yml` (1 when missing) and the steps
//! between versions. Every read applies them in memory, with a warning for
//! each old key; only `pumboprox migrate-config` changes the file.
//!
//! Version 2: `bungeecord-channel` is `messaging-channel`.

use serde_json::Value;

use crate::config::{Config, Notes};
use crate::route::{indent_of, split_key};

/// The version this build writes and reads.
pub const CONFIG_VERSION: u64 = 2;

const OLD_CHANNEL: &str = "bungeecord-channel";
const CHANNEL: &str = "messaging-channel";

/// The line of a top-level key written in block style.
fn top_line(text: &str, key: &str) -> Option<usize> {
    text.lines()
        .position(|l| indent_of(l) == 0 && split_key(l).is_some_and(|(k, _, _)| k == key))
}

/// The steps on a freshly read config; `value` and `text` are the same file.
/// Returns a warning (with its line) for each old key in use.
pub(crate) fn apply(cfg: &mut Config, value: &Value, text: &str) -> Notes {
    let mut notes = Notes::new();
    let line = |key: &str| top_line(text, key).map(|i| i + 1);
    if let Some(v) = value.get("config-version").and_then(Value::as_u64)
        && v > CONFIG_VERSION
    {
        notes.push((
            line("config-version"),
            format!(
                "config-version {v} is newer than this PumboProx knows ({CONFIG_VERSION}); settings it does not know are ignored"
            ),
        ));
    }
    if let Some(old) = cfg.legacy_messaging_channel.take() {
        if value.get(CHANNEL).is_some() {
            notes.push((
                line(OLD_CHANNEL),
                format!("{OLD_CHANNEL} is ignored because {CHANNEL} is set; remove it"),
            ));
        } else {
            cfg.messaging_channel = old;
            notes.push((
                line(OLD_CHANNEL),
                format!(
                    "{OLD_CHANNEL} is now {CHANNEL}; it still works, and `pumboprox migrate-config` renames it in the file"
                ),
            ));
        }
    }
    notes
}

/// `pumboprox migrate-config`: the text at [`CONFIG_VERSION`] with every
/// other line, comments too, as it was, and what changed; `None` when
/// nothing would change. Refuses (the file stays) when an old key cannot be
/// renamed in the text.
pub fn migrate_text(text: &str) -> Result<Option<(String, Vec<String>)>, String> {
    let value: Value = pumbo_core::yaml::from_str(text)?;
    let version = value
        .get("config-version")
        .and_then(Value::as_u64)
        .unwrap_or(1);
    if version > CONFIG_VERSION {
        return Err(format!(
            "config-version {version} is newer than this PumboProx knows ({CONFIG_VERSION})"
        ));
    }
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    let mut changes = Vec::new();
    if value.get(OLD_CHANNEL).is_some() {
        if value.get(CHANNEL).is_some() {
            return Err(format!(
                "both {OLD_CHANNEL} and {CHANNEL} are set; remove {OLD_CHANNEL} ({CHANNEL} is the one used) and run it again"
            ));
        }
        let at = top_line(text, OLD_CHANNEL).ok_or_else(|| {
            format!("{OLD_CHANNEL} is not written as a key on its own line; rename it to {CHANNEL} by hand")
        })?;
        if let Some(l) = lines.get_mut(at) {
            let written = split_key(l)
                .map(|(_, w, _)| w.to_string())
                .unwrap_or_default();
            *l = l.replacen(&written, CHANNEL, 1);
        }
        changes.push(format!("line {}: {OLD_CHANNEL} is now {CHANNEL}", at + 1));
    }
    if changes.is_empty() {
        return Ok(None);
    }
    let version_line = format!("config-version: {CONFIG_VERSION}");
    match top_line(text, "config-version") {
        Some(at) => {
            if let Some(l) = lines.get_mut(at) {
                *l = version_line;
            }
        }
        None => {
            // Before the first key, after the comments at the top.
            let first = lines
                .iter()
                .position(|l| {
                    let t = l.trim();
                    !t.is_empty() && !t.starts_with('#') && t != "---"
                })
                .unwrap_or(lines.len());
            lines.insert(first, String::new());
            lines.insert(first, version_line);
        }
    }
    // A file saved on Windows keeps its line ends.
    let eol = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let mut out = lines.join(eol);
    if text.ends_with('\n') {
        out.push_str(eol);
    }
    Config::parse(&out).map_err(|e| format!("the migrated file would not load: {e}"))?;
    Ok(Some((out, changes)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_old_channel_key_is_renamed_in_place() {
        let text = "# my network\nlisten: x\n\n# old plugins\nbungeecord-channel:   # keep\n  subchannels: [Connect]\n";
        let text = text.replace("listen: x", "pumbox:\n  enabled: true");
        let (out, changes) = migrate_text(&text).unwrap().unwrap();
        assert_eq!(
            out,
            "# my network\nconfig-version: 2\n\npumbox:\n  enabled: true\n\n# old plugins\nmessaging-channel:   # keep\n  subchannels: [Connect]\n"
        );
        assert_eq!(
            changes,
            ["line 6: bungeecord-channel is now messaging-channel"]
        );
        assert_eq!(
            migrate_text(&out).unwrap(),
            None,
            "a second run changes nothing"
        );
        // Nothing to rename: the file stays, without a version line.
        assert_eq!(migrate_text("pumbox:\n  enabled: true\n").unwrap(), None);
    }

    #[test]
    fn windows_line_ends_stay() {
        let text = "# net\r\nbungeecord-channel: {enabled: false}\r\nlogging: {level: info}\r\n";
        let (out, _) = migrate_text(text).unwrap().unwrap();
        assert_eq!(
            out,
            "# net\r\nconfig-version: 2\r\n\r\nmessaging-channel: {enabled: false}\r\nlogging: {level: info}\r\n"
        );
    }

    #[test]
    fn what_cannot_be_renamed_is_refused() {
        let both = "bungeecord-channel: {}\nmessaging-channel: {}\n";
        assert!(migrate_text(both).unwrap_err().contains("both"));
        let flow = "{bungeecord-channel: {enabled: false}}\n";
        assert!(migrate_text(flow).unwrap_err().contains("by hand"));
        let newer = "config-version: 9\n";
        assert!(migrate_text(newer).unwrap_err().contains("newer"));
    }
}
