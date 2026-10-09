//! A subset of the MiniMessage format (tag syntax as documented by Adventure).
//!
//! Supported tags: named colors (`<red>`, also `grey`/`dark_grey`), `<#RRGGBB>`,
//! `<color:...>` (`colour`, `c`), decorations `<bold>`/`<b>`, `<italic>`/`<i>`/`<em>`,
//! `<underlined>`/`<u>`, `<strikethrough>`/`<st>`, `<obfuscated>`/`<obf>` (with `!`
//! or `:false` to turn off), `<reset>`, `<newline>`/`<br>`, `<click:action:value>`,
//! `<hover:show_text:'text'>`, `<insert:...>`, `<font:...>` and closing tags
//! (`</red>`, `</>`). Arguments may be quoted with `'` or `"`. A backslash
//! escapes `<` and `\`. Unknown or malformed tags stay as literal text.

use crate::legacy::collapse;
use crate::{ClickEvent, Color, Component, Content, HoverEvent, NamedColor, Style};

/// Nested `<hover>` texts parsed at most this deep.
const MAX_HOVER_DEPTH: usize = 8;

/// Parses MiniMessage text.
pub fn parse_mini(input: &str) -> Component {
    parse_at(input, 0, None)
}

/// Style tags of the network (plan §6.6.1): `<p>`, `<s>`, `<ok>`, `<warn>`,
/// `<err>`, `<muted>`, defined once in the proxy config (`style`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StyleSheet {
    tags: Vec<(String, Style)>,
}

impl StyleSheet {
    /// Builds a sheet from tag name → MiniMessage opening tags, e.g.
    /// `("err", "<red><bold>")`. Unknown tags in a value are ignored.
    pub fn new<'a>(defs: impl IntoIterator<Item = (&'a str, &'a str)>) -> StyleSheet {
        let tags = defs
            .into_iter()
            .map(|(name, mini)| {
                (
                    name.to_ascii_lowercase(),
                    parse_mini(&format!("{mini}x")).style,
                )
            })
            .collect();
        StyleSheet { tags }
    }

    fn get(&self, name: &str) -> Option<&Style> {
        self.tags.iter().find(|(n, _)| n == name).map(|(_, s)| s)
    }
}

/// Parses MiniMessage text with the network's style tags.
pub fn parse_mini_styled(input: &str, styles: &StyleSheet) -> Component {
    parse_at(input, 0, Some(styles))
}

/// Removes click, hover and insertion tags (opening and closing) and keeps
/// everything else as written. Used for placeholder values from plugins
/// (plan §5.8.3).
pub fn strip_events(input: &str) -> String {
    let chars: Vec<char> = input.chars().collect();
    let mut out = String::with_capacity(input.len());
    let mut i = 0;
    while let Some(&c) = chars.get(i) {
        if c == '\\' {
            out.push(c);
            if let Some(&next) = chars.get(i + 1) {
                out.push(next);
            }
            i += 2;
            continue;
        }
        if c == '<'
            && let Some((end, body)) = read_tag(&chars, i)
        {
            let args = split_args(&body);
            let name = args
                .first()
                .map(|n| canonical(n.strip_prefix('/').unwrap_or(n)))
                .unwrap_or_default();
            if matches!(name.as_str(), "click" | "hover" | "insert") {
                i = end + 1;
                continue;
            }
        }
        out.push(c);
        i += 1;
    }
    out
}

struct Frame {
    /// Canonical tag name used to match the closing tag.
    name: String,
    is_color: bool,
    style: Style,
}

fn parse_at(input: &str, depth: usize, styles: Option<&StyleSheet>) -> Component {
    let mut parts: Vec<Component> = Vec::new();
    let mut frames: Vec<Frame> = Vec::new();
    let mut buf = String::new();
    let chars: Vec<char> = input.chars().collect();
    let mut i = 0;

    let current = |frames: &[Frame]| {
        frames
            .iter()
            .fold(Style::default(), |acc, f| acc.merged_with(&f.style))
    };
    let flush = |buf: &mut String, frames: &[Frame], parts: &mut Vec<Component>| {
        if !buf.is_empty() {
            parts.push(Component {
                content: Content::Text(std::mem::take(buf)),
                style: current(frames),
                extra: Vec::new(),
            });
        }
    };

    while let Some(&c) = chars.get(i) {
        if c == '\\' {
            match chars.get(i + 1) {
                Some(&next @ ('<' | '\\')) => {
                    buf.push(next);
                    i += 2;
                }
                _ => {
                    buf.push(c);
                    i += 1;
                }
            }
            continue;
        }
        if c != '<' {
            buf.push(c);
            i += 1;
            continue;
        }
        let Some((end, body)) = read_tag(&chars, i) else {
            buf.push(c);
            i += 1;
            continue;
        };
        let literal: String = chars
            .get(i..=end)
            .map(|s| s.iter().collect())
            .unwrap_or_default();
        i = end + 1;
        let args = split_args(&body);
        let Some(raw_name) = args.first() else {
            buf.push_str(&literal);
            continue;
        };

        if let Some(closing) = raw_name.strip_prefix('/') {
            let target = if closing.is_empty() {
                frames.len().checked_sub(1)
            } else {
                let wanted = canonical(closing);
                let color_close = matches!(wanted.as_str(), "color") || is_color_name(&wanted);
                frames.iter().rposition(|f| f.name == wanted).or_else(|| {
                    color_close
                        .then(|| frames.iter().rposition(|f| f.is_color))
                        .flatten()
                })
            };
            match target {
                Some(pos) => {
                    flush(&mut buf, &frames, &mut parts);
                    frames.truncate(pos);
                }
                None => buf.push_str(&literal),
            }
            continue;
        }

        let name = raw_name.to_ascii_lowercase();
        match name.as_str() {
            "reset" => {
                flush(&mut buf, &frames, &mut parts);
                frames.clear();
                continue;
            }
            "newline" | "br" => {
                buf.push('\n');
                continue;
            }
            _ => {}
        }
        if let Some(style) = styles
            .and_then(|s| s.get(&name))
            .filter(|_| args.len() == 1)
        {
            flush(&mut buf, &frames, &mut parts);
            frames.push(Frame {
                name: name.clone(),
                is_color: style.color.is_some(),
                style: style.clone(),
            });
            continue;
        }
        match open_tag(&name, &args, depth, styles) {
            Some((canonical_name, is_color, style)) => {
                flush(&mut buf, &frames, &mut parts);
                frames.push(Frame {
                    name: canonical_name,
                    is_color,
                    style,
                });
            }
            None => buf.push_str(&literal),
        }
    }
    flush(&mut buf, &frames, &mut parts);
    collapse(parts)
}

/// Finds the `>` that closes the tag starting at `start` (outside quotes).
/// Returns its index and the text between the brackets.
fn read_tag(chars: &[char], start: usize) -> Option<(usize, String)> {
    let mut body = String::new();
    let mut quote: Option<char> = None;
    let mut i = start + 1;
    while let Some(&c) = chars.get(i) {
        match quote {
            Some(q) => {
                if c == '\\'
                    && let Some(&next) = chars.get(i + 1)
                {
                    body.push(c);
                    body.push(next);
                    i += 2;
                    continue;
                }
                if c == q {
                    quote = None;
                }
                body.push(c);
            }
            None => match c {
                '>' => return (!body.is_empty()).then_some((i, body)),
                '<' => return None,
                '\'' | '"' => {
                    quote = Some(c);
                    body.push(c);
                }
                _ => body.push(c),
            },
        }
        i += 1;
    }
    None
}

/// Splits `name:arg:'quoted:arg'` on colons outside quotes; quotes removed,
/// `\'`, `\"` and `\\` unescaped inside them.
fn split_args(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut it = body.chars().peekable();
    while let Some(c) = it.next() {
        match quote {
            Some(q) => {
                if c == '\\'
                    && let Some(&next) = it.peek()
                    && (next == q || next == '\\')
                {
                    cur.push(next);
                    it.next();
                    continue;
                }
                if c == q {
                    quote = None;
                } else {
                    cur.push(c);
                }
            }
            None => match c {
                ':' => out.push(std::mem::take(&mut cur)),
                '\'' | '"' if cur.is_empty() => quote = Some(c),
                _ => cur.push(c),
            },
        }
    }
    out.push(cur);
    out
}

fn named_color(name: &str) -> Option<NamedColor> {
    let name = match name {
        "grey" => "gray",
        "dark_grey" => "dark_gray",
        other => other,
    };
    NamedColor::from_name(name)
}

fn is_color_name(name: &str) -> bool {
    named_color(name).is_some() || (name.starts_with('#') && Color::parse(name).is_some())
}

/// Canonical name for matching closing tags.
fn canonical(name: &str) -> String {
    let name = name.to_ascii_lowercase();
    let name = name.strip_prefix('!').unwrap_or(&name).to_string();
    match name.as_str() {
        "b" => "bold".into(),
        "i" | "em" => "italic".into(),
        "u" => "underlined".into(),
        "st" => "strikethrough".into(),
        "obf" => "obfuscated".into(),
        "colour" | "c" => "color".into(),
        "grey" => "gray".into(),
        "dark_grey" => "dark_gray".into(),
        "insertion" => "insert".into(),
        _ => name,
    }
}

/// Style of an opening tag: (canonical name, is a color, style).
fn open_tag(
    name: &str,
    args: &[String],
    depth: usize,
    styles: Option<&StyleSheet>,
) -> Option<(String, bool, Style)> {
    let arg = |n: usize| args.get(n).map(String::as_str);
    // Everything from argument `n` on, so values may contain colons.
    let rest = |n: usize| (args.len() > n).then(|| args.get(n..).unwrap_or_default().join(":"));
    let negated = name.starts_with('!');
    let key = canonical(name);
    let mut style = Style::default();
    match key.as_str() {
        "bold" | "italic" | "underlined" | "strikethrough" | "obfuscated" => {
            let on = !negated && arg(1) != Some("false");
            let slot = match key.as_str() {
                "bold" => &mut style.bold,
                "italic" => &mut style.italic,
                "underlined" => &mut style.underlined,
                "strikethrough" => &mut style.strikethrough,
                _ => &mut style.obfuscated,
            };
            *slot = Some(on);
            Some((key, false, style))
        }
        "color" => {
            let value = arg(1)?.to_ascii_lowercase();
            style.color = Some(parse_color(&value)?);
            Some((key, true, style))
        }
        "click" => {
            let action = arg(1)?.to_ascii_lowercase();
            let value = rest(2)?;
            style.click_event = Some(match action.as_str() {
                "open_url" => ClickEvent::OpenUrl(value),
                "open_file" => ClickEvent::OpenFile(value),
                "run_command" => ClickEvent::RunCommand(value),
                "suggest_command" => ClickEvent::SuggestCommand(value),
                "copy_to_clipboard" => ClickEvent::CopyToClipboard(value),
                "change_page" => ClickEvent::ChangePage(value.trim().parse().ok()?),
                _ => return None,
            });
            Some((key, false, style))
        }
        "hover" => {
            if !arg(1)?.eq_ignore_ascii_case("show_text") || depth >= MAX_HOVER_DEPTH {
                return None;
            }
            let text = parse_at(&rest(2)?, depth + 1, styles);
            style.hover_event = Some(HoverEvent::ShowText(Box::new(text)));
            Some((key, false, style))
        }
        "insert" => {
            style.insertion = Some(rest(1)?);
            Some((key, false, style))
        }
        "font" => {
            style.font = Some(rest(1)?);
            Some((key, false, style))
        }
        other if args.len() == 1 && !negated => {
            let color = parse_color(other)?;
            style.color = Some(color);
            Some((key, true, style))
        }
        _ => None,
    }
}

fn parse_color(s: &str) -> Option<Color> {
    if s.starts_with('#') {
        return Color::parse(s);
    }
    named_color(s).map(Color::Named)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn styles(c: &Component) -> Vec<(String, Style)> {
        let mut out = Vec::new();
        if let Content::Text(t) = &c.content
            && !t.is_empty()
        {
            out.push((t.clone(), c.style.clone()));
        }
        for e in &c.extra {
            if let Content::Text(t) = &e.content {
                out.push((t.clone(), e.style.clone()));
            }
        }
        out
    }

    #[test]
    fn colors_decorations_and_closing() {
        let c = parse_mini("<red>Hello <bold>world</bold>!</red> plain");
        let s = styles(&c);
        assert_eq!(s.len(), 4);
        assert_eq!(s[0].0, "Hello ");
        assert_eq!(s[0].1.color, Some(Color::Named(NamedColor::Red)));
        assert_eq!(s[1].1.bold, Some(true));
        assert_eq!(s[1].1.color, Some(Color::Named(NamedColor::Red)));
        assert_eq!(s[2].0, "!");
        assert_eq!(s[2].1.bold, None);
        assert!(s[3].1.is_empty());
        assert_eq!(c.plain_text(), "Hello world! plain");
    }

    #[test]
    fn hex_aliases_negation_reset_newline() {
        let c = parse_mini("<#00ff00>a<c:grey>b</c><!i>c<reset>d<br>e");
        let s = styles(&c);
        assert_eq!(s[0].1.color, Some(Color::Rgb(0x00FF00)));
        assert_eq!(s[1].1.color, Some(Color::Named(NamedColor::Gray)));
        assert_eq!(s[2].1.color, Some(Color::Rgb(0x00FF00)));
        assert_eq!(s[2].1.italic, Some(false));
        assert!(s[3].1.is_empty());
        assert_eq!(c.plain_text(), "abcd\ne");
    }

    #[test]
    fn click_hover_insert_font() {
        let c = parse_mini(
            "<click:run_command:'/server lobby'><hover:show_text:'<gold>Go to <b>lobby'>Lobby</hover></click>",
        );
        assert_eq!(
            c.style.click_event,
            Some(ClickEvent::RunCommand("/server lobby".into()))
        );
        let Some(HoverEvent::ShowText(t)) = &c.style.hover_event else {
            panic!("no hover");
        };
        assert_eq!(t.plain_text(), "Go to lobby");
        assert_eq!(c.plain_text(), "Lobby");
        let c = parse_mini("<insert:hi><font:minecraft:uniform>x");
        assert_eq!(c.style.insertion.as_deref(), Some("hi"));
        assert_eq!(c.style.font.as_deref(), Some("minecraft:uniform"));
        let c = parse_mini("<click:open_url:https://example.com/a>x");
        assert_eq!(
            c.style.click_event,
            Some(ClickEvent::OpenUrl("https://example.com/a".into()))
        );
        let c = parse_mini("<font:'minecraft:uniform'>x");
        assert_eq!(c.style.font.as_deref(), Some("minecraft:uniform"));
    }

    #[test]
    fn unknown_and_malformed_stay_literal() {
        assert_eq!(
            parse_mini("<unknown>x</nope>").plain_text(),
            "<unknown>x</nope>"
        );
        assert_eq!(parse_mini("a < b > c").plain_text(), "a < b > c");
        assert_eq!(parse_mini("<red").plain_text(), "<red");
        assert_eq!(parse_mini("\\<red>x").plain_text(), "<red>x");
        assert_eq!(parse_mini("a\\\\b\\c").plain_text(), "a\\b\\c");
        assert_eq!(parse_mini("<>").plain_text(), "<>");
        assert_eq!(parse_mini("<click:bad:x>y").plain_text(), "<click:bad:x>y");
        assert_eq!(
            parse_mini("<hover:show_text>y").plain_text(),
            "<hover:show_text>y"
        );
        assert_eq!(parse_mini("plain").as_plain_str(), Some("plain"));
    }

    #[test]
    fn style_tags_and_event_stripping() {
        let sheet = StyleSheet::new([("err", "<red><bold>"), ("p", "<#55ffff>")]);
        let c = parse_mini_styled("<err>no</err> <p>yes</p> <ok>x", &sheet);
        let s = styles(&c);
        assert_eq!(s[0].1.color, Some(Color::Named(NamedColor::Red)));
        assert_eq!(s[0].1.bold, Some(true));
        assert_eq!(s[2].1.color, Some(Color::Rgb(0x55FFFF)));
        assert_eq!(c.plain_text(), "no yes <ok>x");
        assert_eq!(parse_mini("<err>x").plain_text(), "<err>x");

        let v = strip_events(
            "<red><click:run_command:'/op me'>a</click><hover:show_text:'<b>h'>b</hover>\\<click:x>",
        );
        assert_eq!(v, "<red>ab\\<click:x>");
    }

    #[test]
    fn deep_hover_is_bounded() {
        let mut s = String::from("x");
        for _ in 0..20 {
            s = format!(
                "<hover:show_text:'{}'>y",
                s.replace('\\', "\\\\").replace('\'', "\\'")
            );
        }
        let _ = parse_mini(&s);
    }
}
