//! Benchmark binary: runs the full suite and prints one JSON record per line.

use hft_bench::{ALLOCATIONS, DEALLOCATIONS};
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::Ordering;

struct CountingAllocator;

// SAFETY: every operation delegates to `System` with the identical pointer and
// layout contract. Counters do not affect allocation ownership or alignment.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        // SAFETY: the caller provides GlobalAlloc's valid layout contract.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        DEALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        // SAFETY: the caller returns the pointer with its original layout.
        unsafe { System.dealloc(pointer, layout) };
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        DEALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        // SAFETY: the caller provides the allocation and new-size contracts.
        unsafe { System.realloc(pointer, layout, size) }
    }
}

#[global_allocator]
static GLOBAL: CountingAllocator = CountingAllocator;

fn main() {
    // Scenario fixtures place large fixed-capacity engines on the stack; the
    // worker thread gives them room on every platform.
    let handle = std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(|| hft_bench::run_suite(hft_bench::SuiteConfig::full()))
        .expect("suite worker thread");
    for line in handle.join().expect("suite finished") {
        println!("{line}");
    }
}
