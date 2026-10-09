//! Validation of plugin configs against the JSON Schema of their description
//! (plan §6.6.5): the subset `schemars` writes (`type`, `properties`,
//! `required`, `additionalProperties`, `items`, `enum`, `const`, numeric and
//! length bounds, `$ref` to `$defs`, `anyOf`, `oneOf`, `allOf`). Other
//! keywords are ignored. Config files are merged like the SDK merges them.

use std::path::Path;

use serde_json::{Map, Value};

/// Deep merge as in the SDK: mappings merge, other values (lists too) replace.
pub fn merge(base: &mut Map<String, Value>, over: &Map<String, Value>) {
    for (k, v) in over {
        match (base.get_mut(k), v) {
            (Some(Value::Object(b)), Value::Object(o)) => merge(b, o),
            _ => {
                base.insert(k.clone(), v.clone());
            }
        }
    }
}

fn type_ok(t: &str, v: &Value) -> bool {
    match t {
        "object" => v.is_object(),
        "array" => v.is_array(),
        "string" => v.is_string(),
        "boolean" => v.is_boolean(),
        "number" => v.is_number(),
        "integer" => v.is_i64() || v.is_u64(),
        "null" => v.is_null(),
        _ => true,
    }
}

fn resolve<'a>(root: &'a Value, schema: &'a Value) -> &'a Value {
    match schema.get("$ref").and_then(Value::as_str) {
        Some(r) => r
            .strip_prefix("#/")
            .map(|p| {
                p.split('/')
                    .fold(root, |acc, part| acc.get(part).unwrap_or(&Value::Null))
            })
            .unwrap_or(&Value::Null),
        None => schema,
    }
}

fn join(path: &str, part: &str) -> String {
    if path.is_empty() {
        part.to_string()
    } else {
        format!("{path}.{part}")
    }
}

fn check(root: &Value, schema: &Value, v: &Value, path: &str, depth: usize) -> Result<(), String> {
    if depth > 64 {
        return Err(format!("{path}: schema too deep"));
    }
    let s = resolve(root, schema);
    let here = if path.is_empty() { "config" } else { path };
    match s {
        Value::Bool(true) | Value::Null => return Ok(()),
        Value::Bool(false) => return Err(format!("{here}: not allowed")),
        _ => {}
    }
    if let Some(t) = s.get("type") {
        let ok = match t {
            Value::String(t) => type_ok(t, v),
            Value::Array(ts) => ts.iter().filter_map(Value::as_str).any(|t| type_ok(t, v)),
            _ => true,
        };
        if !ok {
            return Err(format!("{here}: expected {t}"));
        }
    }
    if let Some(e) = s.get("enum").and_then(Value::as_array)
        && !e.contains(v)
    {
        return Err(format!(
            "{here}: must be one of {}",
            Value::Array(e.clone())
        ));
    }
    if let Some(c) = s.get("const")
        && c != v
    {
        return Err(format!("{here}: must be {c}"));
    }
    if let Some(n) = v.as_f64() {
        let bound = |k: &str| s.get(k).and_then(Value::as_f64);
        if bound("minimum").is_some_and(|m| n < m)
            || bound("exclusiveMinimum").is_some_and(|m| n <= m)
        {
            return Err(format!("{here}: too small"));
        }
        if bound("maximum").is_some_and(|m| n > m)
            || bound("exclusiveMaximum").is_some_and(|m| n >= m)
        {
            return Err(format!("{here}: too large"));
        }
    }
    if let Some(st) = v.as_str() {
        let len = st.chars().count() as u64;
        if s.get("minLength")
            .and_then(Value::as_u64)
            .is_some_and(|m| len < m)
        {
            return Err(format!("{here}: too short"));
        }
        if s.get("maxLength")
            .and_then(Value::as_u64)
            .is_some_and(|m| len > m)
        {
            return Err(format!("{here}: too long"));
        }
    }
    if let Some(obj) = v.as_object() {
        let props = s.get("properties").and_then(Value::as_object);
        if let Some(req) = s.get("required").and_then(Value::as_array) {
            for r in req.iter().filter_map(Value::as_str) {
                if !obj.contains_key(r) {
                    return Err(format!("{}: missing", join(path, r)));
                }
            }
        }
        for (k, val) in obj {
            let p = join(path, k);
            match props.and_then(|p| p.get(k)) {
                Some(ps) => check(root, ps, val, &p, depth + 1)?,
                None => match s.get("additionalProperties") {
                    Some(Value::Bool(false)) => return Err(format!("{p}: unknown option")),
                    Some(extra) => check(root, extra, val, &p, depth + 1)?,
                    None => {}
                },
            }
        }
    }
    if let (Some(items), Some(arr)) = (s.get("items"), v.as_array()) {
        for (i, val) in arr.iter().enumerate() {
            check(root, items, val, &format!("{here}[{i}]"), depth + 1)?;
        }
    }
    if let Some(all) = s.get("allOf").and_then(Value::as_array) {
        for sub in all {
            check(root, sub, v, path, depth + 1)?;
        }
    }
    for key in ["anyOf", "oneOf"] {
        if let Some(any) = s.get(key).and_then(Value::as_array) {
            let mut first_err = None;
            let ok = any
                .iter()
                .any(|sub| match check(root, sub, v, path, depth + 1) {
                    Ok(()) => true,
                    Err(e) => {
                        first_err.get_or_insert(e);
                        false
                    }
                });
            if !ok {
                return Err(first_err.unwrap_or_else(|| format!("{here}: no variant matches")));
            }
        }
    }
    Ok(())
}

/// Validates a value against a schema given as JSON text (empty = anything).
pub fn validate(schema: &str, v: &Value) -> Result<(), String> {
    if schema.trim().is_empty() {
        return Ok(());
    }
    let root: Value = serde_json::from_str(schema).map_err(|e| format!("schema: {e}"))?;
    check(&root, &root, v, "", 0)
}

/// A YAML mapping file (an empty file is an empty mapping); `None` when the
/// file does not exist.
pub fn read_table(path: &Path) -> Result<Option<Map<String, Value>>, String> {
    match std::fs::read_to_string(path) {
        Ok(t) => match pumbo_core::yaml::from_str::<Value>(&t)? {
            Value::Object(m) => Ok(Some(m)),
            Value::Null => Ok(Some(Map::new())),
            _ => Err("expected `key: value` lines at the top level".into()),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.to_string()),
    }
}

/// Validates `config.yml` and every overlay over it (each on its own);
/// errors name the file.
pub fn validate_dir(schema: &str, dir: &Path) -> Result<(), String> {
    let global = read_table(&dir.join("config.yml"))
        .map_err(|e| format!("config.yml: {e}"))?
        .unwrap_or_default();
    validate(schema, &Value::Object(global.clone())).map_err(|e| format!("config.yml: {e}"))?;
    for sub in ["servers", "groups"] {
        let Ok(entries) = std::fs::read_dir(dir.join(sub)) else {
            continue;
        };
        let mut files: Vec<_> = entries.flatten().map(|e| e.path()).collect();
        files.sort();
        for f in files
            .iter()
            .filter(|f| f.extension().is_some_and(|e| e == "yml"))
        {
            let name = format!(
                "{sub}/{}",
                f.file_name().and_then(|n| n.to_str()).unwrap_or_default()
            );
            let over = read_table(f)
                .map_err(|e| format!("{name}: {e}"))?
                .unwrap_or_default();
            let mut merged = global.clone();
            merge(&mut merged, &over);
            validate(schema, &Value::Object(merged)).map_err(|e| format!("{name}: {e}"))?;
        }
    }
    Ok(())
}

/// The config with values of `x-pumbo-secret` fields replaced (for tools).
pub fn redact(schema: &str, v: &mut Value) {
    let Ok(root) = serde_json::from_str::<Value>(schema) else {
        return;
    };
    fn walk(root: &Value, s: &Value, v: &mut Value, depth: usize) {
        if depth > 64 {
            return;
        }
        let s = resolve(root, s);
        if s.get("x-pumbo-secret").and_then(Value::as_bool) == Some(true) {
            *v = Value::String("***".into());
            return;
        }
        if let (Some(props), Some(obj)) = (
            s.get("properties").and_then(Value::as_object),
            v.as_object_mut(),
        ) {
            for (k, val) in obj.iter_mut() {
                if let Some(ps) = props.get(k) {
                    walk(root, ps, val, depth + 1);
                }
            }
        }
        for key in ["anyOf", "oneOf", "allOf"] {
            if let Some(list) = s.get(key).and_then(Value::as_array) {
                for sub in list {
                    walk(root, sub, v, depth + 1);
                }
            }
        }
    }
    walk(&root, &root, v, 0);
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCHEMA: &str = r##"{
      "$schema": "https://json-schema.org/draft/2020-12/schema",
      "type": "object",
      "properties": {
        "greeting": { "type": "string", "minLength": 1 },
        "level": { "type": "integer", "minimum": 1, "maximum": 10 },
        "mode": { "type": "string", "enum": ["a", "b"] },
        "key": { "type": ["string", "null"], "x-pumbo-secret": true },
        "limits": { "$ref": "#/$defs/Limits" },
        "tags": { "type": "array", "items": { "type": "string" } }
      },
      "required": ["greeting"],
      "additionalProperties": false,
      "$defs": {
        "Limits": { "type": "object", "properties": { "a": { "type": "integer" } }, "additionalProperties": false }
      }
    }"##;

    fn v(yaml: &str) -> Value {
        pumbo_core::yaml::from_str(yaml).unwrap()
    }

    #[test]
    fn validates_the_subset() {
        assert_eq!(
            validate(
                SCHEMA,
                &v("greeting: hi\nlevel: 3\nmode: a\nlimits:\n  a: 1\n")
            ),
            Ok(())
        );
        let cases = [
            ("level: 3\n", "greeting: missing"),
            ("greeting: \"\"\n", "greeting: too short"),
            ("greeting: x\nlevel: 11\n", "level: too large"),
            ("greeting: x\nlevel: 1.5\n", "level: expected \"integer\""),
            ("greeting: x\nmode: c\n", "mode: must be one of"),
            ("greeting: x\ncolour: 1\n", "colour: unknown option"),
            ("greeting: x\nlimits:\n  b: 1\n", "limits.b: unknown option"),
            ("greeting: x\ntags: [a, 1]\n", "tags[1]: expected"),
            // YAML 1.2: `no` is text, not a boolean
            ("greeting: no\nmode: off\n", "mode: must be one of"),
        ];
        for (text, want) in cases {
            let err = validate(SCHEMA, &v(text)).unwrap_err();
            assert!(err.starts_with(want) || err.contains(want), "{text}: {err}");
        }
        assert_eq!(validate("", &v("x: 1\n")), Ok(()));
    }

    #[test]
    fn secrets_are_redacted() {
        let mut c = v("greeting: hi\nkey: s3cr3t\n");
        redact(SCHEMA, &mut c);
        assert_eq!(c["key"], "***");
        assert_eq!(c["greeting"], "hi");
        assert!(!c.to_string().contains("s3cr3t"));
    }
}
