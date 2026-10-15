//! Tab completion from a table of usage lines, for [`crate::Plugin::on_suggest`].

/// Completions for the last word of `args` (as [`crate::Plugin::on_suggest`]
/// gets it) from usage lines without the command name, such as
/// `"user <player> parent add <group>"`. Pass only the lines the player may
/// use. A literal word completes as written; a `<slot>` or `[slot]` asks
/// `values` with the slot name (`player`, `group`); a slot ending in `...`
/// takes the rest of the line. Words compare without case; each answer comes
/// once, in table order.
///
/// ```
/// let table = ["user <player> parent add <group>", "user <player> info", "groups"];
/// let values = |slot: &str| match slot {
///     "player" => vec!["Steve".to_string()],
///     "group" => vec!["admin".to_string(), "vip".to_string()],
///     _ => Vec::new(),
/// };
/// let s = pumbo_sdk::suggest::from_table(table, "", values);
/// assert_eq!(s, ["user", "groups"]);
/// let s = pumbo_sdk::suggest::from_table(table, "user Steve parent add v", values);
/// assert_eq!(s, ["vip"]);
/// ```
pub fn from_table<'a>(
    table: impl IntoIterator<Item = &'a str>,
    args: &str,
    values: impl Fn(&str) -> Vec<String>,
) -> Vec<String> {
    let mut words: Vec<&str> = args.split(' ').collect();
    let last = words.pop().unwrap_or_default().to_lowercase();
    words.retain(|w| !w.is_empty());
    let mut out: Vec<String> = Vec::new();
    for line in table {
        let pattern: Vec<&str> = line.split_whitespace().collect();
        let Some(options) = next_word(&pattern, &words, &values) else {
            continue;
        };
        for o in options {
            if o.to_lowercase().starts_with(&last) && !out.contains(&o) {
                out.push(o);
            }
        }
    }
    out
}

/// What may follow `typed` in `pattern`, or `None` when it does not match.
fn next_word(
    pattern: &[&str],
    typed: &[&str],
    values: &impl Fn(&str) -> Vec<String>,
) -> Option<Vec<String>> {
    for (i, word) in typed.iter().enumerate() {
        let p = pattern.get(i)?;
        match slot(p) {
            Some((name, true)) => return Some(values(name)),
            Some(_) => {}
            None if p.eq_ignore_ascii_case(word) => {}
            None => return None,
        }
    }
    let p = pattern.get(typed.len())?;
    Some(match slot(p) {
        Some((name, _)) => values(name),
        None => vec![(*p).to_string()],
    })
}

/// `<name>` or `[name]`: the name and whether it takes the rest (`name...`).
fn slot(word: &str) -> Option<(&str, bool)> {
    let inner = word
        .strip_prefix('<')
        .and_then(|w| w.strip_suffix('>'))
        .or_else(|| word.strip_prefix('[').and_then(|w| w.strip_suffix(']')))?;
    Some(match inner.strip_suffix("...") {
        Some(name) => (name, true),
        None => (inner, false),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn values(slot: &str) -> Vec<String> {
        match slot {
            "player" => vec!["Steve".into(), "Alex".into()],
            "group" => vec!["admin".into(), "vip".into()],
            "reason" => vec!["spam".into()],
            _ => Vec::new(),
        }
    }

    const TABLE: [&str; 5] = [
        "user <player> parent add <group>",
        "user <player> parent remove <group>",
        "user <player> info",
        "ban <player> [reason...]",
        "groups",
    ];

    #[test]
    fn from_table_completes_words_and_slots() {
        let s = |args: &str| from_table(TABLE, args, values);
        assert_eq!(s(""), ["user", "ban", "groups"]);
        assert_eq!(s("G"), ["groups"]);
        assert_eq!(s("user "), ["Steve", "Alex"]);
        assert_eq!(s("USER st"), ["Steve"]);
        assert_eq!(s("user Steve "), ["parent", "info"]);
        assert_eq!(s("user Steve parent "), ["add", "remove"]);
        assert_eq!(s("user Steve parent add "), ["admin", "vip"]);
        assert_eq!(s("user  Steve parent add v"), ["vip"]);
        assert_eq!(s("ban Steve too much sp"), ["spam"]);
        assert!(s("user Steve info x").is_empty());
        assert!(s("track ").is_empty());
        // Only the lines the caller passes.
        assert_eq!(from_table(["groups"], "", values), ["groups"]);
    }
}
