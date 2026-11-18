//! The session's hooks and the table of what they want: a frame that no
//! hook asked for costs one look into a bit mask.

use std::borrow::Cow;

use pumbo_core::hook::{HookCx, HookOut, SessionHook, Verdict};
use pumbo_core::profile::ForwardedPlayer;
use pumbo_protocol::packets::common::ServerboundCustomPayload;
use pumbo_protocol::types::Reader;
use pumbo_protocol::{Direction, KNOWN_PACKETS, PacketKind, Phase, RawFrame, VersionModule};
use tracing::debug;

use crate::session::{Fail, queue};

use super::{Backend, Play, Step};

/// One bit per packet kind.
type Kinds = [u64; PacketKind::ALL.len().div_ceil(64)];

// The masks take a kind's discriminant for its bit: it must be its place in
// `PacketKind::ALL`.
#[allow(clippy::indexing_slicing)]
const _: () = {
    let mut i = 0;
    while i < PacketKind::ALL.len() {
        assert!(
            PacketKind::ALL[i] as usize == i,
            "PacketKind::ALL is out of order"
        );
        i += 1;
    }
};

fn kinds(list: &[PacketKind]) -> Kinds {
    let mut m = Kinds::default();
    for k in list {
        let i = *k as usize;
        if let Some(w) = m.get_mut(i >> 6) {
            *w |= 1 << (i & 63);
        }
    }
    m
}

#[inline]
fn has(m: &Kinds, k: PacketKind) -> bool {
    let i = k as usize;
    m.get(i >> 6).is_some_and(|w| w >> (i & 63) & 1 != 0)
}

/// Whether every kind of `list` can reach a hook in direction `d`: the proxy
/// reads it once the player is past the login, and it is not the server's
/// `login` (that reaches hooks as `on_joined`) or `disconnect` (never).
fn known(d: Direction, list: &[PacketKind]) -> bool {
    list.iter().all(|k| {
        let core =
            d == Direction::Clientbound && matches!(k, PacketKind::Login | PacketKind::Disconnect);
        !core
            && KNOWN_PACKETS.iter().any(|(phase, dir, kind)| {
                matches!(phase, Phase::Configuration | Phase::Play) && *dir == d && kind == k
            })
    })
}

/// What reaches the hooks.
#[derive(Clone, Copy)]
enum Call<'a> {
    /// A frame of this kind from the client.
    Client(PacketKind),
    /// A frame of this kind from the server.
    Backend(PacketKind),
    /// A plugin message on this channel from the server.
    Channel(&'a str),
}

struct Entry {
    hook: Box<dyn SessionHook>,
    client: Kinds,
    backend: Kinds,
    channels: Cow<'static, [Cow<'static, str>]>,
}

impl Entry {
    fn wants(&self, call: Call<'_>, f: &RawFrame) -> bool {
        match call {
            Call::Client(k) => has(&self.client, k),
            // A message on one of its channels reached it as `Channel` already.
            Call::Backend(k) => {
                has(&self.backend, k) && !(k == PacketKind::CustomPayload && self.on_channel(f))
            }
            Call::Channel(c) => self.channels.iter().any(|w| w == c),
        }
    }

    fn on_channel(&self, f: &RawFrame) -> bool {
        !self.channels.is_empty()
            && Reader::new(&f.payload)
                .identifier()
                .is_ok_and(|c| self.channels.iter().any(|w| *w == c))
    }
}

/// What a hook sees of the session, besides its output.
struct Cx<'a> {
    player: &'a ForwardedPlayer,
    module: &'a dyn VersionModule,
    server: Option<&'a str>,
    phase: Phase,
}

impl Cx<'_> {
    fn with<'b>(&'b self, out: &'b mut HookOut) -> HookCx<'b> {
        HookCx {
            player: self.player,
            module: self.module,
            server: self.server,
            phase: self.phase,
            out,
        }
    }
}

/// The session as its hooks see it for a frame in `phase`: the server counts
/// once its play `login` arrived.
fn cx_of<'a>(
    player: &'a ForwardedPlayer,
    module: &'a dyn VersionModule,
    backend: Option<&'a Backend>,
    phase: Phase,
) -> Cx<'a> {
    let server = backend.filter(|b| b.joined).map(|b| b.name.as_str());
    Cx {
        player,
        module,
        server,
        phase,
    }
}

/// The hooks of one session, in calling order, and what they want.
pub(super) struct Hooks {
    list: Vec<Entry>,
    client: Kinds,
    backend: Kinds,
    channels: bool,
    out: HookOut,
}

impl Hooks {
    pub(super) fn new(hooks: Vec<Box<dyn SessionHook>>) -> Self {
        let mut t = Hooks {
            list: Vec::with_capacity(hooks.len()),
            client: Kinds::default(),
            backend: Kinds::default(),
            channels: false,
            out: HookOut::default(),
        };
        for hook in hooks {
            let wants = hook.interests();
            debug_assert!(
                known(Direction::Serverbound, wants.client)
                    && known(Direction::Clientbound, wants.backend),
                "a session hook wants a packet that never reaches hooks"
            );
            let e = Entry {
                hook,
                client: kinds(wants.client),
                backend: kinds(wants.backend),
                channels: wants.backend_channels,
            };
            for (all, w) in t.client.iter_mut().zip(e.client) {
                *all |= w;
            }
            for (all, w) in t.backend.iter_mut().zip(e.backend) {
                *all |= w;
            }
            t.channels |= !e.channels.is_empty();
            t.list.push(e);
        }
        t
    }

    #[inline]
    pub(super) fn wants_client(&self, kind: Option<PacketKind>) -> bool {
        kind.is_some_and(|k| has(&self.client, k))
    }

    #[inline]
    pub(super) fn wants_backend(&self, kind: Option<PacketKind>) -> bool {
        kind.is_some_and(|k| has(&self.backend, k))
    }

    pub(super) fn wants_channel(&self, channel: &str) -> bool {
        self.channels
            && self
                .list
                .iter()
                .any(|e| e.channels.iter().any(|w| w == channel))
    }

    /// The hooks that want `call`, in order, until one decides otherwise
    /// than `Forward`.
    fn call(&mut self, cx: &Cx<'_>, call: Call<'_>, f: &RawFrame) -> Verdict {
        let Hooks { list, out, .. } = self;
        for e in list.iter_mut().filter(|e| e.wants(call, f)) {
            let hcx = &mut cx.with(out);
            let v = match call {
                Call::Client(_) => e.hook.on_client(hcx, f),
                Call::Backend(_) | Call::Channel(_) => e.hook.on_backend(hcx, f),
            };
            if v != Verdict::Forward {
                return v;
            }
        }
        Verdict::Forward
    }

    fn joined(&mut self, cx: &Cx<'_>) {
        let Hooks { list, out, .. } = self;
        for e in list.iter_mut() {
            e.hook.on_joined(&mut cx.with(out));
        }
    }
}

impl Play<'_> {
    /// The hooks that want this frame of the client, which the proxy lets
    /// through.
    pub(super) fn client_hooks(
        &mut self,
        phase: Phase,
        kind: Option<PacketKind>,
        f: &RawFrame,
    ) -> Result<Step, Fail> {
        kind.map_or(Ok(Step::Forward), |k| {
            self.run_hooks(phase, Call::Client(k), f)
        })
    }

    /// The hooks that want this frame of the server, which the proxy lets
    /// through.
    pub(super) fn backend_hooks(
        &mut self,
        phase: Phase,
        kind: Option<PacketKind>,
        f: &RawFrame,
    ) -> Result<Step, Fail> {
        kind.map_or(Ok(Step::Forward), |k| {
            self.run_hooks(phase, Call::Backend(k), f)
        })
    }

    /// The hooks that want this plugin message channel of the server, in
    /// any phase.
    pub(super) fn channel_hooks(
        &mut self,
        phase: Phase,
        channel: &str,
        f: &RawFrame,
    ) -> Result<Step, Fail> {
        self.run_hooks(phase, Call::Channel(channel), f)
    }

    fn run_hooks(&mut self, phase: Phase, call: Call<'_>, f: &RawFrame) -> Result<Step, Fail> {
        let cx = cx_of(&self.player, self.module, self.backend.as_ref(), phase);
        let v = self.hooks.call(&cx, call, f);
        self.hook_out();
        match v {
            Verdict::Forward => Ok(Step::Forward),
            Verdict::Drop => Ok(Step::Drop),
            Verdict::End(why) => Err(Fail::Protocol(why)),
        }
    }

    /// The server's play `login` reached the client; what the hooks ask for
    /// waits for [`Play::hook_out`].
    pub(super) fn joined_hooks(&mut self) {
        let cx = cx_of(
            &self.player,
            self.module,
            self.backend.as_ref(),
            Phase::Play,
        );
        self.hooks.joined(&cx);
    }

    /// What the hooks asked for: plugin messages to the current server, then
    /// a move to another one. The messages are encoded in the phase of the
    /// server's stream to the client, which matches the stream to the server
    /// for the messaging channel, the only one that writes today (it answers
    /// in play). A hook that writes from `on_client` while the phases change
    /// needs the phase of the client's stream here instead.
    pub(super) fn hook_out(&mut self) {
        let HookOut {
            to_backend,
            connect,
        } = std::mem::take(&mut self.hooks.out);
        let name = &self.player.profile.name;
        if let Some(b) = self.backend.as_mut() {
            for (channel, data) in to_backend {
                let p = ServerboundCustomPayload {
                    channel: channel.into_owned(),
                    data,
                };
                if let Err(e) = queue(
                    &mut b.conn,
                    self.module,
                    b.phase,
                    Direction::Serverbound,
                    &p,
                ) {
                    debug!("{name}: plugin message of a session hook: {e:?}");
                }
            }
        }
        if let Some(server) = connect
            && let Err(msg) = self.switch_to(&server, true)
        {
            debug!(
                "{}: connect to {server} for a session hook: {msg}",
                self.player.profile.name
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use pumbo_core::hook::HookInterests;

    use super::*;

    struct Wants(HookInterests);

    impl SessionHook for Wants {
        fn interests(&self) -> HookInterests {
            self.0.clone()
        }
    }

    #[test]
    fn the_table_holds_what_the_hooks_want() {
        let none = Hooks::new(Vec::new());
        assert!(!none.wants_client(Some(PacketKind::ChatAck)));
        assert!(!none.wants_backend(None));
        assert!(!none.wants_channel("pumbo:test"));
        let last = PacketKind::ALL.last().copied().unwrap();
        let t = Hooks::new(vec![Box::new(Wants(HookInterests {
            client: &[PacketKind::ClientTickEnd],
            backend: &[PacketKind::BossEvent, PacketKind::Sound],
            backend_channels: Cow::Borrowed(&[Cow::Borrowed("pumbo:test")]),
        }))]);
        assert!(t.wants_client(Some(PacketKind::ClientTickEnd)));
        assert!(!t.wants_client(Some(PacketKind::Sound)));
        assert!(!t.wants_client(None));
        assert!(t.wants_backend(Some(PacketKind::Sound)));
        assert!(t.wants_backend(Some(PacketKind::BossEvent)));
        assert!(!t.wants_backend(Some(last)));
        assert!(t.wants_channel("pumbo:test") && !t.wants_channel("pumbo:other"));
        assert!(has(&kinds(&[last]), last), "the last kind has its bit");
    }

    /// The proxy maps ids only for the packets it reads, and the server's
    /// `login` and `disconnect` stay with the session; a hook on any of them
    /// would never run.
    #[test]
    fn hooks_want_packets_that_reach_them() {
        assert!(known(
            Direction::Serverbound,
            &[PacketKind::AcceptTeleportation]
        ));
        assert!(known(Direction::Clientbound, &[PacketKind::PlayerPosition]));
        assert!(!known(
            Direction::Clientbound,
            &[PacketKind::AcceptTeleportation]
        ));
        assert!(!known(Direction::Serverbound, &[PacketKind::Intention]));
        assert!(!known(
            Direction::Serverbound,
            &[PacketKind::ChatAck, PacketKind::Hello]
        ));
        assert!(!known(Direction::Clientbound, &[PacketKind::Login]));
        assert!(!known(Direction::Clientbound, &[PacketKind::Disconnect]));
    }

    /// The table in a session and its heap part stay small: the per-session
    /// cost of the hooks.
    #[test]
    fn a_session_table_is_small() {
        let (table, entry) = (size_of::<Hooks>(), size_of::<Entry>());
        println!("Hooks {table} B in the session, {entry} B per hook on the heap");
        assert!(table <= 128 && entry <= 96, "{table} {entry}");
    }
}
