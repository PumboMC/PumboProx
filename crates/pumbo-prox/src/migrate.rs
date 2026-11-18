//! `config-version` of `pumboprox.yml` (1 when missing) and the steps
//! between versions. Every read applies them in memory, with a warning for
//! each old key; only `pumboprox migrate-config` changes the file.
//!
//! Version 2: `bungeecord-channel` is `messaging-channel`, and
//! [`OLD_PROVIDER`] is gone from `translation.chain` and the top level.

use serde_json::Value;

use crate::config::{Config, Notes};
use crate::config_edit::{flow_list, indent_of, split_key, trailing_comment};
use crate::keys::{Seg, line_of};

/// The version this build writes and reads.
pub const CONFIG_VERSION: u64 = 2;

/// The name of `messaging-channel` before `config-version` 2.
pub(crate) const OLD_CHANNEL: &str = "bungeecord-channel";
const CHANNEL: &str = "messaging-channel";

/// The translation provider that sent players to an external translation
/// service: gone from `translation.chain` and the top level since
/// `config-version` 2.
pub const OLD_PROVIDER: &str = "viaproxy";

/// The index of a top-level key written in block style.
fn top_at<'a>(lines: impl IntoIterator<Item = &'a str>, key: &str) -> Option<usize> {
    lines
        .into_iter()
        .position(|l| indent_of(l) == 0 && split_key(l).is_some_and(|(k, _, _)| k == key))
}

/// The line of `translation.chain` in the file.
fn chain_line(text: &str) -> Option<usize> {
    line_of(text, &[Seg::from("translation"), Seg::from("chain")])
}

/// A comment line in column 0.
fn top_comment(l: &str) -> bool {
    l.starts_with('#')
}

/// The steps on a freshly read config; `value` and `text` are the same file.
/// Returns a warning (with its line) for each old key in use.
pub(crate) fn apply(cfg: &mut Config, value: &Value, text: &str) -> Notes {
    let mut notes = Notes::new();
    let line = |key: &str| top_at(text.lines(), key).map(|i| i + 1);
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
    let chain = &mut cfg.translation.chain;
    if chain.iter().any(|p| p == OLD_PROVIDER) {
        chain.retain(|p| p != OLD_PROVIDER);
        notes.push((
            chain_line(text),
            format!(
                "translation.chain: {OLD_PROVIDER} is no longer a provider; it is skipped, and `pumboprox migrate-config` removes it"
            ),
        ));
    }
    if cfg.sections.remove(OLD_PROVIDER).is_some() {
        notes.push((
            line(OLD_PROVIDER),
            format!("{OLD_PROVIDER} is no longer read; `pumboprox migrate-config` removes it"),
        ));
    }
    notes
}

/// `pumboprox migrate-config`: the text at [`CONFIG_VERSION`] with every
/// other line, comments too, as it was, and what changed, then warnings
/// about the result; `None` when nothing would change. Refuses (the file
/// stays) when an old key cannot be changed in the text. Every edit finds
/// its lines in the text as the earlier edits left it; the changes name the
/// lines of the old file. A byte-order mark at the start stays.
pub fn migrate_text(text: &str) -> Result<Option<(String, Vec<String>)>, String> {
    let (mark, text) = match text.strip_prefix('\u{feff}') {
        Some(rest) => ("\u{feff}", rest),
        None => ("", text),
    };
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
        let at = top_at(lines.iter().map(String::as_str), OLD_CHANNEL).ok_or_else(|| {
            format!("{OLD_CHANNEL} is not written as a key on its own line; rename it to {CHANNEL} by hand")
        })?;
        if let Some(l) = lines.get_mut(at) {
            let written = split_key(l)
                .map(|(_, w, _)| w.to_string())
                .unwrap_or_default();
            *l = l.replacen(&written, CHANNEL, 1);
        }
        let old = top_at(text.lines(), OLD_CHANNEL).unwrap_or(at);
        changes.push(format!("line {}: {OLD_CHANNEL} is now {CHANNEL}", old + 1));
    }
    if let Some(chain) = value
        .pointer("/translation/chain")
        .and_then(Value::as_array)
        && chain.iter().any(|p| p.as_str() == Some(OLD_PROVIDER))
    {
        let rest: Vec<String> = chain
            .iter()
            .filter_map(Value::as_str)
            .filter(|p| *p != OLD_PROVIDER)
            .map(str::to_string)
            .collect();
        drop_from_chain(&mut lines, &rest)?;
        let at = chain_line(text).unwrap_or_default();
        changes.push(format!(
            "line {at}: translation.chain no longer names {OLD_PROVIDER}"
        ));
    }
    if value.get(OLD_PROVIDER).is_some() {
        drop_section(&mut lines)?;
        let at = top_at(text.lines(), OLD_PROVIDER).map_or(0, |i| i + 1);
        changes.push(format!("line {at}: the {OLD_PROVIDER} section is removed"));
    }
    if changes.is_empty() {
        return Ok(None);
    }
    set_version(&mut lines);
    // A file saved on Windows keeps its line ends.
    let eol = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let mut out = format!("{mark}{}", lines.join(eol));
    if text.ends_with('\n') {
        out.push_str(eol);
    }
    let (_, remarks) = Config::parse_checked(&out)
        .map_err(|e| format!("the migrated file would not load: {e}"))?;
    changes.extend(
        remarks
            .warnings
            .into_iter()
            .map(|(_, w)| format!("warning: {w}")),
    );
    Ok(Some((out, changes)))
}

/// `config-version` at [`CONFIG_VERSION`]: a line that already says so
/// stays as it is, another keeps its comment, and a file without one gets it
/// before the first key, after the comments at the top.
fn set_version(lines: &mut Vec<String>) {
    let version = CONFIG_VERSION.to_string();
    match top_at(lines.iter().map(String::as_str), "config-version") {
        Some(at) => {
            if let Some(l) = lines.get_mut(at)
                && let Some((_, written, after)) = split_key(l)
            {
                let comment = trailing_comment(after);
                let value = after
                    .get(..after.len() - comment.len())
                    .unwrap_or_default()
                    .trim();
                if value != version {
                    *l = format!("{written}: {version}{comment}");
                }
            }
        }
        None => {
            let first = lines
                .iter()
                .position(|l| {
                    let t = l.trim();
                    !t.is_empty() && !t.starts_with('#') && t != "---"
                })
                .unwrap_or(lines.len());
            if first < lines.len() {
                lines.insert(first, String::new());
            }
            lines.insert(first, format!("config-version: {version}"));
        }
    }
}

/// `translation.chain` without [`OLD_PROVIDER`]; `rest` is what stays. A
/// list on one line is written anew; in a block list only the item lines of
/// the old provider go, so the other items keep their comments. `[]` when
/// nothing is left. Refused when no line of a block list names the old
/// provider as written (an escape, a tag), so nothing is half done.
fn drop_from_chain(lines: &mut Vec<String>, rest: &[String]) -> Result<(), String> {
    let by_hand = || format!("remove {OLD_PROVIDER} from translation.chain by hand");
    let top = top_at(lines.iter().map(String::as_str), "translation").ok_or_else(by_hand)?;
    let inline = lines
        .get(top)
        .and_then(|l| split_key(l))
        .is_some_and(|(_, _, r)| !r.trim().is_empty() && !r.trim().starts_with('#'));
    if inline {
        return Err(by_hand());
    }
    let at = lines
        .iter()
        .enumerate()
        .skip(top + 1)
        .take_while(|(_, l)| indent_of(l) > 0 || l.trim().is_empty() || top_comment(l))
        .find(|(_, l)| indent_of(l) > 0 && split_key(l).is_some_and(|(k, _, _)| k == "chain"))
        .map(|(i, _)| i)
        .ok_or_else(by_hand)?;
    let line = lines.get(at).cloned().unwrap_or_default();
    let (_, written, after) = split_key(&line).ok_or_else(by_hand)?;
    let comment = trailing_comment(after);
    let value = after
        .get(..after.len() - comment.len())
        .unwrap_or_default()
        .trim();
    let indent = indent_of(&line);
    let rewritten = format!(
        "{}{written}: {}{comment}",
        " ".repeat(indent),
        flow_list(rest)
    );
    if value.starts_with('[') && value.ends_with(']') {
        if let Some(l) = lines.get_mut(at) {
            *l = rewritten;
        }
        return Ok(());
    }
    if !value.is_empty() {
        return Err(by_hand());
    }
    // A block list: its items, with comments and blank lines between them.
    let mut removed = 0;
    let mut i = at + 1;
    while let Some(l) = lines.get(i) {
        let t = l.trim();
        let item = indent_of(l) >= indent && (t == "-" || t.starts_with("- "));
        if !(item || t.is_empty() || t.starts_with('#') || indent_of(l) > indent) {
            break;
        }
        if item && item_name(t) == OLD_PROVIDER {
            lines.remove(i);
            removed += 1;
        } else {
            i += 1;
        }
    }
    if removed == 0 {
        return Err(by_hand());
    }
    if rest.is_empty()
        && let Some(l) = lines.get_mut(at)
    {
        *l = rewritten;
    }
    Ok(())
}

/// The value of a block list item (`- "name"  # comment` gives `name`).
fn item_name(item: &str) -> &str {
    let v = item.strip_prefix('-').unwrap_or(item).trim_start();
    let v = v
        .get(..v.len() - trailing_comment(v).len())
        .unwrap_or(v)
        .trim();
    v.strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .or_else(|| v.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')))
        .unwrap_or(v)
}

/// Cuts the top-level [`OLD_PROVIDER`] section: its key, the lines indented
/// under it (with the comments between them, also a key commented out in
/// column 0) and the comments right above it (no blank line between). Blank
/// lines and comments after its last indented line belong to the next key
/// and stay. Blank lines do not pile up where it was, nor at the end of the
/// file.
fn drop_section(lines: &mut Vec<String>) -> Result<(), String> {
    let at = top_at(lines.iter().map(String::as_str), OLD_PROVIDER).ok_or_else(|| {
        format!("{OLD_PROVIDER} is not written as a key on its own line; remove it by hand")
    })?;
    let mut last = at;
    for (i, l) in lines.iter().enumerate().skip(at + 1) {
        if l.trim().is_empty() || top_comment(l) {
            continue;
        }
        if indent_of(l) == 0 {
            break;
        }
        last = i;
    }
    let mut first = at;
    while first > 0 && lines.get(first - 1).is_some_and(|l| top_comment(l)) {
        first -= 1;
    }
    lines.drain(first..=last);
    let blank = |lines: &[String], i: usize| lines.get(i).is_some_and(|l| l.trim().is_empty());
    while blank(lines, first) && (first == 0 || blank(lines, first - 1)) {
        lines.remove(first);
    }
    while first == lines.len() && first > 0 && blank(lines, first - 1) {
        first -= 1;
        lines.remove(first);
    }
    Ok(())
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

    #[test]
    fn the_version_line_is_found_after_lines_move() {
        let p = OLD_PROVIDER;
        let text = format!(
            "translation:\n  chain:\n    - multiversion\n    - {p}\n    - passthrough\n\n{p}:\n  address: \"127.0.0.1:25568\"\n\nconfig-version: 1\npumbox:\n  enabled: true\nlogging:\n  level: info\n"
        );
        let (out, changes) = migrate_text(&text).unwrap().unwrap();
        assert_eq!(
            out,
            "translation:\n  chain:\n    - multiversion\n    - passthrough\n\nconfig-version: 2\npumbox:\n  enabled: true\nlogging:\n  level: info\n"
        );
        assert_eq!(
            changes,
            [
                format!("line 2: translation.chain no longer names {p}"),
                format!("line 7: the {p} section is removed"),
            ]
        );
        assert_eq!(migrate_text(&out).unwrap(), None);
    }

    #[test]
    fn the_old_provider_section_goes_alone() {
        let p = OLD_PROVIDER;
        let text = format!(
            "pumbox:\n  enabled: true\n\n# The external translator\n{p}:\n  address: x\n  # its port\n  route: below-native\n\n# Storage\nstorage:\n  backend: memory\n# the end\n"
        );
        let (out, changes) = migrate_text(&text).unwrap().unwrap();
        assert_eq!(
            out,
            "config-version: 2\n\npumbox:\n  enabled: true\n\n# Storage\nstorage:\n  backend: memory\n# the end\n"
        );
        assert_eq!(changes, [format!("line 5: the {p} section is removed")]);
        // The last section: no blank line is left at the end.
        let last = format!("pumbox:\n  enabled: true\n\n{p}:\n  address: x\n");
        let (out, _) = migrate_text(&last).unwrap().unwrap();
        assert_eq!(out, "config-version: 2\n\npumbox:\n  enabled: true\n");
    }

    #[test]
    fn only_the_old_provider_leaves_the_chain() {
        let p = OLD_PROVIDER;
        let migrated = |text: String| migrate_text(&text).unwrap().unwrap().0;
        // A block list: one item line goes, the comments and the blank line stay.
        assert_eq!(
            migrated(format!(
                "translation:\n  chain:   # order matters\n    # in the proxy first\n    - multiversion\n    - {p}   # gone\n\n    - passthrough\nlogging:\n  level: info\n"
            )),
            "config-version: 2\n\ntranslation:\n  chain:   # order matters\n    # in the proxy first\n    - multiversion\n\n    - passthrough\nlogging:\n  level: info\n"
        );
        // A list on one line keeps its comment.
        assert_eq!(
            migrated(format!(
                "translation:\n  chain: [multiversion, \"{p}\", passthrough]  # keep\n"
            )),
            "config-version: 2\n\ntranslation:\n  chain: [multiversion, passthrough]  # keep\n"
        );
        // Nothing left: an empty list, as a list on one line.
        assert_eq!(
            migrated(format!("translation:\n  chain: [{p}]\n")),
            "config-version: 2\n\ntranslation:\n  chain: []\n"
        );
        let (_, changes) = migrate_text(&format!("translation:\n  chain: [{p}]\n"))
            .unwrap()
            .unwrap();
        assert_eq!(
            changes.last().map(String::as_str),
            Some("warning: translation.chain is empty, so no player can join: add passthrough")
        );
        assert_eq!(
            migrated(format!("translation:\n  chain:   # was\n    - '{p}'\n")),
            "config-version: 2\n\ntranslation:\n  chain: []   # was\n"
        );
        // Both old keys in one run, line ends kept.
        let both = format!(
            "bungeecord-channel:\r\n  subchannels: [Connect]\r\ntranslation:\r\n  chain:\r\n    - {p}\r\n    - passthrough\r\n"
        );
        let (out, changes) = migrate_text(&both).unwrap().unwrap();
        assert_eq!(
            out,
            "config-version: 2\r\n\r\nmessaging-channel:\r\n  subchannels: [Connect]\r\ntranslation:\r\n  chain:\r\n    - passthrough\r\n"
        );
        assert_eq!(
            changes,
            [
                "line 1: bungeecord-channel is now messaging-channel".to_string(),
                format!("line 4: translation.chain no longer names {p}"),
            ]
        );
        assert_eq!(migrate_text(&out).unwrap(), None);
    }

    #[test]
    fn the_old_provider_by_hand_where_the_file_is_flow() {
        let p = OLD_PROVIDER;
        for text in [
            format!("translation: {{chain: [{p}]}}\n"),
            format!("{{translation: {{chain: [{p}]}}}}\n"),
            format!("translation:\n  chain: [multiversion,\n    {p}]\n"),
            format!("{{{p}: {{}}}}\n"),
        ] {
            let err = migrate_text(&text).unwrap_err();
            assert!(err.contains("by hand"), "{text}: {err}");
        }
    }

    #[test]
    fn a_key_commented_out_in_the_section_goes_with_it() {
        let p = OLD_PROVIDER;
        let text = format!(
            "logging:\n  level: info\n{p}:\n  address: \"127.0.0.1:25568\"\n#  route: below-native\n  min-native-protocol: 767\npumbox:\n  enabled: true\n"
        );
        let (out, _) = migrate_text(&text).unwrap().unwrap();
        assert_eq!(
            out,
            "config-version: 2\n\nlogging:\n  level: info\npumbox:\n  enabled: true\n"
        );
    }

    #[test]
    fn an_item_is_removed_only_as_written() {
        let p = OLD_PROVIDER;
        // A tab before the comment still ends the item.
        let (out, _) = migrate_text(&format!(
            "translation:\n  chain:\n    - {p}\t# gone\n    - passthrough\n"
        ))
        .unwrap()
        .unwrap();
        assert_eq!(
            out,
            "config-version: 2\n\ntranslation:\n  chain:\n    - passthrough\n"
        );
        // An item that names it only through an escape: nothing is written.
        let escaped = p.replacen('p', "\\x70", 1);
        let err = migrate_text(&format!(
            "translation:\n  chain:\n    - \"{escaped}\"\n    - passthrough\n"
        ))
        .unwrap_err();
        assert!(err.contains("by hand"), "{err}");
    }

    #[test]
    fn the_version_line_keeps_its_comment() {
        let p = OLD_PROVIDER;
        for version in ["1", "2"] {
            let text = format!("config-version: {version}   # set by hand\n{p}:\n  address: x\n");
            assert_eq!(
                migrate_text(&text).unwrap().unwrap().0,
                "config-version: 2   # set by hand\n",
                "{version}"
            );
        }
    }

    #[test]
    fn blank_lines_do_not_pile_up() {
        let p = OLD_PROVIDER;
        // Only the section: the version is all that is left.
        assert_eq!(
            migrate_text(&format!("{p}:\n  address: x\n\n"))
                .unwrap()
                .unwrap()
                .0,
            "config-version: 2\n"
        );
        // Blank lines on both sides become one.
        assert_eq!(
            migrate_text(&format!(
                "pumbox:\n  enabled: true\n\n{p}:\n  address: x\n\n\nlogging:\n  level: info\n"
            ))
            .unwrap()
            .unwrap()
            .0,
            "config-version: 2\n\npumbox:\n  enabled: true\n\nlogging:\n  level: info\n"
        );
        // A byte-order mark stays at the start.
        let (out, changes) =
            migrate_text("\u{feff}bungeecord-channel:\n  subchannels: [Connect]\n")
                .unwrap()
                .unwrap();
        assert_eq!(
            out,
            "\u{feff}config-version: 2\n\nmessaging-channel:\n  subchannels: [Connect]\n"
        );
        assert_eq!(
            changes,
            ["line 1: bungeecord-channel is now messaging-channel"]
        );
    }
}
