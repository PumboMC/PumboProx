//! Text for players. The host encodes it for each client version and
//! resolves templates for each recipient.
//!
//! Text from a player (chat, command arguments, names) never goes into a
//! MiniMessage string directly: pass it as a template argument
//! ([`template`]) or escape it ([`escape`]).

use crate::bindings::pumbo::prox::types::{Text, TextTemplate};

pub fn plain(s: impl Into<String>) -> Text {
    Text::Plain(s.into())
}

/// MiniMessage with the network's style tags (`<p>`, `<s>`, `<ok>`,
/// `<warn>`, `<err>`, `<muted>`).
pub fn mini(s: impl Into<String>) -> Text {
    Text::Mini(s.into())
}

/// `&` codes.
pub fn legacy(s: impl Into<String>) -> Text {
    Text::Legacy(s.into())
}

/// A template with placeholders (`%namespace_key%`) and arguments
/// (`{name}`); arguments are inserted literally.
pub fn template(mini: impl Into<String>, args: &[(&str, &dyn std::fmt::Display)]) -> Text {
    Text::Template(TextTemplate {
        mini: mini.into(),
        args: args
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
    })
}

/// Escapes text so MiniMessage shows it literally (`<` and `\`). Use it for
/// player data inside placeholder values with formatting.
pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c == '<' || c == '\\' {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

impl From<&str> for Text {
    fn from(s: &str) -> Text {
        Text::Plain(s.to_string())
    }
}

impl From<String> for Text {
    fn from(s: String) -> Text {
        Text::Plain(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helpers() {
        assert_eq!(escape("<b>\\x"), "\\<b>\\\\x");
        let t = template("<p>Hi {name}", &[("name", &"<red>Bob")]);
        let Text::Template(t) = t else { unreachable!() };
        assert_eq!(t.args, vec![("name".to_string(), "<red>Bob".to_string())]);
        assert_eq!(Text::from("x"), Text::Plain("x".into()));
    }
}
