//! Values this crate keeps without interpreting them (item components,
//! dialogs, custom payloads, unknown actions), in the form they were read.

use pumbo_nbt::{Compound, List, Tag};
use serde_json::Value;

/// A raw value from JSON or NBT. Written back unchanged in the same form;
/// converted best effort to the other form.
#[derive(Debug, Clone, PartialEq)]
pub enum Raw {
    Json(Value),
    Nbt(Tag),
}

impl Raw {
    pub fn to_json(&self) -> Value {
        match self {
            Raw::Json(v) => v.clone(),
            Raw::Nbt(t) => nbt_to_json(t),
        }
    }

    pub fn to_nbt(&self) -> Tag {
        match self {
            Raw::Nbt(t) => t.clone(),
            Raw::Json(v) => json_to_nbt(v),
        }
    }

    /// Text if the value is a string.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Raw::Json(Value::String(s)) => Some(s),
            Raw::Nbt(Tag::String(s)) => Some(s),
            _ => None,
        }
    }
}

fn nbt_to_json(t: &Tag) -> Value {
    match t {
        Tag::Byte(v) => Value::from(*v),
        Tag::Short(v) => Value::from(*v),
        Tag::Int(v) => Value::from(*v),
        Tag::Long(v) => Value::from(*v),
        Tag::Float(v) => {
            serde_json::Number::from_f64(f64::from(*v)).map_or(Value::Null, Value::Number)
        }
        Tag::Double(v) => serde_json::Number::from_f64(*v).map_or(Value::Null, Value::Number),
        Tag::ByteArray(v) => Value::Array(v.iter().map(|b| Value::from(*b as i8)).collect()),
        Tag::String(s) => Value::String(s.clone()),
        Tag::List(l) => Value::Array(
            l.items
                .iter()
                .map(|item| nbt_to_json(unwrap_heterogeneous(item)))
                .collect(),
        ),
        Tag::Compound(c) => Value::Object(
            c.iter()
                .map(|(k, v)| (k.to_string(), nbt_to_json(v)))
                .collect(),
        ),
        Tag::IntArray(v) => Value::Array(v.iter().map(|x| Value::from(*x)).collect()),
        Tag::LongArray(v) => Value::Array(v.iter().map(|x| Value::from(*x)).collect()),
    }
}

fn json_to_nbt(v: &Value) -> Tag {
    match v {
        Value::Null => Tag::Compound(Compound::new()),
        Value::Bool(b) => Tag::Byte(i8::from(*b)),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                i32::try_from(i).map_or(Tag::Long(i), Tag::Int)
            } else {
                Tag::Double(n.as_f64().unwrap_or(0.0))
            }
        }
        Value::String(s) => Tag::String(s.clone()),
        Value::Array(items) => Tag::List(nbt_list(items.iter().map(json_to_nbt).collect())),
        Value::Object(m) => Tag::Compound(Compound(
            m.iter().map(|(k, v)| (k.clone(), json_to_nbt(v))).collect(),
        )),
    }
}

/// A compound with the single key `""` wraps a list element of another type
/// (vanilla's way of storing lists of mixed types in NBT).
pub(crate) fn is_wrapper(c: &Compound) -> bool {
    c.len() == 1 && c.get("").is_some()
}

pub(crate) fn unwrap_heterogeneous(t: &Tag) -> &Tag {
    match t {
        Tag::Compound(c) if is_wrapper(c) => c.get("").unwrap_or(t),
        _ => t,
    }
}

/// Builds an NBT list; mixed element types are stored as compounds, with
/// non-compound elements (and compounds that look like wrappers) wrapped in
/// `{"": element}`.
pub(crate) fn nbt_list(items: Vec<Tag>) -> List {
    let Some(first) = items.first().map(Tag::id) else {
        return List::default();
    };
    if items.iter().all(|t| t.id() == first) {
        return List::new(first, items);
    }
    let wrapped = items
        .into_iter()
        .map(|t| match t {
            Tag::Compound(c) if !is_wrapper(&c) => Tag::Compound(c),
            other => Tag::Compound(Compound(vec![(String::new(), other)])),
        })
        .collect();
    List::new(pumbo_nbt::id::COMPOUND, wrapped)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mixed_lists_are_wrapped() {
        let l = nbt_list(vec![
            Tag::String("a".into()),
            Tag::Compound(Compound(vec![("text".into(), Tag::String("b".into()))])),
        ]);
        assert_eq!(l.element, pumbo_nbt::id::COMPOUND);
        let first = l.items.first().unwrap();
        assert_eq!(unwrap_heterogeneous(first), &Tag::String("a".into()));
        let same = nbt_list(vec![Tag::Int(1), Tag::Int(2)]);
        assert_eq!(same.element, pumbo_nbt::id::INT);
    }

    #[test]
    fn json_nbt_conversion() {
        let v: Value =
            serde_json::from_str(r#"{"a":[1,"x"],"b":true,"c":2.5,"d":5000000000}"#).unwrap();
        let back = Raw::Nbt(Raw::Json(v.clone()).to_nbt()).to_json();
        assert_eq!(back.get("a"), v.get("a"));
        assert_eq!(back.get("b"), Some(&Value::from(1)));
        assert_eq!(back.get("c"), v.get("c"));
        assert_eq!(back.get("d"), v.get("d"));
    }
}
