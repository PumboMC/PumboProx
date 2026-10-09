//! Map images and a filled map in hand (plan §5.4).
//!
//! `map_item_data` has the same layout in 767–777, so an image is encoded once
//! for all versions; only the packet ID and the item (`minecraft:filled_map`
//! with the `minecraft:map_id` component) differ per version.

use std::sync::atomic::{AtomicI32, Ordering};

use bytes::Bytes;
use pumbo_protocol::VersionModule;
use pumbo_protocol::packets::world::{ItemStack, MapItemData, MapPatch};
use pumbo_protocol::packets::{self, Ctx};
use pumbo_protocol::types::WriteExt;

use crate::Error;

pub const SIDE: usize = 128;

/// Map IDs of the proxy; the client keeps map data per level, and a backend
/// gives it a new level, so they cannot clash with a server's maps.
static NEXT_ID: AtomicI32 = AtomicI32::new(0);

/// A 128×128 image in map colour indices, ready to send.
#[derive(Debug, Clone)]
pub struct MapImage {
    pub id: i32,
    /// `map_item_data` payload.
    pub payload: Bytes,
}

impl MapImage {
    pub fn new(pixels: &[u8]) -> Result<Self, Error> {
        if pixels.len() != SIDE * SIDE {
            return Err(Error::MapSize(pixels.len()));
        }
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let data = MapItemData {
            map_id: id,
            scale: 0,
            locked: true,
            icons: None,
            patch: Some(MapPatch {
                columns: SIDE as u8,
                rows: SIDE as u8,
                x: 0,
                z: 0,
                colors: pixels.to_vec(),
            }),
        };
        // The layout does not depend on the version.
        let any = pumbo_data::protocols()
            .next()
            .ok_or(Error::NoData("protocol tables"))?;
        let m = pumbo_data::DataVersion::new(pumbo_data::tables(any)?);
        let payload =
            packets::encode(&data, &Ctx::new(&m, pumbo_protocol::Direction::Clientbound))?;
        Ok(Self {
            id,
            payload: Bytes::from(payload),
        })
    }
}

/// A filled map showing `map_id`, as an item stack of this version.
pub fn filled_map(module: &dyn VersionModule, map_id: i32) -> Result<ItemStack, Error> {
    let tables = pumbo_data::tables(module.protocol())?;
    let item = tables
        .registry("minecraft:item")
        .and_then(|r| r.id("minecraft:filled_map"))
        .ok_or(Error::NoData("item minecraft:filled_map"))?;
    let component = tables
        .registry("minecraft:data_component_type")
        .and_then(|r| r.id("minecraft:map_id"))
        .ok_or(Error::NoData("component minecraft:map_id"))?;
    let mut components = Vec::new();
    components.put_varint(i32::try_from(component).unwrap_or(0));
    components.put_varint(map_id);
    Ok(ItemStack {
        count: 1,
        item: i32::try_from(item).unwrap_or(0),
        added: 1,
        removed: 0,
        components,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_and_item() {
        assert!(MapImage::new(&[0; 10]).is_err());
        let a = MapImage::new(&[34; SIDE * SIDE]).unwrap();
        let b = MapImage::new(&[34; SIDE * SIDE]).unwrap();
        assert_ne!(a.id, b.id);
        // IDs checked by the generator (plan §2.4): item 982 and component 26
        // in 1.21.1, item 1238 and component 48 in 26.3.
        for (v, item, component) in [(767, 982, 26), (777, 1238, 48)] {
            let m = pumbo_data::DataVersion::new(
                pumbo_data::tables(pumbo_protocol::ProtocolVersion(v)).unwrap(),
            );
            let s = filled_map(&m, 5).unwrap();
            assert_eq!(s.item, item);
            assert_eq!(s.components, vec![component, 5]);
        }
    }
}
