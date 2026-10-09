//! Translations (plan §6.6.1): `lang/en.yml` and `lang/pl.yml` built into
//! the plugin, the administrator's texts in `lang/<language>.yml` of the
//! plugin's config directory (the host writes the built-in files there at
//! the first start when [`crate::embed!`] carries them; keys missing from
//! a file keep the built-in text). The language comes from the client's locale
//! (`pl_pl` → `pl`), then `general.language`, then `en`. Nested mappings give
//! dotted keys (`greeting` under `hello` is `hello.greeting`). Values are
//! templates: `{name}` arguments go in literally, `%...%` placeholders work.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};

use crate::bindings::pumbo::prox::types::{PlayerId, Text};

type Table = BTreeMap<String, String>;

fn flatten(prefix: &str, t: &serde_json::Map<String, serde_json::Value>, out: &mut Table) {
    for (k, v) in t {
        let key = if prefix.is_empty() {
            k.clone()
        } else {
            format!("{prefix}.{k}")
        };
        match v {
            serde_json::Value::String(s) => {
                out.insert(key, s.clone());
            }
            serde_json::Value::Object(sub) => flatten(&key, sub, out),
            _ => {}
        }
    }
}

fn parse(text: &str) -> Result<Table, String> {
    let mut out = Table::new();
    match crate::config::from_yaml::<serde_json::Value>(text)? {
        serde_json::Value::Object(t) => flatten("", &t, &mut out),
        serde_json::Value::Null => {}
        _ => return Err("expected `key: text` lines at the top level".into()),
    }
    Ok(out)
}

fn args_of(s: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut rest = s;
    while let Some(start) = rest.find('{') {
        let after = rest.get(start + 1..).unwrap_or_default();
        match after.find('}') {
            Some(end) => {
                out.insert(after.get(..end).unwrap_or_default().to_string());
                rest = after.get(end + 1..).unwrap_or_default();
            }
            None => break,
        }
    }
    out
}

pub struct Lang {
    builtin: Vec<(String, Table)>,
    overrides: RefCell<BTreeMap<String, Table>>,
    default: RefCell<String>,
    prefix: RefCell<String>,
}

impl Lang {
    /// Built-in files: `[("en", include_str!("../lang/en.yml")), ...]`.
    pub fn new(builtin: &[(&str, &str)]) -> Lang {
        let builtin = builtin
            .iter()
            .map(|(l, text)| {
                let table = parse(text).unwrap_or_else(|e| {
                    crate::log::error(&format!("lang/{l}.yml: {e}"));
                    Table::new()
                });
                (l.to_string(), table)
            })
            .collect();
        Lang {
            builtin,
            overrides: RefCell::new(BTreeMap::new()),
            default: RefCell::new("en".into()),
            prefix: RefCell::new(String::new()),
        }
    }

    /// Reads the administrator's overrides.
    pub fn reload(&self) -> Result<(), String> {
        let mut o = BTreeMap::new();
        for (l, _) in &self.builtin {
            if let Some(text) = crate::host::read_config_file(&format!("lang/{l}.yml")) {
                o.insert(
                    l.clone(),
                    parse(&text).map_err(|e| format!("lang/{l}.yml: {e}"))?,
                );
            } else if crate::host::read_config_file(&format!("lang/{l}.toml")).is_some() {
                crate::log::warn(&format!(
                    "found lang/{l}.toml, this version reads lang/{l}.yml - convert it (see README)"
                ));
            }
        }
        *self.overrides.borrow_mut() = o;
        Ok(())
    }

    pub fn set_default(&self, language: &str) {
        *self.default.borrow_mut() = language.to_string();
    }

    /// MiniMessage of the `<prefix>` tag (`messages.prefix`).
    pub fn set_prefix(&self, mini: &str) {
        *self.prefix.borrow_mut() = mini.to_string();
    }

    fn has(&self, l: &str) -> bool {
        self.builtin.iter().any(|(b, _)| b == l)
    }

    pub fn language(&self, player: Option<PlayerId>) -> String {
        let locale = player
            .and_then(crate::players::get)
            .and_then(|p| p.settings)
            .map(|s| s.locale.to_ascii_lowercase());
        if let Some(loc) = locale {
            let short = loc.split(['_', '-']).next().unwrap_or_default().to_string();
            if self.has(&short) {
                return short;
            }
        }
        let d = self.default.borrow().clone();
        if self.has(&d) { d } else { "en".into() }
    }

    pub fn raw(&self, language: &str, key: &str) -> Option<String> {
        if let Some(v) = self
            .overrides
            .borrow()
            .get(language)
            .and_then(|t| t.get(key))
        {
            return Some(v.clone());
        }
        let find = |l: &str| {
            self.builtin
                .iter()
                .find(|(b, _)| b == l)
                .and_then(|(_, t)| t.get(key).cloned())
        };
        find(language).or_else(|| find("en"))
    }

    /// The translated text for a player, as a template.
    pub fn text(
        &self,
        player: Option<PlayerId>,
        key: &str,
        args: &[(&str, &dyn std::fmt::Display)],
    ) -> Text {
        let lang = self.language(player);
        let mini = self
            .raw(&lang, key)
            .unwrap_or_else(|| crate::text::escape(key));
        let mini = mini.replace("<prefix>", &self.prefix.borrow());
        crate::text::template(mini, args)
    }

    /// Every built-in language has the same keys with the same arguments
    /// (for a test in every plugin's CI).
    pub fn check(&self) -> Result<(), String> {
        let Some((first, base)) = self.builtin.first() else {
            return Ok(());
        };
        for (l, t) in self.builtin.iter().skip(1) {
            for (k, v) in base {
                match t.get(k) {
                    None => return Err(format!("{l}: missing key {k} (present in {first})")),
                    Some(other) if args_of(other) != args_of(v) => {
                        return Err(format!("{l}: key {k} has other arguments than in {first}"));
                    }
                    _ => {}
                }
            }
            if let Some(k) = t.keys().find(|k| !base.contains_key(*k)) {
                return Err(format!("{l}: extra key {k} (missing in {first})"));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing;

    const EN: &str = "hello:\n  greeting: \"<prefix>Hello {name}!\"\n  bye: Bye\n";
    const PL: &str = "hello:\n  greeting: \"<prefix>Cześć {name}!\"\n  bye: Pa\n";

    #[test]
    fn language_per_player_overrides_and_check() {
        testing::reset();
        let lang = Lang::new(&[("en", EN), ("pl", PL)]);
        lang.set_prefix("<p>[Ex] ");
        lang.check().unwrap();
        let pl = testing::add_player("Ania", Some("lobby"));
        testing::with(|h| {
            if let Some(p) = h.players.get_mut(&pl) {
                p.settings = Some(testing::settings("pl_PL"));
            }
        });
        let de = testing::add_player("Hans", Some("lobby"));
        testing::with(|h| {
            if let Some(p) = h.players.get_mut(&de) {
                p.settings = Some(testing::settings("de_de"));
            }
        });
        assert_eq!(lang.language(Some(pl)), "pl");
        assert_eq!(lang.language(Some(de)), "en");
        lang.set_default("pl");
        assert_eq!(lang.language(Some(de)), "pl");
        let Text::Template(t) = lang.text(Some(pl), "hello.greeting", &[("name", &"<b>x")]) else {
            unreachable!()
        };
        assert_eq!(t.mini, "<p>[Ex] Cześć {name}!");
        assert_eq!(t.args, vec![("name".to_string(), "<b>x".to_string())]);

        testing::config_file("lang/pl.yml", "hello:\n  bye: Do widzenia\n");
        lang.reload().unwrap();
        assert_eq!(lang.raw("pl", "hello.bye").as_deref(), Some("Do widzenia"));
        assert_eq!(
            lang.raw("pl", "hello.greeting").as_deref(),
            Some("<prefix>Cześć {name}!")
        );

        let broken = Lang::new(&[
            ("en", EN),
            ("pl", "hello:\n  greeting: Cześć {who}\n  bye: Pa\n"),
        ]);
        assert!(broken.check().is_err());
        let missing = Lang::new(&[("en", EN), ("pl", "hello:\n  bye: Pa\n")]);
        assert!(missing.check().is_err());
    }
}
