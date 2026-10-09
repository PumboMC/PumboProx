//! Run on every target: checks that aws-lc-rs works and measures decryption
//! time (the RSA cost of one online-mode login).

use std::process::ExitCode;
use std::time::Instant;

fn main() -> ExitCode {
    let Ok(key) = spike_rsa::ServerKey::generate() else {
        eprintln!("key generation failed");
        return ExitCode::FAILURE;
    };
    let secret = [7u8; 16];
    let Ok(cipher) = spike_rsa::client_encrypt(key.public_der(), &secret) else {
        eprintln!("encryption failed");
        return ExitCode::FAILURE;
    };
    let rounds = 500u32;
    let started = Instant::now();
    for _ in 0..rounds {
        match key.decrypt(&cipher) {
            Ok(plain) if plain == secret => {}
            _ => {
                eprintln!("decryption failed");
                return ExitCode::FAILURE;
            }
        }
    }
    let per = started.elapsed() / rounds;
    println!(
        "{} {}: RSA-{} PKCS#1 v1.5, public key {} B DER, ciphertext {} B, decryption {:?}",
        std::env::consts::OS,
        std::env::consts::ARCH,
        key.key_bits(),
        key.public_der().len(),
        cipher.len(),
        per
    );
    ExitCode::SUCCESS
}
