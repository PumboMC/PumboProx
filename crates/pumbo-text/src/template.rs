//! Templates with placeholders (plan §5.8.3): MiniMessage text with
//! `%namespace_key%`, `%namespace_key:arg%`, `%namespace_key@context%`, short
//! aliases `%rank%`, literal `%%` and argument slots `{name}`.
//!
//! Rendering is one pass: values and arguments are never scanned again, so a
//! value cannot smuggle another placeholder and nothing can loop. Arguments go
//! in literally (escaped for MiniMessage).

/// Template size limit in bytes.
pub const MAX_TEMPLATE_BYTES: usize = 8 * 1024;
/// Placeholders per template.
pub const MAX_PLACEHOLDERS: usize = 32;
/// Argument of a placeholder (`%server_online:lobby%`), in characters.
pub const MAX_ARG_CHARS: usize = 64;

/// Explicit context of a placeholder (`@survival`, `@group=survivals`, `@global`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PlaceholderContext {
    Global,
    Group(String),
    Server(String),
}

/// One placeholder as written. `name` is `namespace_key` or an alias (no `_`
/// needed); the host splits it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Placeholder {
    pub name: String,
    pub arg: Option<String>,
    pub context: Option<PlaceholderContext>,
}

impl Placeholder {
    /// `(namespace, key)` when the name has the `namespace_key` form.
    pub fn split(&self) -> Option<(&str, &str)> {
        let (ns, key) = self.name.split_once('_')?;
        (!ns.is_empty() && !key.is_empty()).then_some((ns, key))
    }

    /// The placeholder as written, for `unresolved: keep`.
    pub fn written(&self) -> String {
        let mut s = format!("%{}", self.name);
        if let Some(a) = &self.arg {
            s.push(':');
            s.push_str(a);
        }
        match &self.context {
            None => {}
            Some(PlaceholderContext::Global) => s.push_str("@global"),
            Some(PlaceholderContext::Group(g)) => {
                s.push_str("@group=");
                s.push_str(g);
            }
            Some(PlaceholderContext::Server(v)) => {
                s.push('@');
                s.push_str(v);
            }
        }
        s.push('%');
        s
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Segment {
    /// MiniMessage text as written.
    Literal(String),
    Placeholder(Placeholder),
    /// `{name}`.
    Arg(String),
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TemplateError {
    #[error("template over {MAX_TEMPLATE_BYTES} bytes")]
    TooLarge,
    #[error("more than {MAX_PLACEHOLDERS} placeholders")]
    TooMany,
}

/// A parsed template.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Template {
    pub segments: Vec<Segment>,
}

impl Template {
    pub fn parse(input: &str) -> Result<Template, TemplateError> {
        if input.len() > MAX_TEMPLATE_BYTES {
            return Err(TemplateError::TooLarge);
        }
        let mut segments = Vec::new();
        let mut lit = String::new();
        let mut count = 0;
        let mut rest = input;
        while let Some(pos) = rest.find(['%', '{']) {
            let (before, tail) = rest.split_at(pos);
            lit.push_str(before);
            if let Some(after) = tail.strip_prefix("%%") {
                lit.push('%');
                rest = after;
                continue;
            }
            if let Some(body) = tail.strip_prefix('%') {
                if let Some((ph, after)) = read_placeholder(body) {
                    count += 1;
                    if count > MAX_PLACEHOLDERS {
                        return Err(TemplateError::TooMany);
                    }
                    flush(&mut lit, &mut segments);
                    segments.push(Segment::Placeholder(ph));
                    rest = after;
                } else {
                    lit.push('%');
                    rest = body;
                }
                continue;
            }
            let body = tail.get(1..).unwrap_or_default();
            match body.split_once('}') {
                Some((name, after)) if is_arg_name(name) => {
                    flush(&mut lit, &mut segments);
                    segments.push(Segment::Arg(name.to_string()));
                    rest = after;
                }
                _ => {
                    lit.push('{');
                    rest = body;
                }
            }
        }
        lit.push_str(rest);
        flush(&mut lit, &mut segments);
        Ok(Template { segments })
    }

    pub fn placeholders(&self) -> impl Iterator<Item = &Placeholder> {
        self.segments.iter().filter_map(|s| match s {
            Segment::Placeholder(p) => Some(p),
            _ => None,
        })
    }

    /// Renders MiniMessage text. `value` returns a MiniMessage-safe value for
    /// a placeholder (see [`escape_mini`], [`strip_events`]) or `None` when it
    /// is unresolved; `keep_unresolved` writes those back as text. Arguments
    /// missing from `args` stay as `{name}`.
    pub fn render(
        &self,
        args: &[(String, String)],
        keep_unresolved: bool,
        mut value: impl FnMut(&Placeholder) -> Option<String>,
    ) -> String {
        let mut out = String::new();
        for seg in &self.segments {
            match seg {
                Segment::Literal(s) => out.push_str(s),
                Segment::Arg(name) => match args.iter().find(|(n, _)| n == name) {
                    Some((_, v)) => out.push_str(&escape_mini(v)),
                    None => {
                        out.push('{');
                        out.push_str(name);
                        out.push('}');
                    }
                },
                Segment::Placeholder(p) => match value(p) {
                    Some(v) => out.push_str(&v),
                    None if keep_unresolved => out.push_str(&escape_mini(&p.written())),
                    None => {}
                },
            }
        }
        out
    }
}

fn flush(lit: &mut String, segments: &mut Vec<Segment>) {
    if !lit.is_empty() {
        segments.push(Segment::Literal(std::mem::take(lit)));
    }
}

fn is_arg_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 32
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

fn is_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

fn is_server(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
}

/// Reads `name[:arg][@ctx]%` at the start of `body`.
fn read_placeholder(body: &str) -> Option<(Placeholder, &str)> {
    let end = body.find('%')?;
    let inner = body.get(..end)?;
    let after = body.get(end + 1..)?;
    let (head, context) = match inner.split_once('@') {
        Some((h, c)) => (h, Some(parse_context(c)?)),
        None => (inner, None),
    };
    let (name, arg) = match head.split_once(':') {
        Some((n, a)) => {
            if a.is_empty() || a.chars().count() > MAX_ARG_CHARS {
                return None;
            }
            (n, Some(a.to_string()))
        }
        None => (head, None),
    };
    is_name(name).then(|| {
        (
            Placeholder {
                name: name.to_string(),
                arg,
                context,
            },
            after,
        )
    })
}

fn parse_context(c: &str) -> Option<PlaceholderContext> {
    if c == "global" {
        return Some(PlaceholderContext::Global);
    }
    if let Some(g) = c.strip_prefix("group=") {
        return is_server(g).then(|| PlaceholderContext::Group(g.to_string()));
    }
    is_server(c).then(|| PlaceholderContext::Server(c.to_string()))
}

/// Escapes text so MiniMessage shows it literally.
pub fn escape_mini(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c == '<' || c == '\\' {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Legacy `&` codes to MiniMessage tags (`&a` → `<green>`, `&#RRGGBB` →
/// `<#rrggbb>`); other text is escaped.
pub fn legacy_to_mini(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '&' {
            if c == '<' || c == '\\' {
                out.push('\\');
            }
            out.push(c);
            continue;
        }
        match chars.peek().copied() {
            Some('#') => {
                let hex: String = chars.clone().skip(1).take(6).collect();
                if hex.len() == 6 && hex.chars().all(|h| h.is_ascii_hexdigit()) {
                    for _ in 0..7 {
                        chars.next();
                    }
                    out.push_str("<#");
                    out.push_str(&hex.to_ascii_lowercase());
                    out.push('>');
                } else {
                    out.push('&');
                }
            }
            Some(code) => {
                let tag = match code.to_ascii_lowercase() {
                    'k' => Some("obfuscated"),
                    'l' => Some("bold"),
                    'm' => Some("strikethrough"),
                    'n' => Some("underlined"),
                    'o' => Some("italic"),
                    'r' => Some("reset"),
                    other => crate::NamedColor::from_code(other).map(crate::NamedColor::name),
                };
                match tag {
                    Some(t) => {
                        chars.next();
                        out.push('<');
                        out.push_str(t);
                        out.push('>');
                    }
                    None => out.push('&'),
                }
            }
            None => out.push('&'),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ph(name: &str, arg: Option<&str>, ctx: Option<PlaceholderContext>) -> Segment {
        Segment::Placeholder(Placeholder {
            name: name.into(),
            arg: arg.map(Into::into),
            context: ctx,
        })
    }

    #[test]
    fn parses_placeholders_args_and_literals() {
        let t = Template::parse(
            "<p>Hi {name}, %player_name% on %server_online:lobby% %rank@survival% \
             %meta_rank:x@group=survivals% %proxy_online@global% 50%% off 10% {Bad} {}",
        )
        .unwrap();
        assert_eq!(
            t.segments,
            vec![
                Segment::Literal("<p>Hi ".into()),
                Segment::Arg("name".into()),
                Segment::Literal(", ".into()),
                ph("player_name", None, None),
                Segment::Literal(" on ".into()),
                ph("server_online", Some("lobby"), None),
                Segment::Literal(" ".into()),
                ph(
                    "rank",
                    None,
                    Some(PlaceholderContext::Server("survival".into()))
                ),
                Segment::Literal(" ".into()),
                ph(
                    "meta_rank",
                    Some("x"),
                    Some(PlaceholderContext::Group("survivals".into()))
                ),
                Segment::Literal(" ".into()),
                ph("proxy_online", None, Some(PlaceholderContext::Global)),
                Segment::Literal(" 50% off 10% {Bad} {}".into()),
            ]
        );
        let p = t.placeholders().nth(1).unwrap();
        assert_eq!(p.split(), Some(("server", "online")));
        assert_eq!(p.written(), "%server_online:lobby%");
    }

    #[test]
    fn one_pass_and_literal_arguments() {
        let t = Template::parse("<red>{msg}</red> %a_b% {msg}").unwrap();
        let args = vec![(
            "msg".to_string(),
            "%a_b% <click:run_command:/op me>x".to_string(),
        )];
        let out = t.render(&args, true, |_| Some("%a_b%<bold>{msg}".into()));
        // The argument is escaped and its placeholder stays text; the value is
        // inserted once and never scanned again.
        assert_eq!(
            out,
            "<red>%a_b% \\<click:run_command:/op me>x</red> %a_b%<bold>{msg} %a_b% \\<click:run_command:/op me>x"
        );
        let c = crate::parse_mini(&out);
        assert!(c.plain_text().contains("<click:run_command:/op me>x"));
    }

    #[test]
    fn unresolved_keep_or_empty() {
        let t = Template::parse("a %x_y:1% b").unwrap();
        assert_eq!(t.render(&[], true, |_| None), "a %x_y:1% b");
        assert_eq!(t.render(&[], false, |_| None), "a  b");
    }

    #[test]
    fn limits() {
        assert_eq!(
            Template::parse(&"a".repeat(MAX_TEMPLATE_BYTES + 1)),
            Err(TemplateError::TooLarge)
        );
        assert_eq!(
            Template::parse(&"%a_b%".repeat(MAX_PLACEHOLDERS + 1)),
            Err(TemplateError::TooMany)
        );
        let long = format!("%a_b:{}%", "x".repeat(MAX_ARG_CHARS + 1));
        assert_eq!(Template::parse(&long).unwrap().placeholders().count(), 0);
    }

    #[test]
    fn legacy_conversion() {
        assert_eq!(
            legacy_to_mini("&aHi &l<b> &#FF00aa! &z"),
            "<green>Hi <bold>\\<b> <#ff00aa>! &z"
        );
        assert_eq!(escape_mini("a<b\\c"), "a\\<b\\\\c");
    }
}
