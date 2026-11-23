//! Status pings to servers: a handshake for status and a status request.

use bytes::BytesMut;
use pumbo_protocol::frame::{self, FrameConfig};
use pumbo_protocol::packets::status::{Intention, StatusRequest};
use pumbo_protocol::packets::{self, Ctx, Packet};
use pumbo_protocol::{Direction, Phase, VersionModule};

/// `intention` (status, to `host`, protocol `ask`) and `status_request` as
/// uncompressed frames.
pub(crate) fn frames(
    module: &dyn VersionModule,
    ask: i32,
    host: &str,
    port: u16,
) -> Result<BytesMut, String> {
    fn put<P: Packet>(
        out: &mut BytesMut,
        ctx: &Ctx<'_>,
        phase: Phase,
        p: &P,
    ) -> Result<(), String> {
        let id = packets::id_of::<P>(ctx, phase, Direction::Serverbound)
            .ok_or_else(|| format!("{:?} has no {:?}", ctx.protocol(), P::KIND))?;
        let payload = packets::encode(p, ctx).map_err(|e| e.to_string())?;
        frame::encode(out, id, &payload, &FrameConfig::from_client()).map_err(|e| e.to_string())
    }
    let ctx = Ctx::new(module, Direction::Serverbound);
    let intention = Intention {
        protocol: ask,
        address: host.to_string(),
        port,
        intent: Intention::STATUS,
    };
    let mut out = BytesMut::new();
    put(&mut out, &ctx, Phase::Handshake, &intention)?;
    put(&mut out, &ctx, Phase::Status, &StatusRequest)?;
    Ok(out)
}
