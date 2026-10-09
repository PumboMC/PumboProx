//! Network NBT (nameless root): found by length and copied as bytes, and text
//! components in the client's layout.

use pumbo_nbt::Limits;
use pumbo_text::{Component, TextFormat};

use crate::version::V1_21_5;
use crate::wire::Get;

/// Deeper nesting than this is treated as malformed.
const MAX_DEPTH: usize = 512;

fn be_len(r: &mut &[u8], width: usize) -> Option<usize> {
    match width {
        2 => Some(usize::from(r.get_i16()? as u16)),
        _ => usize::try_from(r.get_i32()?).ok(),
    }
}

/// Skips one payload of `tag` type.
fn skip_payload(r: &mut &[u8], tag: u8, depth: usize) -> Option<()> {
    if depth > MAX_DEPTH {
        return None;
    }
    match tag {
        0 => {}
        1 => {
            r.bytes(1)?;
        }
        2 => {
            r.bytes(2)?;
        }
        3 | 5 => {
            r.bytes(4)?;
        }
        4 | 6 => {
            r.bytes(8)?;
        }
        7 => {
            let n = be_len(r, 4)?;
            r.bytes(n)?;
        }
        8 => {
            let n = be_len(r, 2)?;
            r.bytes(n)?;
        }
        9 => {
            let element = r.get_u8()?;
            let count = be_len(r, 4)?;
            for _ in 0..count {
                skip_payload(r, element, depth + 1)?;
            }
        }
        10 => loop {
            let child = r.get_u8()?;
            if child == 0 {
                break;
            }
            let n = be_len(r, 2)?;
            r.bytes(n)?;
            skip_payload(r, child, depth + 1)?;
        },
        11 => {
            let n = be_len(r, 4)?;
            r.bytes(n.checked_mul(4)?)?;
        }
        12 => {
            let n = be_len(r, 4)?;
            r.bytes(n.checked_mul(8)?)?;
        }
        _ => return None,
    }
    Some(())
}

/// Splits one network NBT (type byte and nameless root) off `r`.
pub(crate) fn split<'a>(r: &mut &'a [u8]) -> Option<&'a [u8]> {
    let start = *r;
    let tag = r.get_u8()?;
    skip_payload(r, tag, 0)?;
    let len = start.len() - r.len();
    start.get(..len)
}

/// Copies one network NBT from `r` to `out`.
pub(crate) fn copy(r: &mut &[u8], out: &mut Vec<u8>) -> Option<()> {
    out.extend_from_slice(split(r)?);
    Some(())
}

/// Copies one text component (network NBT) in the client's layout. Before
/// 1.21.5 click and hover events had other names and fields
/// (`clickEvent {action, value}`; ViaBackwards `ComponentRewriter1_21_5`):
/// text with events is rewritten by `pumbo-text`, the rest is copied.
pub(crate) fn text(client: i32, r: &mut &[u8], out: &mut Vec<u8>) -> Option<()> {
    let raw = split(r)?;
    let events = raw.windows(6).any(|w| w == b"_event");
    let converted = (client < V1_21_5 && events)
        .then(|| {
            pumbo_nbt::read_network(&mut &raw[..], Limits::BACKEND)
                .ok()
                .flatten()
        })
        .flatten()
        .and_then(|tag| Component::from_nbt(&tag).ok())
        .and_then(|c| {
            let mut bytes = Vec::with_capacity(raw.len());
            let tag = c.to_nbt(TextFormat::for_protocol(client));
            pumbo_nbt::write_network(&mut bytes, Some(&tag)).ok()?;
            Some(bytes)
        });
    out.extend_from_slice(converted.as_deref().unwrap_or(raw));
    Some(())
}

/// An optional text component (present flag, then text).
pub(crate) fn opt_text(client: i32, r: &mut &[u8], out: &mut Vec<u8>) -> Option<()> {
    let present = r.get_bool()?;
    out.push(u8::from(present));
    if present {
        text(client, r, out)?;
    }
    Some(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_nested_compounds() {
        // {a: [I; 1, 2], b: {c: "x"}} then a trailing byte
        let mut data = vec![10];
        data.extend_from_slice(&[11, 0, 1, b'a', 0, 0, 0, 2, 0, 0, 0, 1, 0, 0, 0, 2]);
        data.extend_from_slice(&[10, 0, 1, b'b', 8, 0, 1, b'c', 0, 1, b'x', 0]);
        data.push(0);
        let nbt_len = data.len();
        data.push(0xAB);
        let mut r = data.as_slice();
        assert_eq!(split(&mut r).map(<[u8]>::len), Some(nbt_len));
        assert_eq!(r, &[0xAB]);
        // End tag alone (empty optional NBT)
        let mut r = &[0u8, 7][..];
        assert_eq!(split(&mut r), Some(&[0u8][..]));
        // Too deep
        let mut deep = vec![9u8];
        for _ in 0..600 {
            deep.extend_from_slice(&[9, 0, 0, 0, 1]);
        }
        assert!(split(&mut deep.as_slice()).is_none());
    }

    #[test]
    fn click_events_get_the_old_layout() {
        use pumbo_nbt::{Compound, Tag};
        let mut click = Compound::new();
        click.insert("action", Tag::String("open_url".into()));
        click.insert("url", Tag::String("https://example.org".into()));
        let mut c = Compound::new();
        c.insert("text", Tag::String("site".into()));
        c.insert("click_event", Tag::Compound(click));
        let mut nbt = Vec::new();
        pumbo_nbt::write_network(&mut nbt, Some(&Tag::Compound(c))).unwrap();
        let mut out = Vec::new();
        text(769, &mut nbt.as_slice(), &mut out).unwrap();
        let tag = pumbo_nbt::read_network(&mut out.as_slice(), Limits::BACKEND)
            .unwrap()
            .unwrap();
        let old = tag.as_compound().unwrap().get("clickEvent").unwrap();
        let old = old.as_compound().unwrap();
        assert_eq!(old.get("action").and_then(Tag::as_str), Some("open_url"));
        assert_eq!(
            old.get("value").and_then(Tag::as_str),
            Some("https://example.org")
        );
        let mut same = Vec::new();
        text(770, &mut nbt.as_slice(), &mut same).unwrap();
        assert_eq!(same, nbt);
    }
}
