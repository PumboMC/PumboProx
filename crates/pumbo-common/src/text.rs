//! The text format shared by every Pumbo plugin: style codes and placeholders.
//!
//! Style codes (case-insensitive):
//! - `&0`–`&9`, `&a`–`&f`: the 16 chat colours; a colour also resets the formats
//! - `&l` bold, `&o` italic, `&n` underlined, `&m` strikethrough, `&k` obfuscated
//! - `&r`: back to the base style (plain text, or the tone of the message, see
//!   [`parse_with`])
//! - `&#rrggbb`: any colour
//! - `&&`: a literal `&`; `&` followed by anything else stays as written
//!
//! Placeholders, filled by [`fill`]:
//! - `{0}`, `{1}`, ...: positional arguments
//! - `{name}`: named arguments (letters, digits, `-`, `_`, `.`)
//! - `{{` and `}}`: literal braces; an unknown placeholder stays as written
//!
//! Filling is a single pass, so an argument that contains `{0}` is never expanded
//! again. Arguments are inserted as they are: pass text typed by players through
//! [`escape`] first so that their `&` cannot change the style.

use std::fmt;

/// Arguments for [`fill`].
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Args {
    positional: Vec<String>,
    named: Vec<(String, String)>,
}

impl Args {
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends the next positional argument (`{0}`, then `{1}`, ...).
    pub fn arg(mut self, value: impl fmt::Display) -> Self {
        self.positional.push(value.to_string());
        self
    }

    /// Sets a named argument (`{name}`). A later value for the same name wins.
    pub fn with(mut self, name: &str, value: impl fmt::Display) -> Self {
        self.set(name, value);
        self
    }

    pub fn set(&mut self, name: &str, value: impl fmt::Display) {
        let value = value.to_string();
        match self.named.iter_mut().find(|(n, _)| n == name) {
            Some(slot) => slot.1 = value,
            None => self.named.push((name.to_string(), value)),
        }
    }

    pub fn has(&self, name: &str) -> bool {
        self.named.iter().any(|(n, _)| n == name)
    }

    fn lookup(&self, key: &str) -> Option<&str> {
        if !key.is_empty() && key.bytes().all(|b| b.is_ascii_digit()) {
            return key.parse::<usize>().ok().and_then(|i| self.positional.get(i)).map(String::as_str);
        }
        self.named.iter().find(|(n, _)| n == key).map(|(_, v)| v.as_str())
    }
}

impl<S: fmt::Display> From<&[S]> for Args {
    fn from(values: &[S]) -> Self {
        Self { positional: values.iter().map(ToString::to_string).collect(), named: Vec::new() }
    }
}

impl<S: fmt::Display, const N: usize> From<[S; N]> for Args {
    fn from(values: [S; N]) -> Self {
        Self { positional: values.iter().map(ToString::to_string).collect(), named: Vec::new() }
    }
}

fn is_name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')
}

/// Replaces the placeholders of `template`.
pub fn fill(template: &str, args: &Args) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(pos) = rest.find(['{', '}']) {
        out.push_str(rest.get(..pos).unwrap_or(""));
        let tail = rest.get(pos..).unwrap_or("");
        if let Some(after) = tail.strip_prefix("{{").or_else(|| tail.strip_prefix("}}")) {
            out.push_str(tail.get(..1).unwrap_or(""));
            rest = after;
            continue;
        }
        if let Some(body) = tail.strip_prefix('{')
            && let Some(end) = body.find('}')
        {
            let key = body.get(..end).unwrap_or("");
            if !key.is_empty() && key.chars().all(is_name_char) {
                match args.lookup(key) {
                    Some(value) => out.push_str(value),
                    None => {
                        out.push('{');
                        out.push_str(key);
                        out.push('}');
                    }
                }
                rest = body.get(end + 1..).unwrap_or("");
                continue;
            }
        }
        // A lone brace.
        out.push_str(tail.get(..1).unwrap_or(""));
        rest = tail.get(1..).unwrap_or("");
    }
    out.push_str(rest);
    out
}

/// Doubles every `&`, so that the text shows exactly as typed.
pub fn escape(text: &str) -> String {
    text.replace('&', "&&")
}

/// The 16 named chat colours.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Named {
    Black,
    DarkBlue,
    DarkGreen,
    DarkAqua,
    DarkRed,
    DarkPurple,
    Gold,
    Gray,
    DarkGray,
    Blue,
    Green,
    Aqua,
    Red,
    LightPurple,
    Yellow,
    White,
}

const NAMED: [(Named, &str, u32); 16] = [
    (Named::Black, "black", 0x000000),
    (Named::DarkBlue, "dark_blue", 0x0000AA),
    (Named::DarkGreen, "dark_green", 0x00AA00),
    (Named::DarkAqua, "dark_aqua", 0x00AAAA),
    (Named::DarkRed, "dark_red", 0xAA0000),
    (Named::DarkPurple, "dark_purple", 0xAA00AA),
    (Named::Gold, "gold", 0xFFAA00),
    (Named::Gray, "gray", 0xAAAAAA),
    (Named::DarkGray, "dark_gray", 0x555555),
    (Named::Blue, "blue", 0x5555FF),
    (Named::Green, "green", 0x55FF55),
    (Named::Aqua, "aqua", 0x55FFFF),
    (Named::Red, "red", 0xFF5555),
    (Named::LightPurple, "light_purple", 0xFF55FF),
    (Named::Yellow, "yellow", 0xFFFF55),
    (Named::White, "white", 0xFFFFFF),
];

impl Named {
    /// Colour for a code character `0`-`9`, `a`-`f`.
    pub fn from_code(c: char) -> Option<Self> {
        let i = c.to_digit(16)?;
        NAMED.get(i as usize).map(|(n, _, _)| *n)
    }

    /// Code character (`0`-`f`).
    pub fn code(self) -> char {
        let i = NAMED.iter().position(|(n, _, _)| *n == self).unwrap_or(15);
        char::from_digit(i as u32, 16).unwrap_or('f')
    }

    /// Name used in JSON text components (`dark_red`).
    pub fn name(self) -> &'static str {
        NAMED.iter().find(|(n, _, _)| *n == self).map(|(_, s, _)| *s).unwrap_or("white")
    }

    /// Colour as `0xRRGGBB`.
    pub fn rgb(self) -> u32 {
        NAMED.iter().find(|(n, _, _)| *n == self).map(|(_, _, c)| *c).unwrap_or(0xFFFFFF)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Color {
    Named(Named),
    /// `0xRRGGBB`.
    Rgb(u32),
}

impl Color {
    pub fn rgb(self) -> u32 {
        match self {
            Color::Named(n) => n.rgb(),
            Color::Rgb(c) => c,
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Style {
    pub color: Option<Color>,
    pub bold: bool,
    pub italic: bool,
    pub underlined: bool,
    pub strikethrough: bool,
    pub obfuscated: bool,
}

impl Style {
    /// Plain text in `color`.
    pub const fn colored(color: Color) -> Self {
        Self {
            color: Some(color),
            bold: false,
            italic: false,
            underlined: false,
            strikethrough: false,
            obfuscated: false,
        }
    }

    /// The same style in bold.
    pub const fn bolded(mut self) -> Self {
        self.bold = true;
        self
    }

    /// The same style in italics.
    pub const fn italicized(mut self) -> Self {
        self.italic = true;
        self
    }
}

/// A run of text with one style.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    pub text: String,
    pub style: Style,
}

/// Splits text with style codes into styled runs. Empty runs are left out.
pub fn parse(text: &str) -> Vec<Span> {
    parse_with(text, Style::default())
}

/// Like [`parse`], but the text starts in `base` and `&r` goes back to `base`
/// (for messages that have a tone, such as red errors).
pub fn parse_with(text: &str, base: Style) -> Vec<Span> {
    let mut spans = Vec::new();
    let mut style = base;
    let mut current = String::new();
    let mut chars = text.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        if c != '&' {
            current.push(c);
            continue;
        }
        let Some(&(_, code)) = chars.peek() else {
            current.push('&');
            continue;
        };
        let next = if code == '&' {
            None
        } else if code == '#' {
            hex_color(text.get(i + 2..i + 8).unwrap_or("")).map(|rgb| {
                for _ in 0..7 {
                    chars.next();
                }
                Style { color: Some(Color::Rgb(rgb)), ..Style::default() }
            })
        } else {
            style_code(style, base, code).inspect(|_| {
                chars.next();
            })
        };
        match next {
            Some(new_style) => {
                if new_style != style && !current.is_empty() {
                    spans.push(Span { text: std::mem::take(&mut current), style });
                }
                style = new_style;
            }
            None if code == '&' => {
                chars.next();
                current.push('&');
            }
            None => current.push('&'),
        }
    }
    if !current.is_empty() {
        spans.push(Span { text: current, style });
    }
    spans
}

fn hex_color(digits: &str) -> Option<u32> {
    if digits.len() == 6 && digits.bytes().all(|b| b.is_ascii_hexdigit()) {
        u32::from_str_radix(digits, 16).ok()
    } else {
        None
    }
}

fn style_code(current: Style, base: Style, code: char) -> Option<Style> {
    if let Some(named) = Named::from_code(code) {
        return Some(Style { color: Some(Color::Named(named)), ..Style::default() });
    }
    let mut s = current;
    match code.to_ascii_lowercase() {
        'k' => s.obfuscated = true,
        'l' => s.bold = true,
        'm' => s.strikethrough = true,
        'n' => s.underlined = true,
        'o' => s.italic = true,
        'r' => s = base,
        _ => return None,
    }
    Some(s)
}

/// The text without style codes (for logs, the console and plain-text clients).
pub fn strip(text: &str) -> String {
    parse(text).into_iter().map(|s| s.text).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positional_and_named() {
        let args = Args::new().arg("Steve").arg(3).with("reason", "spam");
        assert_eq!(fill("{0} has {1} warnings: {reason}", &args), "Steve has 3 warnings: spam");
        assert_eq!(fill("{0}{0}", &Args::from(["x"])), "xx");
    }

    #[test]
    fn unknown_and_literal_braces() {
        let args = Args::from(["a"]);
        assert_eq!(fill("{1} {missing} {0}", &args), "{1} {missing} a");
        assert_eq!(fill("{{0}} {", &args), "{0} {");
        assert_eq!(fill("a } b {not a key} {0", &args), "a } b {not a key} {0");
        assert_eq!(fill("", &args), "");
    }

    #[test]
    fn arguments_are_not_expanded_twice() {
        let args = Args::from(["{1}", "boom"]);
        assert_eq!(fill("{0}", &args), "{1}");
    }

    #[test]
    fn later_named_value_wins() {
        let args = Args::new().with("p", "a").with("p", "b");
        assert_eq!(fill("{p}", &args), "b");
        assert!(args.has("p"));
    }

    #[test]
    fn parses_styles() {
        let spans = parse("&6Pumbo &8» &7&lhi&r!");
        let texts: Vec<_> = spans.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(texts, vec!["Pumbo ", "» ", "hi", "!"]);
        assert_eq!(spans[0].style.color, Some(Color::Named(Named::Gold)));
        assert!(spans[2].style.bold);
        assert_eq!(spans[2].style.color, Some(Color::Named(Named::Gray)));
        assert_eq!(spans[3].style, Style::default());
    }

    #[test]
    fn colour_resets_formats_and_hex_works() {
        // a colour after a format drops the format, as in vanilla
        assert!(!parse("&l&cA")[0].style.bold);
        let spans = parse("&c&lA&#ff8800B");
        assert!(spans[0].style.bold);
        assert_eq!(spans[0].style.color, Some(Color::Named(Named::Red)));
        assert_eq!(spans[1].style, Style { color: Some(Color::Rgb(0xFF8800)), ..Style::default() });
        assert_eq!(Color::Rgb(0x123456).rgb(), 0x123456);
    }

    #[test]
    fn base_style_and_reset() {
        let base = Style::colored(Color::Named(Named::Red));
        let spans = parse_with("No &fSteve&r here", base);
        let texts: Vec<_> = spans.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(texts, vec!["No ", "Steve", " here"]);
        assert_eq!(spans[0].style, base);
        assert_eq!(spans[1].style.color, Some(Color::Named(Named::White)));
        assert_eq!(spans[2].style, base);
        // without a base, &r is plain text
        assert_eq!(parse("&cA&rB")[1].style, Style::default());
        assert!(Style::colored(Color::Rgb(1)).bolded().bold);
        assert!(Style::default().italicized().italic);
    }

    #[test]
    fn ampersands_without_code_stay() {
        assert_eq!(strip("Tom & Jerry &&c &z &#12 &"), "Tom & Jerry &c &z &#12 &");
        assert_eq!(strip(&escape("&cred")), "&cred");
        assert_eq!(strip("&AUPPER&Lcase"), "UPPERcase");
    }

    #[test]
    fn named_colour_table() {
        for c in "0123456789abcdef".chars() {
            let n = Named::from_code(c).unwrap();
            assert_eq!(n.code(), c);
        }
        assert_eq!(Named::DarkRed.name(), "dark_red");
        assert_eq!(Named::Gold.rgb(), 0xFFAA00);
        assert!(Named::from_code('g').is_none());
    }
}
