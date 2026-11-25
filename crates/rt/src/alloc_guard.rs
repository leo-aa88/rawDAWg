//! Thread-local real-time flag and an optional instrumented global allocator.
//!
//! With the `alloc-guard` feature the crate exposes [`GuardAllocator`], a
//! `GlobalAlloc` wrapper over the system allocator that counts every
//! allocation, reallocation and deallocation performed while the current thread
//! is inside an [`RtScope`]. Test binaries install it with
//! `#[global_allocator]` and assert the count stays at zero across the owned
//! render path (T-RT). In production builds the flag is still available for
//! debug assertions but the allocator is the plain system allocator.

use std::cell::Cell;

thread_local! {
    static RT_DEPTH: Cell<u32> = const { Cell::new(0) };
    static RT_ALLOCS: Cell<u64> = const { Cell::new(0) };
}

/// Mark the current thread as real-time (nestable).
pub fn enter_rt() {
    RT_DEPTH.with(|d| d.set(d.get() + 1));
}

/// Leave the real-time section.
pub fn exit_rt() {
    RT_DEPTH.with(|d| d.set(d.get().saturating_sub(1)));
}

/// Whether the current thread is inside a real-time section.
pub fn is_rt_thread() -> bool {
    RT_DEPTH.with(|d| d.get() > 0)
}

/// Allocation events (alloc + realloc + dealloc) observed on this thread while
/// inside a real-time section. Always zero unless the `alloc-guard` feature
/// installs [`GuardAllocator`] as the global allocator.
pub fn allocations_in_scope() -> u64 {
    RT_ALLOCS.with(|c| c.get())
}

/// Reset the per-thread allocation counter.
pub fn reset_allocation_count() {
    RT_ALLOCS.with(|c| c.set(0));
}

/// RAII guard for a real-time section.
pub struct RtScope {
    _private: (),
}

impl RtScope {
    pub fn enter() -> Self {
        enter_rt();
        RtScope { _private: () }
    }
}

impl Drop for RtScope {
    fn drop(&mut self) {
        exit_rt();
    }
}

/// Run `f` inside a real-time section and return `(result, allocations observed)`.
pub fn measure_rt<T>(f: impl FnOnce() -> T) -> (T, u64) {
    reset_allocation_count();
    let before = allocations_in_scope();
    let out = {
        let _scope = RtScope::enter();
        f()
    };
    (out, allocations_in_scope() - before)
}

#[cfg(feature = "alloc-guard")]
pub use guard::GuardAllocator;

#[cfg(feature = "alloc-guard")]
mod guard {
    use super::{RT_ALLOCS, RT_DEPTH};
    use std::alloc::{GlobalAlloc, Layout, System};

    /// Counting allocator; see module docs.
    pub struct GuardAllocator;

    impl GuardAllocator {
        #[inline]
        fn note() {
            // `try_with` because thread-locals may be torn down during thread exit.
            let _ = RT_DEPTH.try_with(|d| {
                if d.get() > 0 {
                    let _ = RT_ALLOCS.try_with(|c| c.set(c.get() + 1));
                }
            });
        }
    }

    // SAFETY: every method forwards to `System` with identical arguments and
    // only additionally touches thread-local counters, which do not allocate.
    unsafe impl GlobalAlloc for GuardAllocator {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            Self::note();
            unsafe { System.alloc(layout) }
        }
        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            Self::note();
            unsafe { System.dealloc(ptr, layout) }
        }
        unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
            Self::note();
            unsafe { System.alloc_zeroed(layout) }
        }
        unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
            Self::note();
            unsafe { System.realloc(ptr, layout, new_size) }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_flag_nests() {
        assert!(!is_rt_thread());
        {
            let _a = RtScope::enter();
            assert!(is_rt_thread());
            {
                let _b = RtScope::enter();
                assert!(is_rt_thread());
            }
            assert!(is_rt_thread());
        }
        assert!(!is_rt_thread());
    }
}
