//! The look of the proxy's command answers: prefix, styles, buttons, usage lines.

use std::sync::OnceLock;

use pumbo_text::template::escape_mini as esc;
use pumbo_text::{Component, StyleSheet};

use crate::commands::Reply;

// ------------------------------------------------------------------ texts

/// Every message of this feature starts like this.
const PREFIX: &str = "<#F28C28>PumboProx</#F28C28> <dark_gray>»</dark_gray> ";

/// Style tags of the messages: `ok`, `err`, `warn`, `s` (secondary), `v` (a
/// value: name, version, port), `c` (a command), `muted`.
fn styles() -> &'static StyleSheet {
    static STYLES: OnceLock<StyleSheet> = OnceLock::new();
    STYLES.get_or_init(|| {
        StyleSheet::new([
            ("ok", "<green>"),
            ("err", "<red>"),
            ("warn", "<yellow>"),
            ("s", "<gray>"),
            ("v", "<white>"),
            ("c", "<#FFC27A>"),
            ("muted", "<dark_gray>"),
        ])
    })
}

/// A message with the prefix; `mini` uses the tags of [`styles`], values in
/// it are escaped with [`esc`].
pub(crate) fn line(mini: &str) -> Component {
    pumbo_text::parse_mini_styled(&format!("{PREFIX}{mini}"), styles())
}

/// A line under a message (lists), without the prefix.
pub(crate) fn row(mini: &str) -> Component {
    pumbo_text::parse_mini_styled(mini, styles())
}

pub(crate) fn reply(mini: &str) -> Reply {
    Reply {
        lines: vec![line(mini)],
        connect: None,
    }
}

/// `Usage: /send <player> <server>`; a click types the command.
pub(crate) fn usage(syntax: &str) -> Reply {
    let typed = syntax
        .find(['<', '[', '|'])
        .map_or(syntax, |i| syntax.get(..i).unwrap_or(syntax));
    let typed = typed.rsplit_once(' ').map_or(typed, |(a, _)| a);
    reply(&format!(
        "<s>Usage: <click:suggest_command:'{} '><hover:show_text:'<gray>Click to type it'><c>{}</c></hover></click>",
        typed.trim_end(),
        esc(syntax)
    ))
}

pub(crate) fn denied() -> Reply {
    reply("<err>You do not have permission to do that.")
}

/// A clickable command in brackets; the console gets nothing.
pub(crate) fn button(console: bool, label: &str, command: &str, run: bool) -> String {
    if console {
        return String::new();
    }
    let action = if run {
        "run_command"
    } else {
        "suggest_command"
    };
    let hover = if run { "Click to run" } else { "Click to type" };
    format!(
        " <click:{action}:'{command}'><hover:show_text:'<gray>{hover}: <#FFC27A>{}'><c>[{label}]</c></hover></click>",
        esc(command)
    )
}

/// `pumpkin` → `Pumpkin`.
pub(crate) fn title_case(s: &str) -> String {
    let mut c = s.chars();
    c.next()
        .map(|f| f.to_uppercase().chain(c).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_render_without_raw_codes() {
        let all = [
            line("<err>Usage: <c>/prox servers <v>x"),
            line(&format!(
                "<ok>Downloaded <v>pumpkin 0.2.0</v>, <s>SHA256 checked.{}",
                button(
                    false,
                    "create a server",
                    "/prox servers new <name> 0.2.0",
                    false
                )
            )),
            usage("/prox download <pumpkin> [list|#n|tag]")
                .lines
                .remove(0),
            row("<muted>#1</muted> <v>canary</v> <warn>development</warn>"),
        ];
        for c in all {
            let text = c.plain_text();
            assert!(
                text.starts_with("PumboProx » ") || text.starts_with("#1"),
                "{text}"
            );
            assert!(
                !text.contains('&') && !text.contains("<ok>") && !text.contains("<v>"),
                "{text}"
            );
        }
        assert!(
            usage("/prox servers new <name>").lines[0]
                .plain_text()
                .ends_with("/prox servers new <name>")
        );
        assert_eq!(title_case("pumpkin"), "Pumpkin");
    }
}
