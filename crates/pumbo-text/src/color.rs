//! Text colors: the 16 named colors and `#RRGGBB`.

/// A named (legacy) color.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NamedColor {
    Black,
    DarkBlue,
    DarkGreen,
    DarkAqua,
    DarkRed,
    DarkPurple,
    Gold,
    Gray,
    DarkGray,
    Blue,
    Green,
    Aqua,
    Red,
    LightPurple,
    Yellow,
    White,
}

/// (color, name, legacy code, RGB) from minecraft.wiki "Text component format".
const NAMED: [(NamedColor, &str, char, u32); 16] = [
    (NamedColor::Black, "black", '0', 0x000000),
    (NamedColor::DarkBlue, "dark_blue", '1', 0x0000AA),
    (NamedColor::DarkGreen, "dark_green", '2', 0x00AA00),
    (NamedColor::DarkAqua, "dark_aqua", '3', 0x00AAAA),
    (NamedColor::DarkRed, "dark_red", '4', 0xAA0000),
    (NamedColor::DarkPurple, "dark_purple", '5', 0xAA00AA),
    (NamedColor::Gold, "gold", '6', 0xFFAA00),
    (NamedColor::Gray, "gray", '7', 0xAAAAAA),
    (NamedColor::DarkGray, "dark_gray", '8', 0x555555),
    (NamedColor::Blue, "blue", '9', 0x5555FF),
    (NamedColor::Green, "green", 'a', 0x55FF55),
    (NamedColor::Aqua, "aqua", 'b', 0x55FFFF),
    (NamedColor::Red, "red", 'c', 0xFF5555),
    (NamedColor::LightPurple, "light_purple", 'd', 0xFF55FF),
    (NamedColor::Yellow, "yellow", 'e', 0xFFFF55),
    (NamedColor::White, "white", 'f', 0xFFFFFF),
];

impl NamedColor {
    pub const ALL: [NamedColor; 16] = [
        NamedColor::Black,
        NamedColor::DarkBlue,
        NamedColor::DarkGreen,
        NamedColor::DarkAqua,
        NamedColor::DarkRed,
        NamedColor::DarkPurple,
        NamedColor::Gold,
        NamedColor::Gray,
        NamedColor::DarkGray,
        NamedColor::Blue,
        NamedColor::Green,
        NamedColor::Aqua,
        NamedColor::Red,
        NamedColor::LightPurple,
        NamedColor::Yellow,
        NamedColor::White,
    ];

    fn entry(self) -> (NamedColor, &'static str, char, u32) {
        NAMED.iter().copied().find(|(c, ..)| *c == self).unwrap_or((
            NamedColor::White,
            "white",
            'f',
            0xFFFFFF,
        ))
    }

    /// Name in the text format (`dark_red`).
    pub fn name(self) -> &'static str {
        self.entry().1
    }

    /// Legacy formatting code (`4` for dark red).
    pub fn code(self) -> char {
        self.entry().2
    }

    pub fn rgb(self) -> u32 {
        self.entry().3
    }

    pub fn from_name(name: &str) -> Option<NamedColor> {
        NAMED.iter().find(|(_, n, ..)| *n == name).map(|(c, ..)| *c)
    }

    /// From a legacy code, case-insensitive.
    pub fn from_code(code: char) -> Option<NamedColor> {
        let code = code.to_ascii_lowercase();
        NAMED
            .iter()
            .find(|(_, _, k, _)| *k == code)
            .map(|(c, ..)| *c)
    }
}

/// A text color.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Color {
    Named(NamedColor),
    /// `0xRRGGBB`.
    Rgb(u32),
}

impl Color {
    /// Parses a color name or `#RRGGBB` (case-insensitive hex).
    pub fn parse(s: &str) -> Option<Color> {
        if let Some(hex) = s.strip_prefix('#') {
            if hex.len() != 6 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
                return None;
            }
            return u32::from_str_radix(hex, 16).ok().map(Color::Rgb);
        }
        NamedColor::from_name(s).map(Color::Named)
    }

    /// Serialized form: the name, or `#RRGGBB` in upper case.
    pub fn serialize(self) -> String {
        match self {
            Color::Named(n) => n.name().to_string(),
            Color::Rgb(v) => format!("#{:06X}", v & 0xFF_FFFF),
        }
    }

    pub fn rgb(self) -> u32 {
        match self {
            Color::Named(n) => n.rgb(),
            Color::Rgb(v) => v & 0xFF_FFFF,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_codes_and_hex() {
        for c in NamedColor::ALL {
            assert_eq!(NamedColor::from_name(c.name()), Some(c));
            assert_eq!(NamedColor::from_code(c.code()), Some(c));
        }
        assert_eq!(NamedColor::from_code('C'), Some(NamedColor::Red));
        assert_eq!(Color::parse("#ff0088"), Some(Color::Rgb(0xFF0088)));
        assert_eq!(Color::Rgb(0xFF0088).serialize(), "#FF0088");
        assert_eq!(Color::parse("#ff008"), None);
        assert_eq!(Color::parse("#gg0088"), None);
        assert_eq!(
            Color::parse("light_purple"),
            Some(Color::Named(NamedColor::LightPurple))
        );
        assert_eq!(Color::parse("pink"), None);
    }
}
