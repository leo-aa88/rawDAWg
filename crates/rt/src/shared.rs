//! Lock-free shared scalars for meters, counters and status.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

/// Atomic `f32` stored as bits; relaxed ordering is sufficient for meters.
#[derive(Debug, Default)]
pub struct AtomicF32(AtomicU32);

impl AtomicF32 {
    pub const fn new(v: f32) -> Self {
        Self(AtomicU32::new(v.to_bits()))
    }
    pub fn load(&self) -> f32 {
        f32::from_bits(self.0.load(Ordering::Relaxed))
    }
    pub fn store(&self, v: f32) {
        self.0.store(v.to_bits(), Ordering::Relaxed)
    }
    /// Store `v` only if it is greater than the current value (peak hold).
    pub fn fetch_max(&self, v: f32) {
        let mut cur = self.0.load(Ordering::Relaxed);
        loop {
            if f32::from_bits(cur) >= v {
                return;
            }
            match self.0.compare_exchange_weak(cur, v.to_bits(), Ordering::Relaxed, Ordering::Relaxed) {
                Ok(_) => return,
                Err(actual) => cur = actual,
            }
        }
    }
}

/// Atomic `f64` stored as bits.
#[derive(Debug, Default)]
pub struct AtomicF64(AtomicU64);

impl AtomicF64 {
    pub const fn new(v: f64) -> Self {
        Self(AtomicU64::new(v.to_bits()))
    }
    pub fn load(&self) -> f64 {
        f64::from_bits(self.0.load(Ordering::Relaxed))
    }
    pub fn store(&self, v: f64) {
        self.0.store(v.to_bits(), Ordering::Relaxed)
    }
}

/// Monotonic event counter (xruns, late events, overflows...).
#[derive(Debug, Default)]
pub struct Counter(AtomicU64);

impl Counter {
    pub const fn new() -> Self {
        Self(AtomicU64::new(0))
    }
    pub fn incr(&self) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
    pub fn add(&self, n: u64) {
        self.0.fetch_add(n, Ordering::Relaxed);
    }
    pub fn get(&self) -> u64 {
        self.0.load(Ordering::Relaxed)
    }
    pub fn reset(&self) {
        self.0.store(0, Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn peak_hold_only_increases() {
        let a = AtomicF32::new(0.2);
        a.fetch_max(0.1);
        assert_eq!(a.load(), 0.2);
        a.fetch_max(0.9);
        assert_eq!(a.load(), 0.9);
        a.store(0.0);
        assert_eq!(a.load(), 0.0);
    }
}
