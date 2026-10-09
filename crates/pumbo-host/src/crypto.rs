//! Password hashing for plugins on the blocking thread pool (plan §4.3):
//! argon2 in WASM is several times slower and would block the whole
//! single-threaded instance.

use argon2::password_hash::rand_core::OsRng;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::{Algorithm, Argon2, Params, Version};
use tokio::sync::Semaphore;

use crate::wit::crypto::Argon2Params;

/// Upper bounds, so one plugin cannot take the host's memory or threads.
pub const MAX_MEMORY_KIB: u32 = 128 * 1024;
pub const MAX_ITERATIONS: u32 = 10;
pub const MAX_PARALLELISM: u32 = 4;
pub const MAX_PASSWORD: usize = 1024;

pub fn check_params(p: &Argon2Params) -> Result<Params, String> {
    if p.memory_kib > MAX_MEMORY_KIB
        || p.iterations > MAX_ITERATIONS
        || p.parallelism > MAX_PARALLELISM
    {
        return Err(format!(
            "argon2 parameters above the host limits (memory {MAX_MEMORY_KIB} KiB, {MAX_ITERATIONS} iterations, parallelism {MAX_PARALLELISM})"
        ));
    }
    Params::new(p.memory_kib, p.iterations, p.parallelism, None).map_err(|e| e.to_string())
}

async fn blocking<T: Send + 'static>(
    limit: &Semaphore,
    f: impl FnOnce() -> T + Send + 'static,
) -> Option<T> {
    let _permit = limit.acquire().await.ok()?;
    tokio::task::spawn_blocking(f).await.ok()
}

pub async fn argon2id_hash(
    limit: &Semaphore,
    password: String,
    params: Argon2Params,
) -> Result<String, String> {
    if password.len() > MAX_PASSWORD {
        return Err("password too long".into());
    }
    let params = check_params(&params)?;
    blocking(limit, move || {
        let salt = SaltString::generate(&mut OsRng);
        Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
            .hash_password(password.as_bytes(), &salt)
            .map(|h| h.to_string())
            .map_err(|e| e.to_string())
    })
    .await
    .unwrap_or_else(|| Err("hashing task failed".into()))
}

pub async fn argon2id_verify(limit: &Semaphore, password: String, phc: String) -> bool {
    if password.len() > MAX_PASSWORD || phc.len() > 512 {
        return false;
    }
    blocking(limit, move || {
        let Ok(hash) = PasswordHash::new(&phc) else {
            return false;
        };
        // Parameters from the stored hash, but never above the host limits.
        let Ok(params) = Params::try_from(&hash) else {
            return false;
        };
        if params.m_cost() > MAX_MEMORY_KIB
            || params.t_cost() > MAX_ITERATIONS
            || params.p_cost() > MAX_PARALLELISM
        {
            return false;
        }
        Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
            .verify_password(password.as_bytes(), &hash)
            .is_ok()
    })
    .await
    .unwrap_or(false)
}

pub async fn bcrypt_verify(limit: &Semaphore, password: String, hash: String) -> bool {
    if password.len() > MAX_PASSWORD || hash.len() > 128 {
        return false;
    }
    blocking(limit, move || {
        bcrypt::verify(password, &hash).unwrap_or(false)
    })
    .await
    .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn hash_verify_and_limits() {
        let limit = Semaphore::new(2);
        let p = Argon2Params {
            memory_kib: 8192,
            iterations: 2,
            parallelism: 1,
        };
        let phc = argon2id_hash(&limit, "secret".into(), p).await.unwrap();
        assert!(phc.starts_with("$argon2id$"));
        assert!(argon2id_verify(&limit, "secret".into(), phc.clone()).await);
        assert!(!argon2id_verify(&limit, "wrong".into(), phc).await);
        let too_big = Argon2Params {
            memory_kib: MAX_MEMORY_KIB + 1,
            iterations: 1,
            parallelism: 1,
        };
        assert!(argon2id_hash(&limit, "x".into(), too_big).await.is_err());
        let bc = bcrypt::hash("pw", 4).unwrap();
        assert!(bcrypt_verify(&limit, "pw".into(), bc.clone()).await);
        assert!(!bcrypt_verify(&limit, "nope".into(), bc).await);
    }
}
