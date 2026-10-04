use super::*;
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
struct Counting;
thread_local! { static ALLOCATIONS: Cell<usize> = const { Cell::new(0) }; }
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.with(|n| n.set(n.get() + 1));
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        ALLOCATIONS.with(|n| n.set(n.get() + 1));
        unsafe { System.realloc(ptr, layout, size) }
    }
}
#[global_allocator]
static ALLOCATOR: Counting = Counting;

#[test]
fn atomicity_and_zero_allocations_after_warmup() {
    let mut banks = Banks::new(
        vec![vec![93; 638]; 2],
        vec![(0, 0, 7, 505), (1, 505, 0, 8)],
        513,
    )
    .unwrap();
    banks.floats.fill(1.9);
    banks.prepare(1).unwrap();
    banks.commit();
    let committed = banks.committed.clone();
    let staging = banks.staging.clone();
    banks.floats[512] = f32::NAN;
    assert!(banks.prepare(1).is_err());
    assert_eq!(banks.committed, committed);
    assert_eq!(banks.staging, staging);
    banks.floats.fill(12.0);
    banks.bytes.fill(12);
    banks.doubles.fill(12.0);
    let before = ALLOCATIONS.with(Cell::get);
    for kind in 0..=2 {
        for _ in 0..1000 {
            banks.prepare(kind).unwrap();
            banks.commit();
        }
    }
    assert_eq!(ALLOCATIONS.with(Cell::get), before);
    assert_eq!(banks.committed[0][125], 93);
    assert_eq!(banks.committed[0][133], 12);
}

pub(crate) fn allocation_count() -> usize {
    ALLOCATIONS.with(Cell::get)
}

#[test]
fn failed_conversion_never_scatters_any_packet() {
    for kind in [1, 2] {
        for position in [0, 7, 512] {
            for invalid in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -1.0, 256.0] {
                let mut banks = Banks::new(
                    vec![vec![93; 638]; 2],
                    vec![(0, 0, 7, 505), (1, 505, 0, 8)],
                    513,
                )
                .unwrap();
                banks.floats.fill(17.9);
                banks.doubles.fill(17.9);
                let staging = banks.staging.clone();
                let committed = banks.committed.clone();
                banks.floats[position] = invalid as f32;
                banks.doubles[position] = invalid;
                assert!(banks.prepare(kind).is_err());
                assert_eq!(banks.staging, staging);
                assert_eq!(banks.committed, committed);
                banks.floats[position] = -0.5;
                banks.doubles[position] = -0.5;
                banks.prepare(kind).unwrap();
                assert_eq!(banks.channels[position], 0);
                assert_eq!(banks.channels[(position + 1) % 513], 17);
            }
        }
    }
}
