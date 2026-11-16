//! Sections of `pumboprox.yml`: the top-level keys the proxy and the plugin
//! host read, each with the JSON Schema of its keys, and the mark on the keys
//! that anyone other than the console or an operator may change.

use serde::de::{self, DeserializeOwned, Deserializer, Visitor};
use serde_json::{Map, Value, json};

/// A top-level section of `pumboprox.yml` and the JSON Schema of its value,
/// in the subset `pumbo-host/src/schema.rs` reads.
#[derive(Debug, Clone)]
pub struct SectionInfo {
    pub key: &'static str,
    pub schema: Value,
}

/// `"x-pumbo-delegable"` on a key's schema, with any value: anyone other
/// than the console or an operator (a token, the panel, a module) may change
/// that key and everything under it, within their scopes. The code that
/// registers a section marks its keys; a key that changes who may do what,
/// which code runs or where the proxy listens and connects is never marked.
pub const DELEGABLE: &str = "x-pumbo-delegable";

/// The paths of the keys of `schema` marked with [`DELEGABLE`], found
/// through `properties`, `additionalProperties` and `items`. An entry of a
/// map or an item of a list is `*`; the empty path is `schema` itself. A
/// mark anywhere else (`patternProperties`, `$defs`, a combinator, a keyword
/// this walk does not know) is an error, so no mark goes unseen.
pub fn marked_paths(schema: &Value) -> Result<Vec<String>, String> {
    fn walk(schema: &Value, path: &str, out: &mut Vec<String>) {
        let Some(obj) = schema.as_object() else {
            return;
        };
        if obj.contains_key(DELEGABLE) {
            out.push(path.to_string());
        }
        let join = |k: &str| {
            if path.is_empty() {
                k.to_string()
            } else {
                format!("{path}.{k}")
            }
        };
        if let Some(props) = obj.get("properties").and_then(Value::as_object) {
            for (k, s) in props {
                walk(s, &join(k), out);
            }
        }
        for key in ["additionalProperties", "items"] {
            if let Some(s) = obj.get(key) {
                walk(s, &join("*"), out);
            }
        }
    }
    fn count(v: &Value) -> usize {
        match v {
            Value::Object(m) => {
                usize::from(m.contains_key(DELEGABLE)) + m.values().map(count).sum::<usize>()
            }
            Value::Array(a) => a.iter().map(count).sum(),
            _ => 0,
        }
    }
    let mut out = Vec::new();
    walk(schema, "", &mut out);
    let all = count(schema);
    if all == out.len() {
        Ok(out)
    } else {
        Err(format!(
            "{} of {all} {DELEGABLE} marks sit where only properties, additionalProperties and items are followed",
            all - out.len()
        ))
    }
}

/// The field names a struct's `Deserialize` asks for, renames applied. A
/// struct with a flattened field asks for a map and gives none.
pub fn fields<T: DeserializeOwned>() -> &'static [&'static str] {
    struct Probe<'a>(&'a mut &'static [&'static str]);
    impl<'de> Deserializer<'de> for Probe<'_> {
        type Error = de::value::Error;
        fn deserialize_any<V: Visitor<'de>>(self, _: V) -> Result<V::Value, Self::Error> {
            Err(de::Error::custom("not a struct"))
        }
        fn deserialize_struct<V: Visitor<'de>>(
            self,
            _: &'static str,
            fields: &'static [&'static str],
            _: V,
        ) -> Result<V::Value, Self::Error> {
            *self.0 = fields;
            Err(de::Error::custom("fields read"))
        }
        serde::forward_to_deserialize_any! {
            bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string
            bytes byte_buf option unit unit_struct newtype_struct seq tuple
            tuple_struct map enum identifier ignored_any
        }
    }
    let mut out: &'static [&'static str] = &[];
    let _ = T::deserialize(Probe(&mut out));
    out
}

/// The keys of a struct as a schema: a mapping with exactly the keys of
/// [`fields`], any value under each. A struct with a flattened field has no
/// keys here, so its section needs a schema of its own.
pub fn keys_schema<T: DeserializeOwned>() -> Value {
    let properties: Map<String, Value> = fields::<T>()
        .iter()
        .map(|k| ((*k).to_string(), json!({})))
        .collect();
    json!({ "type": "object", "properties": properties, "additionalProperties": false })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Deserialize)]
    #[serde(rename_all = "kebab-case")]
    #[allow(dead_code)]
    struct Plain {
        max_players: u32,
        #[serde(rename = "virtual")]
        virtual_world: bool,
    }

    #[derive(Deserialize)]
    #[allow(dead_code)]
    struct Flat {
        mode: String,
        #[serde(flatten)]
        rest: Map<String, Value>,
    }

    #[test]
    fn fields_follow_the_serde_names() {
        assert_eq!(fields::<Plain>(), ["max-players", "virtual"]);
        assert!(fields::<Flat>().is_empty());
    }

    #[test]
    fn keys_schema_is_closed() {
        assert_eq!(
            keys_schema::<Plain>(),
            json!({
                "type": "object",
                "properties": { "max-players": {}, "virtual": {} },
                "additionalProperties": false,
            })
        );
        // No field list, no key: every key of such a section is unknown.
        assert_eq!(
            keys_schema::<Flat>(),
            json!({ "type": "object", "properties": {}, "additionalProperties": false })
        );
    }

    #[test]
    fn marks_are_found_under_properties_entries_and_items() {
        let schema = json!({
            "type": "object",
            "x-pumbo-delegable": false,
            "properties": {
                "motd": { "x-pumbo-delegable": true },
                "servers": {
                    "additionalProperties": {
                        "properties": { "address": { "x-pumbo-delegable": true } },
                    },
                },
                "list": { "items": { "x-pumbo-delegable": 1 } },
            },
        });
        assert_eq!(
            marked_paths(&schema).unwrap(),
            ["", "list.*", "motd", "servers.*.address"]
        );
        assert!(
            marked_paths(&json!({ "type": "object" }))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn a_mark_the_walk_does_not_follow_is_an_error() {
        for schema in [
            json!({ "patternProperties": { "^a": { "x-pumbo-delegable": true } } }),
            json!({
                "$defs": { "A": { "x-pumbo-delegable": true } },
                "properties": { "a": { "$ref": "#/$defs/A" } },
            }),
            json!({ "items": [{ "x-pumbo-delegable": true }] }),
            json!({ "anyOf": [{ "x-pumbo-delegable": true }] }),
            json!({ "not": { "x-pumbo-delegable": true } }),
        ] {
            assert!(marked_paths(&schema).is_err(), "{schema}");
        }
    }
}
