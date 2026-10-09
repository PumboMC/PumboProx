//! Key exchange as in the Minecraft login: the server sends its public key
//! (X.509 SubjectPublicKeyInfo in DER), the client encrypts the 16-byte shared
//! secret and the token with RSA PKCS#1 v1.5, the server decrypts.

use aws_lc_rs::encoding::{AsDer, PublicKeyX509Der};
use aws_lc_rs::error::Unspecified;
use aws_lc_rs::rsa::{
    KeySize, Pkcs1PrivateDecryptingKey, Pkcs1PublicEncryptingKey, PrivateDecryptingKey,
    PublicEncryptingKey,
};

/// Server (proxy) key. aws-lc-rs accepts only 2048–8192-bit keys.
pub struct ServerKey {
    private: Pkcs1PrivateDecryptingKey,
    public_der: Vec<u8>,
}

impl ServerKey {
    pub fn generate() -> Result<Self, Unspecified> {
        let key = PrivateDecryptingKey::generate(KeySize::Rsa2048)?;
        let public = key.public_key();
        let der: PublicKeyX509Der<'static> = public.as_der()?;
        Ok(Self {
            private: Pkcs1PrivateDecryptingKey::new(key)?,
            public_der: der.as_ref().to_vec(),
        })
    }

    /// Public key in the format of the Encryption Request `public_key` field.
    pub fn public_der(&self) -> &[u8] {
        &self.public_der
    }

    pub fn key_bits(&self) -> usize {
        self.private.key_size_bits()
    }

    pub fn decrypt(&self, ciphertext: &[u8]) -> Result<Vec<u8>, Unspecified> {
        let mut out = vec![0u8; self.private.min_output_size()];
        let plain = self.private.decrypt(ciphertext, &mut out)?;
        Ok(plain.to_vec())
    }
}

/// Client side: encryption with the public key from the packet.
pub fn client_encrypt(public_der: &[u8], data: &[u8]) -> Result<Vec<u8>, Unspecified> {
    let key = PublicEncryptingKey::from_der(public_der).map_err(|_| Unspecified)?;
    let key = Pkcs1PublicEncryptingKey::new(key)?;
    let mut out = vec![0u8; key.ciphertext_size()];
    let cipher = key.encrypt(data, &mut out)?;
    Ok(cipher.to_vec())
}
