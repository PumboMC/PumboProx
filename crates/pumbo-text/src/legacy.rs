//! Legacy formatting codes (`&c`, `&l`, `&r`, `&#RRGGBB`).

use crate::{Color, Component, Content, NamedColor, Style};

/// Parses text with `&` codes.
pub fn parse_legacy(input: &str) -> Component {
    parse_legacy_with(input, '&')
}

/// Parses text with formatting codes introduced by `marker` (`&` or `§`).
///
/// Colors 0–9 and a–f and `#RRGGBB` reset the decorations, as in the game;
/// `k` obfuscated, `l` bold, `m` strikethrough, `n` underlined, `o` italic,
/// `r` resets everything. Unknown codes stay as text.
pub fn parse_legacy_with(input: &str, marker: char) -> Component {
    let mut parts: Vec<Component> = Vec::new();
    let mut style = Style::default();
    let mut buf = String::new();
    let mut chars = input.chars().peekable();
    let flush = |buf: &mut String, style: &Style, parts: &mut Vec<Component>| {
        if !buf.is_empty() {
            parts.push(Component {
                content: Content::Text(std::mem::take(buf)),
                style: style.clone(),
                extra: Vec::new(),
            });
        }
    };
    while let Some(c) = chars.next() {
        if c != marker {
            buf.push(c);
            continue;
        }
        let Some(&code) = chars.peek() else {
            buf.push(c);
            continue;
        };
        if code == '#' {
            let hex: String = chars.clone().skip(1).take(6).collect();
            if hex.len() == 6
                && hex.chars().all(|h| h.is_ascii_hexdigit())
                && let Ok(rgb) = u32::from_str_radix(&hex, 16)
            {
                flush(&mut buf, &style, &mut parts);
                style = Style {
                    color: Some(Color::Rgb(rgb)),
                    ..Style::default()
                };
                for _ in 0..7 {
                    chars.next();
                }
                continue;
            }
            buf.push(c);
            continue;
        }
        let lower = code.to_ascii_lowercase();
        if let Some(color) = NamedColor::from_code(lower) {
            flush(&mut buf, &style, &mut parts);
            style = Style {
                color: Some(Color::Named(color)),
                ..Style::default()
            };
            chars.next();
            continue;
        }
        let decoration: Option<fn(&mut Style)> = match lower {
            'k' => Some(|s| s.obfuscated = Some(true)),
            'l' => Some(|s| s.bold = Some(true)),
            'm' => Some(|s| s.strikethrough = Some(true)),
            'n' => Some(|s| s.underlined = Some(true)),
            'o' => Some(|s| s.italic = Some(true)),
            'r' => Some(|s| *s = Style::default()),
            _ => None,
        };
        match decoration {
            Some(apply) => {
                flush(&mut buf, &style, &mut parts);
                apply(&mut style);
                chars.next();
            }
            None => buf.push(c),
        }
    }
    flush(&mut buf, &style, &mut parts);
    collapse(parts)
}

/// One part as is; several under an empty root.
pub(crate) fn collapse(mut parts: Vec<Component>) -> Component {
    match parts.len() {
        0 => Component::text(""),
        1 => parts.pop().unwrap_or_default(),
        _ => Component {
            content: Content::Text(String::new()),
            style: Style::default(),
            extra: parts,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colors_and_decorations() {
        let c = parse_legacy("&cRed &lbold&r plain &#00ff00green & done&");
        assert_eq!(c.plain_text(), "Red bold plain green & done&");
        let parts = &c.extra;
        assert_eq!(parts.len(), 4);
        assert_eq!(parts[0].style.color, Some(Color::Named(NamedColor::Red)));
        assert_eq!(parts[0].style.bold, None);
        assert_eq!(parts[1].style.bold, Some(true));
        assert_eq!(parts[1].style.color, Some(Color::Named(NamedColor::Red)));
        assert!(parts[2].style.is_empty());
        assert_eq!(parts[3].style.color, Some(Color::Rgb(0x00FF00)));
    }

    #[test]
    fn color_resets_decorations_and_unknown_codes_stay() {
        let c = parse_legacy("&l&abc&zx&#12");
        assert_eq!(c.plain_text(), "bc&zx&#12");
        assert_eq!(c.style.bold, None);
        assert_eq!(c.style.color, Some(Color::Named(NamedColor::Green)));
        let s = parse_legacy_with("§6gold", '§');
        assert_eq!(s.style.color, Some(Color::Named(NamedColor::Gold)));
        assert_eq!(parse_legacy("plain").as_plain_str(), Some("plain"));
        assert_eq!(parse_legacy("").as_plain_str(), Some(""));
    }
}
