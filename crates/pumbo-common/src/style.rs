//! The look shared by every Pumbo plugin: colours, the prefix, messages with a
//! tone, highlighted values, durations and numbers for people.
//!
//! A reply to a command is one line: the plugin prefix (`PumboBans » `, name in
//! [`BRAND`]), then the message in the colour of its [`Tone`]. Message files
//! hold only the words; [`success`], [`error`], [`warn`] and [`info`] add the
//! prefix and the colour, so every plugin looks the same and a translation never
//! has to repeat colour codes:
//!
//! ```
//! use pumbo_common::lang::{Bundle, COMMON, Lang};
//! use pumbo_common::style;
//! use pumbo_common::text::Args;
//!
//! const PLUGIN: Bundle = Bundle {
//!     name: "demo",
//!     files: &[("en", "prefix: \"&#f28c28PumboDemo &8» \"\nbanned: \"{0} is banned for {1}.\"\n")],
//! };
//! let (lang, _) = Lang::load(&[COMMON, PLUGIN], "en", None);
//! let duration = style::duration(&lang, std::time::Duration::from_secs(2 * 86_400 + 3 * 3_600));
//! let reply = style::success(&lang, "banned", &Args::new().arg(style::value("Steve")).arg(duration));
//! assert_eq!(reply.plain(), "PumboDemo » Steve is banned for 2 days 3 hours.");
//! ```
//!
//! Values inside messages go through [`value`] (or [`command`] for commands):
//! highlighted, escaped so that `&` typed by players shows as typed, and
//! followed by `&r`, which goes back to the colour of the tone. Version and
//! other technical details belong to the `version` subcommand ([`version`]),
//! never to the help or to error messages.

use std::fmt;
use std::time::Duration;

use crate::lang::Lang;
use crate::rich::{Click, Line, Segment, Text};
use crate::text::{self, Args, Color, Named, Style};
use crate::time::Term;

/// Pumbo orange: "Pumbo" at the start of every plugin name.
pub const BRAND: Color = Color::Rgb(0xF28C28);
/// Commands inside messages (`/login <password>`).
pub const COMMAND: Color = Color::Rgb(0xFFC27A);
/// Field labels in info views (`Weight:`): the light orange of [`COMMAND`],
/// which a plugin with its own accent no longer uses for command words.
pub const LABEL: Color = COMMAND;

/// The colour of each plugin: the second word of its name (`Perms` in
/// `PumboPerms`), its command words in help and usage lines ([`words`]) and
/// the titles of its views ([`title`]). The plugins that still have
/// [`BRAND`] keep [`COMMAND`] for command words.
pub mod accent {
    use super::{BRAND, Color};

    /// A warm yellow, apart from the warning yellow (`&e`, #FFFF55) and from
    /// the orange of "Pumbo" next to it.
    pub const PERMS: Color = Color::Rgb(0xFFD54F);
    pub const BANS: Color = BRAND;
    pub const FILTER: Color = BRAND;
    pub const AUTH: Color = BRAND;
}
/// Required arguments (`<player>`).
pub const ARGUMENT: Color = Color::Named(Named::White);
/// Optional arguments (`[reason]`).
pub const OPTIONAL: Color = Color::Named(Named::Gray);
pub const SUCCESS: Color = Color::Named(Named::Green);
pub const ERROR: Color = Color::Named(Named::Red);
pub const WARN: Color = Color::Named(Named::Yellow);
/// Information and descriptions.
pub const INFO: Color = Color::Named(Named::Gray);
/// Values inside messages: names, numbers, times.
pub const VALUE: Color = Color::Named(Named::White);
/// Separators, hints, things that are switched off.
pub const MUTED: Color = Color::Named(Named::DarkGray);

/// What kind of reply a message is; sets its colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tone {
    Success,
    Error,
    Warn,
    Info,
}

impl Tone {
    pub const fn color(self) -> Color {
        match self {
            Tone::Success => SUCCESS,
            Tone::Error => ERROR,
            Tone::Warn => WARN,
            Tone::Info => INFO,
        }
    }
}

/// The `&` code of a colour (`&c`, `&#f28c28`).
pub fn code(color: Color) -> String {
    match color {
        Color::Named(n) => format!("&{}", n.code()),
        Color::Rgb(rgb) => format!("&#{rgb:06x}"),
    }
}

/// `PumboPerms` -> (`Pumbo`, `Perms`).
fn split_name(plugin: &str) -> (&str, &str) {
    plugin.strip_prefix("Pumbo").map_or(("", plugin), |rest| ("Pumbo", rest))
}

/// The standard text of a plugin's `prefix` message: "Pumbo" in [`BRAND`], the
/// rest of the name in the plugin's `accent`, then `» ` (`&#f28c28PumboBans &8» `
/// while the accent is the brand colour).
pub fn prefix_code(plugin: &str, accent: Color) -> String {
    let (pumbo, rest) = split_name(plugin);
    let rest_code = if accent == BRAND { String::new() } else { code(accent) };
    format!("{}{pumbo}{rest_code}{rest} {}» ", code(BRAND), code(MUTED))
}

/// The short tag in front of every chat line:
/// the first letter of "Pumbo" in [`BRAND`] and the first letter of the rest of
/// the name in the plugin's `accent`, both bold, in plain dark grey brackets (a
/// colour code ends bold: `&8[&#f28c28&lP&#ffd54f&lP&8] ` for PumboPerms).
pub fn tag_code(plugin: &str, accent: Color) -> String {
    let (pumbo, rest) = split_name(plugin);
    let first = |s: &str| s.chars().next().map(String::from).unwrap_or_default();
    format!("{m}[{}&l{}{}&l{}{m}] ", code(BRAND), first(pumbo), code(accent), first(rest), m = code(MUTED))
}

/// A line `width` pixels wide, ruled across like a header or a page bar: a
/// struck-through rule in `left`, the `centre` text, the rule again in `right`
/// (PumboPerms: [`BRAND`] and its accent, the colours of `[PP]`). Without
/// centre text it is one rule in the two colours; a centre wider than the
/// line is cut to it.
#[expect(clippy::integer_division, reason = "a rule is made of whole spaces")]
pub fn bar(centre: Line, width: u32, left: Color, right: Color) -> Line {
    let space = crate::rich::text_width(" ", false).max(1);
    let gap = if centre.is_empty() { 0 } else { 2 * space };
    let centre = if centre.width() + gap > width { centre.truncate(width.saturating_sub(gap)) } else { centre };
    let side = (width.saturating_sub(centre.width() + gap) / 2 / space) as usize;
    let rule = |color: Color| {
        let mut style = Style::colored(color);
        style.strikethrough = true;
        Segment::new(" ".repeat(side), style)
    };
    let mut line = Line::from(rule(left));
    if !centre.is_empty() {
        line = line.with(Segment::new(" ", Style::default())).append(centre).with(Segment::new(" ", Style::default()));
    }
    line.with(rule(right))
}

/// A plugin name in bold for headers: "Pumbo" in [`BRAND`], the rest in `accent`.
pub fn name(plugin: &str, accent: Color) -> Line {
    let (pumbo, rest) = split_name(plugin);
    Line::from(Segment::new(pumbo, Style::colored(BRAND).bolded()))
        .with(Segment::new(rest, Style::colored(accent).bolded()))
}

/// The plugin's `prefix` message as a line (empty when it is not set).
pub fn prefix(lang: &Lang) -> Line {
    Line::parse(&lang.get("prefix"))
}

/// The message `key` as a reply: the prefix, then the text in the colour of
/// `tone`. `{prefix}` inside the message is left empty, so messages written for
/// the older style do not get the prefix twice. A disabled (empty) message
/// gives empty text, which the platform layer does not send.
pub fn message(lang: &Lang, tone: Tone, key: &str, args: &Args) -> Text {
    colored(lang, tone.color(), key, args)
}

/// The title line of a view (`Group vip`): like [`message`], in the plugin's
/// `accent`; highlighted values keep their colour.
pub fn title(lang: &Lang, accent: Color, key: &str, args: &Args) -> Text {
    colored(lang, accent, key, args)
}

fn colored(lang: &Lang, color: Color, key: &str, args: &Args) -> Text {
    let mut args = args.clone();
    if !args.has("prefix") {
        args.set("prefix", "");
    }
    prefixed(lang, color, &lang.format(key, &args))
}

/// Like [`message`] for text that is already formatted.
pub fn toned(lang: &Lang, tone: Tone, body: &str) -> Text {
    prefixed(lang, tone.color(), body)
}

fn prefixed(lang: &Lang, color: Color, body: &str) -> Text {
    let mut text = Text::parse_with(body, Style::colored(color));
    if let Some(first) = text.lines.first_mut() {
        let rest = std::mem::take(first);
        *first = prefix(lang).append(rest);
    }
    text
}

pub fn success(lang: &Lang, key: &str, args: &Args) -> Text {
    message(lang, Tone::Success, key, args)
}

pub fn error(lang: &Lang, key: &str, args: &Args) -> Text {
    message(lang, Tone::Error, key, args)
}

pub fn warn(lang: &Lang, key: &str, args: &Args) -> Text {
    message(lang, Tone::Warn, key, args)
}

pub fn info(lang: &Lang, key: &str, args: &Args) -> Text {
    message(lang, Tone::Info, key, args)
}

/// A value for a message argument: highlighted, escaped, then back to the tone.
pub fn value(v: impl fmt::Display) -> String {
    highlight(v, VALUE)
}

/// A command for a message argument, in the command colour.
pub fn command(c: impl fmt::Display) -> String {
    highlight(c, COMMAND)
}

fn highlight(v: impl fmt::Display, color: Color) -> String {
    format!("{}{}&r", code(color), text::escape(&v.to_string()))
}

/// The colour of command words in help and usage lines: the plugin's
/// `accent`, or [`COMMAND`] while the accent is still the brand colour.
pub fn words(accent: Color) -> Color {
    if accent == BRAND { COMMAND } else { accent }
}

/// A command with its arguments, coloured by kind: `<required>` in
/// [`ARGUMENT`], `[optional]` in [`OPTIONAL`], words typed as they are in
/// [`words`] of the plugin's `accent`. `/pumbobans ban <player> [reason...]`.
pub fn syntax(line: &str, accent: Color) -> Line {
    let accent = words(accent);
    let mut out = Line::new();
    let mut run = String::new();
    let mut run_color = accent;
    for (i, token) in line.split_whitespace().enumerate() {
        let color = match token.chars().next() {
            Some('<') => ARGUMENT,
            Some('[') => OPTIONAL,
            _ => accent,
        };
        if color != run_color && !run.is_empty() {
            out.push(Segment::colored(std::mem::take(&mut run), run_color));
        }
        if i > 0 {
            // the space belongs to the run before, so a run never starts with one
            if run.is_empty() {
                if let Some(last) = out.segments.last_mut() {
                    last.text.push(' ');
                }
            } else {
                run.push(' ');
            }
        }
        run_color = color;
        run.push_str(token);
    }
    if !run.is_empty() {
        out.push(Segment::colored(run, run_color));
    }
    out
}

/// The command part of a usage line, for a click that types it: everything
/// before the first `<` or `[` argument, with a space when arguments follow.
pub fn suggestion(usage: &str) -> String {
    let tokens: Vec<&str> = usage.split_whitespace().collect();
    let typed: Vec<&str> = tokens.iter().copied().take_while(|t| !t.starts_with('<') && !t.starts_with('[')).collect();
    let line = typed.join(" ");
    if typed.len() < tokens.len() { format!("{line} ") } else { line }
}

fn tooltip(lang: &Lang, key: &str) -> Option<Text> {
    let hover = lang.get(key);
    (!hover.is_empty()).then(|| Text::parse_with(&hover, Style::colored(INFO)))
}

fn with_actions(text: Text, click: Click, hover: Option<Text>) -> Text {
    Text {
        lines: text
            .lines
            .into_iter()
            .map(|line| {
                let line = line.on_click(click.clone());
                match &hover {
                    Some(h) => line.on_hover(h.clone()),
                    None => line,
                }
            })
            .collect(),
    }
}

/// Reply to an unknown subcommand: a short error with a hint to use
/// `help_command` (in [`words`] of the plugin's `accent`); a click runs it.
pub fn unknown_subcommand(lang: &Lang, accent: Color, name: &str, help_command: &str) -> Text {
    let args = Args::new().arg(value(name)).arg(highlight(help_command, words(accent)));
    let text = error(lang, "command-unknown", &args);
    with_actions(text, Click::Run(help_command.to_string()), tooltip(lang, "command-unknown-hover"))
}

/// Reply to a command with missing arguments: `Usage: <usage>`, coloured like
/// the help; a click types the command.
pub fn usage(lang: &Lang, accent: Color, usage: &str) -> Text {
    let shown = format!("{}&r", syntax(usage, accent).legacy());
    let text = error(lang, "command-usage", &Args::new().arg(shown));
    with_actions(text, Click::Suggest(suggestion(usage)), tooltip(lang, "command-usage-hover"))
}

/// The `version` subcommand: the plugin name and version, then one
/// `label: value` line per row (platform, API, mode...). The same text suits
/// the console.
pub fn version<L: fmt::Display, V: fmt::Display>(
    plugin: &str,
    accent: Color,
    version: &str,
    rows: impl IntoIterator<Item = (L, V)>,
) -> Text {
    let mut text = Text::from(name(plugin, accent).with(Segment::colored(format!(" {version}"), VALUE)));
    for (label, value) in rows {
        text.push(
            Line::new()
                .with(Segment::colored(format!("{label}: "), INFO))
                .with(Segment::colored(value.to_string(), VALUE)),
        );
    }
    text
}

const UNITS: [(&str, u64); 4] = [("time-day", 86_400), ("time-hour", 3_600), ("time-minute", 60), ("time-second", 1)];

/// A duration in words, at most the two largest parts: `2 dni 3 godziny`,
/// `1 minute`, `5 minut`. The largest parts are kept and the rest is cut off,
/// not rounded. In Polish the words are in the nominative case, so put them
/// where it fits (`Pozostało: 5 minut`, `Czas: 1 minuta`). For text admins type
/// back (`1d 2h`) use [`crate::time::format_duration`].
pub fn duration(lang: &Lang, d: Duration) -> String {
    duration_parts(lang, d, 2)
}

/// Like [`duration`] with at most `parts` parts (days, hours, minutes, seconds).
#[expect(clippy::integer_division, reason = "whole units of time")]
pub fn duration_parts(lang: &Lang, d: Duration, parts: usize) -> String {
    let mut secs = d.as_secs();
    let mut out = Vec::new();
    for (key, size) in UNITS {
        if out.len() >= parts.max(1) {
            break;
        }
        let n = secs / size;
        secs %= size;
        if n > 0 {
            out.push(lang.plural(key, n));
        }
    }
    if out.is_empty() { lang.plural("time-second", 0) } else { out.join(" ") }
}

/// `na zawsze` / `permanent`, or the duration in words.
pub fn term(lang: &Lang, term: Term) -> String {
    match term {
        Term::Permanent => lang.get("time-permanent"),
        Term::Limited(d) => duration(lang, d),
    }
}

/// A number with thousands grouped by the `number-separator` message:
/// `12,345` in English, `12 345` in Polish. Polish leaves four-digit numbers
/// whole (`1234`), as Polish typography does.
pub fn number(lang: &Lang, n: u64) -> String {
    let digits = n.to_string();
    let min = if lang.code().starts_with("pl") { 10_000 } else { 1_000 };
    if n < min {
        return digits;
    }
    let separator = lang.get("number-separator");
    let mut out = String::new();
    let len = digits.len();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (len - i).is_multiple_of(3) {
            out.push_str(&separator);
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lang::{Bundle, COMMON};
    use crate::text::parse;

    const PLUGIN: Bundle = Bundle {
        name: "test",
        files: &[
            (
                "en",
                "prefix: \"&#f28c28PumboTest &8» \"\nold: \"{prefix}&aDone {0}\"\nlines: \"One {0}{nl}two\"\noff: \"\"\n",
            ),
            (
                "pl",
                "prefix: \"&#f28c28PumboTest &8» \"\nold: \"{prefix}&aGotowe {0}\"\nlines: \"Jeden {0}{nl}dwa\"\noff: \"\"\n",
            ),
        ],
    };

    fn lang(code: &str) -> Lang {
        let (l, w) = Lang::load(&[COMMON, PLUGIN], code, None);
        assert!(w.is_empty(), "{w:?}");
        l
    }

    #[test]
    fn prefix_and_tone() {
        let en = lang("en");
        let t = error(&en, "command-player-not-found", &Args::new().arg(value("St&eve")));
        assert_eq!(t.plain(), "PumboTest » Player St&eve was not found.");
        let line = &t.lines[0];
        assert_eq!(line.segments[0].style.color, Some(BRAND));
        assert_eq!(line.segments[1].style.color, Some(MUTED));
        assert_eq!(line.segments[2].style.color, Some(ERROR));
        // the value is white, and the rest goes back to red
        assert_eq!(line.segments[3].text, "St&eve");
        assert_eq!(line.segments[3].style.color, Some(VALUE));
        assert_eq!(line.segments[4].style.color, Some(ERROR));
        assert_eq!(success(&en, "command-reloaded", &Args::new()).lines[0].segments[2].style.color, Some(SUCCESS));
        assert_eq!(warn(&en, "command-reloaded", &Args::new()).lines[0].segments[2].style.color, Some(WARN));
        assert_eq!(info(&en, "command-reloaded", &Args::new()).lines[0].segments[2].style.color, Some(INFO));
    }

    #[test]
    fn old_messages_disabled_messages_and_lines() {
        let en = lang("en");
        // {prefix} inside the message is not doubled
        assert_eq!(success(&en, "old", &Args::from(["x"])).plain(), "PumboTest » Done x");
        assert!(success(&en, "off", &Args::new()).is_empty());
        // the prefix goes on the first line only, the tone carries over
        let t = info(&en, "lines", &Args::from(["1"]));
        assert_eq!(t.plain(), "PumboTest » One 1\ntwo");
        assert_eq!(t.lines[1].segments[0].style.color, Some(INFO));
        // no prefix set: only the message
        let (bare, _) = Lang::load(&[COMMON], "en", None);
        assert_eq!(
            error(&bare, "command-no-permission", &Args::new()).plain(),
            "You don't have permission to do that."
        );
    }

    #[test]
    fn prefix_colors_pumbo_and_perms() {
        let yellow = Color::Named(Named::Yellow);
        let spans = parse(&prefix_code("PumboPerms", yellow));
        let parts: Vec<_> = spans.iter().map(|s| (s.text.as_str(), s.style.color)).collect();
        assert_eq!(parts, vec![("Pumbo", Some(BRAND)), ("Perms ", Some(yellow)), ("» ", Some(MUTED))]);
        // an accent equal to the brand colour keeps the text the plugins ship today
        assert_eq!(prefix_code("PumboBans", BRAND), "&#f28c28PumboBans &8» ");
        let spans = parse(&prefix_code("PumboPerms", accent::PERMS));
        assert!(spans[0].text.starts_with("Pumbo") && spans[0].style.color == Some(BRAND));
        assert!(spans.iter().any(|s| s.text.contains("Perms") && s.style.color == Some(accent::PERMS)));
        let header: Vec<_> = name("PumboPerms", yellow).segments.into_iter().map(|s| (s.text, s.style)).collect();
        assert_eq!(
            header,
            vec![("Pumbo".into(), Style::colored(BRAND).bolded()), ("Perms".into(), Style::colored(yellow).bolded())]
        );
        assert_eq!(code(BRAND), "&#f28c28");
        assert_eq!(code(ERROR), "&c");
        assert_eq!(prefix_code("PumboPerms", accent::PERMS), "&#f28c28Pumbo&#ffd54fPerms &8» ");
        assert_eq!(tag_code("PumboPerms", accent::PERMS), "&8[&#f28c28&lP&#ffd54f&lP&8] ");
    }

    #[test]
    fn perms_yellow_is_its_own_colour() {
        // compared as RGB: Color::Rgb(0xFFFF55) would be the warning yellow too
        for (other, what) in [(BRAND, "brand"), (WARN, "warning"), (LABEL, "label"), (COMMAND, "command")] {
            assert_ne!(accent::PERMS.rgb(), other.rgb(), "PERMS looks like the {what} colour");
        }
        // the plugins without their own colour keep the light orange for command words
        assert_eq!(words(accent::PERMS), accent::PERMS);
        assert_eq!(words(accent::BANS), COMMAND);
    }

    #[test]
    fn title_in_the_accent() {
        let en = lang("en");
        let t = title(&en, accent::PERMS, "lines", &Args::from([value("vip")]));
        assert_eq!(t.plain(), "PumboTest » One vip\ntwo");
        let parts: Vec<_> = t.lines[0].segments.iter().map(|s| (s.text.as_str(), s.style.color)).collect();
        assert_eq!(
            parts,
            vec![("PumboTest ", Some(BRAND)), ("» ", Some(MUTED)), ("One ", Some(accent::PERMS)), ("vip", Some(VALUE))]
        );
        assert_eq!(t.lines[1].segments[0].style.color, Some(accent::PERMS));
    }

    #[test]
    fn polish_durations() {
        let pl = lang("pl");
        let d = |s: u64| duration(&pl, Duration::from_secs(s));
        assert_eq!(d(2 * 86_400 + 3 * 3_600), "2 dni 3 godziny");
        assert_eq!(d(60), "1 minuta");
        assert_eq!(d(300), "5 minut");
        assert_eq!(d(22 * 60), "22 minuty");
        assert_eq!(d(12 * 60), "12 minut");
        assert_eq!(d(86_400 + 60), "1 dzień 1 minuta");
        assert_eq!(d(5 * 86_400 + 3_600 + 61), "5 dni 1 godzina");
        assert_eq!(d(2), "2 sekundy");
        assert_eq!(d(0), "0 sekund");
        assert_eq!(duration_parts(&pl, Duration::from_secs(90_061), 4), "1 dzień 1 godzina 1 minuta 1 sekunda");
        assert_eq!(term(&pl, Term::Permanent), "na zawsze");
        assert_eq!(term(&pl, Term::Limited(Duration::from_secs(7_200))), "2 godziny");
    }

    #[test]
    fn english_durations() {
        let en = lang("en");
        let d = |s: u64| duration(&en, Duration::from_secs(s));
        assert_eq!(d(2 * 86_400 + 3 * 3_600), "2 days 3 hours");
        assert_eq!(d(60), "1 minute");
        assert_eq!(d(86_400 + 1), "1 day 1 second");
        assert_eq!(d(0), "0 seconds");
        assert_eq!(duration_parts(&en, Duration::from_secs(90_061), 0), "1 day");
        assert_eq!(term(&en, Term::Permanent), "permanent");
    }

    #[test]
    fn numbers() {
        let (pl, en) = (lang("pl"), lang("en"));
        assert_eq!(number(&pl, 1234), "1234");
        assert_eq!(number(&pl, 12_345), "12 345");
        assert_eq!(number(&pl, 1_234_567), "1 234 567");
        assert_eq!(number(&en, 999), "999");
        assert_eq!(number(&en, 1234), "1,234");
        assert_eq!(number(&en, 100_000), "100,000");
    }

    #[test]
    fn syntax_colours() {
        let line = syntax("/pumbobans ban <player> [reason...]", accent::BANS);
        let parts: Vec<_> = line.segments.iter().map(|s| (s.text.as_str(), s.style.color)).collect();
        assert_eq!(
            parts,
            vec![("/pumbobans ban ", Some(COMMAND)), ("<player> ", Some(ARGUMENT)), ("[reason...]", Some(OPTIONAL))]
        );
        // a plugin with its own colour gets it for its command words
        let yellow = Color::Named(Named::Yellow);
        assert_eq!(syntax("/pp user <player>", yellow).segments[0].style.color, Some(yellow));
        assert_eq!(syntax("/x import authme <file>", COMMAND).segments[0].text, "/x import authme ");
        assert_eq!(suggestion("/pumbobans ban <player> [reason...]"), "/pumbobans ban ");
        assert_eq!(suggestion("/pumboauth reload"), "/pumboauth reload");
        assert!(syntax("", COMMAND).segments.is_empty());
    }

    #[test]
    fn standard_replies() {
        let pl = lang("pl");
        let t = unknown_subcommand(&pl, accent::BANS, "foo", "/pumbotest help");
        assert_eq!(t.plain(), "PumboTest » Nieznana podkomenda foo. Użyj /pumbotest help.");
        let line = &t.lines[0];
        assert!(line.segments.iter().all(|s| s.click == Some(Click::Run("/pumbotest help".into()))));
        assert_eq!(line.segments[0].hover.as_ref().map(Text::plain).as_deref(), Some("Kliknij, żeby zobaczyć komendy"));
        assert!(line.segments.iter().any(|s| s.text == "/pumbotest help" && s.style.color == Some(COMMAND)));

        let t = usage(&pl, COMMAND, "/pumbotest ban <gracz> [powód]");
        assert_eq!(t.plain(), "PumboTest » Użycie: /pumbotest ban <gracz> [powód]");
        assert!(t.lines[0].segments.iter().all(|s| s.click == Some(Click::Suggest("/pumbotest ban ".into()))));
        assert!(t.lines[0].segments.iter().any(|s| s.text.starts_with("<gracz>") && s.style.color == Some(ARGUMENT)));

        let v = version(
            "PumboAuth",
            accent::AUTH,
            "0.4.0",
            [(pl.get("version-platform"), "Pumpkin 0.2.0 (26.3)"), ("Tryb".into(), "standalone")],
        );
        assert_eq!(v.plain(), "PumboAuth 0.4.0\nPlatforma: Pumpkin 0.2.0 (26.3)\nTryb: standalone");
        assert!(v.lines[0].segments[0].style.bold);
    }

    #[test]
    fn bar_is_ruled_in_two_colours() {
        let centre = Line::from(Segment::colored("2/5", VALUE));
        let line = bar(centre.clone(), 300, BRAND, accent::PERMS);
        assert!(line.width() <= 300 && line.width() + 8 >= 300, "{}", line.width());
        let (first, last) = (&line.segments[0], line.segments.last().unwrap());
        assert!(first.style.strikethrough && first.style.color == Some(BRAND) && first.text.trim().is_empty());
        assert!(last.style.strikethrough && last.style.color == Some(accent::PERMS));
        assert_eq!(first.text.len(), last.text.len());
        assert!(line.plain().contains(" 2/5 "));
        let plain = bar(Line::new(), 300, BRAND, accent::PERMS);
        assert_eq!(plain.segments.len(), 2);
        assert!(bar(Line::from(Segment::colored("x".repeat(200), VALUE)), 100, BRAND, BRAND).width() <= 100);
    }
}
