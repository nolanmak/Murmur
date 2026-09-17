//! A Core Audio property listener may not allocate, and this asserts it.
//!
//! `AudioObjectAddPropertyListenerBlock` delivers on a Core Audio-owned
//! dispatch queue. It is not the IOProc's real-time thread, but it is a thread
//! the HAL is waiting on: blocking it stalls device notifications system-wide,
//! and a `malloc` there can block on the allocator's lock behind any other
//! thread in the process. So the listener does two atomic read-modify-writes
//! and returns, and the decision-making happens on the supervisor's own
//! thread.
//!
//! The detector has to be installed by *this* binary. A `#[cfg(test)]` global
//! allocator inside the library would not apply to an integration test, which
//! links the library compiled without `cfg(test)` — the assertions would pass
//! while measuring nothing.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use fotw_audio::device_change::{DeviceChangeKind, DeviceChangeSignal};

thread_local! {
    // `const` init is required, not an optimisation: a lazily-initialised
    // thread_local allocates on first touch, re-entering the allocator being
    // hooked.
    static ALLOCS: Cell<usize> = const { Cell::new(0) };
}

/// Counts allocations per thread. Per-thread rather than global because
/// `cargo test` runs these in parallel and a global counter would make every
/// test observe every other test's allocations.
struct Counting;

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCS.try_with(|c| c.set(c.get() + 1)).ok();
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        ALLOCS.try_with(|c| c.set(c.get() + 1)).ok();
        unsafe { System.realloc(ptr, layout, new_size) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        ALLOCS.try_with(|c| c.set(c.get() + 1)).ok();
        unsafe { System.alloc_zeroed(layout) }
    }
}

#[global_allocator]
static ALLOC: Counting = Counting;

fn allocations() -> usize {
    ALLOCS.with(Cell::get)
}

/// If this fails, every other assertion in this file is vacuous.
#[test]
fn the_detector_is_actually_installed() {
    let before = allocations();
    let v: Vec<u8> = Vec::with_capacity(64);
    std::hint::black_box(&v);
    assert!(
        allocations() > before,
        "the counting allocator is not hooked up"
    );
}

#[test]
fn raising_a_device_change_allocates_nothing() {
    // Everything the listener touches is allocated up front, exactly as the
    // real one is: the Arc is created when the listener is installed.
    let signal = DeviceChangeSignal::new();
    // Warm every path once so nothing lazily initialises inside the window.
    signal.raise(DeviceChangeKind::DefaultOutput);
    let _ = signal.take();

    let before = allocations();
    for _ in 0..1_000 {
        signal.raise(DeviceChangeKind::DefaultOutput);
        signal.raise(DeviceChangeKind::DefaultInput);
        signal.raise(DeviceChangeKind::DeviceList);
        signal.raise(DeviceChangeKind::StreamFormat);
    }
    assert_eq!(
        allocations(),
        before,
        "the Core Audio listener thread must not malloc"
    );

    // Taking is the supervisor's side and is equally cheap, which is what lets
    // it be polled from a loop rather than a dedicated thread.
    let before = allocations();
    let taken = signal.take();
    assert_eq!(allocations(), before);
    assert!(taken.contains(DeviceChangeKind::DeviceList));
}
