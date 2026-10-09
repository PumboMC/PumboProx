//! Host data store modules. In E0 only memory (tests and a template); redb
//! and SQL come as further modules when they are needed (§4.3).

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use pumbo_core::BoxFuture;
use pumbo_core::registry::{ModuleConfig, ModuleError, ModuleRegistry};
use pumbo_core::storage::{KeyValueStore, StorageError, StorageResult};

pub const MEMORY: &str = "memory";

type Key = (String, Vec<u8>);

#[derive(Debug, Default)]
pub struct MemoryStore {
    map: Mutex<BTreeMap<Key, Bytes>>,
}

impl MemoryStore {
    fn with<R>(&self, f: impl FnOnce(&mut BTreeMap<Key, Bytes>) -> R) -> StorageResult<R> {
        let mut guard = self
            .map
            .lock()
            .map_err(|_| StorageError::Unavailable("poisoned lock".into()))?;
        Ok(f(&mut guard))
    }
}

impl KeyValueStore for MemoryStore {
    fn get<'a>(
        &'a self,
        ns: &'a str,
        key: &'a [u8],
    ) -> BoxFuture<'a, StorageResult<Option<Bytes>>> {
        let r = self.with(|m| m.get(&(ns.to_string(), key.to_vec())).cloned());
        Box::pin(async move { r })
    }

    fn put<'a>(
        &'a self,
        ns: &'a str,
        key: &'a [u8],
        value: Bytes,
    ) -> BoxFuture<'a, StorageResult<()>> {
        let r = self.with(|m| {
            m.insert((ns.to_string(), key.to_vec()), value);
        });
        Box::pin(async move { r })
    }

    fn delete<'a>(&'a self, ns: &'a str, key: &'a [u8]) -> BoxFuture<'a, StorageResult<bool>> {
        let r = self.with(|m| m.remove(&(ns.to_string(), key.to_vec())).is_some());
        Box::pin(async move { r })
    }

    fn scan_prefix<'a>(
        &'a self,
        ns: &'a str,
        prefix: &'a [u8],
        limit: usize,
    ) -> BoxFuture<'a, StorageResult<Vec<(Bytes, Bytes)>>> {
        let r = self.with(|m| {
            m.range((ns.to_string(), prefix.to_vec())..)
                .take_while(|((n, k), _)| n == ns && k.starts_with(prefix))
                .take(limit)
                .map(|((_, k), v)| (Bytes::copy_from_slice(k), v.clone()))
                .collect()
        });
        Box::pin(async move { r })
    }
}

fn memory_factory(_: &ModuleConfig) -> Result<Arc<dyn KeyValueStore>, ModuleError> {
    Ok(Arc::new(MemoryStore::default()))
}

pub fn register(reg: &mut ModuleRegistry) -> Result<(), ModuleError> {
    reg.stores.register(MEMORY, memory_factory)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn operations() {
        let s = MemoryStore::default();
        s.put("a", b"k1", Bytes::from_static(b"1")).await.unwrap();
        s.put("a", b"k2", Bytes::from_static(b"2")).await.unwrap();
        s.put("a", b"x", Bytes::from_static(b"3")).await.unwrap();
        s.put("b", b"k1", Bytes::from_static(b"4")).await.unwrap();
        assert_eq!(s.get("a", b"k1").await.unwrap().as_deref(), Some(&b"1"[..]));
        let scan = s.scan_prefix("a", b"k", 10).await.unwrap();
        assert_eq!(scan.len(), 2);
        assert!(s.delete("a", b"k1").await.unwrap());
        assert!(!s.delete("a", b"k1").await.unwrap());
        assert_eq!(s.get("b", b"k1").await.unwrap().as_deref(), Some(&b"4"[..]));
    }
}
