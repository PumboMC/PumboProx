//! NBT as used on the wire and in structure files.
//!
//! - Network NBT (1.20.2+, the whole supported range): a type byte and the
//!   payload, no root name. Type 0 (`End`) means "no value".
//! - Named root (structure `.nbt` files after decompression): type, name, payload.
//!
//! Compounds keep their entry order, so decoding and encoding gives the same
//! bytes. Strings are Java's modified UTF-8. Reading enforces a depth limit
//! and a byte budget and never allocates more than the input can fill.

pub mod mutf8;

use thiserror::Error;

pub use mutf8::{decode as decode_mutf8, encode as encode_mutf8};

/// Tag type IDs.
pub mod id {
    pub const END: u8 = 0;
    pub const BYTE: u8 = 1;
    pub const SHORT: u8 = 2;
    pub const INT: u8 = 3;
    pub const LONG: u8 = 4;
    pub const FLOAT: u8 = 5;
    pub const DOUBLE: u8 = 6;
    pub const BYTE_ARRAY: u8 = 7;
    pub const STRING: u8 = 8;
    pub const LIST: u8 = 9;
    pub const COMPOUND: u8 = 10;
    pub const INT_ARRAY: u8 = 11;
    pub const LONG_ARRAY: u8 = 12;
}

/// An NBT value.
#[derive(Debug, Clone, PartialEq)]
pub enum Tag {
    Byte(i8),
    Short(i16),
    Int(i32),
    Long(i64),
    Float(f32),
    Double(f64),
    ByteArray(Vec<u8>),
    String(String),
    List(List),
    Compound(Compound),
    IntArray(Vec<i32>),
    LongArray(Vec<i64>),
}

/// A list: element type and elements (all of that type).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct List {
    /// Element type ID; `id::END` for an empty list without a type.
    pub element: u8,
    pub items: Vec<Tag>,
}

/// A compound in wire order.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Compound(pub Vec<(String, Tag)>);

impl Compound {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self, key: &str) -> Option<&Tag> {
        self.0.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    /// Sets a key, replacing an existing entry in place.
    pub fn insert(&mut self, key: impl Into<String>, value: Tag) {
        let key = key.into();
        if let Some(slot) = self.0.iter_mut().find(|(k, _)| *k == key) {
            slot.1 = value;
        } else {
            self.0.push((key, value));
        }
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &Tag)> {
        self.0.iter().map(|(k, v)| (k.as_str(), v))
    }
}

impl List {
    pub fn new(element: u8, items: Vec<Tag>) -> Self {
        Self { element, items }
    }

    /// A list of the given tags; their type becomes the element type.
    pub fn of(items: Vec<Tag>) -> Self {
        let element = items.first().map_or(id::END, Tag::id);
        Self { element, items }
    }
}

impl Tag {
    pub fn id(&self) -> u8 {
        match self {
            Tag::Byte(_) => id::BYTE,
            Tag::Short(_) => id::SHORT,
            Tag::Int(_) => id::INT,
            Tag::Long(_) => id::LONG,
            Tag::Float(_) => id::FLOAT,
            Tag::Double(_) => id::DOUBLE,
            Tag::ByteArray(_) => id::BYTE_ARRAY,
            Tag::String(_) => id::STRING,
            Tag::List(_) => id::LIST,
            Tag::Compound(_) => id::COMPOUND,
            Tag::IntArray(_) => id::INT_ARRAY,
            Tag::LongArray(_) => id::LONG_ARRAY,
        }
    }

    pub fn as_compound(&self) -> Option<&Compound> {
        match self {
            Tag::Compound(c) => Some(c),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Tag::String(s) => Some(s),
            _ => None,
        }
    }

    /// Equality that ignores the order of compound entries (vanilla writes
    /// compounds in hash-map order, so only this comparison is meaningful
    /// between independently built trees).
    pub fn equivalent(&self, other: &Tag) -> bool {
        match (self, other) {
            (Tag::Compound(a), Tag::Compound(b)) => {
                a.len() == b.len()
                    && a.iter()
                        .all(|(k, v)| b.get(k).is_some_and(|w| v.equivalent(w)))
            }
            (Tag::List(a), Tag::List(b)) => {
                (a.element == b.element || a.items.is_empty())
                    && a.items.len() == b.items.len()
                    && a.items.iter().zip(&b.items).all(|(x, y)| x.equivalent(y))
            }
            (Tag::Float(a), Tag::Float(b)) => a.to_bits() == b.to_bits(),
            (Tag::Double(a), Tag::Double(b)) => a.to_bits() == b.to_bits(),
            (a, b) => a == b,
        }
    }
}

/// Limits for reading untrusted NBT.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Maximum nesting of lists and compounds.
    pub max_depth: usize,
    /// Maximum bytes consumed by one value.
    pub max_bytes: usize,
}

impl Limits {
    /// From a backend (as strict as the vanilla client: depth 512, 2 MiB).
    pub const BACKEND: Limits = Limits {
        max_depth: 512,
        max_bytes: 2 * 1024 * 1024,
    };
    /// From a client (plan §2.7: depth 64, 32 KiB).
    pub const CLIENT: Limits = Limits {
        max_depth: 64,
        max_bytes: 32 * 1024,
    };
}

#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum NbtError {
    #[error("NBT ends early")]
    UnexpectedEof,
    #[error("unknown NBT tag type {0}")]
    UnknownTag(u8),
    #[error("NBT nested deeper than {0}")]
    TooDeep(usize),
    #[error("NBT larger than {0} bytes")]
    TooLarge(usize),
    #[error("negative NBT length")]
    NegativeLength,
    #[error("NBT list without a type has elements")]
    UntypedList,
    #[error("invalid modified UTF-8 in NBT string")]
    InvalidString,
    #[error("NBT string longer than 65535 bytes")]
    StringTooLong,
    #[error("NBT list element does not match the list type")]
    MixedList,
    #[error("NBT array longer than i32::MAX")]
    ArrayTooLong,
}

struct Reader<'a, 'b> {
    input: &'b mut &'a [u8],
    limits: Limits,
    consumed: usize,
}

impl<'a> Reader<'a, '_> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], NbtError> {
        if n > self.input.len() {
            return Err(NbtError::UnexpectedEof);
        }
        self.consumed = self.consumed.saturating_add(n);
        if self.consumed > self.limits.max_bytes {
            return Err(NbtError::TooLarge(self.limits.max_bytes));
        }
        let (head, rest) = self.input.split_at(n);
        *self.input = rest;
        Ok(head)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], NbtError> {
        let b = self.take(N)?;
        let mut out = [0u8; N];
        out.copy_from_slice(b);
        Ok(out)
    }

    fn u8(&mut self) -> Result<u8, NbtError> {
        Ok(u8::from_be_bytes(self.array()?))
    }

    fn i32(&mut self) -> Result<i32, NbtError> {
        Ok(i32::from_be_bytes(self.array()?))
    }

    /// A length prefix for `elem_size`-byte elements, checked against the input
    /// before anything is allocated.
    fn len(&mut self, elem_size: usize) -> Result<usize, NbtError> {
        let n = usize::try_from(self.i32()?).map_err(|_| NbtError::NegativeLength)?;
        if n.saturating_mul(elem_size.max(1)) > self.input.len() {
            return Err(NbtError::UnexpectedEof);
        }
        Ok(n)
    }

    fn string(&mut self) -> Result<String, NbtError> {
        let n = usize::from(u16::from_be_bytes(self.array()?));
        mutf8::decode(self.take(n)?).ok_or(NbtError::InvalidString)
    }

    fn payload(&mut self, tag: u8, depth: usize) -> Result<Tag, NbtError> {
        Ok(match tag {
            id::BYTE => Tag::Byte(i8::from_be_bytes(self.array()?)),
            id::SHORT => Tag::Short(i16::from_be_bytes(self.array()?)),
            id::INT => Tag::Int(self.i32()?),
            id::LONG => Tag::Long(i64::from_be_bytes(self.array()?)),
            id::FLOAT => Tag::Float(f32::from_be_bytes(self.array()?)),
            id::DOUBLE => Tag::Double(f64::from_be_bytes(self.array()?)),
            id::BYTE_ARRAY => {
                let n = self.len(1)?;
                Tag::ByteArray(self.take(n)?.to_vec())
            }
            id::STRING => Tag::String(self.string()?),
            id::LIST => {
                if depth >= self.limits.max_depth {
                    return Err(NbtError::TooDeep(self.limits.max_depth));
                }
                let element = self.u8()?;
                let size = min_size(element)?;
                let n = usize::try_from(self.i32()?).map_err(|_| NbtError::NegativeLength)?;
                if element == id::END && n > 0 {
                    return Err(NbtError::UntypedList);
                }
                if n.saturating_mul(size) > self.input.len() {
                    return Err(NbtError::UnexpectedEof);
                }
                let mut items = Vec::with_capacity(n);
                for _ in 0..n {
                    items.push(self.payload(element, depth + 1)?);
                }
                Tag::List(List { element, items })
            }
            id::COMPOUND => {
                if depth >= self.limits.max_depth {
                    return Err(NbtError::TooDeep(self.limits.max_depth));
                }
                let mut entries = Vec::new();
                loop {
                    let t = self.u8()?;
                    if t == id::END {
                        break;
                    }
                    let key = self.string()?;
                    entries.push((key, self.payload(t, depth + 1)?));
                }
                Tag::Compound(Compound(entries))
            }
            id::INT_ARRAY => {
                let n = self.len(4)?;
                let mut v = Vec::with_capacity(n);
                for _ in 0..n {
                    v.push(self.i32()?);
                }
                Tag::IntArray(v)
            }
            id::LONG_ARRAY => {
                let n = self.len(8)?;
                let mut v = Vec::with_capacity(n);
                for _ in 0..n {
                    v.push(i64::from_be_bytes(self.array()?));
                }
                Tag::LongArray(v)
            }
            other => return Err(NbtError::UnknownTag(other)),
        })
    }
}

/// Smallest encoded size of one payload of the type (for allocation checks).
fn min_size(tag: u8) -> Result<usize, NbtError> {
    Ok(match tag {
        id::END => 0,
        id::BYTE => 1,
        id::SHORT | id::STRING => 2,
        id::INT | id::FLOAT | id::BYTE_ARRAY | id::INT_ARRAY | id::LONG_ARRAY | id::LIST => 4,
        id::LONG | id::DOUBLE => 8,
        id::COMPOUND => 1,
        other => return Err(NbtError::UnknownTag(other)),
    })
}

/// Reads network NBT and advances `input`. `Ok(None)` for the `End` type.
pub fn read_network(input: &mut &[u8], limits: Limits) -> Result<Option<Tag>, NbtError> {
    let mut r = Reader {
        input,
        limits,
        consumed: 0,
    };
    let tag = r.u8()?;
    if tag == id::END {
        return Ok(None);
    }
    r.payload(tag, 0).map(Some)
}

/// Reads a named root (file form) and advances `input`.
pub fn read_named(input: &mut &[u8], limits: Limits) -> Result<(String, Tag), NbtError> {
    let mut r = Reader {
        input,
        limits,
        consumed: 0,
    };
    let tag = r.u8()?;
    if tag == id::END {
        return Err(NbtError::UnknownTag(tag));
    }
    let name = r.string()?;
    Ok((name, r.payload(tag, 0)?))
}

fn write_len(out: &mut Vec<u8>, n: usize) -> Result<(), NbtError> {
    let n = i32::try_from(n).map_err(|_| NbtError::ArrayTooLong)?;
    out.extend_from_slice(&n.to_be_bytes());
    Ok(())
}

fn write_string(out: &mut Vec<u8>, s: &str) -> Result<(), NbtError> {
    let bytes = mutf8::encode(s);
    let n = u16::try_from(bytes.len()).map_err(|_| NbtError::StringTooLong)?;
    out.extend_from_slice(&n.to_be_bytes());
    out.extend_from_slice(&bytes);
    Ok(())
}

fn write_payload(out: &mut Vec<u8>, tag: &Tag) -> Result<(), NbtError> {
    match tag {
        Tag::Byte(v) => out.extend_from_slice(&v.to_be_bytes()),
        Tag::Short(v) => out.extend_from_slice(&v.to_be_bytes()),
        Tag::Int(v) => out.extend_from_slice(&v.to_be_bytes()),
        Tag::Long(v) => out.extend_from_slice(&v.to_be_bytes()),
        Tag::Float(v) => out.extend_from_slice(&v.to_be_bytes()),
        Tag::Double(v) => out.extend_from_slice(&v.to_be_bytes()),
        Tag::ByteArray(v) => {
            write_len(out, v.len())?;
            out.extend_from_slice(v);
        }
        Tag::String(s) => write_string(out, s)?,
        Tag::List(list) => {
            if list.element == id::END && !list.items.is_empty() {
                return Err(NbtError::UntypedList);
            }
            out.push(list.element);
            write_len(out, list.items.len())?;
            for item in &list.items {
                if item.id() != list.element {
                    return Err(NbtError::MixedList);
                }
                write_payload(out, item)?;
            }
        }
        Tag::Compound(c) => {
            for (k, v) in &c.0 {
                out.push(v.id());
                write_string(out, k)?;
                write_payload(out, v)?;
            }
            out.push(id::END);
        }
        Tag::IntArray(v) => {
            write_len(out, v.len())?;
            for x in v {
                out.extend_from_slice(&x.to_be_bytes());
            }
        }
        Tag::LongArray(v) => {
            write_len(out, v.len())?;
            for x in v {
                out.extend_from_slice(&x.to_be_bytes());
            }
        }
    }
    Ok(())
}

/// Writes network NBT; `None` writes the `End` type.
pub fn write_network(out: &mut Vec<u8>, tag: Option<&Tag>) -> Result<(), NbtError> {
    match tag {
        None => {
            out.push(id::END);
            Ok(())
        }
        Some(t) => {
            out.push(t.id());
            write_payload(out, t)
        }
    }
}

/// Writes a named root.
pub fn write_named(out: &mut Vec<u8>, name: &str, tag: &Tag) -> Result<(), NbtError> {
    out.push(tag.id());
    write_string(out, name)?;
    write_payload(out, tag)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Tag {
        Tag::Compound(Compound(vec![
            ("text".into(), Tag::String("héllo \u{0}\u{1F600}".into())),
            ("b".into(), Tag::Byte(-1)),
            ("s".into(), Tag::Short(300)),
            ("f".into(), Tag::Float(1.5)),
            ("d".into(), Tag::Double(-2.25)),
            ("l".into(), Tag::Long(i64::MIN)),
            ("ba".into(), Tag::ByteArray(vec![1, 2, 255])),
            ("ia".into(), Tag::IntArray(vec![1, -1])),
            ("la".into(), Tag::LongArray(vec![7])),
            ("empty".into(), Tag::List(List::default())),
            (
                "list".into(),
                Tag::List(List::of(vec![
                    Tag::Compound(Compound::new()),
                    Tag::Compound(Compound(vec![("x".into(), Tag::Int(5))])),
                ])),
            ),
        ]))
    }

    #[test]
    fn network_round_trip() {
        let t = sample();
        let mut out = Vec::new();
        write_network(&mut out, Some(&t)).unwrap();
        let mut input = out.as_slice();
        let back = read_network(&mut input, Limits::BACKEND).unwrap().unwrap();
        assert!(input.is_empty());
        assert_eq!(back, t);
        let mut again = Vec::new();
        write_network(&mut again, Some(&back)).unwrap();
        assert_eq!(again, out);
    }

    #[test]
    fn end_means_none_and_named_root() {
        let mut out = Vec::new();
        write_network(&mut out, None).unwrap();
        assert_eq!(out, [0]);
        assert_eq!(read_network(&mut out.as_slice(), Limits::CLIENT), Ok(None));

        let mut named = Vec::new();
        write_named(&mut named, "root", &sample()).unwrap();
        let (name, tag) = read_named(&mut named.as_slice(), Limits::BACKEND).unwrap();
        assert_eq!(name, "root");
        assert_eq!(tag, sample());
    }

    #[test]
    fn modified_utf8_matches_java() {
        // U+0000 is C0 80, U+1F600 is a surrogate pair of 3-byte sequences.
        assert_eq!(encode_mutf8("\u{0}"), [0xC0, 0x80]);
        assert_eq!(
            encode_mutf8("\u{1F600}"),
            [0xED, 0xA0, 0xBD, 0xED, 0xB8, 0x80]
        );
        assert_eq!(decode_mutf8(&[0xED, 0xA0, 0xBD]), None, "lone surrogate");
        assert_eq!(decode_mutf8(&[0xC3]), None);
    }

    #[test]
    fn limits_hold() {
        // A list nested 100 deep passes the backend limit and fails the client one.
        let mut t = Tag::Int(1);
        for _ in 0..100 {
            t = Tag::List(List::of(vec![t]));
        }
        let mut out = Vec::new();
        write_network(&mut out, Some(&t)).unwrap();
        assert!(read_network(&mut out.as_slice(), Limits::BACKEND).is_ok());
        assert_eq!(
            read_network(&mut out.as_slice(), Limits::CLIENT),
            Err(NbtError::TooDeep(64))
        );
        // Declared array of 2^31-1 longs with no data: no allocation, EOF.
        let bomb = [id::LONG_ARRAY, 0x7F, 0xFF, 0xFF, 0xFF];
        assert_eq!(
            read_network(&mut bomb.as_slice(), Limits::BACKEND),
            Err(NbtError::UnexpectedEof)
        );
        let negative = [id::INT_ARRAY, 0xFF, 0xFF, 0xFF, 0xFF];
        assert_eq!(
            read_network(&mut negative.as_slice(), Limits::BACKEND),
            Err(NbtError::NegativeLength)
        );
        // Byte budget.
        let big = Tag::ByteArray(vec![0; 40_000]);
        let mut out = Vec::new();
        write_network(&mut out, Some(&big)).unwrap();
        assert_eq!(
            read_network(&mut out.as_slice(), Limits::CLIENT),
            Err(NbtError::TooLarge(32 * 1024))
        );
        let untyped = [id::LIST, id::END, 0, 0, 0, 1];
        assert_eq!(
            read_network(&mut untyped.as_slice(), Limits::BACKEND),
            Err(NbtError::UntypedList)
        );
    }

    #[test]
    fn equivalent_ignores_compound_order() {
        let a = Tag::Compound(Compound(vec![
            ("a".into(), Tag::Int(1)),
            ("b".into(), Tag::Int(2)),
        ]));
        let b = Tag::Compound(Compound(vec![
            ("b".into(), Tag::Int(2)),
            ("a".into(), Tag::Int(1)),
        ]));
        assert_ne!(a, b);
        assert!(a.equivalent(&b));
    }

    mod props {
        use super::super::*;
        use proptest::prelude::*;

        fn leaf() -> impl Strategy<Value = Tag> {
            prop_oneof![
                any::<i8>().prop_map(Tag::Byte),
                any::<i16>().prop_map(Tag::Short),
                any::<i32>().prop_map(Tag::Int),
                any::<i64>().prop_map(Tag::Long),
                any::<f32>().prop_map(Tag::Float),
                any::<f64>().prop_map(Tag::Double),
                proptest::collection::vec(any::<u8>(), 0..8).prop_map(Tag::ByteArray),
                ".{0,8}".prop_map(Tag::String),
                proptest::collection::vec(any::<i32>(), 0..4).prop_map(Tag::IntArray),
                proptest::collection::vec(any::<i64>(), 0..4).prop_map(Tag::LongArray),
            ]
        }

        fn tag() -> impl Strategy<Value = Tag> {
            leaf().prop_recursive(4, 32, 4, |inner| {
                prop_oneof![
                    proptest::collection::btree_map("[a-z]{0,4}", inner.clone(), 0..4)
                        .prop_map(|e| Tag::Compound(Compound(e.into_iter().collect()))),
                    proptest::collection::vec(any::<i32>(), 0..4)
                        .prop_map(|v| Tag::List(List::of(v.into_iter().map(Tag::Int).collect()))),
                    inner.prop_map(|t| Tag::List(List::of(vec![t]))),
                ]
            })
        }

        proptest! {
            #[test]
            fn round_trip_bytes(t in tag()) {
                let mut out = Vec::new();
                write_network(&mut out, Some(&t)).unwrap();
                let back = read_network(&mut out.as_slice(), Limits::BACKEND).unwrap().unwrap();
                let mut again = Vec::new();
                write_network(&mut again, Some(&back)).unwrap();
                prop_assert_eq!(again, out);
                prop_assert!(back.equivalent(&t));
            }

            #[test]
            fn arbitrary_input_never_panics(data in proptest::collection::vec(any::<u8>(), 0..256)) {
                let _ = read_network(&mut data.as_slice(), Limits::CLIENT);
            }

            #[test]
            fn mutf8_round_trip(s in any::<String>()) {
                prop_assert_eq!(decode_mutf8(&encode_mutf8(&s)), Some(s));
            }
        }
    }
}
