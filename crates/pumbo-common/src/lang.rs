//! Player-facing messages from `lang/<code>.yml`, with built-in defaults.
//!
//! Messages come in [`Bundle`]s: this crate ships [`COMMON`] (commands, time),
//! each core crate ships its own, and the platform layer adds its own last. A
//! message table for language `code` is built in layers, each one overriding
//! the previous:
//!
//! 1. the built-in English messages of every bundle,
//! 2. the built-in messages in `code` (bundles without that language skip it),
//! 3. the user's file (`lang/<code>.yml` in the data folder).
//!
//! So a partial or outdated translation never shows raw keys. Files are flat
//! YAML: `key: text` in the [`crate::text`] format (texts starting with `&`,
//! `{` and other YAML signs, or containing `: `, go in quotes). Besides the arguments of
//! a call, messages can use `{prefix}` (the `prefix` message) and `{nl}` (a new
//! line). An empty message is disabled: formatting it gives an empty string.
//!
//! Messages that depend on a number list their forms separated by `|`, in the
//! order of the language's plural rule ([`plural_index`]): `{0} minute|{0}
//! minutes` in English, `{0} minuta|{0} minuty|{0} minut` in Polish. See
//! [`Lang::plural`].

use std::collections::HashMap;

use crate::config::Warning;
use crate::text::{self, Args};

/// Built-in message files of one crate, as `(language code, YAML text)`. The
/// first entry is English and defines the keys.
#[derive(Debug, Clone, Copy)]
pub struct Bundle {
    pub name: &'static str,
    pub files: &'static [(&'static str, &'static str)],
}

impl Bundle {
    pub fn file(&self, code: &str) -> Option<&'static str> {
        self.files.iter().find(|(c, _)| *c == code).map(|(_, t)| *t)
    }
}

/// Messages shared by every plugin.
pub const COMMON: Bundle =
    Bundle { name: "common", files: &[("en", include_str!("../lang/en.yml")), ("pl", include_str!("../lang/pl.yml"))] };

#[derive(Debug, Clone)]
pub struct Lang {
    code: String,
    messages: HashMap<String, String>,
}

impl Lang {
    /// Builds the table for `code` from `bundles` and the user's file `user`.
    pub fn load(bundles: &[Bundle], code: &str, user: Option<&str>) -> (Self, Vec<Warning>) {
        let mut warnings = Vec::new();
        // `prefix` always exists (empty until a bundle or the user sets it), so a
        // user file may set it even when no bundle does.
        let mut messages = HashMap::from([("prefix".to_string(), String::new())]);
        for b in bundles {
            match b.files.first() {
                Some((_, en)) => match parse(en) {
                    Ok(m) => messages.extend(m),
                    Err(e) => {
                        warnings.push(Warning::new(None, format!("internal: built-in messages of {}: {e}", b.name)))
                    }
                },
                None => warnings.push(Warning::new(None, format!("internal: {} has no messages", b.name))),
            }
        }
        let builtin = bundles.iter().any(|b| b.file(code).is_some());
        if code != "en" {
            for text in bundles.iter().filter_map(|b| b.file(code)) {
                if let Ok(m) = parse(text) {
                    overlay(&mut messages, m, None);
                }
            }
        }
        match user.map(parse) {
            Some(Ok(m)) => overlay(&mut messages, m, Some(&mut warnings)),
            Some(Err(e)) => warnings.push(Warning::fatal(format!("not valid YAML: {e}"))),
            None if !builtin => {
                warnings.push(Warning::new(None, format!("no messages for language '{code}', using English")));
            }
            None => {}
        }
        (Self { code: code.to_string(), messages }, warnings)
    }

    /// Default content of `lang/<code>.yml`: the built-in files of every bundle
    /// in that language (English where a bundle has no such translation).
    pub fn template(bundles: &[Bundle], code: &str) -> String {
        let mut out = String::new();
        for b in bundles {
            let text = b.file(code).or_else(|| b.files.first().map(|(_, t)| *t)).unwrap_or("");
            out.push_str(&format!("# --- {} ---\n", b.name));
            out.push_str(text.trim_end());
            out.push_str("\n\n");
        }
        out
    }

    /// Language codes with built-in messages in at least one bundle.
    pub fn builtin_codes(bundles: &[Bundle]) -> Vec<&'static str> {
        let mut codes: Vec<&'static str> = bundles.iter().flat_map(|b| b.files.iter().map(|(c, _)| *c)).collect();
        codes.sort_unstable();
        codes.dedup();
        codes
    }

    /// For platforms that build the language files into the plugin
    /// (PumboProx, `pumbo_sdk::embed!`): makes `dir/<code>.yml` the
    /// [`Lang::template`] of each built-in language and returns the files it
    /// had to write. A test asserts none, so a changed message shows up as a
    /// file to commit.
    pub fn write_templates(bundles: &[Bundle], dir: &std::path::Path) -> Vec<String> {
        let mut written = Vec::new();
        for code in Self::builtin_codes(bundles) {
            let path = dir.join(format!("{code}.yml"));
            let text = Self::template(bundles, code);
            if std::fs::read_to_string(&path).ok().as_deref() != Some(text.as_str()) {
                let _ = std::fs::create_dir_all(dir);
                let _ = std::fs::write(&path, &text);
                written.push(path.display().to_string());
            }
        }
        written
    }

    pub fn code(&self) -> &str {
        &self.code
    }

    /// Formats a message. A missing key gives the key itself (so it shows up in
    /// testing), a disabled message an empty string.
    pub fn format(&self, key: &str, args: &Args) -> String {
        let Some(raw) = self.messages.get(key) else {
            return key.to_string();
        };
        if raw.is_empty() {
            return String::new();
        }
        let mut args = args.clone();
        if !args.has("prefix") {
            args.set("prefix", self.messages.get("prefix").map(String::as_str).unwrap_or(""));
        }
        if !args.has("nl") {
            args.set("nl", "\n");
        }
        text::fill(raw, &args)
    }

    /// Formats a message with positional arguments only.
    pub fn fmt(&self, key: &str, args: &[&str]) -> String {
        self.format(key, &Args::from(args))
    }

    pub fn get(&self, key: &str) -> String {
        self.format(key, &Args::new())
    }

    /// The form of a `|`-separated message that fits `n` in this language,
    /// with `{0}` set to `n`. A file with fewer forms than the rule needs uses
    /// its last one.
    pub fn plural(&self, key: &str, n: u64) -> String {
        let all = self.format(key, &Args::new().arg(n));
        let index = plural_index(&self.code, n);
        let forms: Vec<&str> = all.split('|').collect();
        forms.get(index).or(forms.last()).map(|f| f.to_string()).unwrap_or_default()
    }

    pub fn has(&self, key: &str) -> bool {
        self.messages.contains_key(key)
    }

    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.messages.keys().map(String::as_str)
    }
}

/// Which of `langs` fits a client locale (`pl_pl`, `en_US`): the one whose code
/// is the locale's language. `None`: no such language (use the configured one).
pub fn index_for_locale(langs: &[Lang], locale: Option<&str>) -> Option<usize> {
    let code = locale?.split(['_', '-']).next()?.to_lowercase();
    langs.iter().position(|l| l.code == code)
}

/// Which plural form fits `n` in language `code` (`pl`, `pl-PL`...), as the
/// index into a `|`-separated message:
/// - Polish: `1` → 0 (minuta), 2–4, 22–24, ... but not 12–14 → 1 (minuty), else 2 (minut)
/// - Russian, Ukrainian, Belarusian: like Polish, but 21, 31, ... also take 0
/// - Czech, Slovak: `1` → 0, 2–4 → 1, else 2
/// - English and everything else: `1` → 0, else 1
pub fn plural_index(code: &str, n: u64) -> usize {
    let language = code.split(['-', '_']).next().unwrap_or(code).to_ascii_lowercase();
    let (n10, n100) = (n % 10, n % 100);
    let few = (2..=4).contains(&n10) && !(12..=14).contains(&n100);
    match language.as_str() {
        "pl" if n == 1 => 0,
        "pl" if few => 1,
        "pl" => 2,
        "ru" | "uk" | "be" if n10 == 1 && n100 != 11 => 0,
        "ru" | "uk" | "be" if few => 1,
        "ru" | "uk" | "be" => 2,
        "cs" | "sk" if n == 1 => 0,
        "cs" | "sk" if (2..=4).contains(&n) => 1,
        "cs" | "sk" => 2,
        _ if n == 1 => 0,
        _ => 1,
    }
}

fn overlay(messages: &mut HashMap<String, String>, from: HashMap<String, String>, mut w: Option<&mut Vec<Warning>>) {
    for (k, v) in from {
        match messages.get_mut(&k) {
            Some(slot) => *slot = v,
            None => {
                if let Some(w) = w.as_deref_mut() {
                    w.push(Warning::new(Some(&k), format!("unknown message key '{k}' ignored")));
                }
            }
        }
    }
}

/// Parses a flat message file. Values that are not text (numbers, `true`,
/// sections) are skipped.
pub fn parse(text: &str) -> Result<HashMap<String, String>, String> {
    let table = match crate::config::parse_yaml(text)? {
        serde_json::Value::Object(t) => t,
        serde_json::Value::Null => serde_json::Map::new(),
        _ => return Err("expected `key: text` lines at the top level".into()),
    };
    Ok(table
        .into_iter()
        .filter_map(|(k, v)| match v {
            serde_json::Value::String(s) => Some((k, s)),
            _ => None,
        })
        .collect())
}

/// Checks a bundle: every file parses and has exactly the keys of the English
/// one. Returns the problems; meant for each crate's tests.
pub fn check_bundle(bundle: &Bundle) -> Vec<String> {
    let mut problems = Vec::new();
    let Some((_, en)) = bundle.files.first() else {
        return vec![format!("{} has no files", bundle.name)];
    };
    let en = match parse(en) {
        Ok(m) => m,
        Err(e) => return vec![format!("{}/en: {e}", bundle.name)],
    };
    for (code, text) in bundle.files.iter().skip(1) {
        match parse(text) {
            Ok(m) => {
                let mut missing: Vec<_> = en.keys().filter(|k| !m.contains_key(*k)).collect();
                let mut extra: Vec<_> = m.keys().filter(|k| !en.contains_key(*k)).collect();
                missing.sort();
                extra.sort();
                if !missing.is_empty() {
                    problems.push(format!("{}/{code} misses {missing:?}", bundle.name));
                }
                if !extra.is_empty() {
                    problems.push(format!("{}/{code} has unknown {extra:?}", bundle.name));
                }
            }
            Err(e) => problems.push(format!("{}/{code}: {e}", bundle.name)),
        }
    }
    problems
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLUGIN: Bundle = Bundle {
        name: "test",
        files: &[
            ("en", "prefix: \"[T] \"\nhello: \"{prefix}Hello {0}{nl}bye\"\nonly-en: english\noff: \"\"\n"),
            ("pl", "prefix: \"[T] \"\nhello: \"{prefix}Cześć {0}{nl}pa\"\noff: \"\"\n"),
        ],
    };

    #[test]
    fn language_for_a_locale() {
        let langs = [Lang::load(&[PLUGIN], "pl", None).0];
        assert_eq!(index_for_locale(&langs, Some("pl_pl")), Some(0));
        assert_eq!(index_for_locale(&langs, Some("PL-pl")), Some(0));
        assert_eq!(index_for_locale(&langs, Some("de_de")), None);
        assert_eq!(index_for_locale(&langs, None), None);
    }

    #[test]
    fn common_bundle_is_complete() {
        assert_eq!(check_bundle(&COMMON), Vec::<String>::new());
    }

    #[test]
    fn layers_override_in_order() {
        let (l, w) = Lang::load(&[COMMON, PLUGIN], "pl", Some("prefix: \"<P> \"\n"));
        assert!(w.is_empty(), "{w:?}");
        assert_eq!(l.fmt("hello", &["Ala"]), "<P> Cześć Ala\npa");
        // missing in the translation: English
        assert_eq!(l.get("only-en"), "english");
        // common messages come in the chosen language
        assert_eq!(l.get("command-reloaded"), "Konfiguracja przeładowana.");
        assert_eq!(l.code(), "pl");
    }

    #[test]
    fn unknown_broken_and_missing() {
        let (l, w) = Lang::load(&[PLUGIN], "en", Some("nope: x\nhello: 5\n"));
        assert_eq!(w.len(), 1);
        assert_eq!(w[0].option.as_deref(), Some("nope"));
        assert_eq!(l.fmt("hello", &["Bob"]), "[T] Hello Bob\nbye");
        // a prefix may be set even when no bundle defines one
        let (l, w) = Lang::load(&[COMMON], "en", Some("prefix: \"<C> \"\n"));
        assert!(w.is_empty(), "{w:?}");
        assert_eq!(l.get("prefix"), "<C> ");
        let (l, w) = Lang::load(&[PLUGIN], "en", Some("hello: [x\n"));
        assert!(w.len() == 1 && w[0].fatal, "{w:?}");
        assert!(w[0].message.contains("line 1"), "{}", w[0].message);
        assert!(l.has("hello"));
        assert_eq!(l.get("missing-key"), "missing-key");
        assert_eq!(l.get("off"), "");
    }

    #[test]
    fn language_without_builtin_messages() {
        let (l, w) = Lang::load(&[PLUGIN], "de", None);
        assert_eq!(w.len(), 1);
        assert_eq!(l.fmt("hello", &["X"]), "[T] Hello X\nbye");
        let (_, w) = Lang::load(&[PLUGIN], "de", Some("hello: Hallo {0}\n"));
        assert!(w.is_empty());
    }

    #[test]
    fn named_arguments_and_template() {
        let (l, _) = Lang::load(&[PLUGIN], "en", Some("hello: \"{prefix}{who} {0}\"\n"));
        assert_eq!(l.format("hello", &Args::new().arg(1).with("who", "Ann")), "[T] Ann 1");
        let t = Lang::template(&[COMMON, PLUGIN], "pl");
        let (parsed, w) = Lang::load(&[COMMON, PLUGIN], "pl", Some(&t));
        assert!(w.is_empty(), "{w:?}");
        assert_eq!(parsed.fmt("hello", &["A"]), "[T] Cześć A\npa");
        assert_eq!(Lang::builtin_codes(&[COMMON, PLUGIN]), vec!["en", "pl"]);
    }

    #[test]
    fn plural_rules() {
        let pl: Vec<usize> = [0u64, 1, 2, 4, 5, 11, 12, 14, 21, 22, 25, 102, 112].map(|n| plural_index("pl", n)).into();
        assert_eq!(pl, vec![2, 0, 1, 1, 2, 2, 2, 2, 2, 1, 2, 1, 2]);
        assert_eq!(plural_index("pl_PL", 3), 1);
        assert_eq!([0u64, 1, 2].map(|n| plural_index("en", n)), [1, 0, 1]);
        assert_eq!([1u64, 21, 11, 3].map(|n| plural_index("ru", n)), [0, 0, 2, 1]);
        assert_eq!([1u64, 3, 5].map(|n| plural_index("cs", n)), [0, 1, 2]);
        assert_eq!(plural_index("de", 7), 1);
    }

    #[test]
    fn plural_messages() {
        let (pl, _) = Lang::load(&[COMMON], "pl", None);
        let forms: Vec<String> = [1u64, 3, 5, 12, 22].iter().map(|n| pl.plural("time-minute", *n)).collect();
        assert_eq!(forms, vec!["1 minuta", "3 minuty", "5 minut", "12 minut", "22 minuty"]);
        let (en, _) = Lang::load(&[COMMON], "en", None);
        assert_eq!(en.plural("time-minute", 1), "1 minute");
        assert_eq!(en.plural("time-minute", 0), "0 minutes");
        // fewer forms than the rule needs: the last one
        let (short, _) = Lang::load(&[COMMON], "pl", Some("time-day: \"{0} d\"\n"));
        assert_eq!(short.plural("time-day", 5), "5 d");
    }

    #[test]
    fn bundle_check_reports_differences() {
        let p = check_bundle(&PLUGIN);
        assert_eq!(p.len(), 1);
        assert!(p[0].contains("only-en"));
    }
}
