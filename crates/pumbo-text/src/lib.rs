//! Text components (chat, titles, kicks, MOTD) for the supported protocols.
//!
//! - [`Component`]: the model (content, style, children).
//! - JSON and NBT forms: [`Component::to_json`], [`Component::from_json`],
//!   [`Component::to_nbt`], [`Component::from_nbt`]. The written layout depends
//!   on [`TextFormat`]: 1.21.5 (protocol 770) renamed the event fields to
//!   snake_case and restructured them, 1.21.4 (769) added `shadow_color`.
//!   Reading accepts both layouts, so text from any backend in the range parses.
//! - Input formats for administrators and plugins: legacy `&` codes
//!   ([`parse_legacy`]) and a MiniMessage subset ([`parse_mini`]).
//!
//! Sources (minecraft.wiki): "Text component format" (current layout and its
//! history table: 24w44a `shadow_color`, 25w02a/25w03a event changes, 25w20a
//! `custom` and `show_dialog` click actions, 25w32a `object` content) and
//! "Text component format/Before Java Edition 1.21.5" (old event layout).

mod codec;
mod color;
mod legacy;
mod mini;
mod raw;
pub mod template;

pub use codec::MAX_DEPTH;

pub use color::{Color, NamedColor};
pub use legacy::{parse_legacy, parse_legacy_with};
pub use mini::{StyleSheet, parse_mini, parse_mini_styled, strip_events};
pub use raw::Raw;

/// Layout of serialized components for one protocol version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TextFormat {
    /// `click_event`/`hover_event` with per-action fields (1.21.5, protocol
    /// 770+); otherwise `clickEvent {action, value}` and
    /// `hoverEvent {action, contents}`.
    pub snake_case_events: bool,
    /// `shadow_color` style field exists (1.21.4, protocol 769+). When false
    /// the field is left out on writing.
    pub shadow_color: bool,
}

impl TextFormat {
    /// Protocols 767–768.
    pub const V767: TextFormat = TextFormat {
        snake_case_events: false,
        shadow_color: false,
    };
    /// Protocol 769.
    pub const V769: TextFormat = TextFormat {
        snake_case_events: false,
        shadow_color: true,
    };
    /// Protocol 770 and later.
    pub const V770: TextFormat = TextFormat {
        snake_case_events: true,
        shadow_color: true,
    };

    /// Layout for a protocol number (767 and later; older numbers get the
    /// 767 layout).
    pub const fn for_protocol(protocol: i32) -> TextFormat {
        if protocol >= 770 {
            Self::V770
        } else if protocol >= 769 {
            Self::V769
        } else {
            Self::V767
        }
    }
}

/// Errors from parsing JSON or NBT text.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TextError {
    #[error("invalid JSON: {0}")]
    Json(String),
    #[error("invalid text component: {0}")]
    Invalid(String),
    #[error("text component nested deeper than {0}")]
    TooDeep(usize),
}

/// A text component: content, style and children (`extra`). Children inherit
/// the style of their parent unless they override it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Component {
    pub content: Content,
    pub style: Style,
    pub extra: Vec<Component>,
}

/// What a component displays.
#[derive(Debug, Clone, PartialEq)]
pub enum Content {
    /// Plain text.
    Text(String),
    /// A translation key with optional fallback and arguments.
    Translatable {
        key: String,
        fallback: Option<String>,
        with: Vec<Argument>,
    },
    /// The name of a key binding (`key.inventory`).
    Keybind(String),
    /// A scoreboard value (resolved by the server).
    Score { name: String, objective: String },
    /// Entity names from a selector (resolved by the server).
    Selector {
        pattern: String,
        separator: Option<Box<Component>>,
    },
    /// NBT values (resolved by the server).
    Nbt(NbtContent),
    /// `object` content (1.21.9+: sprites, player heads), kept as raw fields.
    Object(Vec<(String, Raw)>),
}

/// A translation argument: a component, or a primitive value (number or
/// boolean) that vanilla keeps as is. Strings are components (a bare string
/// component is written the same way).
#[derive(Debug, Clone, PartialEq)]
pub enum Argument {
    Component(Box<Component>),
    Value(Raw),
}

impl Argument {
    pub fn as_component(&self) -> Option<&Component> {
        match self {
            Argument::Component(c) => Some(c),
            Argument::Value(_) => None,
        }
    }

    /// Text of the argument: the component's plain text or the value as
    /// written (`50`, `true`).
    pub fn plain_text(&self) -> String {
        match self {
            Argument::Component(c) => c.plain_text(),
            Argument::Value(v) => match v.to_json() {
                serde_json::Value::String(s) => s,
                other => other.to_string(),
            },
        }
    }
}

impl From<Component> for Argument {
    fn from(c: Component) -> Self {
        Argument::Component(Box::new(c))
    }
}

impl Default for Content {
    fn default() -> Self {
        Content::Text(String::new())
    }
}

/// Fields of `nbt` content, kept as written.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct NbtContent {
    pub path: String,
    pub interpret: Option<bool>,
    pub plain: Option<bool>,
    pub separator: Option<Box<Component>>,
    /// Explicit `source` (`block`, `entity`, `storage`).
    pub source: Option<String>,
    pub block: Option<String>,
    pub entity: Option<String>,
    pub storage: Option<String>,
}

/// Formatting and interactivity. `None` means "inherit from the parent".
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Style {
    pub color: Option<Color>,
    /// ARGB shadow color (1.21.4+).
    pub shadow_color: Option<i32>,
    pub bold: Option<bool>,
    pub italic: Option<bool>,
    pub underlined: Option<bool>,
    pub strikethrough: Option<bool>,
    pub obfuscated: Option<bool>,
    pub click_event: Option<ClickEvent>,
    pub hover_event: Option<HoverEvent>,
    pub insertion: Option<String>,
    /// Font resource location.
    pub font: Option<String>,
}

impl Style {
    pub fn is_empty(&self) -> bool {
        *self == Style::default()
    }

    /// Fields set in `other` override the ones here.
    pub fn merged_with(&self, other: &Style) -> Style {
        Style {
            color: other.color.or(self.color),
            shadow_color: other.shadow_color.or(self.shadow_color),
            bold: other.bold.or(self.bold),
            italic: other.italic.or(self.italic),
            underlined: other.underlined.or(self.underlined),
            strikethrough: other.strikethrough.or(self.strikethrough),
            obfuscated: other.obfuscated.or(self.obfuscated),
            click_event: other
                .click_event
                .clone()
                .or_else(|| self.click_event.clone()),
            hover_event: other
                .hover_event
                .clone()
                .or_else(|| self.hover_event.clone()),
            insertion: other.insertion.clone().or_else(|| self.insertion.clone()),
            font: other.font.clone().or_else(|| self.font.clone()),
        }
    }
}

/// What happens on click.
#[derive(Debug, Clone, PartialEq)]
pub enum ClickEvent {
    OpenUrl(String),
    OpenFile(String),
    RunCommand(String),
    SuggestCommand(String),
    ChangePage(i32),
    CopyToClipboard(String),
    /// 1.21.6+: a dialog ID or an inline dialog.
    ShowDialog(Raw),
    /// 1.21.6+: a custom event sent to the server.
    Custom {
        id: String,
        payload: Option<Raw>,
    },
    /// An action this crate does not know, kept as its raw fields.
    Other {
        action: String,
        fields: Vec<(String, Raw)>,
    },
}

/// What happens on hover.
#[derive(Debug, Clone, PartialEq)]
pub enum HoverEvent {
    ShowText(Box<Component>),
    ShowItem {
        id: String,
        count: Option<i32>,
        /// Item components, kept raw.
        components: Option<Raw>,
    },
    ShowEntity {
        /// Entity type (`minecraft:pig`).
        entity_type: String,
        /// UUID as four big-endian ints, as vanilla writes it.
        uuid: [i32; 4],
        name: Option<Box<Component>>,
    },
    /// An action this crate does not know, kept as its raw fields.
    Other {
        action: String,
        fields: Vec<(String, Raw)>,
    },
}

impl Component {
    /// Plain text without style.
    pub fn text(s: impl Into<String>) -> Self {
        Component {
            content: Content::Text(s.into()),
            ..Component::default()
        }
    }

    /// A translatable component.
    pub fn translatable(key: impl Into<String>, with: Vec<Component>) -> Self {
        Component {
            content: Content::Translatable {
                key: key.into(),
                fallback: None,
                with: with.into_iter().map(Argument::from).collect(),
            },
            ..Component::default()
        }
    }

    pub fn with_style(mut self, style: Style) -> Self {
        self.style = style;
        self
    }

    pub fn color(mut self, color: Color) -> Self {
        self.style.color = Some(color);
        self
    }

    pub fn append(mut self, child: Component) -> Self {
        self.extra.push(child);
        self
    }

    /// The text if this is plain text with no style and no children (vanilla
    /// writes such a component as a bare string).
    pub fn as_plain_str(&self) -> Option<&str> {
        match &self.content {
            Content::Text(t) if self.style.is_empty() && self.extra.is_empty() => Some(t),
            _ => None,
        }
    }

    /// Text without formatting, for logs: translation keys (or their
    /// fallback), key bindings and selectors appear as written.
    pub fn plain_text(&self) -> String {
        let mut out = String::new();
        self.push_plain(&mut out);
        out
    }

    fn push_plain(&self, out: &mut String) {
        match &self.content {
            Content::Text(t) => out.push_str(t),
            Content::Translatable { key, fallback, .. } => {
                out.push_str(fallback.as_deref().unwrap_or(key));
            }
            Content::Keybind(k) => out.push_str(k),
            Content::Selector { pattern, .. } => out.push_str(pattern),
            Content::Score { .. } | Content::Nbt(_) | Content::Object(_) => {}
        }
        for child in &self.extra {
            child.push_plain(out);
        }
    }

    /// JSON text (status MOTD, `login_disconnect`).
    pub fn to_json(&self, format: TextFormat) -> String {
        self.to_json_value(format).to_string()
    }

    pub fn to_json_value(&self, format: TextFormat) -> serde_json::Value {
        codec::encode(self, format)
    }

    /// Parses JSON text in either layout.
    pub fn from_json(s: &str) -> Result<Component, TextError> {
        let value: serde_json::Value =
            serde_json::from_str(s).map_err(|e| TextError::Json(e.to_string()))?;
        Self::from_json_value(&value)
    }

    pub fn from_json_value(value: &serde_json::Value) -> Result<Component, TextError> {
        codec::decode(value)
    }

    /// NBT text (configuration and play packets).
    pub fn to_nbt(&self, format: TextFormat) -> pumbo_nbt::Tag {
        codec::encode(self, format)
    }

    /// Parses NBT text in either layout.
    pub fn from_nbt(tag: &pumbo_nbt::Tag) -> Result<Component, TextError> {
        codec::decode(tag)
    }
}

impl From<&str> for Component {
    fn from(s: &str) -> Self {
        Component::text(s)
    }
}

impl From<String> for Component {
    fn from(s: String) -> Self {
        Component::text(s)
    }
}
