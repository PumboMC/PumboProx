//! 128x128 map images in Minecraft map colour indices (`base_id * 4 + shade`),
//! and the bits of protocol data needed to show one (map block entity NBT,
//! VarInt). Used for CAPTCHA images and QR codes.

pub const SIZE: usize = 128;
pub const PIXELS: usize = SIZE * SIZE;

/// Map colour index for a base colour and shade (0 = darker, 1 = dark, 2 = normal, 3 = darkest).
pub const fn color(base: u8, shade: u8) -> u8 {
    base * 4 + shade
}

// Base colour ids of the vanilla map palette.
pub const SAND: u8 = 2;
pub const SNOW: u8 = 8;
pub const QUARTZ: u8 = 14;
pub const GRAY: u8 = 21;
pub const LIGHT_GRAY: u8 = 22;
pub const PURPLE: u8 = 24;
pub const BLUE: u8 = 25;
pub const BROWN: u8 = 26;
pub const GREEN: u8 = 27;
pub const RED: u8 = 28;
pub const BLACK: u8 = 29;

/// Approximate RGB of the colours above (normal shade), used for previews in tests.
pub fn base_rgb(base: u8) -> (u8, u8, u8) {
    match base {
        SAND => (247, 233, 163),
        SNOW => (255, 255, 255),
        QUARTZ => (255, 252, 245),
        LIGHT_GRAY => (153, 153, 153),
        GRAY => (76, 76, 76),
        PURPLE => (127, 63, 178),
        BLUE => (51, 76, 178),
        BROWN => (102, 76, 51),
        GREEN => (102, 127, 51),
        RED => (153, 51, 51),
        BLACK => (25, 25, 25),
        _ => (0, 0, 0),
    }
}

/// Approximate RGB of a colour index.
pub fn rgb(index: u8) -> (u8, u8, u8) {
    let (r, g, b) = base_rgb(index / 4);
    let mult: u32 = match index % 4 {
        0 => 180,
        1 => 220,
        2 => 255,
        _ => 135,
    };
    let m = |c: u8| ((u32::from(c) * mult) / 255) as u8;
    (m(r), m(g), m(b))
}

/// A map image being drawn. Writes outside the image are ignored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Canvas {
    pub px: Vec<u8>,
}

impl Canvas {
    pub fn new(fill: u8) -> Self {
        Self { px: vec![fill; PIXELS] }
    }

    pub fn set(&mut self, x: i32, y: i32, c: u8) {
        if (0..SIZE as i32).contains(&x)
            && (0..SIZE as i32).contains(&y)
            && let Some(p) = self.px.get_mut(y as usize * SIZE + x as usize)
        {
            *p = c;
        }
    }

    pub fn get(&self, x: i32, y: i32) -> Option<u8> {
        if (0..SIZE as i32).contains(&x) && (0..SIZE as i32).contains(&y) {
            self.px.get(y as usize * SIZE + x as usize).copied()
        } else {
            None
        }
    }

    pub fn fill_rect(&mut self, x: i32, y: i32, w: i32, h: i32, c: u8) {
        for yy in y..y + h {
            for xx in x..x + w {
                self.set(xx, yy, c);
            }
        }
    }
}

/// Minimal NBT for a map block entity holding the image (network NBT, unnamed root).
pub fn map_block_entity_nbt(map_id: i32, pixels: &[u8]) -> Vec<u8> {
    let mut out = vec![0x0A];
    // TAG_String "id"
    put_name(&mut out, 0x08, "id");
    put_str(&mut out, "minecraft:map");
    // TAG_Int "MapId"
    put_name(&mut out, 0x03, "MapId");
    out.extend_from_slice(&map_id.to_be_bytes());
    // TAG_Byte_Array "Colors"
    put_name(&mut out, 0x07, "Colors");
    let len = i32::try_from(pixels.len()).unwrap_or(0);
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(pixels);
    out.push(0x00);
    out
}

fn put_name(out: &mut Vec<u8>, tag: u8, name: &str) {
    out.push(tag);
    put_str(out, name);
}

fn put_str(out: &mut Vec<u8>, s: &str) {
    let len = u16::try_from(s.len()).unwrap_or(0);
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(s.as_bytes());
}

/// Encodes a VarInt (used for the `minecraft:map_id` item component).
pub fn varint(value: i32) -> Vec<u8> {
    let mut v = value as u32;
    let mut out = Vec::new();
    loop {
        let byte = (v & 0x7F) as u8;
        v >>= 7;
        if v == 0 {
            out.push(byte);
            return out;
        }
        out.push(byte | 0x80);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canvas_clips() {
        let mut c = Canvas::new(color(SNOW, 2));
        c.set(-1, 0, 1);
        c.set(128, 0, 1);
        c.fill_rect(126, 126, 5, 5, color(BLACK, 2));
        assert_eq!(c.get(127, 127), Some(color(BLACK, 2)));
        assert_eq!(c.get(128, 0), None);
        assert_eq!(c.px.len(), PIXELS);
        assert_eq!(rgb(color(SNOW, 2)), (255, 255, 255));
    }

    #[test]
    fn nbt_layout() {
        let nbt = map_block_entity_nbt(7, &[1, 2, 3]);
        assert_eq!(nbt[0], 0x0A);
        assert_eq!(*nbt.last().unwrap(), 0x00);
        assert!(nbt.windows(13).any(|w| w == b"minecraft:map"));
    }

    #[test]
    fn varints() {
        assert_eq!(varint(0), vec![0]);
        assert_eq!(varint(300), vec![0xAC, 0x02]);
        assert_eq!(varint(-1), vec![0xFF, 0xFF, 0xFF, 0xFF, 0x0F]);
    }
}
