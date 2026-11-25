//! Real-time ownership primitives (ARCHITECTURE.md §3).
//!
//! * [`ControlQueue`]: bounded SPSC queue with overflow counting; the producer
//!   never blocks and the consumer (audio thread) never allocates.
//! * [`Handoff`]: bounded publish/retire channel for prepared plans. The
//!   worker publishes a new plan; the audio thread swaps it in at a block
//!   boundary and sends the old one back for retirement on the worker.
//!   The swap is refused when the retirement queue has no capacity, so the
//!   audio thread never drops an object (which would deallocate).
//! * [`Shared`]: lock-free atomics for meters/status.
//! * [`rt_scope`]: thread-local real-time flag plus an optional instrumented
//!   allocator (`alloc-guard` feature) that counts allocations inside the scope.

pub mod alloc_guard;
pub mod handoff;
pub mod queue;
pub mod shared;

pub use alloc_guard::{RtScope, allocations_in_scope, enter_rt, exit_rt, is_rt_thread};
pub use handoff::{Handoff, HandoffConsumer, HandoffProducer, PublishError, RetiredItem};
pub use queue::{ControlQueue, QueueConsumer, QueueProducer};
pub use shared::{AtomicF32, AtomicF64, Counter};
