//! Key-value storage with transactions.
//!
//! Data lives in named tables of `string key -> bytes`. Everything in one
//! [`Store::write`] call commits together or not at all. Backends:
//! - [`RedbBackend`]: a single redb file (the default for plugins)
//! - [`MemoryBackend`]: in memory, for tests and throwaway data
//!
//! Core crates build their typed storage (accounts, bans, ...) on [`Store`] and
//! never see the backend, so another backend (SQL) can be added later without
//! touching them. Values are usually JSON ([`ReadExt::get_json`],
//! [`WriteExt::put_json`]). Keys are plain strings; use the canonical forms from
//! [`crate::id`] (`name_key`, UUID strings, `ip_key`).
//!
//! A write transaction must not be opened inside another one: the memory
//! backend refuses it with an error, redb would wait forever.

mod memory;
mod redb_backend;

use std::fmt;

use serde::Serialize;
use serde::de::DeserializeOwned;

pub use memory::MemoryBackend;
pub use redb_backend::RedbBackend;

/// A storage error. Callers usually log it and refuse the action ("fail closed").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreError(pub String);

impl StoreError {
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for StoreError {}

pub type Result<T> = std::result::Result<T, StoreError>;

/// How hard a commit tries to reach the disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Durability {
    /// On disk when the call returns.
    #[default]
    Immediate,
    /// Faster; may be lost if the process dies before the next `Immediate`
    /// commit. For caches and counters that can be rebuilt.
    Eventual,
}

/// Reads inside a transaction. A table that was never written reads as empty.
pub trait ReadOps {
    fn get(&self, table: &str, key: &str) -> Result<Option<Vec<u8>>>;

    /// Visits the entries whose key starts with `prefix`, in key order, until
    /// `visit` returns false. An empty prefix visits the whole table.
    fn scan(&self, table: &str, prefix: &str, visit: &mut dyn FnMut(&str, &[u8]) -> bool) -> Result<()>;

    /// Number of entries in a table.
    fn len(&self, table: &str) -> Result<u64>;
}

/// Reads and writes inside a write transaction.
pub trait WriteOps: ReadOps {
    fn put(&mut self, table: &str, key: &str, value: &[u8]) -> Result<()>;

    /// Removes a key; true if it existed.
    fn remove(&mut self, table: &str, key: &str) -> Result<bool>;

    /// Keeps only the entries for which `keep` returns true; returns how many
    /// were removed.
    fn retain(&mut self, table: &str, keep: &mut dyn FnMut(&str, &[u8]) -> bool) -> Result<u64>;
}

/// A storage engine. Implementations run the closure in one transaction and
/// commit it when the closure returns `Ok`, discard it otherwise.
pub trait Backend: Send + Sync {
    fn read(&self, f: &mut dyn FnMut(&dyn ReadOps) -> Result<()>) -> Result<()>;
    fn write(&self, durability: Durability, f: &mut dyn FnMut(&mut dyn WriteOps) -> Result<()>) -> Result<()>;
}

/// Handle to a store.
pub struct Store {
    backend: Box<dyn Backend>,
}

impl fmt::Debug for Store {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Store")
    }
}

impl Store {
    pub fn new(backend: impl Backend + 'static) -> Self {
        Self { backend: Box::new(backend) }
    }

    /// Opens (or creates) a redb file.
    pub fn open(path: impl AsRef<std::path::Path>) -> Result<Self> {
        RedbBackend::open(path).map(Self::new)
    }

    pub fn in_memory() -> Self {
        Self::new(MemoryBackend::default())
    }

    /// Runs `f` in a read transaction (a consistent snapshot).
    pub fn read<T>(&self, f: impl FnOnce(&dyn ReadOps) -> Result<T>) -> Result<T> {
        let mut f = Some(f);
        let mut out = None;
        self.backend.read(&mut |tx| {
            let f = f.take().ok_or_else(|| StoreError::new("internal: read closure called twice"))?;
            out = Some(f(tx)?);
            Ok(())
        })?;
        out.ok_or_else(|| StoreError::new("internal: read closure not called"))
    }

    /// Runs `f` in a durable write transaction.
    pub fn write<T>(&self, f: impl FnOnce(&mut dyn WriteOps) -> Result<T>) -> Result<T> {
        self.write_with(Durability::Immediate, f)
    }

    /// Runs `f` in a write transaction: all of its changes commit together if it
    /// returns `Ok`, none of them if it returns `Err`.
    pub fn write_with<T>(&self, durability: Durability, f: impl FnOnce(&mut dyn WriteOps) -> Result<T>) -> Result<T> {
        let mut f = Some(f);
        let mut out = None;
        self.backend.write(durability, &mut |tx| {
            let f = f.take().ok_or_else(|| StoreError::new("internal: write closure called twice"))?;
            out = Some(f(tx)?);
            Ok(())
        })?;
        out.ok_or_else(|| StoreError::new("internal: write closure not called"))
    }

    pub fn get_json<T: DeserializeOwned>(&self, table: &str, key: &str) -> Result<Option<T>> {
        self.read(|tx| tx.get_json(table, key))
    }

    pub fn put_json<T: Serialize>(&self, table: &str, key: &str, value: &T) -> Result<()> {
        self.write(|tx| tx.put_json(table, key, value))
    }

    pub fn remove(&self, table: &str, key: &str) -> Result<bool> {
        self.write(|tx| tx.remove(table, key))
    }
}

/// Typed reads on top of [`ReadOps`].
pub trait ReadExt {
    /// Reads a JSON value. A value that does not decode is an error, not `None`:
    /// treating a damaged record as missing could let it be overwritten.
    fn get_json<T: DeserializeOwned>(&self, table: &str, key: &str) -> Result<Option<T>>;
    fn get_u64(&self, table: &str, key: &str) -> Result<Option<u64>>;
    /// All entries under a key prefix, in key order.
    fn collect_prefix(&self, table: &str, prefix: &str) -> Result<Vec<(String, Vec<u8>)>>;
}

impl<R: ReadOps + ?Sized> ReadExt for R {
    fn get_json<T: DeserializeOwned>(&self, table: &str, key: &str) -> Result<Option<T>> {
        match self.get(table, key)? {
            Some(bytes) => serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(|e| StoreError::new(format!("damaged value {table}/{key}: {e}"))),
            None => Ok(None),
        }
    }

    fn get_u64(&self, table: &str, key: &str) -> Result<Option<u64>> {
        match self.get(table, key)? {
            Some(bytes) => <[u8; 8]>::try_from(bytes.as_slice())
                .map(|b| Some(u64::from_be_bytes(b)))
                .map_err(|_| StoreError::new(format!("damaged number {table}/{key}"))),
            None => Ok(None),
        }
    }

    fn collect_prefix(&self, table: &str, prefix: &str) -> Result<Vec<(String, Vec<u8>)>> {
        let mut out = Vec::new();
        self.scan(table, prefix, &mut |k, v| {
            out.push((k.to_string(), v.to_vec()));
            true
        })?;
        Ok(out)
    }
}

/// Typed writes on top of [`WriteOps`].
pub trait WriteExt {
    fn put_json<T: Serialize>(&mut self, table: &str, key: &str, value: &T) -> Result<()>;
    /// Stores a number as 8 big-endian bytes (sorts like the number).
    fn put_u64(&mut self, table: &str, key: &str, value: u64) -> Result<()>;
}

impl<W: WriteOps + ?Sized> WriteExt for W {
    fn put_json<T: Serialize>(&mut self, table: &str, key: &str, value: &T) -> Result<()> {
        let bytes =
            serde_json::to_vec(value).map_err(|e| StoreError::new(format!("cannot encode {table}/{key}: {e}")))?;
        self.put(table, key, &bytes)
    }

    fn put_u64(&mut self, table: &str, key: &str, value: u64) -> Result<()> {
        self.put(table, key, &value.to_be_bytes())
    }
}

/// Decodes a JSON value from a scan, falling back to the default for damaged
/// entries (for caches and counters, where a damaged entry may be dropped).
pub fn json_or_default<T: DeserializeOwned + Default>(bytes: &[u8]) -> T {
    serde_json::from_slice(bytes).unwrap_or_default()
}

/// Decodes a number written by [`WriteExt::put_u64`] (0 if damaged).
pub fn u64_or_zero(bytes: &[u8]) -> u64 {
    <[u8; 8]>::try_from(bytes).map(u64::from_be_bytes).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs the same checks against every backend.
    fn exercise(store: &Store) {
        // empty tables read as empty
        assert_eq!(store.read(|tx| tx.get("t", "a")).unwrap(), None);
        assert_eq!(store.read(|tx| tx.len("t")).unwrap(), 0);

        store
            .write(|tx| {
                tx.put("t", "user|b", b"2")?;
                tx.put("t", "user|a", b"1")?;
                tx.put("t", "other", b"x")?;
                tx.put_u64("n", "count", 42)?;
                tx.put_json("j", "k", &vec![1, 2, 3])
            })
            .unwrap();
        assert_eq!(store.read(|tx| tx.get("t", "user|a")).unwrap(), Some(b"1".to_vec()));
        assert_eq!(store.read(|tx| tx.get_u64("n", "count")).unwrap(), Some(42));
        assert_eq!(store.get_json::<Vec<u32>>("j", "k").unwrap(), Some(vec![1, 2, 3]));
        assert_eq!(store.read(|tx| tx.len("t")).unwrap(), 3);

        let users = store.read(|tx| tx.collect_prefix("t", "user|")).unwrap();
        let keys: Vec<_> = users.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, vec!["user|a", "user|b"]);
        // scan stops when asked
        let mut seen = 0;
        store
            .read(|tx| {
                tx.scan("t", "", &mut |_, _| {
                    seen += 1;
                    false
                })
            })
            .unwrap();
        assert_eq!(seen, 1);

        // a failing write changes nothing
        let failed = store.write(|tx| {
            tx.put("t", "user|c", b"3")?;
            tx.remove("t", "other")?;
            Err::<(), _>(StoreError::new("stop"))
        });
        assert_eq!(failed, Err(StoreError::new("stop")));
        assert_eq!(store.read(|tx| tx.get("t", "user|c")).unwrap(), None);
        assert!(store.read(|tx| tx.get("t", "other")).unwrap().is_some());

        // writes see their own changes
        let removed = store
            .write_with(Durability::Eventual, |tx| {
                tx.put("t", "user|c", b"3")?;
                assert_eq!(tx.get("t", "user|c")?, Some(b"3".to_vec()));
                tx.retain("t", &mut |k, _| k.starts_with("user|"))
            })
            .unwrap();
        assert_eq!(removed, 1);
        assert_eq!(store.read(|tx| tx.len("t")).unwrap(), 3);
        assert!(store.remove("t", "user|c").unwrap());
        assert!(!store.remove("t", "user|c").unwrap());

        // damaged values are errors for typed reads
        store.write(|tx| tx.put("j", "bad", b"{not json")).unwrap();
        assert!(store.get_json::<Vec<u32>>("j", "bad").is_err());
        assert!(store.read(|tx| tx.get_u64("j", "bad")).is_err());
        assert_eq!(json_or_default::<Vec<u32>>(b"{not json"), Vec::<u32>::new());
        assert_eq!(u64_or_zero(&7u64.to_be_bytes()), 7);
        assert_eq!(u64_or_zero(b"x"), 0);
    }

    #[test]
    fn memory_backend() {
        let store = Store::in_memory();
        exercise(&store);
        // nested write transactions are refused, not deadlocked
        let nested = store.write(|_| store.write(|tx| tx.put("t", "x", b"1")));
        assert!(nested.is_err());
    }

    #[test]
    fn redb_backend() {
        let path = std::env::temp_dir().join(format!("pumbo-common-store-{}.redb", std::process::id()));
        let _ = std::fs::remove_file(&path);
        {
            let store = Store::open(&path).unwrap();
            exercise(&store);
        }
        // data survives reopening
        let store = Store::open(&path).unwrap();
        assert_eq!(store.read(|tx| tx.get_u64("n", "count")).unwrap(), Some(42));
        drop(store);
        let _ = std::fs::remove_file(&path);
    }
}
