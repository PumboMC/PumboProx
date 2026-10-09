//! Velocity "modern" forwarding, implemented from the protocol description
//! (plan §3.1).
//!
//! Answer to a `custom_query` on the `velocity:player_info` channel:
//! 32 bytes of HMAC-SHA256 (key = secret) over the rest, and the rest is a
//! VarInt version, String IP, UUID, String name, VarInt property count and the
//! properties (String name, String value, Boolean signed, optional String signature).

use std::sync::Arc;

use aws_lc_rs::hmac;
use pumbo_core::forwarding::{ForwardingError, ForwardingMode};
use pumbo_core::profile::ForwardedPlayer;
use pumbo_core::registry::{ModuleConfig, ModuleError, opt_str};
use pumbo_protocol::wire::{write_string, write_varint};

pub const NAME: &str = "modern";
pub const CHANNEL: &str = "velocity:player_info";

/// Version 1 (`MODERN_DEFAULT`) works with every backend.
pub const VERSION_DEFAULT: u8 = 1;
/// Version 4 (`MODERN_LAZY_SESSION`), same data layout.
pub const VERSION_LAZY_SESSION: u8 = 4;

/// Minimum secret length in bytes (§3.1).
pub const MIN_SECRET_LEN: usize = 16;

pub struct VelocityModern {
    key: hmac::Key,
}

impl std::fmt::Debug for VelocityModern {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The secret never goes to logs or diagnostics.
        f.debug_struct("VelocityModern").finish_non_exhaustive()
    }
}

impl VelocityModern {
    pub fn new(secret: &[u8]) -> Result<Self, String> {
        if secret.len() < MIN_SECRET_LEN {
            return Err(format!("secret is shorter than {MIN_SECRET_LEN} bytes"));
        }
        Ok(Self {
            key: hmac::Key::new(hmac::HMAC_SHA256, secret),
        })
    }

    /// Forwarding version: the highest common one, skipping 2 and 3 (§3.1 item 4).
    pub fn negotiate(requested: Option<u8>) -> u8 {
        match requested {
            Some(v) if v >= VERSION_LAZY_SESSION => VERSION_LAZY_SESSION,
            _ => VERSION_DEFAULT,
        }
    }

    /// Data signed with the HMAC.
    pub fn payload(version: u8, player: &ForwardedPlayer) -> Vec<u8> {
        let mut out = Vec::with_capacity(128);
        write_varint(&mut out, i32::from(version));
        write_string(&mut out, &player.address.to_string());
        out.extend_from_slice(player.profile.id.as_bytes());
        write_string(&mut out, &player.profile.name);
        let props = &player.profile.properties;
        write_varint(&mut out, i32::try_from(props.len()).unwrap_or(i32::MAX));
        for p in props {
            write_string(&mut out, &p.name);
            write_string(&mut out, &p.value);
            match &p.signature {
                Some(sig) => {
                    out.push(1);
                    write_string(&mut out, sig);
                }
                None => out.push(0),
            }
        }
        out
    }

    pub fn answer(&self, requested: Option<u8>, player: &ForwardedPlayer) -> Vec<u8> {
        let payload = Self::payload(Self::negotiate(requested), player);
        let tag = hmac::sign(&self.key, &payload);
        let mut out = Vec::with_capacity(32 + payload.len());
        out.extend_from_slice(tag.as_ref());
        out.extend_from_slice(&payload);
        out
    }
}

impl ForwardingMode for VelocityModern {
    fn name(&self) -> &str {
        NAME
    }

    fn answer_login_query(
        &self,
        channel: &str,
        data: &[u8],
        player: &ForwardedPlayer,
    ) -> Option<Result<Vec<u8>, ForwardingError>> {
        if channel != CHANNEL {
            return None;
        }
        Some(Ok(self.answer(data.first().copied(), player)))
    }

    fn expects_login_query(&self) -> bool {
        true
    }
}

/// Factory: the secret comes from the environment variable named by
/// `secret-env`, or from `secret-file` (default `forwarding.secret`). A
/// missing file is created on the first start with 32 random bytes as hex and
/// mode 0600 (§3.1).
pub fn factory(cfg: &ModuleConfig) -> Result<Arc<dyn ForwardingMode>, ModuleError> {
    let err = |message: String| ModuleError::Config {
        name: NAME.to_string(),
        message,
    };
    if let Some(var) = opt_str(cfg, NAME, "secret-env")? {
        let secret = std::env::var(var)
            .map_err(|_| err(format!("environment variable {var} is not set")))?;
        let module = VelocityModern::new(secret.trim().as_bytes()).map_err(err)?;
        return Ok(Arc::new(module));
    }
    let path = opt_str(cfg, NAME, "secret-file")?.unwrap_or("forwarding.secret");
    if !std::path::Path::new(path).exists() {
        create_secret(path).map_err(err)?;
        tracing::warn!(
            "created a new forwarding secret in {path}; put the same secret in every backend"
        );
    }
    check_permissions(path).map_err(err)?;
    let secret = std::fs::read_to_string(path).map_err(|e| err(format!("{path}: {e}")))?;
    let module = VelocityModern::new(secret.trim().as_bytes()).map_err(err)?;
    Ok(Arc::new(module))
}

fn create_secret(path: &str) -> Result<(), String> {
    use std::io::Write as _;
    let mut bytes = [0u8; 32];
    aws_lc_rs::rand::fill(&mut bytes).map_err(|_| "random generator failed".to_string())?;
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.mode(0o600);
    }
    let mut file = opts.open(path).map_err(|e| format!("{path}: {e}"))?;
    file.write_all(hex.as_bytes())
        .map_err(|e| format!("{path}: {e}"))
}

#[cfg(unix)]
fn check_permissions(path: &str) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt as _;
    let meta = std::fs::metadata(path).map_err(|e| format!("{path}: {e}"))?;
    if meta.permissions().mode() & 0o077 != 0 {
        return Err(format!(
            "{path}: secret file is readable by other users (set mode 0600)"
        ));
    }
    Ok(())
}

#[cfg(not(unix))]
fn check_permissions(_: &str) -> Result<(), String> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use pumbo_core::profile::{GameProfile, Property};

    fn player() -> ForwardedPlayer {
        ForwardedPlayer {
            profile: GameProfile {
                id: uuid::Uuid::from_u128(0x0011_2233_4455_6677_8899_aabb_ccdd_eeff),
                name: "Player".into(),
                properties: vec![Property {
                    name: "textures".into(),
                    value: "dGVzdA==".into(),
                    signature: Some("c2ln".into()),
                }],
            },
            address: "203.0.113.7".parse().unwrap(),
        }
    }

    #[test]
    fn data_layout() {
        let p = VelocityModern::payload(1, &player());
        let mut expected = vec![1u8, 11];
        expected.extend_from_slice(b"203.0.113.7");
        expected.extend_from_slice(&0x0011_2233_4455_6677_8899_aabb_ccdd_eeffu128.to_be_bytes());
        expected.push(6);
        expected.extend_from_slice(b"Player");
        expected.push(1);
        expected.push(8);
        expected.extend_from_slice(b"textures");
        expected.push(8);
        expected.extend_from_slice(b"dGVzdA==");
        expected.push(1);
        expected.push(4);
        expected.extend_from_slice(b"c2ln");
        assert_eq!(p, expected);
    }

    #[test]
    fn signature_verifies_with_the_same_secret() {
        let secret = b"0123456789abcdef0123";
        let m = VelocityModern::new(secret).unwrap();
        let answer = m
            .answer_login_query(CHANNEL, &[4], &player())
            .unwrap()
            .unwrap();
        let (tag, data) = answer.split_at(32);
        assert_eq!(data.first(), Some(&4));
        let key = hmac::Key::new(hmac::HMAC_SHA256, secret);
        assert!(hmac::verify(&key, data, tag).is_ok());
        let other = hmac::Key::new(hmac::HMAC_SHA256, b"other-secret-other-secret");
        assert!(hmac::verify(&other, data, tag).is_err());
    }

    #[test]
    fn version_negotiation() {
        assert_eq!(VelocityModern::negotiate(None), 1);
        assert_eq!(VelocityModern::negotiate(Some(1)), 1);
        assert_eq!(VelocityModern::negotiate(Some(2)), 1);
        assert_eq!(VelocityModern::negotiate(Some(3)), 1);
        assert_eq!(VelocityModern::negotiate(Some(4)), 4);
        assert_eq!(VelocityModern::negotiate(Some(9)), 4);
    }

    #[test]
    fn secret_file_is_created_once_with_private_mode() {
        let dir = std::env::temp_dir().join(format!("pumbo-secret-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("forwarding.secret");
        let _ = std::fs::remove_file(&path);
        let mut cfg = ModuleConfig::new();
        cfg.insert("secret-file".into(), path.to_str().unwrap().into());
        factory(&cfg).unwrap();
        let first = std::fs::read_to_string(&path).unwrap();
        assert_eq!(first.len(), 64);
        factory(&cfg).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), first);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
            assert!(factory(&cfg).unwrap_err().to_string().contains("0600"));
        }
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn foreign_channel_and_short_secret() {
        let m = VelocityModern::new(b"0123456789abcdef").unwrap();
        assert!(
            m.answer_login_query("other:channel", &[], &player())
                .is_none()
        );
        assert!(VelocityModern::new(b"short").is_err());
        assert!(!format!("{m:?}").contains("0123"));
    }
}
