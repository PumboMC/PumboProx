//! A lock file next to a database, for hosts that may load a second instance
//! of a plugin on the same folder: WASI has no file locks and redb opens the
//! file without one.
//!
//! The lock holds the owner's random nonce and the time it was last
//! refreshed. A lock refreshed less than [`STALE_MS`] ago refuses another
//! instance; an older one is taken over (the owner crashed or stands still).

use std::io::Write;
use std::path::{Path, PathBuf};

use super::{Result, StoreError};
use crate::random::random_u64;

/// A lock not refreshed for this long belongs to nobody.
pub const STALE_MS: u64 = 30_000;

/// `<db>.lock`, owned while this value lives (dropping it removes the file).
#[derive(Debug)]
pub struct DbLock {
    path: PathBuf,
    nonce: String,
}

impl DbLock {
    /// Takes the lock of the database file `db` at `now` (Unix ms).
    pub fn acquire(db: impl AsRef<Path>, now: u64) -> Result<Self> {
        let mut path = db.as_ref().as_os_str().to_owned();
        path.push(".lock");
        let lock = Self { path: PathBuf::from(path), nonce: format!("{:016x}", random_u64()) };
        if lock.create(now)? {
            return Ok(lock);
        }
        let refreshed = lock.read().map_or(0, |(_, time)| time);
        let age = now.saturating_sub(refreshed);
        if age < STALE_MS {
            return Err(StoreError::new(format!(
                "the database is open in another instance (its lock {} was refreshed {age} ms ago)",
                lock.path.display()
            )));
        }
        // Stale: remove and take it, unless another instance was quicker
        // (it may also have removed the file this one just made).
        let _ = std::fs::remove_file(&lock.path);
        if lock.create(now)? && lock.read().is_some_and(|(nonce, _)| nonce == lock.nonce) {
            Ok(lock)
        } else {
            // Not ours: dropping `lock` leaves the file alone.
            Err(StoreError::new("the database is open in another instance (it took the stale lock first)"))
        }
    }

    /// Writes a new time; an error when another instance took the lock over.
    pub fn refresh(&self, now: u64) -> Result<()> {
        if self.read().map(|(nonce, _)| nonce) != Some(self.nonce.clone()) {
            return Err(StoreError::new("the database was taken over by another instance"));
        }
        let mut tmp = self.path.as_os_str().to_owned();
        tmp.push(format!(".{}.tmp", self.nonce));
        let tmp = PathBuf::from(tmp);
        let written = std::fs::File::create(&tmp)
            .and_then(|mut f| f.write_all(self.line(now).as_bytes()).and_then(|()| f.sync_all()))
            .and_then(|()| std::fs::rename(&tmp, &self.path));
        written.map_err(|e| StoreError::new(format!("cannot refresh {}: {e}", self.path.display())))
    }

    fn line(&self, now: u64) -> String {
        format!("{} {now}\n", self.nonce)
    }

    /// Creates the file; false when it already exists.
    fn create(&self, now: u64) -> Result<bool> {
        let file = std::fs::OpenOptions::new().write(true).create_new(true).open(&self.path);
        let mut file = match file {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => return Ok(false),
            Err(e) => return Err(StoreError::new(format!("cannot create {}: {e}", self.path.display()))),
        };
        file.write_all(self.line(now).as_bytes())
            .and_then(|()| file.sync_all())
            .map_err(|e| StoreError::new(format!("cannot write {}: {e}", self.path.display())))?;
        Ok(true)
    }

    /// The nonce and time in the file.
    fn read(&self) -> Option<(String, u64)> {
        let text = std::fs::read_to_string(&self.path).ok()?;
        let (nonce, time) = text.trim().split_once(' ')?;
        Some((nonce.to_string(), time.parse().ok()?))
    }
}

impl Drop for DbLock {
    fn drop(&mut self) {
        if self.read().is_some_and(|(nonce, _)| nonce == self.nonce) {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db(test: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("pumbo-common-lock-{test}-{}.redb", std::process::id()));
        let _ = std::fs::remove_file(format!("{}.lock", path.display()));
        path
    }

    #[test]
    fn second_instance_refused_while_lock_fresh() {
        let path = db("fresh");
        let first = DbLock::acquire(&path, 1_000).unwrap();
        let err = DbLock::acquire(&path, 1_000 + STALE_MS - 1).unwrap_err();
        assert!(err.0.contains("open in another instance"), "{err}");
        // A refresh keeps it fresh; after the owner is gone the next one gets it.
        first.refresh(20_000).unwrap();
        assert!(DbLock::acquire(&path, 20_000 + STALE_MS - 1).is_err());
        drop(first);
        let second = DbLock::acquire(&path, 20_001).unwrap();
        drop(second);
        assert!(!Path::new(&format!("{}.lock", path.display())).exists());
    }

    #[test]
    fn stale_lock_taken_over() {
        let path = db("stale");
        let old = DbLock::acquire(&path, 1_000).unwrap();
        let new = DbLock::acquire(&path, 1_000 + STALE_MS).unwrap();
        // The old owner learns it lost the lock and leaves the file alone.
        assert!(old.refresh(1_000 + STALE_MS + 1).is_err());
        drop(old);
        new.refresh(1_000 + STALE_MS + 2).unwrap();
        drop(new);
        assert!(!Path::new(&format!("{}.lock", path.display())).exists());
    }
}
