//! Rich text: lines of styled segments with click and hover actions.
//!
//! Plugins build [`Text`] (help pages, messages with a tone) and hand it to their
//! platform layer, which turns every [`Segment`] into one text component of its
//! platform: text, colour and formats from [`Segment::style`], the click action,
//! and the tooltip (a [`Text`] whose lines are joined with line breaks). A
//! [`Text`] is sent as one chat message with its lines joined by line breaks, so
//! that nothing else lands in the middle. Senders without components, such as
//! the server console, get [`Text::plain`]: no colours, no codes.
//!
//! Widths follow the default Minecraft font ([`char_width`]) so that columns in
//! chat line up; [`pad`] fills an exact number of pixels with normal (4 px) and
//! bold (5 px) spaces. Other fonts (Bedrock) are only roughly aligned.

use std::fmt;

use crate::text::{self, Color, Style};

/// Width of the chat box in pixels with default client settings.
pub const CHAT_WIDTH: u32 = 320;

/// Added where [`Line::truncate`] cuts text.
const ELLIPSIS: &str = "...";

/// What a click on a segment does.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Click {
    /// Puts the text into the chat box; the player edits and sends it.
    Suggest(String),
    /// Runs the command right away, as if the player typed it.
    Run(String),
    /// Copies the text to the clipboard.
    Copy(String),
    /// Opens a web page (the client asks the player first).
    Url(String),
}

/// A run of text with one style and the same actions.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Segment {
    pub text: String,
    pub style: Style,
    pub click: Option<Click>,
    /// Tooltip shown while the pointer is over the segment.
    pub hover: Option<Text>,
}

impl Segment {
    pub fn new(text: impl Into<String>, style: Style) -> Self {
        Self { text: text.into(), style, click: None, hover: None }
    }

    /// Text in one colour, no formats.
    pub fn colored(text: impl Into<String>, color: Color) -> Self {
        Self::new(text, Style::colored(color))
    }

    pub fn on_click(mut self, click: Click) -> Self {
        self.click = Some(click);
        self
    }

    pub fn on_hover(mut self, hover: Text) -> Self {
        self.hover = Some(hover);
        self
    }

    /// Width in pixels in the default Minecraft font.
    pub fn width(&self) -> u32 {
        text_width(&self.text, self.style.bold)
    }
}

/// One line of chat.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Line {
    pub segments: Vec<Segment>,
}

impl Line {
    pub fn new() -> Self {
        Self::default()
    }

    /// One line in the [`crate::text`] format. Use [`Text::parse`] for text
    /// that may contain line breaks.
    pub fn parse(text: &str) -> Self {
        Self::parse_with(text, Style::default())
    }

    /// Like [`Line::parse`], starting in `base`; `&r` goes back to `base`.
    pub fn parse_with(text: &str, base: Style) -> Self {
        Self { segments: text::parse_with(text, base).into_iter().map(|s| Segment::new(s.text, s.style)).collect() }
    }

    /// Appends a segment (builder style).
    pub fn with(mut self, segment: Segment) -> Self {
        self.segments.push(segment);
        self
    }

    pub fn push(&mut self, segment: Segment) {
        self.segments.push(segment);
    }

    /// Appends the segments of `other`.
    pub fn append(mut self, other: Line) -> Self {
        self.segments.extend(other.segments);
        self
    }

    /// Sets the click action of every segment that has none.
    pub fn on_click(mut self, click: Click) -> Self {
        for s in self.segments.iter_mut().filter(|s| s.click.is_none()) {
            s.click = Some(click.clone());
        }
        self
    }

    /// Sets the tooltip of every segment that has none.
    pub fn on_hover(mut self, hover: Text) -> Self {
        for s in self.segments.iter_mut().filter(|s| s.hover.is_none()) {
            s.hover = Some(hover.clone());
        }
        self
    }

    /// Whether the line has no text.
    pub fn is_empty(&self) -> bool {
        self.segments.iter().all(|s| s.text.is_empty())
    }

    /// Width in pixels in the default Minecraft font.
    pub fn width(&self) -> u32 {
        self.segments.iter().map(Segment::width).sum()
    }

    /// The text without styles and actions.
    pub fn plain(&self) -> String {
        self.segments.iter().map(|s| s.text.as_str()).collect()
    }

    /// The line in the `&` format (click and hover are lost). Parses back to the
    /// same text and styles with [`Line::parse`].
    pub fn legacy(&self) -> String {
        self.legacy_from(Style::default()).0
    }

    /// [`Line::legacy`] when the text before ends in `current` (styles carry over
    /// line breaks). Returns the style at the end.
    fn legacy_from(&self, mut current: Style) -> (String, Style) {
        let mut out = String::new();
        for s in self.segments.iter().filter(|s| !s.text.is_empty()) {
            if s.style != current {
                out.push_str(&codes(&s.style));
                current = s.style;
            }
            out.push_str(&text::escape(&s.text));
        }
        (out, current)
    }

    /// Cuts the line to at most `max` pixels, ending it with `...` in the style
    /// of the cut segment. A line that fits stays as it is.
    pub fn truncate(self, max: u32) -> Self {
        if self.width() <= max {
            return self;
        }
        let mut out = Vec::new();
        let mut used = 0;
        for seg in self.segments {
            let reserve = text_width(ELLIPSIS, seg.style.bold);
            let mut kept = String::new();
            let mut cut = false;
            for c in seg.text.chars() {
                let w = char_width(c, seg.style.bold);
                if used + w + reserve > max {
                    cut = true;
                    break;
                }
                used += w;
                kept.push(c);
            }
            if cut {
                let text = format!("{}{ELLIPSIS}", kept.trim_end());
                out.push(Segment { text, ..seg });
                break;
            }
            out.push(Segment { text: kept, ..seg });
        }
        Self { segments: out }
    }
}

impl From<Segment> for Line {
    fn from(segment: Segment) -> Self {
        Self { segments: vec![segment] }
    }
}

impl fmt::Display for Line {
    /// The plain text.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.plain())
    }
}

fn codes(style: &Style) -> String {
    let mut out = match style.color {
        Some(Color::Named(n)) => format!("&{}", n.code()),
        Some(Color::Rgb(rgb)) => format!("&#{rgb:06x}"),
        None => "&r".to_string(),
    };
    for (on, code) in [
        (style.obfuscated, "&k"),
        (style.bold, "&l"),
        (style.strikethrough, "&m"),
        (style.underlined, "&n"),
        (style.italic, "&o"),
    ] {
        if on {
            out.push_str(code);
        }
    }
    out
}

/// Lines of rich text, sent together as one message.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Text {
    pub lines: Vec<Line>,
}

impl Text {
    pub fn new() -> Self {
        Self::default()
    }

    /// Text in the [`crate::text`] format; `\n` starts a new line and the style
    /// carries over to it. Empty text gives no lines.
    pub fn parse(text: &str) -> Self {
        Self::parse_with(text, Style::default())
    }

    /// Like [`Text::parse`], starting in `base`; `&r` goes back to `base`.
    pub fn parse_with(text: &str, base: Style) -> Self {
        let spans = text::parse_with(text, base);
        if spans.is_empty() {
            return Self::new();
        }
        let mut lines = vec![Line::new()];
        for span in spans {
            for (i, part) in span.text.split('\n').enumerate() {
                if i > 0 {
                    lines.push(Line::new());
                }
                if !part.is_empty()
                    && let Some(line) = lines.last_mut()
                {
                    line.push(Segment::new(part, span.style));
                }
            }
        }
        Self { lines }
    }

    /// Appends a line (builder style).
    pub fn with(mut self, line: Line) -> Self {
        self.lines.push(line);
        self
    }

    pub fn push(&mut self, line: Line) {
        self.lines.push(line);
    }

    /// Appends the lines of `other`.
    pub fn extend(&mut self, other: Text) {
        self.lines.extend(other.lines);
    }

    /// Whether there is nothing to send (a disabled message gives no lines).
    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// The text without styles and actions, lines joined with `\n`. This is
    /// what the console gets.
    pub fn plain(&self) -> String {
        self.lines.iter().map(Line::plain).collect::<Vec<_>>().join("\n")
    }

    /// The text in the `&` format, lines joined with `\n` (click and hover are lost).
    pub fn legacy(&self) -> String {
        let mut current = Style::default();
        let mut lines = Vec::with_capacity(self.lines.len());
        for line in &self.lines {
            let (text, end) = line.legacy_from(current);
            lines.push(text);
            current = end;
        }
        lines.join("\n")
    }
}

/// Text as a JSON text component (1.21.5 layout: `click_event`,
/// `hover_event`); PumboProx encodes it for each client version. Lines are
/// joined with line breaks.
pub fn json(t: &Text) -> String {
    component(t).to_string()
}

fn component(t: &Text) -> serde_json::Value {
    use serde_json::{Value, json};
    let mut extra = Vec::new();
    for (i, line) in t.lines.iter().enumerate() {
        if i > 0 {
            extra.push(json!({"text": "\n"}));
        }
        // A click or tooltip shared by neighbouring segments (a whole list line)
        // goes once on a parent, which keeps long lists small.
        let mut rest = line.segments.as_slice();
        while let Some(first) = rest.first() {
            let n = rest.iter().take_while(|s| s.click == first.click && s.hover == first.hover).count();
            let (run, tail) = rest.split_at(n);
            rest = tail;
            if first.click.is_none() && first.hover.is_none() || n == 1 {
                extra.extend(run.iter().map(|s| segment_json(s, true)));
            } else {
                let mut parent = actions_json(first);
                parent.insert("text".into(), "".into());
                parent.insert("extra".into(), run.iter().map(|s| segment_json(s, false)).collect());
                extra.push(Value::Object(parent));
            }
        }
    }
    if extra.is_empty() { json!({"text": ""}) } else { json!({"text": "", "extra": extra}) }
}

fn actions_json(s: &Segment) -> serde_json::Map<String, serde_json::Value> {
    use serde_json::json;
    let mut m = serde_json::Map::new();
    if let Some(click) = &s.click {
        let c = match click {
            Click::Suggest(cmd) => json!({"action": "suggest_command", "command": cmd}),
            Click::Run(cmd) => json!({"action": "run_command", "command": cmd}),
            Click::Copy(v) => json!({"action": "copy_to_clipboard", "value": v}),
            Click::Url(u) => json!({"action": "open_url", "url": u}),
        };
        m.insert("click_event".into(), c);
    }
    if let Some(hover) = &s.hover {
        m.insert("hover_event".into(), json!({"action": "show_text", "value": component(hover)}));
    }
    m
}

fn segment_json(s: &Segment, with_actions: bool) -> serde_json::Value {
    let mut m = serde_json::Map::new();
    m.insert("text".into(), s.text.clone().into());
    match s.style.color {
        Some(Color::Named(n)) => {
            m.insert("color".into(), n.name().into());
        }
        Some(Color::Rgb(v)) => {
            m.insert("color".into(), format!("#{v:06x}").into());
        }
        None => {}
    }
    let st = s.style;
    for (key, on) in [
        ("bold", st.bold),
        ("italic", st.italic),
        ("underlined", st.underlined),
        ("strikethrough", st.strikethrough),
        ("obfuscated", st.obfuscated),
    ] {
        if on {
            m.insert(key.into(), true.into());
        }
    }
    if with_actions {
        m.extend(actions_json(s));
    }
    serde_json::Value::Object(m)
}

impl From<Line> for Text {
    fn from(line: Line) -> Self {
        Self { lines: vec![line] }
    }
}

impl fmt::Display for Text {
    /// The plain text.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.plain())
    }
}

/// Advance of a character in the default Minecraft font, in pixels, including
/// the 1 px gap after it; bold adds 1 px. Characters outside the table count as
/// 6 px (most letters and digits).
pub fn char_width(c: char, bold: bool) -> u32 {
    let w = match c {
        '!' | ',' | '.' | ':' | ';' | '|' | 'i' => 2,
        '\'' | '`' | 'l' => 3,
        ' ' | 'I' | '[' | ']' | 't' | 'ł' => 4,
        '"' | '(' | ')' | '*' | '<' | '>' | 'f' | 'k' | '{' | '}' => 5,
        '@' | '~' => 7,
        _ => 6,
    };
    w + u32::from(bold)
}

/// Width of `text` in pixels (see [`char_width`]).
pub fn text_width(text: &str, bold: bool) -> u32 {
    text.chars().map(|c| char_width(c, bold)).sum()
}

/// Spaces `px` pixels wide: normal spaces are 4 px, bold ones 5 px, so every
/// width from 12 px up is exact. Smaller widths are rounded down.
pub fn pad(px: u32) -> Line {
    // (normal, bold) with 4 * normal + 5 * bold == width; from 12 px up the
    // first try succeeds.
    let split = |width: u32| {
        (0..=3u32).find(|b| 5 * b <= width && (width - 5 * b).is_multiple_of(4)).map(|b| ((width - 5 * b) / 4, b))
    };
    let (normal, bold) = (0..=px).rev().find_map(split).unwrap_or((0, 0));
    let mut line = Line::new();
    if normal > 0 {
        line.push(Segment::new(" ".repeat(normal as usize), Style::default()));
    }
    if bold > 0 {
        line.push(Segment::new(" ".repeat(bold as usize), Style::default().bolded()));
    }
    line
}

#[cfg(test)]
mod tests {
    #[test]
    fn rich_text_as_json() {
        let line =
            Line::parse("&cNo &lway").on_click(Click::Suggest("/x info #1".into())).on_hover(Text::parse("&7more"));
        let t = Text::from(line).with(Line::parse("&#f28c28x"));
        let v: serde_json::Value = serde_json::from_str(&json(&t)).unwrap();
        let extra = v["extra"].as_array().unwrap();
        // Both segments share the click and tooltip: one parent carries them.
        let parent = &extra[0];
        assert_eq!(parent["click_event"]["command"], "/x info #1");
        assert_eq!(parent["hover_event"]["value"]["extra"][0]["text"], "more");
        assert_eq!(parent["extra"][0]["color"], "red");
        assert_eq!(parent["extra"][1]["bold"], true);
        assert!(parent["extra"][1].get("click_event").is_none());
        assert_eq!(extra[1]["text"], "\n");
        assert_eq!(extra[2]["color"], "#f28c28");
        assert_eq!(json(&Text::new()), r#"{"text":""}"#);
    }

    use super::*;
    use crate::text::Named;

    #[test]
    fn parses_lines_and_carries_style() {
        let t = Text::parse("&cFirst\nsecond &aok\n");
        assert_eq!(t.lines.len(), 3);
        assert_eq!(t.lines[0].plain(), "First");
        // the colour of the first line carries over
        assert_eq!(t.lines[1].segments[0].style.color, Some(Color::Named(Named::Red)));
        assert_eq!(t.lines[1].segments[1].text, "ok");
        assert!(t.lines[2].is_empty());
        assert_eq!(t.plain(), "First\nsecond ok\n");
        assert!(Text::parse("").is_empty());
        assert!(Text::parse("&c").is_empty());
        assert_eq!(t.to_string(), t.plain());
    }

    #[test]
    fn legacy_round_trip() {
        let line = Line::new()
            .with(Segment::colored("Pumbo", Color::Rgb(0xF28C28)))
            .with(Segment::new(" & ", Style::default()))
            .with(Segment::new("bold", Style::colored(Color::Named(Named::Gray)).bolded()))
            .with(Segment::new("plain", Style::default().italicized()));
        let legacy = line.legacy();
        assert_eq!(legacy, "&#f28c28Pumbo&r && &7&lbold&r&oplain");
        assert_eq!(Line::parse(&legacy), line);
        let text = Text::from(line.clone()).with(Line::parse("&atwo"));
        assert_eq!(Text::parse(&text.legacy()), text);
        // a plain line after a styled one resets the carried style
        let text = Text::parse("&cred").with(Line::parse("plain"));
        assert_eq!(text.legacy(), "&cred\n&rplain");
        assert_eq!(Text::parse(&text.legacy()), text);
    }

    #[test]
    fn actions_fill_only_empty_slots() {
        let line = Line::parse("&eA&fB")
            .with(Segment::new("C", Style::default()).on_click(Click::Run("/c".into())))
            .on_click(Click::Suggest("/x ".into()))
            .on_hover(Text::parse("tip"));
        assert_eq!(line.segments[0].click, Some(Click::Suggest("/x ".into())));
        assert_eq!(line.segments[2].click, Some(Click::Run("/c".into())));
        assert!(line.segments.iter().all(|s| s.hover.as_ref().map(Text::plain).as_deref() == Some("tip")));
    }

    #[test]
    fn widths() {
        assert_eq!(text_width("/pumboauth", false), 6 * 9 + 4);
        assert_eq!(text_width("il!", false), 7);
        assert_eq!(text_width("ab", true), 14);
        assert_eq!(Line::parse("&lab&rc").width(), 14 + 6);
        for px in 12..200 {
            assert_eq!(pad(px).width(), px, "{px}");
        }
        assert_eq!(pad(7).width(), 5);
        assert!(pad(0).segments.is_empty());
    }

    #[test]
    fn truncates_to_width() {
        let line = Line::parse("&7Short");
        assert_eq!(line.clone().truncate(100), line);
        let long = Line::parse("&eabcdef &7ghijklmnop");
        let cut = long.clone().truncate(60);
        assert!(cut.width() <= 60, "{}", cut.width());
        assert!(cut.plain().ends_with("..."));
        assert!(long.plain().starts_with(cut.plain().trim_end_matches("...")));
        // a space before the cut is dropped
        assert_eq!(Line::parse("abcdef ghijklmnop").truncate(48).plain(), "abcdef...");
    }
}
