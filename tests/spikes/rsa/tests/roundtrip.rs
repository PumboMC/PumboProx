use spike_rsa::{ServerKey, client_encrypt};

#[test]
fn secret_exchange() {
    let key = ServerKey::generate().unwrap();
    assert_eq!(key.key_bits(), 2048);
    // SubjectPublicKeyInfo starts with a SEQUENCE.
    assert_eq!(key.public_der().first(), Some(&0x30));
    let secret: Vec<u8> = (0u8..16).collect();
    let token = [1u8, 2, 3, 4];
    let c1 = client_encrypt(key.public_der(), &secret).unwrap();
    let c2 = client_encrypt(key.public_der(), &token).unwrap();
    assert_eq!(c1.len(), 256);
    assert_eq!(key.decrypt(&c1).unwrap(), secret);
    assert_eq!(key.decrypt(&c2).unwrap(), token);
}

#[test]
fn bad_ciphertext_rejected() {
    let key = ServerKey::generate().unwrap();
    let mut c = client_encrypt(key.public_der(), &[9u8; 16]).unwrap();
    c[100] ^= 0xFF;
    assert!(key.decrypt(&c).is_err());
    assert!(key.decrypt(&[0u8; 10]).is_err());
}
