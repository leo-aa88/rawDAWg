//! T-RT harness self-test: the guard allocator must detect allocations inside
//! an RT scope and report zero for allocation-free code.
#![cfg(feature = "alloc-guard")]

use rawdawg_rt::alloc_guard::{GuardAllocator, measure_rt};

#[global_allocator]
static GLOBAL: GuardAllocator = GuardAllocator;

#[test]
fn detects_allocation_inside_scope() {
    let (_, n) = measure_rt(|| {
        let v: Vec<u8> = Vec::with_capacity(64);
        std::hint::black_box(v);
    });
    assert!(n >= 1, "expected to observe the Vec allocation, got {n}");
}

#[test]
fn allocation_free_code_reports_zero() {
    let mut buf = vec![0f32; 1024];
    let (_, n) = measure_rt(|| {
        for (i, s) in buf.iter_mut().enumerate() {
            *s = (i as f32).sin();
        }
        std::hint::black_box(&buf);
    });
    assert_eq!(n, 0);
}

#[test]
fn allocation_outside_scope_not_counted() {
    let (_, n) = measure_rt(|| 1 + 1);
    let _v = [1u8; 10];
    assert_eq!(n, 0);
}
