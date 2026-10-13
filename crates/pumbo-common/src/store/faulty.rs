//! A memory backend that fails on demand, for tests of storage errors.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use super::{Backend, Durability, MemoryBackend, ReadOps, Result, StoreError, WriteOps};

/// [`MemoryBackend`] whose next `fail_reads` reads and `fail_writes` writes
/// fail. Clone the counters before handing the backend to [`super::Store::new`];
/// `u32::MAX` means "until set back to 0".
#[derive(Debug, Default)]
pub struct FaultyBackend {
    inner: MemoryBackend,
    pub fail_reads: Arc<AtomicU32>,
    pub fail_writes: Arc<AtomicU32>,
}

fn take(counter: &AtomicU32) -> bool {
    counter.try_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1)).is_ok()
}

impl Backend for FaultyBackend {
    fn read(&self, f: &mut dyn FnMut(&dyn ReadOps) -> Result<()>) -> Result<()> {
        if take(&self.fail_reads) {
            return Err(StoreError::new("read failed"));
        }
        self.inner.read(f)
    }

    fn write(&self, durability: Durability, f: &mut dyn FnMut(&mut dyn WriteOps) -> Result<()>) -> Result<()> {
        if take(&self.fail_writes) {
            return Err(StoreError::new("write failed"));
        }
        self.inner.write(durability, f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;

    #[test]
    fn fails_on_demand() {
        let backend = FaultyBackend::default();
        let (reads, writes) = (backend.fail_reads.clone(), backend.fail_writes.clone());
        let store = Store::new(backend);
        store.put_json("t", "k", &1).unwrap();

        writes.store(1, Ordering::SeqCst);
        assert!(store.put_json("t", "k", &2).is_err());
        store.put_json("t", "k", &3).unwrap();

        reads.store(2, Ordering::SeqCst);
        assert!(store.get_json::<u32>("t", "k").is_err());
        assert!(store.get_json::<u32>("t", "k").is_err());
        assert_eq!(store.get_json::<u32>("t", "k").unwrap(), Some(3));
    }
}
