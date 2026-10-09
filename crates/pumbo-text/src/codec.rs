//! One codec for both JSON and NBT: the layout is the same since 1.20.3,
//! only the value types differ (NBT booleans are bytes, mixed lists are
//! wrapped, UUIDs are int arrays).

use std::borrow::Cow;

use pumbo_nbt::{Compound, Tag};
use serde_json::Value;

use crate::raw::{Raw, nbt_list, unwrap_heterogeneous};
use crate::{
    Argument, ClickEvent, Color, Component, Content, HoverEvent, NbtContent, Style, TextError,
    TextFormat,
};

/// Deepest component nesting accepted when reading (children, arguments,
/// separators and hover text all count). Real components stay well below
/// it; the bound keeps the recursive decoder within a small stack.
pub const MAX_DEPTH: usize = 32;

/// Keys that belong to the style or structure rather than the content.
const NON_CONTENT_KEYS: &[&str] = &[
    "type",
    "extra",
    "color",
    "shadow_color",
    "bold",
    "italic",
    "underlined",
    "strikethrough",
    "obfuscated",
    "click_event",
    "clickEvent",
    "hover_event",
    "hoverEvent",
    "insertion",
    "font",
];

/// A JSON or NBT value seen by the codec.
pub(crate) trait Data: Sized + Clone {
    fn string(s: &str) -> Self;
    fn int(v: i32) -> Self;
    fn boolean(v: bool) -> Self;
    fn list(items: Vec<Self>) -> Self;
    fn map(entries: Vec<(String, Self)>) -> Self;
    fn uuid(v: [i32; 4]) -> Self;
    fn from_raw(raw: &Raw) -> Self;

    fn to_raw(&self) -> Raw;
    fn as_str(&self) -> Option<&str>;
    fn as_f64(&self) -> Option<f64>;
    fn as_bool(&self) -> Option<bool>;
    /// List elements (NBT wrappers removed, numeric arrays as numbers).
    fn items(&self) -> Option<Vec<Cow<'_, Self>>>;
    fn get(&self, key: &str) -> Option<&Self>;
    fn entries(&self) -> Option<Vec<(&str, &Self)>>;
}

impl Data for Value {
    fn string(s: &str) -> Self {
        Value::String(s.to_string())
    }
    fn int(v: i32) -> Self {
        Value::from(v)
    }
    fn boolean(v: bool) -> Self {
        Value::Bool(v)
    }
    fn list(items: Vec<Self>) -> Self {
        Value::Array(items)
    }
    fn map(entries: Vec<(String, Self)>) -> Self {
        Value::Object(entries.into_iter().collect())
    }
    fn uuid(v: [i32; 4]) -> Self {
        Value::Array(v.iter().map(|x| Value::from(*x)).collect())
    }
    fn from_raw(raw: &Raw) -> Self {
        raw.to_json()
    }
    fn to_raw(&self) -> Raw {
        Raw::Json(self.clone())
    }
    fn as_str(&self) -> Option<&str> {
        self.as_str()
    }
    fn as_f64(&self) -> Option<f64> {
        self.as_f64()
    }
    fn as_bool(&self) -> Option<bool> {
        self.as_bool()
    }
    fn items(&self) -> Option<Vec<Cow<'_, Self>>> {
        self.as_array()
            .map(|a| a.iter().map(Cow::Borrowed).collect())
    }
    fn get(&self, key: &str) -> Option<&Self> {
        self.as_object().and_then(|m| m.get(key))
    }
    fn entries(&self) -> Option<Vec<(&str, &Self)>> {
        self.as_object()
            .map(|m| m.iter().map(|(k, v)| (k.as_str(), v)).collect())
    }
}

impl Data for Tag {
    fn string(s: &str) -> Self {
        Tag::String(s.to_string())
    }
    fn int(v: i32) -> Self {
        Tag::Int(v)
    }
    fn boolean(v: bool) -> Self {
        Tag::Byte(i8::from(v))
    }
    fn list(items: Vec<Self>) -> Self {
        Tag::List(nbt_list(items))
    }
    fn map(entries: Vec<(String, Self)>) -> Self {
        Tag::Compound(Compound(entries))
    }
    fn uuid(v: [i32; 4]) -> Self {
        Tag::IntArray(v.to_vec())
    }
    fn from_raw(raw: &Raw) -> Self {
        raw.to_nbt()
    }
    fn to_raw(&self) -> Raw {
        Raw::Nbt(self.clone())
    }
    fn as_str(&self) -> Option<&str> {
        Tag::as_str(self)
    }
    fn as_f64(&self) -> Option<f64> {
        match self {
            Tag::Byte(v) => Some(f64::from(*v)),
            Tag::Short(v) => Some(f64::from(*v)),
            Tag::Int(v) => Some(f64::from(*v)),
            Tag::Long(v) => Some(*v as f64),
            Tag::Float(v) => Some(f64::from(*v)),
            Tag::Double(v) => Some(*v),
            _ => None,
        }
    }
    fn as_bool(&self) -> Option<bool> {
        match self {
            Tag::Byte(v) => Some(*v != 0),
            _ => self.as_f64().map(|v| v != 0.0),
        }
    }
    fn items(&self) -> Option<Vec<Cow<'_, Self>>> {
        let owned = |t: Tag| Cow::Owned(t);
        match self {
            Tag::List(l) => Some(
                l.items
                    .iter()
                    .map(|t| Cow::Borrowed(unwrap_heterogeneous(t)))
                    .collect(),
            ),
            Tag::ByteArray(v) => Some(v.iter().map(|b| owned(Tag::Byte(*b as i8))).collect()),
            Tag::IntArray(v) => Some(v.iter().map(|x| owned(Tag::Int(*x))).collect()),
            Tag::LongArray(v) => Some(v.iter().map(|x| owned(Tag::Long(*x))).collect()),
            _ => None,
        }
    }
    fn get(&self, key: &str) -> Option<&Self> {
        self.as_compound().and_then(|c| c.get(key))
    }
    fn entries(&self) -> Option<Vec<(&str, &Self)>> {
        self.as_compound().map(|c| c.iter().collect())
    }
}

// ---------------------------------------------------------------- writing

pub(crate) fn encode<D: Data>(c: &Component, f: TextFormat) -> D {
    if let Some(s) = c.as_plain_str() {
        return D::string(s);
    }
    let mut m: Vec<(String, D)> = Vec::new();
    let mut put = |k: &str, v: D| m.push((k.to_string(), v));
    match &c.content {
        Content::Text(t) => put("text", D::string(t)),
        Content::Translatable {
            key,
            fallback,
            with,
        } => {
            put("translate", D::string(key));
            if let Some(fb) = fallback {
                put("fallback", D::string(fb));
            }
            if !with.is_empty() {
                put(
                    "with",
                    D::list(
                        with.iter()
                            .map(|a| match a {
                                Argument::Component(c) => encode(c, f),
                                Argument::Value(v) => D::from_raw(v),
                            })
                            .collect(),
                    ),
                );
            }
        }
        Content::Keybind(k) => put("keybind", D::string(k)),
        Content::Score { name, objective } => put(
            "score",
            D::map(vec![
                ("name".into(), D::string(name)),
                ("objective".into(), D::string(objective)),
            ]),
        ),
        Content::Selector { pattern, separator } => {
            put("selector", D::string(pattern));
            if let Some(sep) = separator {
                put("separator", encode(sep, f));
            }
        }
        Content::Nbt(n) => {
            put("nbt", D::string(&n.path));
            if let Some(v) = n.interpret {
                put("interpret", D::boolean(v));
            }
            if let Some(v) = n.plain {
                put("plain", D::boolean(v));
            }
            if let Some(sep) = &n.separator {
                put("separator", encode(sep, f));
            }
            for (k, v) in [
                ("source", &n.source),
                ("block", &n.block),
                ("entity", &n.entity),
                ("storage", &n.storage),
            ] {
                if let Some(v) = v {
                    put(k, D::string(v));
                }
            }
        }
        Content::Object(fields) => {
            for (k, v) in fields {
                put(k, D::from_raw(v));
            }
        }
    }
    if !c.extra.is_empty() {
        put(
            "extra",
            D::list(c.extra.iter().map(|e| encode(e, f)).collect()),
        );
    }
    encode_style(&c.style, f, &mut m);
    D::map(m)
}

fn encode_style<D: Data>(s: &Style, f: TextFormat, m: &mut Vec<(String, D)>) {
    let mut put = |k: &str, v: D| m.push((k.to_string(), v));
    if let Some(c) = s.color {
        put("color", D::string(&c.serialize()));
    }
    if let (Some(v), true) = (s.shadow_color, f.shadow_color) {
        put("shadow_color", D::int(v));
    }
    for (k, v) in [
        ("bold", s.bold),
        ("italic", s.italic),
        ("underlined", s.underlined),
        ("strikethrough", s.strikethrough),
        ("obfuscated", s.obfuscated),
    ] {
        if let Some(v) = v {
            put(k, D::boolean(v));
        }
    }
    if let Some(click) = &s.click_event {
        let key = if f.snake_case_events {
            "click_event"
        } else {
            "clickEvent"
        };
        put(key, encode_click(click, f));
    }
    if let Some(hover) = &s.hover_event {
        let key = if f.snake_case_events {
            "hover_event"
        } else {
            "hoverEvent"
        };
        put(key, encode_hover(hover, f));
    }
    if let Some(i) = &s.insertion {
        put("insertion", D::string(i));
    }
    if let Some(font) = &s.font {
        put("font", D::string(font));
    }
}

fn encode_click<D: Data>(c: &ClickEvent, f: TextFormat) -> D {
    let action = |a: &str| ("action".to_string(), D::string(a));
    let field = |k: &str, v: D| (k.to_string(), v);
    let modern = f.snake_case_events;
    // Before 1.21.5 every action had a single string `value`.
    let simple = |a: &str, modern_key: &str, v: &str| {
        let key = if modern { modern_key } else { "value" };
        D::map(vec![action(a), field(key, D::string(v))])
    };
    match c {
        ClickEvent::OpenUrl(u) => simple("open_url", "url", u),
        ClickEvent::OpenFile(p) => simple("open_file", "path", p),
        ClickEvent::RunCommand(cmd) => simple("run_command", "command", cmd),
        ClickEvent::SuggestCommand(cmd) => simple("suggest_command", "command", cmd),
        ClickEvent::CopyToClipboard(v) => simple("copy_to_clipboard", "value", v),
        ClickEvent::ChangePage(p) => {
            if modern {
                D::map(vec![action("change_page"), field("page", D::int(*p))])
            } else {
                D::map(vec![
                    action("change_page"),
                    field("value", D::string(&p.to_string())),
                ])
            }
        }
        ClickEvent::ShowDialog(d) => {
            D::map(vec![action("show_dialog"), field("dialog", D::from_raw(d))])
        }
        ClickEvent::Custom { id, payload } => {
            let mut m = vec![action("custom"), field("id", D::string(id))];
            if let Some(p) = payload {
                m.push(field("payload", D::from_raw(p)));
            }
            D::map(m)
        }
        ClickEvent::Other { action: a, fields } => {
            let mut m = vec![action(a)];
            m.extend(fields.iter().map(|(k, v)| field(k, D::from_raw(v))));
            D::map(m)
        }
    }
}

fn encode_hover<D: Data>(h: &HoverEvent, f: TextFormat) -> D {
    let action = |a: &str| ("action".to_string(), D::string(a));
    let field = |k: &str, v: D| (k.to_string(), v);
    let modern = f.snake_case_events;
    match h {
        HoverEvent::ShowText(text) => {
            let key = if modern { "value" } else { "contents" };
            D::map(vec![action("show_text"), field(key, encode(text, f))])
        }
        HoverEvent::ShowItem {
            id,
            count,
            components,
        } => {
            let mut item = vec![field("id", D::string(id))];
            if let Some(c) = count {
                item.push(field("count", D::int(*c)));
            }
            if let Some(c) = components {
                item.push(field("components", D::from_raw(c)));
            }
            if modern {
                let mut m = vec![action("show_item")];
                m.extend(item);
                D::map(m)
            } else {
                D::map(vec![action("show_item"), field("contents", D::map(item))])
            }
        }
        HoverEvent::ShowEntity {
            entity_type,
            uuid,
            name,
        } => {
            // 1.21.5 renamed `type` to `id` and `id` to `uuid`.
            let (type_key, uuid_key) = if modern {
                ("id", "uuid")
            } else {
                ("type", "id")
            };
            let mut entity = Vec::new();
            if let Some(n) = name {
                entity.push(field("name", encode(n, f)));
            }
            entity.push(field(type_key, D::string(entity_type)));
            entity.push(field(uuid_key, D::uuid(*uuid)));
            if modern {
                let mut m = vec![action("show_entity")];
                m.extend(entity);
                D::map(m)
            } else {
                D::map(vec![
                    action("show_entity"),
                    field("contents", D::map(entity)),
                ])
            }
        }
        HoverEvent::Other { action: a, fields } => {
            let mut m = vec![action(a)];
            m.extend(fields.iter().map(|(k, v)| field(k, D::from_raw(v))));
            D::map(m)
        }
    }
}

// ---------------------------------------------------------------- reading

fn invalid(msg: impl Into<String>) -> TextError {
    TextError::Invalid(msg.into())
}

pub(crate) fn decode<D: Data>(d: &D) -> Result<Component, TextError> {
    decode_at(d, 0)
}

fn decode_at<D: Data>(d: &D, depth: usize) -> Result<Component, TextError> {
    if depth > MAX_DEPTH {
        return Err(TextError::TooDeep(MAX_DEPTH));
    }
    if let Some(s) = d.as_str() {
        return Ok(Component::text(s));
    }
    if let Some(items) = d.items() {
        // `[a, b, c]` is `a` with `b` and `c` appended to its children.
        let mut it = items.iter();
        let first = it.next().ok_or_else(|| invalid("empty component list"))?;
        let mut root = decode_at(first.as_ref(), depth + 1)?;
        for item in it {
            root.extra.push(decode_at(item.as_ref(), depth + 1)?);
        }
        return Ok(root);
    }
    if d.entries().is_none() {
        return Err(invalid("a component must be a string, a list or an object"));
    }
    let content = decode_content(d, depth)?;
    let mut extra = Vec::new();
    if let Some(list) = d.get("extra") {
        let items = list
            .items()
            .ok_or_else(|| invalid("`extra` must be a list"))?;
        if items.is_empty() {
            return Err(invalid("`extra` must not be empty"));
        }
        for item in &items {
            extra.push(decode_at(item.as_ref(), depth + 1)?);
        }
    }
    Ok(Component {
        content,
        style: decode_style(d, depth)?,
        extra,
    })
}

/// The content part of an object component.
#[inline(never)]
fn decode_content<D: Data>(d: &D, depth: usize) -> Result<Content, TextError> {
    let str_field = |k: &str| d.get(k).and_then(D::as_str).map(str::to_string);
    let child = |k: &str| -> Result<Option<Box<Component>>, TextError> {
        d.get(k)
            .map(|v| decode_at(v, depth + 1).map(Box::new))
            .transpose()
    };

    let kind = d.get("type").and_then(D::as_str);
    let has = |k: &str| d.get(k).is_some();
    // Explicit type first (if its field is there), then the vanilla
    // detection order: text, translate, score, selector, keybind, nbt; the
    // 1.21.9 `object` content last.
    let typed = kind.filter(|k| match *k {
        "translatable" => has("translate"),
        "text" | "score" | "selector" | "keybind" | "nbt" => has(k),
        "object" => true,
        _ => false,
    });
    let detected = typed
        .or_else(|| {
            ["text", "translate", "score", "selector", "keybind", "nbt"]
                .into_iter()
                .find(|k| has(k))
                .map(|k| if k == "translate" { "translatable" } else { k })
        })
        .or_else(|| {
            ["object", "sprite", "player", "atlas"]
                .into_iter()
                .any(has)
                .then_some("object")
        });

    let content = match detected {
        Some("text") => {
            Content::Text(str_field("text").ok_or_else(|| invalid("`text` must be a string"))?)
        }
        Some("translatable") => {
            let key =
                str_field("translate").ok_or_else(|| invalid("`translate` must be a string"))?;
            let mut with = Vec::new();
            if let Some(args) = d.get("with") {
                let args = args
                    .items()
                    .ok_or_else(|| invalid("`with` must be a list"))?;
                for a in &args {
                    with.push(decode_argument(a.as_ref(), depth + 1)?);
                }
            }
            Content::Translatable {
                key,
                fallback: str_field("fallback"),
                with,
            }
        }
        Some("score") => {
            let score = d.get("score").ok_or_else(|| invalid("missing `score`"))?;
            let field = |k: &str| {
                score
                    .get(k)
                    .and_then(D::as_str)
                    .map(str::to_string)
                    .ok_or_else(|| invalid(format!("score without `{k}`")))
            };
            Content::Score {
                name: field("name")?,
                objective: field("objective")?,
            }
        }
        Some("selector") => Content::Selector {
            pattern: str_field("selector").ok_or_else(|| invalid("`selector` must be a string"))?,
            separator: child("separator")?,
        },
        Some("keybind") => Content::Keybind(
            str_field("keybind").ok_or_else(|| invalid("`keybind` must be a string"))?,
        ),
        Some("nbt") => Content::Nbt(NbtContent {
            path: str_field("nbt").ok_or_else(|| invalid("`nbt` must be a string"))?,
            interpret: d.get("interpret").and_then(D::as_bool),
            plain: d.get("plain").and_then(D::as_bool),
            separator: child("separator")?,
            source: str_field("source"),
            block: str_field("block"),
            entity: str_field("entity"),
            storage: str_field("storage"),
        }),
        Some("object") => Content::Object(
            d.entries()
                .unwrap_or_default()
                .into_iter()
                .filter(|(k, _)| !NON_CONTENT_KEYS.contains(k))
                .map(|(k, v)| (k.to_string(), v.to_raw()))
                .collect(),
        ),
        _ => return Err(invalid("component without content")),
    };

    Ok(content)
}

/// Translation arguments may also be plain numbers or booleans; those are
/// kept as raw values so they are written back unchanged.
fn decode_argument<D: Data>(d: &D, depth: usize) -> Result<Argument, TextError> {
    let primitive = d.as_str().is_none()
        && d.items().is_none()
        && d.entries().is_none()
        && (d.as_f64().is_some() || d.as_bool().is_some());
    if primitive {
        return Ok(Argument::Value(d.to_raw()));
    }
    decode_at(d, depth).map(Argument::from)
}

#[inline(never)]
fn decode_style<D: Data>(d: &D, depth: usize) -> Result<Style, TextError> {
    let color = match d.get("color").map(|c| c.as_str()) {
        None => None,
        Some(Some(s)) => Some(Color::parse(s).ok_or_else(|| invalid(format!("bad color `{s}`")))?),
        Some(None) => return Err(invalid("`color` must be a string")),
    };
    let flag = |k: &str| d.get(k).and_then(D::as_bool);
    let click = d.get("click_event").or_else(|| d.get("clickEvent"));
    let hover = d.get("hover_event").or_else(|| d.get("hoverEvent"));
    Ok(Style {
        color,
        shadow_color: d.get("shadow_color").map(decode_argb).transpose()?,
        bold: flag("bold"),
        italic: flag("italic"),
        underlined: flag("underlined"),
        strikethrough: flag("strikethrough"),
        obfuscated: flag("obfuscated"),
        click_event: click.map(decode_click).transpose()?,
        hover_event: hover.map(|h| decode_hover(h, depth)).transpose()?,
        insertion: d.get("insertion").and_then(D::as_str).map(str::to_string),
        font: d.get("font").and_then(D::as_str).map(str::to_string),
    })
}

/// `shadow_color`: an ARGB int, or a list of four floats (red, green, blue,
/// alpha in 0..1).
fn decode_argb<D: Data>(d: &D) -> Result<i32, TextError> {
    if let Some(items) = d.items() {
        let v: Vec<f64> = items.iter().filter_map(|i| i.as_f64()).collect();
        let [r, g, b, a] = v.as_slice() else {
            return Err(invalid("`shadow_color` list must have 4 numbers"));
        };
        let channel = |x: f64| ((x * 255.0).floor().clamp(0.0, 255.0)) as u32;
        let argb = (channel(*a) << 24) | (channel(*r) << 16) | (channel(*g) << 8) | channel(*b);
        return Ok(argb as i32);
    }
    let v = d
        .as_f64()
        .ok_or_else(|| invalid("`shadow_color` must be a number"))?;
    if v.fract() != 0.0 || v < f64::from(i32::MIN) || v > f64::from(u32::MAX) {
        return Err(invalid("`shadow_color` out of range"));
    }
    Ok((v as i64) as i32)
}

fn raw_fields<D: Data>(d: &D, skip: &[&str]) -> Vec<(String, Raw)> {
    d.entries()
        .unwrap_or_default()
        .into_iter()
        .filter(|(k, _)| !skip.contains(k))
        .map(|(k, v)| (k.to_string(), v.to_raw()))
        .collect()
}

fn decode_click<D: Data>(d: &D) -> Result<ClickEvent, TextError> {
    let action = d
        .get("action")
        .and_then(D::as_str)
        .ok_or_else(|| invalid("click event without `action`"))?;
    let action = action.strip_prefix("minecraft:").unwrap_or(action);
    // Field of the 1.21.5 layout, or the old `value`.
    let text = |modern: &str| -> Result<String, TextError> {
        d.get(modern)
            .or_else(|| d.get("value"))
            .and_then(D::as_str)
            .map(str::to_string)
            .ok_or_else(|| invalid(format!("`{action}` without `{modern}`")))
    };
    Ok(match action {
        "open_url" => ClickEvent::OpenUrl(text("url")?),
        "open_file" => ClickEvent::OpenFile(text("path")?),
        "run_command" => ClickEvent::RunCommand(text("command")?),
        "suggest_command" => ClickEvent::SuggestCommand(text("command")?),
        "copy_to_clipboard" => ClickEvent::CopyToClipboard(text("value")?),
        "change_page" => {
            let page = match d.get("page").or_else(|| d.get("value")) {
                Some(v) => match (v.as_f64(), v.as_str()) {
                    (Some(n), _) if n.fract() == 0.0 && n.abs() <= f64::from(i32::MAX) => n as i32,
                    (_, Some(s)) => s
                        .trim()
                        .parse()
                        .map_err(|_| invalid("`change_page` page is not a number"))?,
                    _ => return Err(invalid("`change_page` page is not a number")),
                },
                None => return Err(invalid("`change_page` without `page`")),
            };
            ClickEvent::ChangePage(page)
        }
        "show_dialog" => ClickEvent::ShowDialog(
            d.get("dialog")
                .ok_or_else(|| invalid("`show_dialog` without `dialog`"))?
                .to_raw(),
        ),
        "custom" => ClickEvent::Custom {
            id: d
                .get("id")
                .and_then(D::as_str)
                .map(str::to_string)
                .ok_or_else(|| invalid("`custom` without `id`"))?,
            payload: d.get("payload").map(D::to_raw),
        },
        other => ClickEvent::Other {
            action: other.to_string(),
            fields: raw_fields(d, &["action"]),
        },
    })
}

fn decode_hover<D: Data>(d: &D, depth: usize) -> Result<HoverEvent, TextError> {
    let action = d
        .get("action")
        .and_then(D::as_str)
        .ok_or_else(|| invalid("hover event without `action`"))?;
    let action = action.strip_prefix("minecraft:").unwrap_or(action);
    let contents = d.get("contents");
    Ok(match action {
        "show_text" => {
            // 1.21.5: `value`; before: `contents` (and the deprecated `value`).
            let v = contents
                .or_else(|| d.get("value"))
                .or_else(|| d.get("text"))
                .ok_or_else(|| invalid("`show_text` without text"))?;
            HoverEvent::ShowText(Box::new(decode_at(v, depth + 1)?))
        }
        "show_item" => {
            // Before 1.21.5 the item is in `contents` (an object, or just
            // the item ID); since then its fields are inline.
            let item = contents.unwrap_or(d);
            if let Some(id) = item.as_str() {
                HoverEvent::ShowItem {
                    id: id.to_string(),
                    count: None,
                    components: None,
                }
            } else {
                HoverEvent::ShowItem {
                    id: item
                        .get("id")
                        .and_then(D::as_str)
                        .map(str::to_string)
                        .ok_or_else(|| invalid("`show_item` without `id`"))?,
                    count: item
                        .get("count")
                        .and_then(D::as_f64)
                        .filter(|n| n.fract() == 0.0 && n.abs() <= f64::from(i32::MAX))
                        .map(|n| n as i32),
                    components: item.get("components").map(D::to_raw),
                }
            }
        }
        "show_entity" => {
            let (entity, type_key, uuid_key) = match contents {
                Some(c) => (c, "type", "id"),
                None => (d, "id", "uuid"),
            };
            HoverEvent::ShowEntity {
                entity_type: entity
                    .get(type_key)
                    .and_then(D::as_str)
                    .map(str::to_string)
                    .ok_or_else(|| invalid("`show_entity` without a type"))?,
                uuid: decode_uuid(
                    entity
                        .get(uuid_key)
                        .ok_or_else(|| invalid("`show_entity` without a UUID"))?,
                )?,
                name: entity
                    .get("name")
                    .map(|n| decode_at(n, depth + 1).map(Box::new))
                    .transpose()?,
            }
        }
        other => HoverEvent::Other {
            action: other.to_string(),
            fields: raw_fields(d, &["action"]),
        },
    })
}

/// A UUID as four ints, a list of four numbers or the hyphenated hex form.
fn decode_uuid<D: Data>(d: &D) -> Result<[i32; 4], TextError> {
    if let Some(s) = d.as_str() {
        return parse_uuid(s).ok_or_else(|| invalid("bad UUID"));
    }
    let items = d.items().ok_or_else(|| invalid("bad UUID"))?;
    let v: Vec<i32> = items
        .iter()
        .filter_map(|i| i.as_f64())
        .map(|n| n as i64 as i32)
        .collect();
    match v.as_slice() {
        [a, b, c, e] if items.len() == 4 => Ok([*a, *b, *c, *e]),
        _ => Err(invalid("a UUID needs four numbers")),
    }
}

fn parse_uuid(s: &str) -> Option<[i32; 4]> {
    let parts: Vec<&str> = s.split('-').collect();
    let [a, b, c, d, e] = parts.as_slice() else {
        return None;
    };
    let lens = [a.len(), b.len(), c.len(), d.len(), e.len()];
    if lens.contains(&0) || lens.iter().take(4).any(|l| *l > 8) || e.len() > 12 {
        return None;
    }
    let num = |p: &str| u64::from_str_radix(p, 16).ok();
    let most = (num(a)? << 32) | (num(b)? << 16) | num(c)?;
    let least = (num(d)? << 48) | num(e)?;
    Some([
        (most >> 32) as u32 as i32,
        most as u32 as i32,
        (least >> 32) as u32 as i32,
        least as u32 as i32,
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uuid_forms() {
        assert_eq!(
            parse_uuid("00000001-0000-0002-0000-000000000003"),
            Some([1, 2, 0, 3])
        );
        assert_eq!(
            parse_uuid("ffffffff-ffff-ffff-ffff-ffffffffffff"),
            Some([-1, -1, -1, -1])
        );
        assert_eq!(parse_uuid("nope"), None);
        let tag = Tag::IntArray(vec![1, 2, 3, 4]);
        assert_eq!(decode_uuid(&tag), Ok([1, 2, 3, 4]));
    }
}
