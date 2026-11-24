//! A counting global allocator for tests (ADR-0003, ADR-0014).
//!
//! nami allocates only during initialisation. This crate proves it: a test binary installs
//! [`CountingAllocator`] as its global allocator, and [`assert_no_allocations`] fails if the
//! code it runs allocates, reallocates or zero-allocates even once.
//!
//! ```ignore
//! #[global_allocator]
//! static ALLOCATOR: nami_alloc_guard::CountingAllocator = nami_alloc_guard::CountingAllocator;
//!
//! let solver = Solver::init(&config);              // allocation is allowed here
//! nami_alloc_guard::assert_no_allocations(|| {
//!     solver.step();                                 // and forbidden here
//! });
//! ```
//!
//! Counts are kept per thread, so tests running in parallel threads do not disturb each other.
//! This is the only crate in nami that may contain `unsafe` code (ADR-0014).

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

thread_local! {
    // Const-initialised and without a destructor, so reading it never allocates.
    static ALLOCATIONS: Cell<u64> = const { Cell::new(0) };
}

/// Global allocator that forwards to the system allocator and counts allocations per thread.
#[derive(Clone, Copy, Debug)]
pub struct CountingAllocator;

fn count_allocation() {
    // `try_with` fails only while the thread is shutting down; those allocations are not counted.
    let _counted = ALLOCATIONS.try_with(|count| count.set(count.get() + 1));
}

#[expect(unsafe_code, reason = "a global allocator must implement the unsafe GlobalAlloc trait")]
// SAFETY: every method forwards to `System` with the caller's layout and pointer unchanged, so
// the `GlobalAlloc` contract holds exactly when it holds for `System`. Counting touches only a
// thread-local `Cell` and never allocates.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        count_allocation();
        // SAFETY: forwarded unchanged; the caller upholds `GlobalAlloc::alloc`'s contract.
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        count_allocation();
        // SAFETY: forwarded unchanged; the caller upholds `GlobalAlloc::alloc_zeroed`'s contract.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        count_allocation();
        // SAFETY: forwarded unchanged; the caller upholds `GlobalAlloc::realloc`'s contract.
        unsafe { System.realloc(pointer, layout, size) }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        // SAFETY: forwarded unchanged; the caller upholds `GlobalAlloc::dealloc`'s contract.
        unsafe { System.dealloc(pointer, layout) }
    }
}

/// Number of allocations made on the current thread since it started.
///
/// Meaningful only in a binary whose global allocator is [`CountingAllocator`].
#[must_use]
pub fn allocations_on_this_thread() -> u64 {
    ALLOCATIONS.with(Cell::get)
}

/// Runs `operation` and asserts that it did not allocate on this thread.
///
/// Use it around everything that runs after initialisation: a time step, a boundary fill, a
/// diagnostic reduction.
pub fn assert_no_allocations<R>(operation: impl FnOnce() -> R) -> R {
    let before = allocations_on_this_thread();
    let result = operation();
    let after = allocations_on_this_thread();
    assert!(after >= before, "allocation count never decreases");
    assert_eq!(after - before, 0, "allocated after init; allocate in init only (ADR-0003)");
    result
}
