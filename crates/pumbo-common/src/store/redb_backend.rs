//! redb backend: one database file, tables of `&str -> &[u8]`.

use std::path::Path;

use redb::{Database, ReadableDatabase, ReadableTable, ReadableTableMetadata, TableDefinition, TableError};

use super::{Backend, Durability, ReadOps, Result, StoreError, WriteOps};

fn def(name: &str) -> TableDefinition<'_, &'static str, &'static [u8]> {
    TableDefinition::new(name)
}

fn err(e: impl std::fmt::Display) -> StoreError {
    StoreError::new(e.to_string())
}

/// A redb database file.
pub struct RedbBackend {
    db: Database,
}

impl RedbBackend {
    /// Opens the file, creating it when it does not exist.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Database::create(path).map(|db| Self { db }).map_err(err)
    }
}

impl Backend for RedbBackend {
    fn read(&self, f: &mut dyn FnMut(&dyn ReadOps) -> Result<()>) -> Result<()> {
        let tx = self.db.begin_read().map_err(err)?;
        f(&Read(tx))
    }

    fn write(&self, durability: Durability, f: &mut dyn FnMut(&mut dyn WriteOps) -> Result<()>) -> Result<()> {
        let mut tx = self.db.begin_write().map_err(err)?;
        if durability == Durability::Eventual {
            tx.set_durability(redb::Durability::None).map_err(err)?;
        }
        let mut w = Write(tx);
        match f(&mut w) {
            Ok(()) => w.0.commit().map_err(err),
            Err(e) => {
                // Aborting only fails when the file is already broken; the
                // closure's error is the one worth reporting.
                let _ = w.0.abort();
                Err(e)
            }
        }
    }
}

struct Read(redb::ReadTransaction);

struct Write(redb::WriteTransaction);

impl ReadOps for Read {
    fn get(&self, table: &str, key: &str) -> Result<Option<Vec<u8>>> {
        match self.0.open_table(def(table)) {
            Ok(t) => Ok(t.get(key).map_err(err)?.map(|v| v.value().to_vec())),
            Err(TableError::TableDoesNotExist(_)) => Ok(None),
            Err(e) => Err(err(e)),
        }
    }

    fn scan(&self, table: &str, prefix: &str, visit: &mut dyn FnMut(&str, &[u8]) -> bool) -> Result<()> {
        let t = match self.0.open_table(def(table)) {
            Ok(t) => t,
            Err(TableError::TableDoesNotExist(_)) => return Ok(()),
            Err(e) => return Err(err(e)),
        };
        for item in t.range(prefix..).map_err(err)? {
            let (k, v) = item.map_err(err)?;
            let k = k.value();
            if !k.starts_with(prefix) || !visit(k, v.value()) {
                break;
            }
        }
        Ok(())
    }

    fn len(&self, table: &str) -> Result<u64> {
        match self.0.open_table(def(table)) {
            Ok(t) => t.len().map_err(err),
            Err(TableError::TableDoesNotExist(_)) => Ok(0),
            Err(e) => Err(err(e)),
        }
    }
}

impl ReadOps for Write {
    fn get(&self, table: &str, key: &str) -> Result<Option<Vec<u8>>> {
        let t = self.0.open_table(def(table)).map_err(err)?;
        Ok(t.get(key).map_err(err)?.map(|v| v.value().to_vec()))
    }

    fn scan(&self, table: &str, prefix: &str, visit: &mut dyn FnMut(&str, &[u8]) -> bool) -> Result<()> {
        let t = self.0.open_table(def(table)).map_err(err)?;
        for item in t.range(prefix..).map_err(err)? {
            let (k, v) = item.map_err(err)?;
            let k = k.value();
            if !k.starts_with(prefix) || !visit(k, v.value()) {
                break;
            }
        }
        Ok(())
    }

    fn len(&self, table: &str) -> Result<u64> {
        let t = self.0.open_table(def(table)).map_err(err)?;
        t.len().map_err(err)
    }
}

impl WriteOps for Write {
    fn put(&mut self, table: &str, key: &str, value: &[u8]) -> Result<()> {
        let mut t = self.0.open_table(def(table)).map_err(err)?;
        t.insert(key, value).map_err(err)?;
        Ok(())
    }

    fn remove(&mut self, table: &str, key: &str) -> Result<bool> {
        let mut t = self.0.open_table(def(table)).map_err(err)?;
        Ok(t.remove(key).map_err(err)?.is_some())
    }

    fn retain(&mut self, table: &str, keep: &mut dyn FnMut(&str, &[u8]) -> bool) -> Result<u64> {
        let mut t = self.0.open_table(def(table)).map_err(err)?;
        let before = t.len().map_err(err)?;
        t.retain(|k, v| keep(k, v)).map_err(err)?;
        let after = t.len().map_err(err)?;
        Ok(before.saturating_sub(after))
    }
}
