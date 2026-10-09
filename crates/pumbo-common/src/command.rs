//! Command helpers shared by every plugin.
//!
//! Conventions:
//! - every plugin has one command tree reachable as `/pumbo <plugin> <sub> ...`
//!   (the umbrella) and under its own name (`/pumbobans <sub> ...`, plus short
//!   aliases the plugin picks, such as `/ban`)
//! - permission of a subcommand: `pumbo.<plugin>.<action>`, all lowercase
//!   (`pumbo.bans.ban`, `pumbo.auth.admin.reload`)
//! - `help` or no subcommand lists the subcommands the sender may use (pages
//!   and tooltips: [`crate::help`]); `version` shows the version and platform,
//!   which never go into the help
//!
//! [`Commands`] does the routing and permission checks; what a subcommand does
//! is up to the plugin (`H` is whatever handler type it likes).

/// Name of the umbrella command.
pub const ROOT: &str = "pumbo";

/// Permission node `pumbo.<plugin>.<action>`.
pub fn permission(plugin: &str, action: &str) -> String {
    format!("{ROOT}.{plugin}.{action}")
}

/// Whether `part` may be a plugin or action name: lowercase letters, digits and
/// `-`, dot-separated segments allowed (`admin.reload`).
pub fn is_valid_node(part: &str) -> bool {
    !part.is_empty()
        && part
            .split('.')
            .all(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'))
}

/// Splits a command line into the lowercase command name and its arguments.
/// A leading `/` and a namespace (`pumbo:login`) are removed.
pub fn split_command(line: &str) -> (String, Vec<String>) {
    let line = line.trim();
    let line = line.strip_prefix('/').unwrap_or(line);
    let mut parts = line.split_whitespace();
    let name = parts.next().unwrap_or("").to_lowercase();
    let name = match name.split_once(':') {
        Some((_, n)) => n.to_string(),
        None => name,
    };
    (name, parts.map(str::to_string).collect())
}

/// Splits the arguments of `/pumbo` into the plugin name (lowercase) and the rest.
pub fn umbrella(args: &[String]) -> Option<(String, &[String])> {
    let (first, rest) = args.split_first()?;
    Some((first.to_lowercase(), rest))
}

/// One subcommand.
#[derive(Debug, Clone)]
pub struct Sub<H> {
    pub name: &'static str,
    pub aliases: &'static [&'static str],
    /// Action part of the permission `pumbo.<plugin>.<action>`.
    pub action: &'static str,
    /// Arguments as shown in usage lines: `<player> <time> [reason...]`.
    pub usage: &'static str,
    /// Message key of the one-line description.
    pub description: &'static str,
    /// Message key of the longer description or example shown when hovering
    /// over the subcommand in the help (empty: none).
    pub details: &'static str,
    /// Fewer arguments than this shows the usage line instead of running.
    pub min_args: usize,
    pub handler: H,
}

impl<H> Sub<H> {
    pub fn new(name: &'static str, action: &'static str, handler: H) -> Self {
        Self { name, aliases: &[], action, usage: "", description: "", details: "", min_args: 0, handler }
    }

    pub fn aliases(mut self, aliases: &'static [&'static str]) -> Self {
        self.aliases = aliases;
        self
    }

    pub fn usage(mut self, usage: &'static str, min_args: usize) -> Self {
        self.usage = usage;
        self.min_args = min_args;
        self
    }

    pub fn description(mut self, key: &'static str) -> Self {
        self.description = key;
        self
    }

    pub fn details(mut self, key: &'static str) -> Self {
        self.details = key;
        self
    }

    fn matches(&self, name: &str) -> bool {
        self.name == name || self.aliases.contains(&name)
    }
}

/// Result of routing a command line.
#[derive(Debug)]
pub enum Dispatch<'a, H> {
    Run {
        sub: &'a Sub<H>,
        args: &'a [String],
    },
    /// No subcommand or `help`: show the subcommands the sender may use.
    Help,
    Unknown {
        name: String,
    },
    NoPermission {
        permission: String,
    },
    /// Too few arguments: show this usage line.
    Usage {
        usage: String,
    },
}

/// The command tree of one plugin.
#[derive(Debug, Clone)]
pub struct Commands<H> {
    plugin: &'static str,
    label: Option<String>,
    short_alias: Option<String>,
    subs: Vec<Sub<H>>,
}

impl<H> Commands<H> {
    pub fn new(plugin: &'static str) -> Self {
        Self { plugin, label: None, short_alias: None, subs: Vec::new() }
    }

    /// The command shown in usage lines and the help, such as `/pumboauth`
    /// (default `/pumbo <plugin>`). On Pumpkin plugins register `/pumbo<plugin>`.
    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// The short command for the label (`/pf` for `/pumbofilter`), shown in
    /// the help header.
    pub fn short_alias(mut self, alias: impl Into<String>) -> Self {
        self.short_alias = Some(alias.into());
        self
    }

    pub fn alias_of_root(&self) -> Option<&str> {
        self.short_alias.as_deref()
    }

    pub fn with(mut self, sub: Sub<H>) -> Self {
        self.subs.push(sub);
        self
    }

    pub fn plugin(&self) -> &'static str {
        self.plugin
    }

    pub fn subs(&self) -> &[Sub<H>] {
        &self.subs
    }

    pub fn permission(&self, sub: &Sub<H>) -> String {
        permission(self.plugin, sub.action)
    }

    /// The command before the subcommand: the label, or `/pumbo <plugin>`.
    pub fn root(&self) -> String {
        self.label.clone().unwrap_or_else(|| format!("/{ROOT} {}", self.plugin))
    }

    /// `<root> <sub> <usage>`.
    pub fn usage(&self, sub: &Sub<H>) -> String {
        let line = format!("{} {}", self.root(), sub.name);
        if sub.usage.is_empty() { line } else { format!("{line} {}", sub.usage) }
    }

    /// `<root> help`, for "use ..." hints.
    pub fn help_line(&self) -> String {
        format!("{} help", self.root())
    }

    /// Routes the arguments after the plugin name. `allowed` answers whether the
    /// sender has a permission node.
    pub fn dispatch<'a>(&'a self, args: &'a [String], allowed: impl Fn(&str) -> bool) -> Dispatch<'a, H> {
        let Some((first, rest)) = args.split_first() else {
            return Dispatch::Help;
        };
        let name = first.to_lowercase();
        if name == "help" || name == "?" {
            return Dispatch::Help;
        }
        let Some(sub) = self.subs.iter().find(|s| s.matches(&name)) else {
            return Dispatch::Unknown { name };
        };
        let permission = self.permission(sub);
        if !allowed(&permission) {
            return Dispatch::NoPermission { permission };
        }
        if rest.len() < sub.min_args {
            return Dispatch::Usage { usage: self.usage(sub) };
        }
        Dispatch::Run { sub, args: rest }
    }

    /// Subcommands the sender may use, in registration order.
    pub fn visible<'a>(&'a self, allowed: impl Fn(&str) -> bool + 'a) -> impl Iterator<Item = &'a Sub<H>> + 'a {
        self.subs.iter().filter(move |s| allowed(&self.permission(s)))
    }

    /// Completions for the subcommand name (the first argument). Later arguments
    /// are completed by the plugin.
    pub fn complete(&self, args: &[String], allowed: impl Fn(&str) -> bool) -> Vec<String> {
        if args.len() > 1 {
            return Vec::new();
        }
        let typed = args.first().map(|a| a.to_lowercase()).unwrap_or_default();
        self.subs
            .iter()
            .filter(|s| allowed(&self.permission(s)))
            .map(|s| s.name.to_string())
            .filter(|n| n.starts_with(&typed))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type Handler = fn(&[String]) -> String;

    fn ban(args: &[String]) -> String {
        format!("ban {}", args.join(" "))
    }

    fn reload(_: &[String]) -> String {
        "reload".into()
    }

    fn tree() -> Commands<Handler> {
        Commands::new("bans")
            .with(Sub::new("ban", "ban", ban as Handler).aliases(&["b"]).usage("<player> [reason...]", 1))
            .with(Sub::new("reload", "admin.reload", reload as Handler).description("bans-help-reload"))
    }

    fn args(s: &str) -> Vec<String> {
        s.split_whitespace().map(str::to_string).collect()
    }

    #[test]
    fn permissions() {
        assert_eq!(permission("bans", "ban"), "pumbo.bans.ban");
        assert!(is_valid_node("admin.reload"));
        assert!(is_valid_node("two-factor"));
        assert!(!is_valid_node("Admin"));
        assert!(!is_valid_node("a..b"));
        assert!(!is_valid_node(""));
    }

    #[test]
    fn splits_lines() {
        let (n, a) = split_command("/Login  secret 123");
        assert_eq!(n, "login");
        assert_eq!(a, vec!["secret", "123"]);
        assert_eq!(split_command("pumbo:reg a b").0, "reg");
        assert_eq!(split_command("").0, "");
        let a = args("BANS ban Steve");
        let (plugin, rest) = umbrella(&a).unwrap();
        assert_eq!(plugin, "bans");
        assert_eq!(rest, &a[1..]);
        assert!(umbrella(&[]).is_none());
    }

    #[test]
    fn routes_and_checks() {
        let t = tree();
        let all = |_: &str| true;
        match t.dispatch(&args("B Steve hacking"), all) {
            Dispatch::Run { sub, args } => assert_eq!((sub.handler)(args), "ban Steve hacking"),
            other => panic!("{other:?}"),
        }
        assert!(
            matches!(t.dispatch(&args("ban"), all), Dispatch::Usage { usage } if usage == "/pumbo bans ban <player> [reason...]")
        );
        assert!(matches!(t.dispatch(&[], all), Dispatch::Help));
        assert!(matches!(t.dispatch(&args("help"), all), Dispatch::Help));
        assert!(matches!(t.dispatch(&args("kick x"), all), Dispatch::Unknown { name } if name == "kick"));
        let only_ban = |p: &str| p == "pumbo.bans.ban";
        assert!(
            matches!(t.dispatch(&args("reload"), only_ban), Dispatch::NoPermission { permission } if permission == "pumbo.bans.admin.reload")
        );
    }

    #[test]
    fn help_and_completion() {
        let t = tree();
        let only_ban = |p: &str| p == "pumbo.bans.ban";
        let names: Vec<_> = t.visible(only_ban).map(|s| s.name).collect();
        assert_eq!(names, vec!["ban"]);
        assert_eq!(t.complete(&args("r"), |_| true), vec!["reload"]);
        assert_eq!(t.complete(&[], only_ban), vec!["ban"]);
        assert!(t.complete(&args("ban St"), |_| true).is_empty());
        assert_eq!(t.usage(&t.subs()[1]), "/pumbo bans reload");
        assert_eq!(t.help_line(), "/pumbo bans help");
        assert_eq!(t.plugin(), "bans");
        let labelled = tree().label("/pumbobans");
        assert_eq!(labelled.root(), "/pumbobans");
        assert_eq!(labelled.help_line(), "/pumbobans help");
        assert_eq!(labelled.usage(&labelled.subs()[0]), "/pumbobans ban <player> [reason...]");
    }
}
