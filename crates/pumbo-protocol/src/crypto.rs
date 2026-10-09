//! Login encryption (plan §2.7): RSA-2048 key exchange with PKCS#1 v1.5
//! (`aws-lc-rs`, decision D-E0-3) and the AES-128-CFB8 stream cipher, where
//! the shared secret is both key and IV.

use aes::Aes128;
use aes::cipher::generic_array::GenericArray;
use aes::cipher::{BlockDecryptMut, BlockEncryptMut, KeyIvInit};
use aws_lc_rs::encoding::{AsDer, PublicKeyX509Der};
use aws_lc_rs::rsa::{
    KeySize, Pkcs1PrivateDecryptingKey, Pkcs1PublicEncryptingKey, PrivateDecryptingKey,
    PublicEncryptingKey,
};
use thiserror::Error;

#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum CryptoError {
    #[error("key generation failed")]
    KeyGeneration,
    /// One error for every decryption problem (bad padding, wrong length), so
    /// the server is not a padding oracle (§2.7).
    #[error("key exchange failed")]
    Exchange,
    #[error("shared secret must be 16 bytes")]
    SecretLength,
}

/// Server key pair, generated at startup.
pub struct ServerKey {
    private: Pkcs1PrivateDecryptingKey,
    public_der: Vec<u8>,
}

impl std::fmt::Debug for ServerKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServerKey").finish_non_exhaustive()
    }
}

impl ServerKey {
    /// A new RSA-2048 key (aws-lc-rs does not support 1024 bits).
    pub fn generate() -> Result<Self, CryptoError> {
        let key = PrivateDecryptingKey::generate(KeySize::Rsa2048)
            .map_err(|_| CryptoError::KeyGeneration)?;
        let der: PublicKeyX509Der<'static> = key
            .public_key()
            .as_der()
            .map_err(|_| CryptoError::KeyGeneration)?;
        Ok(Self {
            public_der: der.as_ref().to_vec(),
            private: Pkcs1PrivateDecryptingKey::new(key).map_err(|_| CryptoError::KeyGeneration)?,
        })
    }

    /// Public key as X.509 SubjectPublicKeyInfo DER (the `hello` packet field).
    pub fn public_der(&self) -> &[u8] {
        &self.public_der
    }

    pub fn decrypt(&self, ciphertext: &[u8]) -> Result<Vec<u8>, CryptoError> {
        let mut out = vec![0u8; self.private.min_output_size()];
        let plain = self
            .private
            .decrypt(ciphertext, &mut out)
            .map_err(|_| CryptoError::Exchange)?;
        Ok(plain.to_vec())
    }
}

/// Client side of the exchange: encrypts with the server's public key.
pub fn encrypt_with_public(public_der: &[u8], data: &[u8]) -> Result<Vec<u8>, CryptoError> {
    let key = PublicEncryptingKey::from_der(public_der).map_err(|_| CryptoError::Exchange)?;
    let key = Pkcs1PublicEncryptingKey::new(key).map_err(|_| CryptoError::Exchange)?;
    let mut out = vec![0u8; key.ciphertext_size()];
    Ok(key
        .encrypt(data, &mut out)
        .map_err(|_| CryptoError::Exchange)?
        .to_vec())
}

/// AES-128-CFB8 for one direction of a connection.
pub struct Cfb8Encrypt(cfb8::Encryptor<Aes128>);
/// AES-128-CFB8 for the other direction.
pub struct Cfb8Decrypt(cfb8::Decryptor<Aes128>);

impl std::fmt::Debug for Cfb8Encrypt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Cfb8Encrypt")
    }
}

impl std::fmt::Debug for Cfb8Decrypt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Cfb8Decrypt")
    }
}

/// Both directions from the 16-byte shared secret (key and IV).
pub fn cfb8_pair(secret: &[u8]) -> Result<(Cfb8Encrypt, Cfb8Decrypt), CryptoError> {
    if secret.len() != 16 {
        return Err(CryptoError::SecretLength);
    }
    let key = GenericArray::from_slice(secret);
    Ok((
        Cfb8Encrypt(cfb8::Encryptor::new(key, key)),
        Cfb8Decrypt(cfb8::Decryptor::new(key, key)),
    ))
}

impl Cfb8Encrypt {
    pub fn apply(&mut self, data: &mut [u8]) {
        for b in data {
            self.0
                .encrypt_block_mut(GenericArray::from_mut_slice(std::slice::from_mut(b)));
        }
    }
}

impl Cfb8Decrypt {
    pub fn apply(&mut self, data: &mut [u8]) {
        for b in data {
            self.0
                .decrypt_block_mut(GenericArray::from_mut_slice(std::slice::from_mut(b)));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn nist_cfb8_vector() {
        // NIST SP 800-38A, F.3.7 CFB8-AES128.Encrypt (key and IV differ there,
        // so the cipher is built directly).
        let key = [
            0x2b, 0x7e, 0x15, 0x16, 0x28, 0xae, 0xd2, 0xa6, 0xab, 0xf7, 0x15, 0x88, 0x09, 0xcf,
            0x4f, 0x3c,
        ];
        let iv: [u8; 16] = core::array::from_fn(|i| i as u8);
        let mut data = [
            0x6b, 0xc1, 0xbe, 0xe2, 0x2e, 0x40, 0x9f, 0x96, 0xe9, 0x3d, 0x7e, 0x11, 0x73, 0x93,
            0x17, 0x2a, 0xae, 0x2d,
        ];
        let mut enc = Cfb8Encrypt(cfb8::Encryptor::new(
            GenericArray::from_slice(&key),
            GenericArray::from_slice(&iv),
        ));
        enc.apply(&mut data);
        assert_eq!(
            data,
            [
                0x3b, 0x79, 0x42, 0x4c, 0x9c, 0x0d, 0xd4, 0x36, 0xba, 0xce, 0x9e, 0x0e, 0xd4, 0x58,
                0x6a, 0x4f, 0x32, 0xb9
            ]
        );
    }

    #[test]
    fn key_exchange_and_stream() {
        let server = ServerKey::generate().unwrap();
        let secret = [7u8; 16];
        let wire = encrypt_with_public(server.public_der(), &secret).unwrap();
        assert_eq!(wire.len(), 256);
        assert_eq!(server.decrypt(&wire).unwrap(), secret);
        assert_eq!(server.decrypt(&[0u8; 256]), Err(CryptoError::Exchange));
        assert_eq!(server.decrypt(b"short"), Err(CryptoError::Exchange));
        assert!(cfb8_pair(&[0u8; 15]).is_err());
    }

    proptest! {
        #[test]
        fn stream_round_trip_in_any_chunks(data in proptest::collection::vec(any::<u8>(), 0..512),
                                           split in 0usize..512) {
            let (mut enc, _) = cfb8_pair(&[3u8; 16]).unwrap();
            let (_, mut dec) = cfb8_pair(&[3u8; 16]).unwrap();
            let mut buf = data.clone();
            let cut = split.min(buf.len());
            let (a, b) = buf.split_at_mut(cut);
            enc.apply(a);
            enc.apply(b);
            dec.apply(&mut buf);
            prop_assert_eq!(buf, data);
        }
    }
}
