//! Protocol versions as pluggable modules.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

use crate::{Direction, PacketKind, Phase, VersionFeatures};

/// Protocol number. Deliberately not an `enum`: a new version does not change
/// the type in the core. The constants are for comparisons in the codec
/// (`v >= ProtocolVersion::V770`) where a layout change is not described by a
/// feature in [`VersionFeatures`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ProtocolVersion(pub i32);

impl ProtocolVersion {
    /// 1.21, 1.21.1
    pub const V767: Self = Self(767);
    /// 1.21.2, 1.21.3
    pub const V768: Self = Self(768);
    /// 1.21.4
    pub const V769: Self = Self(769);
    /// 1.21.5
    pub const V770: Self = Self(770);
    /// 1.21.6
    pub const V771: Self = Self(771);
    /// 1.21.7, 1.21.8
    pub const V772: Self = Self(772);
    /// 1.21.9, 1.21.10
    pub const V773: Self = Self(773);
    /// 1.21.11
    pub const V774: Self = Self(774);
    /// 26.1.x
    pub const V775: Self = Self(775);
    /// 26.2
    pub const V776: Self = Self(776);
    /// 26.3
    pub const V777: Self = Self(777);
}

impl fmt::Display for ProtocolVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// One protocol version: packet ID tables and layout features. Implementations
/// come from the generator's data (`pumbo-data`); a module can also be added by
/// hand (for example a version outside the generator) without touching the core.
pub trait VersionModule: Send + Sync + fmt::Debug {
    fn protocol(&self) -> ProtocolVersion;

    /// Releases with this protocol, e.g. `["1.21", "1.21.1"]` (known packs, §2.6).
    fn release_names(&self) -> &[String];

    /// Frame ID → packet kind, if the proxy understands it. `None` for the
    /// rest, which pass through undecoded.
    fn packet_kind(&self, phase: Phase, direction: Direction, id: i32) -> Option<PacketKind>;

    /// Packet kind → ID in this version; `None` if the version lacks it.
    fn packet_id(&self, phase: Phase, direction: Direction, kind: PacketKind) -> Option<i32>;

    fn features(&self) -> VersionFeatures {
        VersionFeatures::for_protocol(self.protocol())
    }

    /// Name of a command argument parser by ID (registry
    /// `command_argument_type`), needed to walk the `commands` graph.
    fn command_argument_type(&self, _id: i32) -> Option<&str> {
        None
    }
}

#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum RegistryError {
    #[error("protocol {0} is already registered")]
    Duplicate(ProtocolVersion),
}

/// Supported versions, built at startup.
#[derive(Debug, Default, Clone)]
pub struct VersionRegistry {
    modules: BTreeMap<ProtocolVersion, Arc<dyn VersionModule>>,
}

impl VersionRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, module: Arc<dyn VersionModule>) -> Result<(), RegistryError> {
        let v = module.protocol();
        if self.modules.contains_key(&v) {
            return Err(RegistryError::Duplicate(v));
        }
        self.modules.insert(v, module);
        Ok(())
    }

    pub fn get(&self, v: ProtocolVersion) -> Option<&Arc<dyn VersionModule>> {
        self.modules.get(&v)
    }

    pub fn oldest(&self) -> Option<ProtocolVersion> {
        self.modules.keys().next().copied()
    }

    pub fn newest(&self) -> Option<ProtocolVersion> {
        self.modules.keys().next_back().copied()
    }

    pub fn versions(&self) -> impl Iterator<Item = ProtocolVersion> + '_ {
        self.modules.keys().copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct Fake(i32, Vec<String>);

    impl VersionModule for Fake {
        fn protocol(&self) -> ProtocolVersion {
            ProtocolVersion(self.0)
        }
        fn release_names(&self) -> &[String] {
            &self.1
        }
        fn packet_kind(&self, phase: Phase, direction: Direction, id: i32) -> Option<PacketKind> {
            (phase == Phase::Handshake && direction == Direction::Serverbound && id == 0)
                .then_some(PacketKind::Intention)
        }
        fn packet_id(&self, _: Phase, _: Direction, kind: PacketKind) -> Option<i32> {
            (kind == PacketKind::Intention).then_some(0)
        }
    }

    fn fake(v: i32) -> Arc<dyn VersionModule> {
        Arc::new(Fake(v, vec!["test".into()]))
    }

    #[test]
    fn new_version_without_core_changes() {
        let mut reg = VersionRegistry::new();
        reg.register(fake(767)).unwrap();
        reg.register(fake(777)).unwrap();
        // "26.4" as a new module: the core sees it through the same interface.
        reg.register(fake(778)).unwrap();
        assert_eq!(reg.oldest(), Some(ProtocolVersion::V767));
        assert_eq!(reg.newest(), Some(ProtocolVersion(778)));
        assert!(
            reg.get(ProtocolVersion(778))
                .unwrap()
                .features()
                .text_nbt_snake_case
        );
        assert_eq!(
            reg.register(fake(777)),
            Err(RegistryError::Duplicate(ProtocolVersion::V777))
        );
        assert!(ProtocolVersion(778) > ProtocolVersion::V777);
    }
}
