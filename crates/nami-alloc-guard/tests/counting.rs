//! The counting allocator sees allocations, and the guard rejects them.

use nami_alloc_guard::{CountingAllocator, allocations_on_this_thread, assert_no_allocations};
use std::hint::black_box;

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

// Goal: a heap allocation on this thread increases the count by exactly one.
#[test]
fn allocations_are_counted() {
    let before = allocations_on_this_thread();
    let buffer: Vec<f64> = black_box(Vec::with_capacity(16));
    let after = allocations_on_this_thread();
    assert_eq!(after - before, 1);
    drop(buffer);
}

// Goal: allocation-free work passes the guard and returns its result.
#[test]
fn allocation_free_work_passes() {
    let values = [1.0_f64, 2.0, 3.0];
    let total = assert_no_allocations(|| black_box(values).iter().sum::<f64>());
    assert_eq!(total.to_bits(), 6.0_f64.to_bits());
}

// Goal: any allocation inside the guard fails the test with the ADR reference.
#[test]
#[should_panic(expected = "allocated after init")]
fn allocation_inside_the_guard_fails() {
    let capacity = assert_no_allocations(|| black_box(Vec::<f64>::with_capacity(16)).capacity());
    assert_eq!(capacity, 16, "unreachable: the guard panics first");
}
