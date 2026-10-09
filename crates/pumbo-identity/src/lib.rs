//! Player identity: offline UUIDs, the server hash and the authentication
//! module using Mojang's services (trait `pumbo_core::identity::Authenticator`).

pub mod mojang;

use aws_lc_rs::digest;
use md5::{Digest as _, Md5};
use pumbo_core::registry::{ModuleError, ModuleRegistry};
use uuid::Uuid;

pub fn register(reg: &mut ModuleRegistry) -> Result<(), ModuleError> {
    reg.authenticators.register(mojang::NAME, mojang::factory)
}

/// Offline player UUID: a version 3 UUID from the MD5 of just the string
/// `"OfflinePlayer:" + name`, without a namespace (like Java's
/// `UUID.nameUUIDFromBytes`; the RFC `Uuid::new_v3` prepends a namespace and
/// gives a different result).
pub fn offline_uuid(name: &str) -> Uuid {
    let digest = Md5::digest(format!("OfflinePlayer:{name}").as_bytes());
    uuid::Builder::from_md5_bytes(digest.into()).into_uuid()
}

/// Server hash in Mojang's notation: SHA-1 of (server ID, shared secret,
/// public key) as a signed two's-complement number, written in hex without
/// leading zeros, with a minus sign when negative.
pub fn server_hash(parts: &[&[u8]]) -> String {
    let mut ctx = digest::Context::new(&digest::SHA1_FOR_LEGACY_USE_ONLY);
    for p in parts {
        ctx.update(p);
    }
    let d = ctx.finish();
    let mut bytes: Vec<u8> = d.as_ref().to_vec();
    let negative = bytes.first().is_some_and(|b| b & 0x80 != 0);
    if negative {
        // Two's-complement negation.
        let mut carry = true;
        for b in bytes.iter_mut().rev() {
            *b = !*b;
            if carry {
                let (v, overflow) = b.overflowing_add(1);
                *b = v;
                carry = overflow;
            }
        }
    }
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    let trimmed = hex.trim_start_matches('0');
    let body = if trimmed.is_empty() { "0" } else { trimmed };
    if negative {
        format!("-{body}")
    } else {
        body.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_hash_vectors() {
        // Vectors from the protocol description (minecraft.wiki, "Protocol encryption").
        assert_eq!(
            server_hash(&[b"Notch"]),
            "4ed1f46bbe04bc756bcb17c0c7ce3e4632f06a48"
        );
        assert_eq!(
            server_hash(&[b"jeb_"]),
            "-7c9d5b0044c130109a5d7b5fb5c317c02b4e28c1"
        );
        assert_eq!(
            server_hash(&[b"simon"]),
            "88e16a1019277b15d58faf0541e11910eb756f6"
        );
    }

    #[test]
    fn offline_uuid_matches_java() {
        assert_eq!(
            offline_uuid("Notch").to_string(),
            "b50ad385-829d-3141-a216-7e7d7539ba7f"
        );
        assert_eq!(offline_uuid("Notch").get_version_num(), 3);
        assert_eq!(offline_uuid("Notch"), offline_uuid("Notch"));
        assert_ne!(offline_uuid("Notch"), offline_uuid("notch"));
    }
}
