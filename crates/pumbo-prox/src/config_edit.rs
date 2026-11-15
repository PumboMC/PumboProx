//! Commands that change `pumboprox.yml`: only the changed key (the
//! admin's comments and layout stay), a safe write, and a reload that
//! applies it.

use crate::config::Config;
use crate::server::Proxy;

/// Writes the edited config, reloads, and puts the old file back when the
/// reload refuses the new one.
pub(crate) fn save(
    proxy: &Proxy,
    edit: impl FnOnce(&str) -> Result<String, String>,
) -> Result<(), String> {
    let path = proxy.config_path().ok_or("no config file")?.to_path_buf();
    crate::server::off_worker(|| {
        // Read, edit, write and reload as one step: two edits at once would
        // each start from the same file and one would be lost.
        let _one = proxy.config_lock();
        let old = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let new = edit(&old)?;
        Config::parse(&new).map_err(|e| format!("the change would break the config: {e}"))?;
        write(&path, &new)?;
        if let Err(e) = proxy.reload_locked() {
            let _ = write(&path, &old);
            return Err(format!("not applied: {e}"));
        }
        Ok(())
    })
}

/// A copy of the config before a rewrite: `<file>.bak`, or `.bak.1` and up
/// when that exists (an older copy is never replaced), with the file's mode.
pub fn backup(path: &std::path::Path, text: &str) -> Result<std::path::PathBuf, String> {
    use std::io::Write as _;
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    for n in 0..1000 {
        let bak = path.with_file_name(match n {
            0 => format!("{name}.bak"),
            n => format!("{name}.bak.{n}"),
        });
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
            let mode = std::fs::metadata(path).map_or(0o600, |m| m.permissions().mode());
            options.mode(mode & 0o777);
        }
        match options.open(&bak) {
            Ok(mut f) => {
                // Root's copy of the proxy's file stays readable by the proxy's user.
                #[cfg(unix)]
                if let Ok(m) = std::fs::metadata(path) {
                    use std::os::unix::fs::MetadataExt as _;
                    let _ = std::os::unix::fs::fchown(&f, Some(m.uid()), Some(m.gid()));
                }
                f.write_all(text.as_bytes())
                    .and_then(|()| f.sync_all())
                    .map_err(|e| format!("{}: {e}", bak.display()))?;
                return Ok(bak);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(format!("{}: {e}", bak.display())),
        }
    }
    Err(format!("{}: too many .bak copies", path.display()))
}

/// A new temporary file, never more open than the old file and with its
/// owner (a file rewritten by root stays the proxy's), written and synced,
/// then renamed over it. A link is followed: the file it points to is
/// replaced, the link stays.
pub fn write(path: &std::path::Path, text: &str) -> Result<(), String> {
    use std::io::Write as _;
    let target = std::fs::canonicalize(path);
    let path = target.as_deref().unwrap_or(path);
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = path.with_extension(format!("yml.{}.{n}.tmp", std::process::id()));
    let meta = std::fs::metadata(path).ok();
    let old = meta.as_ref().map(|m| m.permissions());
    let result = (|| {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        if let Some(p) = &old {
            use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
            options.mode(p.mode() & 0o777);
        }
        let mut file = options.open(&tmp)?;
        file.write_all(text.as_bytes())?;
        #[cfg(unix)]
        if let Some(m) = &meta {
            use std::os::unix::fs::MetadataExt as _;
            // Only root can give a file away; anyone else keeps their own.
            let _ = std::os::unix::fs::fchown(&file, Some(m.uid()), Some(m.gid()));
        }
        if let Some(p) = old {
            file.set_permissions(p)?;
        }
        file.sync_all()?;
        std::fs::rename(&tmp, path)
    })();
    if let Err(e) = result {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("{}: {e}", path.display()));
    }
    // The rename itself survives a power cut once the directory is synced.
    #[cfg(unix)]
    if let Some(dir) = path.parent() {
        let dir = if dir.as_os_str().is_empty() {
            std::path::Path::new(".")
        } else {
            dir
        };
        if let Ok(d) = std::fs::File::open(dir) {
            let _ = d.sync_all();
        }
    }
    Ok(())
}

// ------------------------------------------------------------------ YAML

/// A YAML scalar: plain when that reads back as the same text, else quoted.
fn scalar(s: &str) -> String {
    let plain = !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        && !matches!(
            s.to_ascii_lowercase().as_str(),
            "true" | "false" | "null" | "yes" | "no" | "on" | "off" | "y" | "n"
        )
        && s.parse::<f64>().is_err();
    if plain {
        s.to_string()
    } else {
        serde_json::to_string(s).unwrap_or_default()
    }
}

/// `[a, b]`.
pub fn flow_list(items: &[String]) -> String {
    format!(
        "[{}]",
        items
            .iter()
            .map(|i| scalar(i))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

pub(crate) fn indent_of(l: &str) -> usize {
    l.len() - l.trim_start().len()
}

fn blank_or_comment(l: &str) -> bool {
    let t = l.trim();
    t.is_empty() || t.starts_with('#')
}

/// `key: rest` of a mapping line: the key without quotes, the key as
/// written, and what follows the colon.
pub(crate) fn split_key(line: &str) -> Option<(String, &str, &str)> {
    let t = line.trim_start();
    let (key, written, rest) = match t.chars().next()? {
        q @ ('"' | '\'') => {
            let end = t.get(1..)?.find(q)? + 1;
            (
                t.get(1..end)?.to_string(),
                t.get(..=end)?,
                t.get(end + 1..)?,
            )
        }
        _ => {
            let i = t.find(':')?;
            (t.get(..i)?.trim().to_string(), t.get(..i)?, t.get(i..)?)
        }
    };
    let rest = rest.strip_prefix(':')?;
    (rest.is_empty() || rest.starts_with([' ', '\t'])).then_some((key, written, rest))
}

/// ` # comment` at the end of a value outside quotes, with its spaces.
fn trailing_comment(rest: &str) -> &str {
    let mut quote = None;
    let bytes = rest.as_bytes();
    for (i, c) in rest.char_indices() {
        match (quote, c) {
            (None, '"' | '\'') => quote = Some(c),
            (Some(q), c) if c == q => quote = None,
            (None, '#') if i > 0 && bytes.get(i - 1).is_some_and(|b| *b == b' ') => {
                let start = rest[..i].trim_end().len();
                return rest.get(start..).unwrap_or_default();
            }
            _ => {}
        }
    }
    ""
}

/// Sets (`Some`, a one-line value) or removes (`None`) `parent.key` of a
/// top-level block mapping and leaves every other line as it is: comments,
/// order, the comment after the old value. A missing parent is added at the
/// end. A parent written on one line (`routing: {try: [a]}`) is refused,
/// except an empty `{}`.
pub fn set_key(text: &str, parent: &str, key: &str, value: Option<&str>) -> Result<String, String> {
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    let top = lines
        .iter()
        .position(|l| indent_of(l) == 0 && split_key(l).is_some_and(|(k, _, _)| k == parent));
    let Some(top) = top else {
        let Some(v) = value else {
            return Ok(text.to_string());
        };
        let mut out = text.to_string();
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(&format!("{parent}:\n  {}: {v}\n", scalar(key)));
        return Ok(out);
    };
    let inline = split_key(lines.get(top).map_or("", |l| l.as_str()))
        .map(|(_, _, r)| {
            let r = r.trim();
            r.get(..r.len() - trailing_comment(r).len())
                .unwrap_or(r)
                .trim()
                .to_string()
        })
        .unwrap_or_default();
    match inline.as_str() {
        "" => {}
        "{}" | "null" | "~" => {
            if value.is_none() {
                return Ok(text.to_string());
            }
            if let Some(l) = lines.get_mut(top) {
                *l = format!("{parent}:");
            }
        }
        _ => {
            return Err(format!(
                "{parent} is written on one line, change it by hand"
            ));
        }
    }
    let end = (top + 1..lines.len())
        .find(|&i| {
            lines
                .get(i)
                .is_some_and(|l| !blank_or_comment(l) && indent_of(l) == 0)
        })
        .unwrap_or(lines.len());
    let child = |i: usize| lines.get(i).filter(|l| !blank_or_comment(l));
    let indent = (top + 1..end)
        .find_map(|i| child(i).map(|l| indent_of(l)))
        .unwrap_or(2);
    let at = (top + 1..end).find(|&i| {
        child(i).is_some_and(|l| {
            indent_of(l) == indent && split_key(l).is_some_and(|(k, _, _)| k == key)
        })
    });
    match (at, value) {
        (Some(i), v) => {
            // The old value's own lines: deeper ones, `- ` items at the key's indent.
            let mut j = i + 1;
            while let Some(l) = child(j).filter(|_| j < end) {
                let deeper = indent_of(l) > indent
                    || (indent_of(l) == indent && l.trim_start().starts_with("- "));
                if !deeper {
                    break;
                }
                j += 1;
            }
            match v {
                Some(v) => {
                    let old = lines.get(i).cloned().unwrap_or_default();
                    let (_, written, rest) = split_key(&old).ok_or("unreadable line")?;
                    let comment = if j == i + 1 {
                        trailing_comment(rest)
                    } else {
                        ""
                    };
                    let new = format!("{}{written}: {v}{comment}", " ".repeat(indent));
                    lines.splice(i..j, [new]);
                }
                None => {
                    let left = (top + 1..end)
                        .filter(|k| !(i..j).contains(k))
                        .any(|k| child(k).is_some());
                    lines.drain(i..j);
                    if !left && let Some(l) = lines.get_mut(top) {
                        *l = format!("{parent}: {{}}");
                    }
                }
            }
        }
        (None, Some(v)) => {
            let last = (top + 1..end)
                .rev()
                .find(|&i| {
                    lines
                        .get(i)
                        .is_some_and(|l| !l.trim().is_empty() && indent_of(l) > 0)
                })
                .unwrap_or(top);
            lines.insert(
                last + 1,
                format!("{}{}: {v}", " ".repeat(indent), scalar(key)),
            );
        }
        (None, None) => {}
    }
    let mut out = lines.join("\n");
    out.push('\n');
    Ok(out)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    const FILE: &str = "# My network\nlistener:\n  - bind: \"0.0.0.0:25565\"   # public\nservers:\n  lobby: { address: \"127.0.0.1:25566\" }\n  survival: { address: \"127.0.0.1:25567\" }\n\nrouting:\n  # players start here\n  try: [lobby]   # keep lobby first\n\n# the end\nlogging:\n  level: info\n";

    /// A config that is a link (to a file kept elsewhere) stays a link.
    #[cfg(unix)]
    #[test]
    fn a_linked_file_is_written_through_the_link() {
        let dir = std::env::temp_dir().join(format!("pumbo-route-link-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("real")).unwrap();
        let (real, link) = (dir.join("real/pumboprox.yml"), dir.join("pumboprox.yml"));
        std::fs::write(&real, "old\n").unwrap();
        std::os::unix::fs::symlink("real/pumboprox.yml", &link).unwrap();
        write(&link, "new\n").unwrap();
        assert!(std::fs::symlink_metadata(&link).unwrap().is_symlink());
        assert_eq!(std::fs::read_to_string(&real).unwrap(), "new\n");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_value_changes_and_the_rest_stays() {
        let out = set_key(FILE, "routing", "try", Some("[lobby, survival]")).unwrap();
        assert_eq!(
            out,
            FILE.replace(
                "try: [lobby]   # keep lobby first",
                "try: [lobby, survival]   # keep lobby first"
            )
        );
    }

    #[test]
    fn block_lists_become_one_line() {
        let text = "routing:\n  try:\n    - lobby\n    - survival\n  selector: try\nother: 1\n";
        let out = set_key(text, "routing", "try", Some("[survival]")).unwrap();
        assert_eq!(
            out,
            "routing:\n  try: [survival]\n  selector: try\nother: 1\n"
        );
        let compact = "routing:\n  try:\n  - lobby\n  - survival\n";
        let out = set_key(compact, "routing", "try", Some("[lobby]")).unwrap();
        assert_eq!(out, "routing:\n  try: [lobby]\n");
    }

    #[test]
    fn missing_keys_and_parents_are_added() {
        let out = set_key(FILE, "forced-hosts", "play.example.org", Some("[lobby]")).unwrap();
        assert!(out.starts_with(FILE));
        assert!(out.ends_with("forced-hosts:\n  play.example.org: [lobby]\n"));
        let out = set_key(&out, "forced-hosts", "mc.example.org", Some("[survival]")).unwrap();
        assert!(out.ends_with(
            "forced-hosts:\n  play.example.org: [lobby]\n  mc.example.org: [survival]\n"
        ));
        let out = set_key(FILE, "plugins", "required-gates", Some("[auth]")).unwrap();
        assert!(out.ends_with("plugins:\n  required-gates: [auth]\n"));
        let text = "plugins:\n  dir: plugins\n\n# comment of the next section\nlogging: {}\n";
        let out = set_key(text, "plugins", "required-gates", Some("[]")).unwrap();
        assert_eq!(
            out,
            "plugins:\n  dir: plugins\n  required-gates: []\n\n# comment of the next section\nlogging: {}\n"
        );
    }

    #[test]
    fn removing_the_last_entry_leaves_an_empty_map() {
        let text = "forced-hosts:\n  \"play.example.org\": [lobby]\nrouting:\n  try: [lobby]\n";
        let out = set_key(text, "forced-hosts", "play.example.org", None).unwrap();
        assert_eq!(out, "forced-hosts: {}\nrouting:\n  try: [lobby]\n");
        let back = set_key(&out, "forced-hosts", "a.org", Some("[lobby]")).unwrap();
        assert_eq!(
            back,
            "forced-hosts:\n  a.org: [lobby]\nrouting:\n  try: [lobby]\n"
        );
        assert_eq!(set_key(&out, "forced-hosts", "x", None).unwrap(), out);
    }

    #[test]
    fn the_first_operator_and_server_go_into_the_default_file() {
        let text = crate::config::DEFAULT_FILE;
        let id = "069a79f4-44e9-4726-a5be-fca90e38aaf5";
        let out = set_key(
            text,
            "commands",
            "operators",
            Some(&flow_list(&[id.into()])),
        )
        .unwrap();
        assert_eq!(
            out,
            text.replace("operators: []", &format!("operators: [{id}]"))
        );
        let out = set_key(&out, "routing", "try", Some("[arena]")).unwrap();
        assert!(out.ends_with("routing:\n  try: [arena]\n"), "{out}");
        let cfg = Config::parse(&out).unwrap();
        assert_eq!(cfg.commands.operators, [id]);
        assert_eq!(cfg.try_order(), ["arena"]);
        assert!(out.contains("# Player UUIDs with every proxy permission."));
    }

    #[test]
    fn one_line_parents_are_refused() {
        assert!(set_key("routing: { try: [lobby] }\n", "routing", "try", Some("[a]")).is_err());
    }

    #[test]
    fn scalars() {
        assert_eq!(
            flow_list(&["lobby".into(), "yes".into(), "a b".into()]),
            "[lobby, \"yes\", \"a b\"]"
        );
        assert_eq!(scalar("25565"), "\"25565\"");
    }
}
