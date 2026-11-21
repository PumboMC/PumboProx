//! What one multiversion provider keeps once every pair is planned: how long
//! the first plan of each pair takes and how many bytes stay allocated after
//! it, and the size of the 26.3 version data. Ignored by default; meaningful
//! only in release:
//! `cargo test --release -p pumbo-translate --test pair_tables -- --ignored --nocapture`
//! One test only: the allocator below counts for the whole test binary.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use pumbo_core::translate::TranslatorProvider;
use pumbo_protocol::ProtocolVersion;
use pumbo_translate::multiversion::{Multiversion, version_data};

struct Live;

/// Bytes allocated and not freed yet.
static LIVE: AtomicUsize = AtomicUsize::new(0);

#[allow(unsafe_code)]
unsafe impl GlobalAlloc for Live {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: forwarded unchanged to the system allocator.
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            LIVE.fetch_add(layout.size(), Ordering::Relaxed);
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
        // SAFETY: forwarded unchanged to the system allocator.
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOC: Live = Live;

fn live() -> usize {
    LIVE.load(Ordering::Relaxed)
}

#[test]
#[ignore = "benchmark"]
fn tables_one_provider_keeps() {
    // The tables of pumbo-data are the same before and after; they are not
    // part of the result.
    for v in pumbo_data::protocols() {
        pumbo_data::tables(v).unwrap();
        pumbo_data::synced(v).unwrap();
    }
    let server = ProtocolVersion(777);
    let before = live();
    let data = version_data(server).unwrap();
    let server_data = live() - before;
    drop(data);

    let clients: Vec<ProtocolVersion> = (767..=776).map(ProtocolVersion).collect();
    let mut rows = Vec::with_capacity(clients.len());
    let provider = Multiversion::default();
    let start = live();
    let mut last = start;
    for &client in &clients {
        let started = Instant::now();
        let plan = provider.plan(client, Some(server));
        let ms = started.elapsed().as_secs_f64() * 1000.0;
        assert!(plan.is_some(), "no plan for {client}");
        drop(plan);
        let now = live();
        rows.push((client, ms, now as isize - last as isize));
        last = now;
    }
    let total = last as isize - start as isize;

    println!("client  first plan (ms)  bytes kept");
    for (client, ms, bytes) in &rows {
        println!("{:<7} {ms:>15.2}  {bytes:>10}", client.0);
    }
    let ms: f64 = rows.iter().map(|r| r.1).sum();
    println!("ten pairs: {ms:.2} ms, {total} bytes kept");
    println!("26.3 version data: {server_data} bytes");
}
