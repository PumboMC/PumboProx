//! Pumpkin 0.1 and 0.2 send an eyeblossom's `trail` particle without its
//! options (Pumpkin issue #3065), and no client can decode that.

use pumbo_core::engine::{Filtered, PacketFilter, Repair};
use pumbo_protocol::packets::particles::add_trail_options;
use pumbo_protocol::{PacketKind, Phase, ProtocolVersion, RawFrame};

pub(crate) static TRAIL: Repair = Repair {
    kind: "trail_particle",
    what: "a malformed trail particle",
    cause: "Pumpkin issue #3065",
};

/// Gives such a particle vanilla's options; correct packets pass unchanged,
/// so the repair stops by itself once the server is fixed.
pub(crate) struct TrailFix {
    trail: i32,
    particle_first: bool,
}

impl TrailFix {
    /// 26.2 and 26.3, the versions of Pumpkin 0.1 and 0.2; a later Pumpkin
    /// with the same fault needs its protocol here.
    pub(crate) fn for_version(wire: ProtocolVersion) -> Option<Self> {
        use ProtocolVersion as V;
        if wire != V::V776 && wire != V::V777 {
            return None;
        }
        let id = pumbo_data::tables(wire)
            .ok()?
            .registry("particle_type")?
            .id("trail")?;
        Some(Self {
            trail: i32::try_from(id).ok()?,
            // 26.3 moved the particle to the front.
            particle_first: wire >= V::V777,
        })
    }
}

impl PacketFilter for TrailFix {
    fn interests(&self) -> &'static [PacketKind] {
        &[PacketKind::LevelParticles]
    }

    fn backend_to_client(&mut self, _: Phase, _: PacketKind, f: &RawFrame) -> Filtered {
        match add_trail_options(&f.payload, self.trail, self.particle_first) {
            Some(payload) => Filtered::Replace {
                payload,
                repair: &TRAIL,
            },
            None => Filtered::Forward,
        }
    }
}

#[cfg(test)]
mod tests {
    use pumbo_core::engine::{EngineAdapter, EngineRef};
    use pumbo_protocol::types::WriteExt;

    use super::*;
    use crate::pumpkin::PumpkinAdapter;

    fn quirks(v: ProtocolVersion) -> Option<Box<dyn PacketFilter>> {
        let pumpkin = EngineRef {
            name: crate::PUMPKIN.into(),
            version: None,
        };
        PumpkinAdapter.quirks(&pumpkin, v)
    }

    /// A `level_particles` frame with `particle` and its `options`, in the
    /// 26.3 layout (`particle_first`) or 26.2's.
    fn frame(particle_first: bool, particle: i32, options: &[u8]) -> RawFrame {
        let mut p = Vec::new();
        if particle_first {
            p.put_varint(particle);
            p.put_bytes(options);
        }
        p.put_bytes(&[0, 0]);
        [10.5, 64.5, -3.5].iter().for_each(|v| p.put_f64(*v));
        if particle_first {
            (0..6).for_each(|_| p.put_f32(0.0));
            p.put_varint(1);
            p.put_varint(0);
        } else {
            (0..4).for_each(|_| p.put_f32(0.0));
            p.put_i32(1);
            p.put_varint(particle);
            p.put_bytes(options);
        }
        RawFrame {
            id: 0,
            payload: p.into(),
        }
    }

    fn trail(v: ProtocolVersion) -> i32 {
        let tables = pumbo_data::tables(v).unwrap();
        let id = tables
            .registry("particle_type")
            .unwrap()
            .id("trail")
            .unwrap();
        i32::try_from(id).unwrap()
    }

    #[test]
    fn repairs_only_for_26_2_and_26_3() {
        for v in [ProtocolVersion::V775, ProtocolVersion::V767] {
            assert!(quirks(v).is_none(), "{v:?}");
        }
        for v in [ProtocolVersion::V776, ProtocolVersion::V777] {
            let filter = quirks(v).unwrap();
            assert_eq!(filter.interests(), [PacketKind::LevelParticles], "{v:?}");
        }
    }

    #[test]
    fn the_filter_repairs_in_the_layout_of_its_version() {
        let mut options = Vec::new();
        [10.5, 66.0, -3.5].iter().for_each(|v| options.put_f64(*v));
        options.put_i32(0x00FC_7812);
        options.put_varint(20);
        let (pl, kind) = (Phase::Play, PacketKind::LevelParticles);
        for (v, first) in [
            (ProtocolVersion::V776, false),
            (ProtocolVersion::V777, true),
        ] {
            let mut filter = quirks(v).unwrap();
            let t = trail(v);
            let repaired = filter.backend_to_client(pl, kind, &frame(first, t, &[]));
            let want = Filtered::Replace {
                payload: frame(first, t, &options).payload.to_vec(),
                repair: &TRAIL,
            };
            assert_eq!(repaired, want, "{v:?}");
            let other = filter.backend_to_client(pl, kind, &frame(!first, t, &[]));
            assert_eq!(other, Filtered::Forward, "{v:?}");
        }
    }

    #[test]
    fn the_repair_keeps_its_metric_label_and_log_words() {
        assert_eq!(TRAIL.kind, "trail_particle");
        assert_eq!(TRAIL.what, "a malformed trail particle");
        assert_eq!(TRAIL.cause, "Pumpkin issue #3065");
    }
}
