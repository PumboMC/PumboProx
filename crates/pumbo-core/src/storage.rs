//! Extension point: host-side data store.
//!
//! In 0.1 plugins keep their data in their own redb inside WASM (§4.3). This
//! interface is for host data and for the future WIT `storage` interface
//! (SQL for several proxy instances, §9 item 9): implementations (memory,
//! redb, SQL) are pluggable modules.

use bytes::Bytes;

use crate::BoxFuture;

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("store unavailable: {0}")]
    Unavailable(String),
}

pub type StorageResult<T> = Result<T, StorageError>;

/// Key → value store with namespaces (e.g. per plugin).
pub trait KeyValueStore: Send + Sync + std::fmt::Debug {
    fn get<'a>(&'a self, ns: &'a str, key: &'a [u8])
    -> BoxFuture<'a, StorageResult<Option<Bytes>>>;
    fn put<'a>(
        &'a self,
        ns: &'a str,
        key: &'a [u8],
        value: Bytes,
    ) -> BoxFuture<'a, StorageResult<()>>;
    fn delete<'a>(&'a self, ns: &'a str, key: &'a [u8]) -> BoxFuture<'a, StorageResult<bool>>;
    /// Keys with a prefix, ascending, at most `limit`.
    fn scan_prefix<'a>(
        &'a self,
        ns: &'a str,
        prefix: &'a [u8],
        limit: usize,
    ) -> BoxFuture<'a, StorageResult<Vec<(Bytes, Bytes)>>>;
}
