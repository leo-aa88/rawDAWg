//! Lock-free scope buffer: the audio thread stores samples with relaxed atomic
//! writes into a fixed ring; the UI copies a snapshot. Tearing between writer
//! and reader is acceptable for a visual scope and never blocks either side.

use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

pub struct ScopeBuffer {
    samples: Box<[AtomicU32]>,
    pos: AtomicUsize,
}

impl std::fmt::Debug for ScopeBuffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ScopeBuffer(len={})", self.samples.len())
    }
}

impl ScopeBuffer {
    pub fn new(len: usize) -> Self {
        let len = len.max(16);
        Self { samples: (0..len).map(|_| AtomicU32::new(0)).collect(), pos: AtomicUsize::new(0) }
    }
    pub fn len(&self) -> usize {
        self.samples.len()
    }
    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }
    /// RT: push a block (mono) of samples.
    #[inline]
    pub fn push(&self, block: &[f32]) {
        let n = self.samples.len();
        let mut p = self.pos.load(Ordering::Relaxed);
        for s in block {
            self.samples[p].store(s.to_bits(), Ordering::Relaxed);
            p += 1;
            if p >= n {
                p = 0;
            }
        }
        self.pos.store(p, Ordering::Release);
    }
    /// UI: copy the most recent `out.len()` samples in chronological order.
    pub fn snapshot(&self, out: &mut [f32]) {
        let n = self.samples.len();
        let end = self.pos.load(Ordering::Acquire);
        let count = out.len().min(n);
        let start = (end + n - count) % n;
        for (i, o) in out.iter_mut().take(count).enumerate() {
            *o = f32::from_bits(self.samples[(start + i) % n].load(Ordering::Relaxed));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn snapshot_returns_latest_in_order() {
        let s = ScopeBuffer::new(8);
        s.push(&[1.0, 2.0, 3.0, 4.0, 5.0, 6.0]);
        let mut out = [0.0; 4];
        s.snapshot(&mut out);
        assert_eq!(out, [3.0, 4.0, 5.0, 6.0]);
        s.push(&[7.0, 8.0, 9.0, 10.0]);
        s.snapshot(&mut out);
        assert_eq!(out, [7.0, 8.0, 9.0, 10.0]);
    }
}
