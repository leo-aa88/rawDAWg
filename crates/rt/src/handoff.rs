//! Bounded publish/retire handoff for prepared real-time plans.

use crate::queue::{ControlQueue, QueueConsumer, QueueProducer};

/// A retired object returned to the worker for deallocation.
pub struct RetiredItem<T>(pub T);

#[derive(PartialEq, Eq)]
pub enum PublishError<T> {
    /// The publish queue is full; the caller keeps the plan and may retry later.
    Full(T),
}

impl<T> std::fmt::Debug for PublishError<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PublishError::Full(_) => f.write_str("PublishError::Full(..)"),
        }
    }
}

/// Create a handoff with `depth` pending publishes and `depth` pending retirements.
pub struct Handoff;

impl Handoff {
    pub fn create<T>(depth: usize) -> (HandoffProducer<T>, HandoffConsumer<T>) {
        let (pub_p, pub_c) = ControlQueue::create::<T>(depth);
        let (ret_p, ret_c) = ControlQueue::create::<RetiredItem<T>>(depth);
        (HandoffProducer { publish: pub_p, retired: ret_c, generation: 0 }, HandoffConsumer { publish: pub_c, retired: ret_p, refused_swaps: 0 })
    }
}

/// Worker/coordinator side.
pub struct HandoffProducer<T> {
    publish: QueueProducer<T>,
    retired: QueueConsumer<RetiredItem<T>>,
    generation: u64,
}

impl<T> HandoffProducer<T> {
    /// Offer a new plan to the audio thread. Only succeeds if there is room.
    pub fn publish(&mut self, plan: T) -> Result<u64, PublishError<T>> {
        match self.publish.push(plan) {
            Ok(()) => {
                self.generation += 1;
                Ok(self.generation)
            }
            Err(p) => Err(PublishError::Full(p)),
        }
    }
    /// Collect retired plans; dropping them here (off the audio thread) deallocates.
    pub fn collect_retired(&mut self) -> Vec<T> {
        let mut out = Vec::new();
        while let Some(RetiredItem(t)) = self.retired.pop() {
            out.push(t);
        }
        out
    }
    pub fn pending_publishes(&self) -> usize {
        self.publish.capacity() - self.publish.free_slots()
    }
}

/// Audio-thread side.
pub struct HandoffConsumer<T> {
    publish: QueueConsumer<T>,
    retired: QueueProducer<RetiredItem<T>>,
    refused_swaps: u64,
}

impl<T> HandoffConsumer<T> {
    /// At a block boundary: if a new plan is available **and** the old one can
    /// be retired without blocking or dropping, swap and return `true`.
    /// Never deallocates on this thread.
    pub fn try_swap(&mut self, current: &mut T) -> bool {
        if self.publish.is_empty() {
            return false;
        }
        if self.retired.free_slots() == 0 {
            self.refused_swaps += 1;
            return false;
        }
        let Some(mut incoming) = self.publish.pop() else { return false };
        std::mem::swap(current, &mut incoming);
        // Capacity was checked above; a failure here would mean a logic error,
        // but we still must not drop on the RT thread: swap back instead.
        if let Err(RetiredItem(mut old)) = self.retired.push(RetiredItem(incoming)) {
            std::mem::swap(current, &mut old);
            // `old` is now the plan we failed to install; try to return it to
            // the retirement queue is impossible (full), so we must keep it.
            // This branch is unreachable given the free_slots check above.
            self.refused_swaps += 1;
            // Leak rather than deallocate on RT; worker will notice refused swaps.
            std::mem::forget(old);
            return false;
        }
        true
    }

    /// Install the latest available plan, retiring all intermediate ones.
    /// Returns the number of swaps performed.
    pub fn drain_to_latest(&mut self, current: &mut T) -> usize {
        let mut swaps = 0;
        while self.try_swap(current) {
            swaps += 1;
        }
        swaps
    }

    pub fn refused_swaps(&self) -> u64 {
        self.refused_swaps
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Debug)]
    struct Plan {
        id: u32,
        drops: Arc<AtomicUsize>,
    }
    impl Drop for Plan {
        fn drop(&mut self) {
            self.drops.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn swap_installs_new_plan_and_retires_old_off_rt() {
        let drops = Arc::new(AtomicUsize::new(0));
        let (mut w, mut rt) = Handoff::create::<Plan>(2);
        let mut current = Plan { id: 0, drops: drops.clone() };
        assert!(!rt.try_swap(&mut current));
        w.publish(Plan { id: 1, drops: drops.clone() }).unwrap();
        assert!(rt.try_swap(&mut current));
        assert_eq!(current.id, 1);
        assert_eq!(drops.load(Ordering::SeqCst), 0, "nothing dropped on RT side");
        let retired = w.collect_retired();
        assert_eq!(retired.len(), 1);
        assert_eq!(retired[0].id, 0);
        drop(retired);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn swap_refused_when_retirement_queue_full() {
        let drops = Arc::new(AtomicUsize::new(0));
        let (mut w, mut rt) = Handoff::create::<Plan>(1);
        let mut current = Plan { id: 0, drops: drops.clone() };
        w.publish(Plan { id: 1, drops: drops.clone() }).unwrap();
        assert!(rt.try_swap(&mut current));
        // retirement queue (depth 1) now holds plan 0; worker has not collected it
        w.publish(Plan { id: 2, drops: drops.clone() }).unwrap();
        assert!(!rt.try_swap(&mut current), "must refuse: no retirement capacity");
        assert_eq!(current.id, 1);
        assert_eq!(rt.refused_swaps(), 1);
        assert_eq!(w.collect_retired().len(), 1);
        assert!(rt.try_swap(&mut current));
        assert_eq!(current.id, 2);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn publish_is_bounded() {
        let drops = Arc::new(AtomicUsize::new(0));
        let (mut w, _rt) = Handoff::create::<Plan>(1);
        assert!(w.publish(Plan { id: 1, drops: drops.clone() }).is_ok());
        match w.publish(Plan { id: 2, drops: drops.clone() }) {
            Err(PublishError::Full(p)) => assert_eq!(p.id, 2),
            Ok(_) => panic!("publish beyond depth must fail"),
        }
        assert_eq!(w.pending_publishes(), 1);
    }

    #[test]
    fn drain_to_latest_retires_intermediates() {
        let drops = Arc::new(AtomicUsize::new(0));
        let (mut w, mut rt) = Handoff::create::<Plan>(4);
        let mut current = Plan { id: 0, drops: drops.clone() };
        for id in 1..=3 {
            w.publish(Plan { id, drops: drops.clone() }).unwrap();
        }
        assert_eq!(rt.drain_to_latest(&mut current), 3);
        assert_eq!(current.id, 3);
        let retired: Vec<u32> = w.collect_retired().iter().map(|p| p.id).collect();
        assert_eq!(retired, vec![0, 1, 2]);
    }
}
