//! One monotonic clock for the whole process (microseconds since its first use), shared by the
//! MIDI input (message arrival) and the engine (block start), so device timestamps can be
//! placed on the audio stream's frame clock (ARCHITECTURE: input timing).
//!
//! Call [`init`] off the audio thread before the engine runs; afterwards [`now_us`] only reads
//! an initialized cell and the monotonic clock (no lock, no allocation).

use std::sync::OnceLock;
use std::time::Instant;

static EPOCH: OnceLock<Instant> = OnceLock::new();

/// Fix the epoch (idempotent).
pub fn init() {
    let _ = EPOCH.get_or_init(Instant::now);
}

/// Microseconds since the epoch.
#[inline]
pub fn now_us() -> u64 {
    let epoch = EPOCH.get_or_init(Instant::now);
    u64::try_from(epoch.elapsed().as_micros()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_clock_is_monotonic() {
        super::init();
        let a = super::now_us();
        std::thread::sleep(std::time::Duration::from_millis(2));
        let b = super::now_us();
        assert!(b >= a + 1500, "{a} {b}");
    }
}
