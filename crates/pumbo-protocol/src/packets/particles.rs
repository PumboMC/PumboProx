//! `level_particles` payloads repaired without decoding the whole packet.

use crate::types::{Reader, WriteExt};

/// Pumpkin 0.1 and 0.2 send the `trail` particle of an eyeblossom without its
/// options (Pumpkin issue #3065), and no client can decode that. Returns such a
/// `level_particles` payload with vanilla's options added, `None` for any other
/// payload (correct ones too), so the repair stops by itself once Pumpkin is
/// fixed. `trail` is the particle's id in the payload's version;
/// `particle_first`: the 26.3 layout (particle before the position),
/// otherwise 26.2's (particle last).
pub fn add_trail_options(payload: &[u8], trail: i32, particle_first: bool) -> Option<Vec<u8>> {
    let mut r = Reader::new(payload);
    let split = if particle_first {
        if r.varint().ok()? != trail {
            return None;
        }
        let split = payload.len() - r.remaining();
        // Without options only the flags, position, offset and speeds (50
        // bytes), the count and the randomization type follow.
        r.take(50).ok()?;
        r.varint().ok()?;
        r.varint().ok()?;
        split
    } else {
        // Flags, position, offset, speed and count (46 bytes), then the particle.
        r.take(46).ok()?;
        if r.varint().ok()? != trail {
            return None;
        }
        payload.len()
    };
    if !r.is_empty() {
        return None;
    }
    let (head, tail) = payload.split_at_checked(split)?;
    let mut position = Reader::new((if particle_first { tail } else { payload }).get(2..)?);
    let (x, y, z) = (
        position.f64().ok()?,
        position.f64().ok()?,
        position.f64().ok()?,
    );
    let mut out = Vec::with_capacity(payload.len() + 29);
    out.put_bytes(head);
    // Vanilla's eyeblossom trail (`EyeblossomBlock.Type.spawnTransformParticle`)
    // at its mean: target 1.5 blocks up, 20 ticks. The packet does not tell
    // opening from closing, so it takes the opening color (closing: 0x5F5F5F).
    out.put_f64(x);
    out.put_f64(y + 1.5);
    out.put_f64(z);
    out.put_i32(0x00FC_7812);
    out.put_varint(20);
    out.put_bytes(tail);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TRAIL: i32 = 59;
    const FLAME: i32 = 3;

    /// What Pumpkin 0.2.0 sends for an eyeblossom (26.3 layout): the particle
    /// first, then flags, block center, zero offset and speeds, count 1.
    fn level_particles_26_3(particle: i32, options: &[u8]) -> Vec<u8> {
        let mut p = Vec::new();
        p.put_varint(particle);
        p.put_bytes(options);
        p.put_bytes(&[0, 0]);
        [10.5, 64.5, -3.5].iter().for_each(|v| p.put_f64(*v));
        (0..6).for_each(|_| p.put_f32(0.0));
        p.put_varint(1);
        p.put_varint(0);
        p
    }

    fn level_particles_26_2(particle: i32, options: &[u8]) -> Vec<u8> {
        let mut p = vec![0, 0];
        [10.5, 64.5, -3.5].iter().for_each(|v| p.put_f64(*v));
        (0..4).for_each(|_| p.put_f32(0.0));
        p.put_i32(1);
        p.put_varint(particle);
        p.put_bytes(options);
        p
    }

    fn trail_options(target: [f64; 3], color: i32, duration: i32) -> Vec<u8> {
        let mut o = Vec::new();
        target.iter().for_each(|v| o.put_f64(*v));
        o.put_i32(color);
        o.put_varint(duration);
        o
    }

    #[test]
    fn empty_trail_gets_vanilla_eyeblossom_options() {
        // The same options as a correct packet (what a fixed Pumpkin sends):
        // the target 1.5 blocks above the position, the opening color, 20 ticks.
        let options = trail_options([10.5, 66.0, -3.5], 0xFC7812, 20);
        let fixed = add_trail_options(&level_particles_26_3(TRAIL, &[]), TRAIL, true).unwrap();
        assert_eq!(fixed, level_particles_26_3(TRAIL, &options));

        let fixed = add_trail_options(&level_particles_26_2(TRAIL, &[]), TRAIL, false).unwrap();
        assert_eq!(fixed, level_particles_26_2(TRAIL, &options));
    }

    #[test]
    fn correct_packets_and_other_particles_stay_as_they_are() {
        let options = trail_options([1.0, 2.0, 3.0], 0x5F5F5F, 7);
        for (packet, first) in [
            (level_particles_26_3(TRAIL, &options), true),
            (level_particles_26_3(FLAME, &[]), true),
            (level_particles_26_2(TRAIL, &options), false),
            (level_particles_26_2(FLAME, &[]), false),
            // A 26.2 packet read as 26.3 and the other way round.
            (level_particles_26_2(TRAIL, &[]), true),
            (level_particles_26_3(TRAIL, &[]), false),
        ] {
            assert_eq!(
                add_trail_options(&packet, TRAIL, first),
                None,
                "{packet:02x?}"
            );
        }
    }

    #[test]
    fn cut_or_garbage_packets_are_left_alone() {
        for (packet, first) in [
            (level_particles_26_3(TRAIL, &[]), true),
            (level_particles_26_2(TRAIL, &[]), false),
        ] {
            for n in 0..packet.len() {
                assert_eq!(add_trail_options(&packet[..n], TRAIL, first), None, "{n}");
            }
        }
        let mut seed = 0x2545_F491_4F6C_DD1D_u64;
        for len in 0..200 {
            let junk: Vec<u8> = (0..len)
                .map(|_| {
                    seed ^= seed << 13;
                    seed ^= seed >> 7;
                    seed ^= seed << 17;
                    seed as u8
                })
                .collect();
            for first in [true, false] {
                let _ = add_trail_options(&junk, TRAIL, first);
            }
        }
    }
}
