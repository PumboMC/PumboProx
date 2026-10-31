//! Untrusted length prefixes must not reserve more than the byte budget allows.
//! One test only: the allocator below counts for the whole test binary.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use pumbo_nbt::{Limits, NbtError, read_network};

struct Largest;

static LARGEST: AtomicUsize = AtomicUsize::new(0);

#[allow(unsafe_code)]
unsafe impl GlobalAlloc for Largest {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        LARGEST.fetch_max(layout.size(), Ordering::Relaxed);
        // SAFETY: forwarded unchanged to the system allocator.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: forwarded unchanged to the system allocator.
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOC: Largest = Largest;

/// The largest single allocation made while reading `data`.
fn largest_while_reading(data: &[u8], limits: Limits) -> (Result<(), NbtError>, usize) {
    LARGEST.store(0, Ordering::Relaxed);
    let result = read_network(&mut &data[..], limits).map(drop);
    (result, LARGEST.load(Ordering::Relaxed))
}

#[test]
fn length_prefixes_reserve_within_the_budget() {
    // An int array that the input could hold but the budget cannot.
    let n: u32 = 256 * 1024;
    let mut data = vec![11u8];
    data.extend_from_slice(&n.to_be_bytes());
    data.resize(data.len() + n as usize * 4, 0);
    let small = Limits {
        max_depth: 64,
        max_bytes: 4096,
    };
    let (result, largest) = largest_while_reading(&data, small);
    assert_eq!(result, Err(NbtError::TooLarge(4096)));
    assert!(largest < 64 * 1024, "reserved {largest} bytes");

    // Lists of lists, each claiming as many elements as the rest could hold:
    // without a cap every level reserves several times the remaining input.
    let mut nested = vec![9u8];
    for _ in 0..40 {
        nested.push(9);
        nested.extend_from_slice(&(1u32 << 15).to_be_bytes());
    }
    nested.resize(nested.len() + 256 * 1024, 0xFF);
    let (result, largest) = largest_while_reading(&nested, Limits::BACKEND);
    assert!(result.is_err());
    assert!(largest < 256 * 1024, "reserved {largest} bytes");

    // A large list within the budget still reads.
    let n: u32 = 100_000;
    let mut ints = vec![9u8, 3];
    ints.extend_from_slice(&n.to_be_bytes());
    ints.resize(ints.len() + n as usize * 4, 0);
    let (result, _) = largest_while_reading(&ints, Limits::BACKEND);
    assert_eq!(result, Ok(()));
}
