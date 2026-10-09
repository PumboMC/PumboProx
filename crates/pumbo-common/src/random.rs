//! Randomness: OS entropy for secrets and a small deterministic generator for
//! things that must be reproducible in tests (CAPTCHA images, layouts).

use rand_core::{OsRng, RngCore};

use crate::clock::now_ms;

/// Fills `buf` with OS randomness. Falls back to a time-seeded generator so that
/// a failing entropy source never aborts the plugin.
pub fn random_bytes(buf: &mut [u8]) {
    if OsRng.try_fill_bytes(buf).is_ok() {
        return;
    }
    let mut rng = Rng::new(now_ms() ^ 0x9E37_79B9_7F4A_7C15);
    for b in buf.iter_mut() {
        *b = (rng.next_u32() & 0xFF) as u8;
    }
}

pub fn random_u64() -> u64 {
    let mut b = [0u8; 8];
    random_bytes(&mut b);
    u64::from_le_bytes(b)
}

/// Small deterministic PRNG (SplitMix64). Not for secrets: use [`random_bytes`].
#[derive(Clone, Debug)]
pub struct Rng {
    state: u64,
}

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    pub fn next_u32(&mut self) -> u32 {
        (self.next_u64() >> 32) as u32
    }

    /// Uniform value in `[0, n)`; returns 0 when `n == 0`.
    pub fn below(&mut self, n: u32) -> u32 {
        if n == 0 { 0 } else { self.next_u32() % n }
    }

    /// Uniform float in `[0, 1)`.
    pub fn unit(&mut self) -> f32 {
        (self.next_u32() >> 8) as f32 / (1u32 << 24) as f32
    }

    /// Uniform float in `[lo, hi)`.
    pub fn range(&mut self, lo: f32, hi: f32) -> f32 {
        lo + (hi - lo) * self.unit()
    }

    pub fn pick<'a, T>(&mut self, items: &'a [T]) -> Option<&'a T> {
        let len = u32::try_from(items.len()).unwrap_or(u32::MAX);
        items.get(self.below(len) as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rng_is_deterministic() {
        let mut a = Rng::new(7);
        let mut b = Rng::new(7);
        for _ in 0..10 {
            assert_eq!(a.next_u64(), b.next_u64());
        }
        assert!(Rng::new(1).unit() < 1.0);
        assert_eq!(Rng::new(1).below(0), 0);
        assert!(Rng::new(1).pick::<u8>(&[]).is_none());
        let x = Rng::new(3).range(2.0, 3.0);
        assert!((2.0..3.0).contains(&x));
    }

    #[test]
    fn os_randomness_differs() {
        let mut a = [0u8; 16];
        let mut b = [0u8; 16];
        random_bytes(&mut a);
        random_bytes(&mut b);
        assert_ne!(a, b);
        assert_ne!(random_u64(), random_u64());
    }
}
