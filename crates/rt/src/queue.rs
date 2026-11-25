//! Bounded single-producer single-consumer queue with overflow accounting.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// Shared statistics for one queue.
#[derive(Debug, Default)]
pub struct QueueStats {
    pushed: AtomicU64,
    popped: AtomicU64,
    overflowed: AtomicU64,
}

impl QueueStats {
    pub fn pushed(&self) -> u64 {
        self.pushed.load(Ordering::Relaxed)
    }
    pub fn popped(&self) -> u64 {
        self.popped.load(Ordering::Relaxed)
    }
    /// Number of messages rejected because the queue was full. Surfaced to the UI.
    pub fn overflowed(&self) -> u64 {
        self.overflowed.load(Ordering::Relaxed)
    }
}

/// Create a bounded SPSC queue of `capacity` slots (allocated now, never again).
pub struct ControlQueue;

impl ControlQueue {
    pub fn create<T>(capacity: usize) -> (QueueProducer<T>, QueueConsumer<T>) {
        assert!(capacity > 0, "queue capacity must be positive");
        let (p, c) = rtrb::RingBuffer::new(capacity);
        let stats = Arc::new(QueueStats::default());
        (QueueProducer { inner: p, stats: stats.clone() }, QueueConsumer { inner: c, stats })
    }
}

/// Non-RT or RT producer side. `push` never blocks.
pub struct QueueProducer<T> {
    inner: rtrb::Producer<T>,
    stats: Arc<QueueStats>,
}

impl<T> QueueProducer<T> {
    /// Push a value; on a full queue the value is returned and the overflow counter increments.
    pub fn push(&mut self, value: T) -> Result<(), T> {
        match self.inner.push(value) {
            Ok(()) => {
                self.stats.pushed.fetch_add(1, Ordering::Relaxed);
                Ok(())
            }
            Err(rtrb::PushError::Full(v)) => {
                self.stats.overflowed.fetch_add(1, Ordering::Relaxed);
                Err(v)
            }
        }
    }
    pub fn free_slots(&self) -> usize {
        self.inner.slots()
    }
    pub fn capacity(&self) -> usize {
        self.inner.buffer().capacity()
    }
    pub fn stats(&self) -> &Arc<QueueStats> {
        &self.stats
    }
    pub fn is_consumer_alive(&self) -> bool {
        !self.inner.is_abandoned()
    }
}

/// Consumer side (usually the audio thread). `pop` never blocks or allocates.
pub struct QueueConsumer<T> {
    inner: rtrb::Consumer<T>,
    stats: Arc<QueueStats>,
}

impl<T> QueueConsumer<T> {
    pub fn pop(&mut self) -> Option<T> {
        match self.inner.pop() {
            Ok(v) => {
                self.stats.popped.fetch_add(1, Ordering::Relaxed);
                Some(v)
            }
            Err(rtrb::PopError::Empty) => None,
        }
    }
    /// Number of items ready to be popped.
    pub fn len(&self) -> usize {
        self.inner.slots()
    }
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }
    pub fn stats(&self) -> &Arc<QueueStats> {
        &self.stats
    }
    pub fn is_producer_alive(&self) -> bool {
        !self.inner.is_abandoned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overflow_is_counted_not_silently_lost() {
        let (mut p, mut c) = ControlQueue::create::<u32>(2);
        assert!(p.push(1).is_ok());
        assert!(p.push(2).is_ok());
        assert_eq!(p.push(3), Err(3));
        assert_eq!(p.stats().overflowed(), 1);
        assert_eq!(c.pop(), Some(1));
        assert_eq!(c.pop(), Some(2));
        assert_eq!(c.pop(), None);
        assert_eq!(c.stats().popped(), 2);
        assert_eq!(c.stats().pushed(), 2);
    }

    #[test]
    fn cross_thread_delivery_preserves_order() {
        let (mut p, mut c) = ControlQueue::create::<u64>(64);
        let t = std::thread::spawn(move || {
            let mut i = 0u64;
            while i < 10_000 {
                if p.push(i).is_ok() {
                    i += 1;
                } else {
                    std::thread::yield_now();
                }
            }
        });
        let mut expected = 0u64;
        while expected < 10_000 {
            if let Some(v) = c.pop() {
                assert_eq!(v, expected);
                expected += 1;
            } else {
                std::thread::yield_now();
            }
        }
        t.join().unwrap();
    }
}
