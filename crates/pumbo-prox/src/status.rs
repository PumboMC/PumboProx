//! Server list answers: the status JSON and the pre-1.7 ping (0xFE).

use base64::Engine as _;
use pumbo_text::{Component, TextFormat};
use serde_json::json;

/// Default `status.favicon`: next to the config, like Velocity and Paper.
pub const DEFAULT_FAVICON: &str = "server-icon.png";

/// The status answer is one string of at most 32767 characters; the icon
/// leaves room for the rest.
const FAVICON_MAX: usize = 28 * 1024;

/// `status.favicon` as the `data:` URL of the status answer. A file that is
/// not a 64x64 PNG (what the client shows) gives a warning and no icon; a
/// missing `server-icon.png` (the default) means no icon, quietly.
pub fn load_favicon(path: &str) -> Option<String> {
    if path.is_empty() {
        return None;
    }
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && path == DEFAULT_FAVICON => {
            return None;
        }
        Err(e) => {
            tracing::warn!("status.favicon: {path}: {e}, the server list shows no icon");
            return None;
        }
    };
    let url = format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(&bytes)
    );
    let problem = match png_size(&bytes) {
        None => Some("not a PNG file".to_string()),
        Some((64, 64)) if url.len() > FAVICON_MAX => Some(format!(
            "{} KB, at most {} KB fit into the status answer",
            bytes.len() / 1024,
            FAVICON_MAX / 4 * 3 / 1024
        )),
        Some((64, 64)) => None,
        Some((w, h)) => Some(format!("{w}x{h} pixels, the server list needs 64x64")),
    };
    match problem {
        None => Some(url),
        Some(why) => {
            tracing::warn!("status.favicon: {path}: {why}, the server list shows no icon");
            None
        }
    }
}

/// Width and height from the PNG signature and the `IHDR` chunk after it.
fn png_size(b: &[u8]) -> Option<(u32, u32)> {
    if b.get(..8)? != b"\x89PNG\r\n\x1a\n" || b.get(12..16)? != b"IHDR" {
        return None;
    }
    let n = |at: usize| Some(u32::from_be_bytes(b.get(at..at + 4)?.try_into().ok()?));
    Some((n(16)?, n(20)?))
}

/// MOTD and kick texts from the config: MiniMessage when it has tags,
/// otherwise `&` colour codes.
pub fn parse_text(s: &str) -> Component {
    if s.contains('<') && s.contains('>') {
        pumbo_text::parse_mini(s)
    } else {
        pumbo_text::parse_legacy(s)
    }
}

/// The `status_response` JSON for a client speaking `protocol`.
pub fn status_json(
    version_name: &str,
    protocol: i32,
    online: usize,
    max: u32,
    motd: &Component,
    favicon: Option<&str>,
) -> String {
    let mut j = json!({
        "version": { "name": version_name, "protocol": protocol },
        "players": { "max": max, "online": online, "sample": [] },
        "description": motd.to_json_value(TextFormat::for_protocol(protocol)),
        "enforcesSecureChat": false,
    });
    if let (Some(f), Some(m)) = (favicon, j.as_object_mut()) {
        m.insert("favicon".into(), f.into());
    }
    j.to_string()
}

/// Answer to a legacy ping. `modern_ping`: the client sent `FE 01` (1.4–1.6),
/// which gets the `§1` format; older clients get `motd§online§max`.
pub fn legacy_response(
    modern_ping: bool,
    version_name: &str,
    motd: &str,
    online: usize,
    max: u32,
) -> Vec<u8> {
    let motd: String = motd.chars().filter(|c| *c != '§').collect();
    let text = if modern_ping {
        format!("§1\0127\0{version_name}\0{motd}\0{online}\0{max}")
    } else {
        format!("{motd}§{online}§{max}")
    };
    let units: Vec<u16> = text.encode_utf16().collect();
    let mut out = Vec::with_capacity(3 + units.len() * 2);
    out.push(0xFF);
    out.extend_from_slice(&u16::try_from(units.len()).unwrap_or(u16::MAX).to_be_bytes());
    for u in units {
        out.extend_from_slice(&u.to_be_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_and_legacy() {
        let motd = parse_text("&6Pumbo &fProx");
        let j: serde_json::Value =
            serde_json::from_str(&status_json("PumboProx", 777, 3, 500, &motd, None)).unwrap();
        assert!(j.get("favicon").is_none());
        assert_eq!(j["version"]["protocol"], 777);
        assert_eq!(j["players"]["online"], 3);
        assert_eq!(
            Component::from_json_value(&j["description"])
                .unwrap()
                .plain_text(),
            "Pumbo Prox"
        );
        let mini = parse_text("<gold>Pumbo</gold>");
        assert_eq!(mini.plain_text(), "Pumbo");

        let r = legacy_response(true, "PumboProx", "Hi", 1, 20);
        assert_eq!(r.first(), Some(&0xFF));
        let units: Vec<u16> = r[3..]
            .chunks(2)
            .map(|c| u16::from_be_bytes([c[0], c[1]]))
            .collect();
        assert_eq!(
            String::from_utf16(&units).unwrap(),
            "§1\u{0}127\u{0}PumboProx\u{0}Hi\u{0}1\u{0}20"
        );
        let old = legacy_response(false, "PumboProx", "Hi", 1, 20);
        assert_eq!(u16::from_be_bytes([old[1], old[2]]), 7);
    }

    /// The signature and `IHDR` of a `w`x`h` PNG (all the check reads).
    fn png(w: u32, h: u32) -> Vec<u8> {
        let mut b = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
        b.extend(w.to_be_bytes());
        b.extend(h.to_be_bytes());
        b.extend([8, 6, 0, 0, 0]);
        b
    }

    #[test]
    fn favicon() {
        let dir = std::env::temp_dir().join(format!("pumbo-favicon-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = |name: &str, bytes: &[u8]| {
            let p = dir.join(name);
            std::fs::write(&p, bytes).unwrap();
            p.display().to_string()
        };
        let icon = load_favicon(&file("ok.png", &png(64, 64))).unwrap();
        assert!(
            icon.starts_with("data:image/png;base64,iVBORw0KGgo"),
            "{icon}"
        );
        let motd = parse_text("x");
        let j: serde_json::Value =
            serde_json::from_str(&status_json("P", 777, 0, 1, &motd, Some(&icon))).unwrap();
        assert_eq!(j["favicon"], icon.as_str());
        // Another size, not a PNG, too large, missing, none: no icon.
        assert_eq!(load_favicon(&file("small.png", &png(32, 32))), None);
        assert_eq!(load_favicon(&file("text.png", b"hello")), None);
        let mut big = png(64, 64);
        big.resize(30_000, 0);
        assert_eq!(load_favicon(&file("big.png", &big)), None);
        assert_eq!(
            load_favicon(&dir.join("none.png").display().to_string()),
            None
        );
        assert_eq!(load_favicon(""), None);
    }
}
