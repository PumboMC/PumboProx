//! Command help shared by every Pumbo plugin.
//!
//! A [`Help`] holds the entries of one plugin or one part of it (players,
//! admins) and renders them in two ways:
//!
//! - [`Help::chat`]: one page of at most [`PER_PAGE`] entries. A header with the
//!   plugin name and version, the section, the root command and its short
//!   alias (`PumboAuth 0.1.0 · Admin   /pumboauth /pa`), then one command per line with its short description in an
//!   aligned column, then `◀ 1/3 ▶` (the arrows run the previous or next page)
//!   and a short hint. Commands under the root are listed without it, which
//!   leaves room for the descriptions. A click on an entry types the full
//!   command into the chat box; hovering shows the full command, the whole
//!   description, the example, and the permission it needs. Lines are cut to
//!   the chat width ([`crate::rich::CHAT_WIDTH`]); the tooltip has everything.
//! - [`Help::console`]: every entry, no pages, no colours, aligned by
//!   characters.
//!
//! Both list only what the sender may use. Other technical lines belong to the
//! `version` subcommand ([`crate::style::version`]).

use crate::command::Commands;
use crate::lang::Lang;
use crate::rich::{self, CHAT_WIDTH, Click, Line, Segment, Text};
use crate::style::{self, BRAND, INFO, MUTED, VALUE};
use crate::text::{self, Args, Style};

/// Entries on one chat page.
pub const PER_PAGE: usize = 8;

/// Space between a command and its description in chat, in pixels.
const GAP: u32 = 12;

/// Commands wider than this (in pixels) never set the description column;
/// their description follows after [`GAP`]. See [`column`].
const COLUMN_MAX: u32 = 180;

/// Space between a command and its description in the console, in characters.
const CONSOLE_GAP: usize = 3;

/// One command in the help.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Entry {
    /// The command as typed, without arguments: `/pumbobans ban`.
    pub command: String,
    /// Arguments: `<required>`, `[optional]`, other words typed as they are.
    pub args: String,
    /// Short description for the help line, a few words (the line is cut to the
    /// chat width). `&` codes allowed.
    pub summary: String,
    /// Longer description or example, shown when hovering; `\n` starts a new
    /// line. Empty: none.
    pub details: String,
    /// Permission the sender needs; `None`: everyone.
    pub permission: Option<String>,
}

impl Entry {
    pub fn new(command: impl Into<String>, summary: impl Into<String>) -> Self {
        Self { command: command.into(), summary: summary.into(), ..Self::default() }
    }

    pub fn args(mut self, args: impl Into<String>) -> Self {
        self.args = args.into();
        self
    }

    pub fn details(mut self, details: impl Into<String>) -> Self {
        self.details = details.into();
        self
    }

    pub fn permission(mut self, permission: impl Into<String>) -> Self {
        self.permission = Some(permission.into());
        self
    }

    /// The command with its arguments.
    pub fn syntax(&self) -> String {
        if self.args.is_empty() { self.command.clone() } else { format!("{} {}", self.command, self.args) }
    }

    /// What a click types into the chat box: the command, with a space when it
    /// takes arguments.
    pub fn suggestion(&self) -> String {
        if self.args.is_empty() { self.command.clone() } else { format!("{} ", self.command) }
    }

    fn allowed(&self, allowed: &impl Fn(&str) -> bool) -> bool {
        self.permission.as_deref().is_none_or(allowed)
    }
}

/// The help of one plugin or one section of it.
#[derive(Debug, Clone)]
pub struct Help {
    plugin: String,
    version: String,
    alias: String,
    section: String,
    help_command: String,
    root: String,
    per_page: usize,
    entries: Vec<Entry>,
}

impl Help {
    /// `plugin` is the name in the header (`PumboAuth`), `help_command` the
    /// command that shows this help (`/pumboauth help`); page arrows run it with
    /// the page number.
    pub fn new(plugin: impl Into<String>, help_command: impl Into<String>) -> Self {
        Self {
            plugin: plugin.into(),
            version: String::new(),
            alias: String::new(),
            section: String::new(),
            help_command: help_command.into(),
            root: String::new(),
            per_page: PER_PAGE,
            entries: Vec::new(),
        }
    }

    /// The help of a command tree: one entry per subcommand, with the
    /// description and details messages of each and its permission; the root is
    /// [`Commands::root`].
    pub fn from_commands<H>(plugin: impl Into<String>, commands: &Commands<H>, lang: &Lang) -> Self {
        let root = commands.root();
        let mut help = Self::new(plugin, commands.help_line()).root(root.clone());
        if let Some(a) = commands.alias_of_root() {
            help = help.alias(a);
        }
        for sub in commands.subs() {
            help.push(
                Entry::new(format!("{root} {}", sub.name), message(lang, sub.description))
                    .args(sub.usage)
                    .details(message(lang, sub.details))
                    .permission(commands.permission(sub)),
            );
        }
        help
    }

    /// Version after the plugin name in the header.
    pub fn version(mut self, version: impl Into<String>) -> Self {
        self.version = version.into();
        self
    }

    /// A shorter command for the root (`/pa` for `/pumboauth`), after it in
    /// the header.
    pub fn alias(mut self, alias: impl Into<String>) -> Self {
        self.alias = alias.into();
        self
    }

    /// Section shown after the plugin name (`Admin`); already translated.
    pub fn section(mut self, section: impl Into<String>) -> Self {
        self.section = section.into();
        self
    }

    /// The command the entries are subcommands of (`/pumboauth`). Chat lists
    /// them without it and shows it once in the header; clicks, tooltips and the
    /// console use the full command. Entries outside it (`/login`) stay whole.
    pub fn root(mut self, root: impl Into<String>) -> Self {
        self.root = root.into();
        self
    }

    /// Entries on one chat page (at least 1).
    pub fn per_page(mut self, n: usize) -> Self {
        self.per_page = n.max(1);
        self
    }

    /// Adds an entry (builder style).
    pub fn entry(mut self, entry: Entry) -> Self {
        self.entries.push(entry);
        self
    }

    pub fn push(&mut self, entry: Entry) {
        self.entries.push(entry);
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// Entries the sender may use, in the order they were added.
    pub fn visible(&self, allowed: impl Fn(&str) -> bool) -> Vec<&Entry> {
        self.entries.iter().filter(|e| e.allowed(&allowed)).collect()
    }

    /// Number of chat pages for the sender (at least 1).
    pub fn page_count(&self, allowed: impl Fn(&str) -> bool) -> usize {
        self.visible(allowed).len().div_ceil(self.per_page).max(1)
    }

    /// Page `page` (counted from 1, out of range: the nearest page) for a
    /// player.
    pub fn chat(&self, lang: &Lang, page: usize, allowed: impl Fn(&str) -> bool) -> Text {
        let visible = self.visible(allowed);
        let pages = visible.len().div_ceil(self.per_page).max(1);
        let page = page.clamp(1, pages);
        let mut out = Text::from(self.header(lang));
        if visible.is_empty() {
            out.extend(Text::parse_with(&lang.get("command-help-empty"), Style::colored(INFO)));
            return out;
        }
        let shown: Vec<&Entry> = visible.into_iter().skip((page - 1) * self.per_page).take(self.per_page).collect();
        let rows: Vec<(Line, Line)> = shown
            .iter()
            .map(|e| (style::syntax(&self.listed(e)), Line::parse_with(&e.summary, Style::colored(INFO))))
            .collect();
        let column = column(&rows.iter().map(|(c, d)| (c.width(), d.width())).collect::<Vec<_>>());
        for (entry, (listed, summary)) in shown.into_iter().zip(rows) {
            out.push(chat_entry(lang, entry, listed, summary, column));
        }
        if let Some(footer) = self.footer(lang, page, pages) {
            out.push(footer);
        }
        out
    }

    /// Every entry for the console: no pages, no colours.
    pub fn console(&self, lang: &Lang, allowed: impl Fn(&str) -> bool) -> Text {
        let visible = self.visible(allowed);
        let mut out = Text::from(Line::from(Segment::new(self.title(), Style::default())));
        if visible.is_empty() {
            out.push(Line::from(Segment::new(text::strip(&lang.get("command-help-empty")), Style::default())));
            return out;
        }
        let width = visible.iter().map(|e| e.syntax().chars().count()).max().unwrap_or(0) + CONSOLE_GAP;
        for entry in visible {
            let summary = text::strip(&entry.summary);
            let line = if summary.is_empty() {
                format!("  {}", entry.syntax())
            } else {
                format!("  {:<width$}{summary}", entry.syntax())
            };
            out.push(Line::from(Segment::new(line, Style::default())));
        }
        out
    }

    fn title(&self) -> String {
        let name =
            if self.version.is_empty() { self.plugin.clone() } else { format!("{} {}", self.plugin, self.version) };
        let title = if self.section.is_empty() { name } else { format!("{name} · {}", self.section) };
        let commands: Vec<&str> =
            [&self.root, &self.alias].into_iter().map(String::as_str).filter(|c| !c.is_empty()).collect();
        if commands.is_empty() { title } else { format!("{title}   {}", commands.join(" ")) }
    }

    fn header(&self, lang: &Lang) -> Line {
        let mut line = Line::from(Segment::new(self.plugin.as_str(), Style::colored(BRAND).bolded()));
        if !self.version.is_empty() {
            line = line.with(Segment::colored(format!(" {}", self.version), MUTED));
        }
        if !self.section.is_empty() {
            line = line.with(Segment::colored(" · ", MUTED)).with(Segment::colored(self.section.as_str(), VALUE));
        }
        if !self.root.is_empty() {
            let mut root = Segment::colored(self.root.as_str(), style::COMMAND)
                .on_click(Click::Suggest(format!("{} ", self.root)));
            let hover = lang.get("command-help-click");
            if !hover.is_empty() {
                root = root.on_hover(Text::parse_with(&hover, Style::colored(INFO)));
            }
            line = line.with(Segment::new("   ", Style::default())).with(root);
            if !self.alias.is_empty() {
                let alias =
                    Segment::colored(self.alias.as_str(), INFO).on_click(Click::Suggest(format!("{} ", self.alias)));
                line = line.with(Segment::new(" ", Style::default())).with(alias);
            }
        }
        line
    }

    /// The command and arguments of an entry as chat lists them: without the
    /// root when the command is under it.
    fn listed(&self, entry: &Entry) -> String {
        let full = entry.syntax();
        if self.root.is_empty() {
            return full;
        }
        match full.strip_prefix(self.root.as_str()).and_then(|rest| rest.strip_prefix(' ')) {
            Some(rest) if !rest.trim().is_empty() => rest.to_string(),
            _ => full,
        }
    }

    fn footer(&self, lang: &Lang, page: usize, pages: usize) -> Option<Line> {
        let mut line = Line::new();
        if pages > 1 {
            line.push(self.arrow("◀", page > 1, page.saturating_sub(1), lang.get("command-help-previous")));
            line.push(Segment::colored(format!(" {page}/{pages} "), INFO));
            line.push(self.arrow("▶", page < pages, page + 1, lang.get("command-help-next")));
        }
        let hint = lang.get("command-help-hint");
        if !hint.is_empty() {
            if !line.is_empty() {
                line.push(Segment::new("   ", Style::default()));
            }
            line = line.append(Line::parse_with(&hint, Style::colored(MUTED)));
        }
        (!line.is_empty()).then(|| line.truncate(CHAT_WIDTH))
    }

    fn arrow(&self, symbol: &str, enabled: bool, target: usize, hover: String) -> Segment {
        if !enabled {
            return Segment::colored(symbol, MUTED);
        }
        let segment = Segment::new(symbol, Style::colored(BRAND).bolded())
            .on_click(Click::Run(format!("{} {target}", self.help_command)));
        if hover.is_empty() { segment } else { segment.on_hover(Text::parse_with(&hover, Style::colored(INFO))) }
    }
}

fn message(lang: &Lang, key: &str) -> String {
    if key.is_empty() { String::new() } else { lang.get(key) }
}

/// The description column in pixels for rows of (command width, description
/// width): the widest command (up to [`COLUMN_MAX`]) plus [`GAP`] at which no
/// aligned description has to be cut; when every choice cuts one, the
/// narrowest. Commands wider than the column get their description after
/// [`GAP`] instead.
fn column(rows: &[(u32, u32)]) -> u32 {
    let mut candidates: Vec<u32> = rows.iter().map(|(w, _)| w + GAP).filter(|c| *c <= COLUMN_MAX + GAP).collect();
    candidates.sort_unstable_by(|a, b| b.cmp(a));
    candidates.dedup();
    let fits = |c: u32| rows.iter().filter(|(w, _)| w + GAP <= c).all(|(_, d)| c + d <= CHAT_WIDTH);
    candidates.iter().copied().find(|c| fits(*c)).or_else(|| candidates.last().copied()).unwrap_or(COLUMN_MAX + GAP)
}

/// One help line: the listed command, padding up to the column, the
/// description; cut to the chat width. Clicking types the full command.
fn chat_entry(lang: &Lang, entry: &Entry, listed: Line, summary: Line, column: u32) -> Line {
    let width = listed.width();
    let mut line = listed;
    if !summary.is_empty() {
        let pad = if width + GAP <= column { column - width } else { GAP };
        line = line.append(rich::pad(pad)).append(summary);
    }
    line.truncate(CHAT_WIDTH).on_click(Click::Suggest(entry.suggestion())).on_hover(tooltip(lang, entry))
}

/// The tooltip of an entry: the full command, the whole description, the
/// details, then the permission and what a click does.
fn tooltip(lang: &Lang, entry: &Entry) -> Text {
    let mut text = Text::from(style::syntax(&entry.syntax()));
    text.extend(Text::parse_with(&entry.summary, Style::colored(VALUE)));
    text.extend(Text::parse_with(&entry.details, Style::colored(INFO)));
    let mut meta = Text::new();
    if let Some(permission) = &entry.permission {
        let line = lang.format("command-help-permission", &Args::new().arg(text::escape(permission)));
        meta.extend(Text::parse_with(&line, Style::colored(MUTED)));
    }
    meta.extend(Text::parse_with(&lang.get("command-help-click"), Style::colored(MUTED).italicized()));
    if !meta.is_empty() {
        text.push(Line::new());
        text.extend(meta);
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::Sub;
    use crate::lang::{Bundle, COMMON};

    const PLUGIN: Bundle = Bundle {
        name: "test",
        files: &[
            (
                "en",
                "help-reload: \"Reloads the config\"\nhelp-ban: \"Bans a player\"\nhelp-ban-details: \"Example: /pumbobans ban Steve 1d\"\n",
            ),
            (
                "pl",
                "help-reload: \"Przeładowuje config\"\nhelp-ban: \"Banuje gracza\"\nhelp-ban-details: \"Przykład: /pumbobans ban Steve 1d\"\n",
            ),
        ],
    };

    fn lang(code: &str) -> Lang {
        Lang::load(&[COMMON, PLUGIN], code, None).0
    }

    /// 20 admin entries (`pumbo.auth.admin.cmdN`) and 2 for everyone.
    fn big() -> Help {
        let mut help = Help::new("PumboAuth", "/pumboauth help").section("Admin");
        for i in 0..20 {
            help.push(
                Entry::new(format!("/pumboauth cmd{i}"), format!("Desc {i}"))
                    .args(if i % 2 == 0 { "<nick>" } else { "" })
                    .details(format!("Example {i}"))
                    .permission(format!("pumbo.auth.admin.cmd{i}")),
            );
        }
        help.entry(Entry::new("/login", "Desc login").args("<password>")).entry(Entry::new("/logout", "Desc logout"))
    }

    fn all(_: &str) -> bool {
        true
    }

    /// Pixels before the description of a help line.
    fn description_x(line: &Line) -> Option<u32> {
        let i = line.segments.iter().position(|s| s.text.starts_with("Desc"))?;
        Some(line.segments.iter().take(i).map(Segment::width).sum())
    }

    #[test]
    fn pages_and_navigation() {
        let help = big();
        let en = lang("en");
        assert_eq!(help.page_count(all), 3);
        let first = help.chat(&en, 1, all);
        // header, 8 entries, footer
        assert_eq!(first.lines.len(), 10);
        assert_eq!(first.lines[0].plain(), "PumboAuth · Admin");
        assert_eq!(first.lines[0].segments[0].style.color, Some(BRAND));
        assert!(first.lines[1].plain().starts_with("/pumboauth cmd0 <nick>"));
        let footer = &first.lines[9];
        assert!(footer.plain().starts_with("◀ 1/3 ▶"), "{}", footer.plain());
        assert_eq!(footer.segments[0].click, None);
        assert_eq!(footer.segments[2].click, Some(Click::Run("/pumboauth help 2".into())));
        assert_eq!(footer.segments[2].hover.as_ref().map(Text::plain).as_deref(), Some("Next page"));

        let second = help.chat(&en, 2, all);
        let footer = second.lines.last().unwrap();
        assert_eq!(footer.segments[0].click, Some(Click::Run("/pumboauth help 1".into())));
        assert_eq!(footer.segments[2].click, Some(Click::Run("/pumboauth help 3".into())));

        // last page: the remaining 6, out of range pages go to the nearest one
        let last = help.chat(&en, 3, all);
        assert_eq!(last.lines.len(), 1 + 6 + 1);
        assert_eq!(help.chat(&en, 99, all), last);
        assert_eq!(help.chat(&en, 0, all), first);
        assert!(last.lines[7].segments[2].click.is_none());
    }

    #[test]
    fn filters_by_permission() {
        let help = big();
        let en = lang("en");
        let player = |_: &str| false;
        assert_eq!(help.visible(player).len(), 2);
        assert_eq!(help.page_count(player), 1);
        let page = help.chat(&en, 1, player);
        let lines: Vec<String> = page.lines.iter().map(Line::plain).collect();
        assert_eq!(lines.len(), 4);
        assert!(lines[1].starts_with("/login <password>"));
        assert!(lines[2].starts_with("/logout"));
        // one page: no arrows, only the hint
        assert_eq!(lines[3], "Hover a command for details, click to type it.");

        let some = |p: &str| p == "pumbo.auth.admin.cmd3";
        assert_eq!(
            help.visible(some).iter().map(|e| e.command.as_str()).collect::<Vec<_>>(),
            vec!["/pumboauth cmd3", "/login", "/logout"]
        );

        let nothing = Help::new("PumboAuth", "/pumboauth help").entry(Entry::new("/x", "y").permission("pumbo.x"));
        assert_eq!(nothing.chat(&en, 1, player).plain(), "PumboAuth\nThere are no commands you can use.");
        assert_eq!(nothing.console(&en, player).plain(), "PumboAuth\nThere are no commands you can use.");
    }

    #[test]
    fn entries_click_hover_and_align() {
        let help = big();
        let pl = lang("pl");
        let page = help.chat(&pl, 1, all);
        let entries = &page.lines[1..9];
        let xs: Vec<u32> = entries.iter().map(|l| description_x(l).unwrap()).collect();
        assert!(xs.iter().all(|x| *x == xs[0]), "{xs:?}");
        for line in entries {
            assert!(line.width() <= CHAT_WIDTH);
        }
        let with_args = &page.lines[1];
        assert!(with_args.segments.iter().all(|s| s.click == Some(Click::Suggest("/pumboauth cmd0 ".into()))));
        let without = &page.lines[2];
        assert!(without.segments.iter().all(|s| s.click == Some(Click::Suggest("/pumboauth cmd1".into()))));
        let hover = with_args.segments[0].hover.as_ref().unwrap().plain();
        assert_eq!(
            hover,
            "/pumboauth cmd0 <nick>\nDesc 0\nExample 0\n\nUprawnienie: pumbo.auth.admin.cmd0\nKliknij, żeby wpisać na czacie"
        );
        // command, required argument, description colours
        assert_eq!(with_args.segments[0].style.color, Some(style::COMMAND));
        assert_eq!(with_args.segments[1].style.color, Some(style::ARGUMENT));
        let footer = page.lines.last().unwrap();
        assert!(footer.width() <= CHAT_WIDTH, "{}", footer.width());
    }

    #[test]
    fn long_lines_are_cut_but_the_tooltip_keeps_everything() {
        let en = lang("en");
        let long = "Changes the password of a player without asking for the old one";
        let help = Help::new("PumboAuth", "/pumboauth help")
            .entry(Entry::new("/pumboauth reload", "Reloads"))
            .entry(Entry::new("/pumboauth forcechangepassword", long).args("<nick> <password>"));
        let page = help.chat(&en, 1, all);
        let line = &page.lines[2];
        assert!(line.width() <= CHAT_WIDTH);
        assert!(line.plain().ends_with("..."));
        assert!(line.segments[0].hover.as_ref().unwrap().plain().contains(long));
        // the long command does not push the short one's description out
        let reload = &page.lines[1];
        let reload_x: u32 =
            reload.segments.iter().take_while(|s| !s.text.starts_with("Reloads")).map(Segment::width).sum();
        assert_eq!(reload_x, style::syntax("/pumboauth reload").width() + GAP);
    }

    #[test]
    fn root_is_shown_once() {
        let pl = lang("pl");
        let help = Help::new("PumboAuth", "/pumboauth help")
            .section("Admin")
            .root("/pumboauth")
            .entry(Entry::new("/pumboauth reload", "Przeładowuje config"))
            .entry(Entry::new("/pumboauth unregister", "Usuwa konto gracza").args("<nick>"))
            .entry(Entry::new("/login", "Logowanie").args("<hasło>"));
        let page = help.chat(&pl, 1, all);
        assert_eq!(page.lines[0].plain(), "PumboAuth · Admin   /pumboauth");
        assert_eq!(page.lines[0].segments.last().unwrap().click, Some(Click::Suggest("/pumboauth ".into())));
        assert!(page.lines[1].plain().starts_with("reload "));
        assert!(page.lines[2].plain().starts_with("unregister <nick> "));
        assert!(page.lines[3].plain().starts_with("/login <hasło> "));
        let unregister = &page.lines[2];
        assert_eq!(unregister.segments[0].click, Some(Click::Suggest("/pumboauth unregister ".into())));
        let hover = unregister.segments[0].hover.as_ref().unwrap().plain();
        assert!(hover.starts_with("/pumboauth unregister <nick>\nUsuwa konto gracza\n"), "{hover}");
        // the console lists full commands
        assert!(help.console(&pl, all).plain().contains("\n  /pumboauth reload "));
    }

    #[test]
    fn column_leaves_room_for_descriptions() {
        // aligning everything after the widest command would cut the first description
        let long = "Przeładowuje config i komunikaty";
        let help = Help::new("PumboAuth", "/pumboauth help")
            .root("/pumboauth")
            .entry(Entry::new("/pumboauth reload", long))
            .entry(Entry::new("/pumboauth unregister", "Usuwa konto gracza").args("<nick>"))
            .entry(Entry::new("/pumboauth forceregister", "Rejestruje konto gracza").args("<nick> <hasło>"));
        let page = help.chat(&lang("pl"), 1, all);
        let x = |i: usize, start: &str| -> u32 {
            page.lines[i].segments.iter().take_while(|s| !s.text.starts_with(start)).map(Segment::width).sum()
        };
        // reload and unregister share the column, forceregister follows its own command
        assert_eq!(x(1, "Przeładowuje"), x(2, "Usuwa"));
        assert_eq!(x(2, "Usuwa"), style::syntax("unregister <nick>").width() + GAP);
        assert_eq!(x(3, "Rejestruje"), style::syntax("forceregister <nick> <hasło>").width() + GAP);
        assert!(page.lines[1].plain().ends_with(long));
        assert!(page.lines.iter().all(|l| l.width() <= CHAT_WIDTH));
        assert_eq!(column(&[]), COLUMN_MAX + GAP);
        // nothing fits: the narrowest column
        assert_eq!(column(&[(10, 400), (50, 400)]), 10 + GAP);
    }

    #[test]
    fn console_has_everything_without_colours() {
        let help = big();
        let en = lang("en");
        let text = help.console(&en, all);
        assert_eq!(text.lines.len(), 1 + 22);
        assert!(
            text.lines
                .iter()
                .flat_map(|l| &l.segments)
                .all(|s| { s.style == Style::default() && s.click.is_none() && s.hover.is_none() })
        );
        let plain = text.plain();
        let lines: Vec<&str> = plain.lines().collect();
        assert_eq!(lines[0], "PumboAuth · Admin");
        // the widest command is `/pumboauth cmd10 <nick>` (23 characters), then 3 spaces
        assert_eq!(lines[1], "  /pumboauth cmd0 <nick>    Desc 0");
        assert_eq!(lines[2], "  /pumboauth cmd1           Desc 1");
        let column = lines[1].find("Desc").unwrap();
        assert!(lines.iter().skip(1).all(|l| l.find("Desc") == Some(column)));
        assert!(!plain.contains('&') && !plain.contains("◀"));
    }

    #[test]
    fn built_from_a_command_tree() {
        let tree = Commands::new("bans")
            .label("/pumbobans")
            .with(
                Sub::new("ban", "ban", ())
                    .usage("<player> [time] [reason...]", 1)
                    .description("help-ban")
                    .details("help-ban-details"),
            )
            .with(Sub::new("reload", "admin.reload", ()).description("help-reload"));
        let pl = lang("pl");
        let help = Help::from_commands("PumboBans", &tree, &pl);
        let ban = &help.entries()[0];
        assert_eq!(ban.command, "/pumbobans ban");
        assert_eq!(ban.args, "<player> [time] [reason...]");
        assert_eq!(ban.summary, "Banuje gracza");
        assert_eq!(ban.details, "Przykład: /pumbobans ban Steve 1d");
        assert_eq!(ban.permission.as_deref(), Some("pumbo.bans.ban"));
        assert_eq!(help.entries()[1].details, "");
        let page = help.chat(&pl, 1, all);
        assert_eq!(page.lines[0].plain(), "PumboBans   /pumbobans");
        assert!(page.lines[1].plain().starts_with("ban <player> [time] [reason...]"));
        let only_ban = |p: &str| p == "pumbo.bans.ban";
        assert_eq!(
            help.console(&pl, only_ban).plain(),
            "PumboBans   /pumbobans\n  /pumbobans ban <player> [time] [reason...]   Banuje gracza"
        );
    }

    #[test]
    fn header_with_version_and_short_alias() {
        let tree = Commands::<()>::new("bans").label("/pumbobans").short_alias("/pb");
        let en = lang("en");
        let help = Help::from_commands("PumboBans", &tree, &en).version("0.1.0").section("Admin");
        let header = &help.chat(&en, 1, all).lines[0];
        assert_eq!(header.plain(), "PumboBans 0.1.0 · Admin   /pumbobans /pb");
        assert_eq!(header.segments[1].style.color, Some(MUTED));
        let alias = header.segments.last().unwrap();
        assert_eq!(alias.click, Some(Click::Suggest("/pb ".into())));
        assert!(help.console(&en, all).plain().starts_with("PumboBans 0.1.0 · Admin   /pumbobans /pb\n"));
    }
}
