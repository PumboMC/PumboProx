//! Shared harness: decode a payload as a packet of one phase in any supported
//! version and direction; whatever decodes must encode, decode again and
//! encode to the same bytes.

use pumbo_data::DataVersion;
use pumbo_protocol::packets::{Ctx, DECODED, decode_any, encode_any};
use pumbo_protocol::{Direction, Phase, ProtocolVersion};

/// Input layout: version selector, direction bit, packet selector, payload.
pub fn packet(phases: &[Phase], data: &[u8]) {
    let [v, d, k, payload @ ..] = data else {
        return;
    };
    let versions: Vec<ProtocolVersion> = pumbo_data::protocols().collect();
    let Some(&version) = versions.get(usize::from(*v) % versions.len().max(1)) else {
        return;
    };
    let Ok(tables) = pumbo_data::tables(version) else {
        return;
    };
    let module = DataVersion::new(tables);
    let direction = if d & 1 == 0 {
        Direction::Clientbound
    } else {
        Direction::Serverbound
    };
    let candidates: Vec<_> = DECODED
        .iter()
        .filter(|(p, dir, _)| phases.contains(p) && *dir == direction)
        .collect();
    let Some(&&(phase, _, kind)) = candidates.get(usize::from(*k) % candidates.len().max(1)) else {
        return;
    };
    let ctx = Ctx::new(&module, direction);
    // What decodes must encode, and one decode/encode cycle must be stable
    // (byte-wise: NaN floats and non-canonical strings make value equality
    // the wrong check).
    if let Ok(Some(p)) = decode_any(phase, direction, kind, payload, &ctx) {
        let bytes = encode_any(&p, &ctx).expect("a decoded packet must encode");
        let again = decode_any(phase, direction, kind, &bytes, &ctx)
            .expect("re-encoded packet must decode")
            .expect("same kind");
        let bytes2 = encode_any(&again, &ctx).expect("must encode again");
        assert_eq!(bytes2, bytes, "encoding is not stable");
    }
}
