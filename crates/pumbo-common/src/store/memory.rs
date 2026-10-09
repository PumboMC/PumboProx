//! In-memory backend: copy-on-write snapshots behind a lock.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, TryLockError};

use super::{Backend, Durability, ReadOps, Result, StoreError, WriteOps};

type Tables = BTreeMap<String, BTreeMap<String, Vec<u8>>>;

/// Keeps everything in memory. Readers get a snapshot; a writer works on a copy
/// that replaces the data on commit. Meant for tests and small throwaway data.
#[derive(Debug, Default)]
pub struct MemoryBackend {
    data: Mutex<Arc<Tables>>,
    writer: Mutex<()>,
}

impl MemoryBackend {
    fn snapshot(&self) -> Result<Arc<Tables>> {
        self.data.lock().map(|d| Arc::clone(&d)).map_err(|_| StoreError::new("memory store lock poisoned"))
    }
}

impl Backend for MemoryBackend {
    fn read(&self, f: &mut dyn FnMut(&dyn ReadOps) -> Result<()>) -> Result<()> {
        let snapshot = self.snapshot()?;
        f(&View(&snapshot))
    }

    fn write(&self, _durability: Durability, f: &mut dyn FnMut(&mut dyn WriteOps) -> Result<()>) -> Result<()> {
        let _guard = match self.writer.try_lock() {
            Ok(g) => g,
            Err(TryLockError::WouldBlock) => return Err(StoreError::new("a write transaction is already open")),
            Err(TryLockError::Poisoned(_)) => return Err(StoreError::new("memory store lock poisoned")),
        };
        let mut copy = Tx((*self.snapshot()?).clone());
        f(&mut copy)?;
        let mut data = self.data.lock().map_err(|_| StoreError::new("memory store lock poisoned"))?;
        *data = Arc::new(copy.0);
        Ok(())
    }
}

struct View<'a>(&'a Tables);

struct Tx(Tables);

fn get(tables: &Tables, table: &str, key: &str) -> Option<Vec<u8>> {
    tables.get(table).and_then(|t| t.get(key)).cloned()
}

fn scan(tables: &Tables, table: &str, prefix: &str, visit: &mut dyn FnMut(&str, &[u8]) -> bool) {
    let Some(t) = tables.get(table) else { return };
    for (k, v) in t.range(prefix.to_string()..) {
        if !k.starts_with(prefix) || !visit(k, v) {
            break;
        }
    }
}

fn len(tables: &Tables, table: &str) -> u64 {
    tables.get(table).map_or(0, |t| t.len() as u64)
}

impl ReadOps for View<'_> {
    fn get(&self, table: &str, key: &str) -> Result<Option<Vec<u8>>> {
        Ok(get(self.0, table, key))
    }

    fn scan(&self, table: &str, prefix: &str, visit: &mut dyn FnMut(&str, &[u8]) -> bool) -> Result<()> {
        scan(self.0, table, prefix, visit);
        Ok(())
    }

    fn len(&self, table: &str) -> Result<u64> {
        Ok(len(self.0, table))
    }
}

impl ReadOps for Tx {
    fn get(&self, table: &str, key: &str) -> Result<Option<Vec<u8>>> {
        Ok(get(&self.0, table, key))
    }

    fn scan(&self, table: &str, prefix: &str, visit: &mut dyn FnMut(&str, &[u8]) -> bool) -> Result<()> {
        scan(&self.0, table, prefix, visit);
        Ok(())
    }

    fn len(&self, table: &str) -> Result<u64> {
        Ok(len(&self.0, table))
    }
}

impl WriteOps for Tx {
    fn put(&mut self, table: &str, key: &str, value: &[u8]) -> Result<()> {
        self.0.entry(table.to_string()).or_default().insert(key.to_string(), value.to_vec());
        Ok(())
    }

    fn remove(&mut self, table: &str, key: &str) -> Result<bool> {
        Ok(self.0.get_mut(table).is_some_and(|t| t.remove(key).is_some()))
    }

    fn retain(&mut self, table: &str, keep: &mut dyn FnMut(&str, &[u8]) -> bool) -> Result<u64> {
        let Some(t) = self.0.get_mut(table) else { return Ok(0) };
        let before = t.len();
        t.retain(|k, v| keep(k, v));
        Ok((before - t.len()) as u64)
    }
}
