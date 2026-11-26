//! Timed live input (ARCHITECTURE: input timing): where on the audio stream a timestamped
//! event (MIDI) plays. The engine stamps each block with its stream frame and the process
//! clock ([`crate::EngineStatus::block_start_frame`], `block_wall_us`, `block_frames`); an
//! event sent at process time `wall` sits at `frame_at(wall)` on that clock and plays a constant
//! latency later. The latency is a jitter buffer: whatever the delivery path costs (the device
//! driver, the MIDI thread, the UI frame that polls it, the next block's command drain), it
//! rises at once to what an event needed and falls back slowly, so notes keep their spacing.

/// Never more latency than two blocks plus this; events that need more play as soon as possible
/// and count as late.
pub const MAX_EXTRA_LATENCY_S: f64 = 0.040;
/// Safety margin on top of what an event needed.
const MARGIN_S: f64 = 0.002;
/// How fast the latency falls back once the delivery path got faster (0.5 ms per second).
const FALL_PER_S: f64 = 0.0005;

/// Stream frame at process-clock time `wall_us`, given the last block's stamp.
pub fn frame_at(block_start_frame: u64, block_wall_us: u64, sample_rate: u32, wall_us: u64) -> i64 {
    let dt_us = wall_us as i64 - block_wall_us as i64;
    block_start_frame as i64 + (dt_us as f64 * f64::from(sample_rate) / 1_000_000.0).round() as i64
}

/// The earliest stream frame a command sent now is sure to reach the engine for: the block after
/// the next one (the next block may already be draining its commands while this one is sent).
pub fn earliest_frame(block_start_frame: u64, block_frames: u32) -> i64 {
    block_start_frame as i64 + 2 * i64::from(block_frames.max(1))
}

/// Where on the stream a timed event plays: `wall_us` placed with the last block's stamp, plus
/// the buffer's latency.
pub fn place_event(jb: &mut JitterBuffer, block_start_frame: u64, block_wall_us: u64, block_frames: u32, sample_rate: u32, wall_us: u64, now_us: u64) -> u64 {
    let frame = frame_at(block_start_frame, block_wall_us, sample_rate, wall_us);
    let cap = 2.0 * f64::from(block_frames.max(1)) + MAX_EXTRA_LATENCY_S * f64::from(sample_rate);
    jb.place(frame, earliest_frame(block_start_frame, block_frames), now_us, sample_rate, cap)
}

/// The adaptive latency of one input (per device).
#[derive(Clone, Copy, Debug, Default)]
pub struct JitterBuffer {
    latency: f64,
    last_us: u64,
}

impl JitterBuffer {
    /// The stream frame for an event that sits at `event_frame` on the stream clock and cannot
    /// play before `earliest`: `event_frame` plus the buffer's latency, which first rises (at
    /// once, capped) to cover this event and otherwise falls back slowly.
    pub fn place(&mut self, event_frame: i64, earliest: i64, now_us: u64, sample_rate: u32, cap_frames: f64) -> u64 {
        let sr = f64::from(sample_rate);
        let needed = ((earliest - event_frame) as f64 + MARGIN_S * sr).max(0.0);
        let elapsed_s = now_us.saturating_sub(self.last_us) as f64 / 1_000_000.0;
        self.last_us = now_us;
        let fallen = (self.latency - elapsed_s * FALL_PER_S * sr).max(0.0);
        self.latency = needed.max(fallen).min(cap_frames);
        (event_frame + self.latency.round() as i64).max(0) as u64
    }

    /// Current latency in frames.
    pub fn latency_frames(&self) -> f64 {
        self.latency
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: u32 = 48_000;

    /// A device rendering `block` frames per callback in real time, a MIDI phrase every 2.5 ms
    /// (exact send times), delivered 0.3–3 ms late, polled by a 60 Hz UI loop. Returns, per
    /// message, its target frame and whether the engine would see it in time.
    fn simulate(block: u32, seconds: f64) -> Vec<(u64, i64, bool)> {
        let block_us = f64::from(block) / f64::from(SR) * 1e6;
        let mut jb = JitterBuffer::default();
        let mut out = Vec::new();
        let mut rng = 7u64;
        let n = (seconds * 400.0) as u64;
        let mut msgs: Vec<(u64, u64)> = (0..n)
            .map(|k| {
                let sent = 1_000_000 + k * 2_500;
                rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                (sent, sent + 300 + (rng >> 33) % 2_700)
            })
            .collect();
        msgs.sort_by_key(|m| m.1);
        let mut i = 0;
        let mut poll = 1_000_000u64;
        while i < msgs.len() {
            poll += 16_667;
            // the last block that started before the poll
            let b = ((poll - 1_000_000) as f64 / block_us).floor() as u64;
            let (bstart, bwall) = (b * u64::from(block), 1_000_000 + (b as f64 * block_us) as u64);
            while i < msgs.len() && msgs[i].1 <= poll {
                let (sent, _) = msgs[i];
                let frame = frame_at(bstart, bwall, SR, sent);
                let target = place_event(&mut jb, bstart, bwall, block, SR, sent, poll);
                // the engine sees the command at the next block start after the poll
                let next_block = ((poll - 1_000_000) as f64 / block_us).ceil() as u64 * u64::from(block);
                out.push((sent, target as i64 - frame, target >= next_block));
                i += 1;
            }
        }
        out
    }

    #[test]
    fn notes_are_on_time_with_small_and_large_blocks_at_a_steady_latency() {
        for block in [64u32, 256, 1024, 2048] {
            let res = simulate(block, 10.0);
            // after the first second nothing is late, and the latency only breathes by the slow
            // fall-back between worst-case deliveries (≤ 2.5 ms, against 16 ms of poll jitter)
            let steady = &res[400..];
            assert!(steady.iter().all(|r| r.2), "block {block}: {} late", steady.iter().filter(|r| !r.2).count());
            let (lo, hi) = steady.iter().fold((i64::MAX, i64::MIN), |(lo, hi), r| (lo.min(r.1), hi.max(r.1)));
            assert!(hi - lo <= 120, "block {block}: latency varies by {} frames", hi - lo);
            // and it stays bounded: two blocks, a UI frame of polling, delivery and the margin
            let bound = 2 * i64::from(block) + (0.017 * 48_000.0) as i64 + 150 + 96 + 48;
            assert!(hi <= bound, "block {block}: latency {hi} frames");
        }
    }

    #[test]
    fn the_latency_falls_back_after_a_stall_and_is_capped() {
        let mut jb = JitterBuffer::default();
        // a 200 ms UI stall with 256-frame blocks: capped at two blocks + 40 ms (that event is late)
        let cap = 2.0 * 256.0 + MAX_EXTRA_LATENCY_S * 48_000.0;
        let t = jb.place(0, 9_600, 1_000_000, SR, cap);
        assert_eq!(t, cap as u64);
        // ten seconds of quick delivery later it has fallen by 5 ms
        let t2 = jb.place(480_000, 480_000 + 500, 11_000_000, SR, cap);
        assert!((t2 as i64 - 480_000 - (cap as i64 - 240)).abs() <= 1, "{}", t2 as i64 - 480_000);
        assert_eq!(frame_at(1_000, 2_000_000, SR, 2_001_000), 1_048);
        assert_eq!(frame_at(1_000, 2_000_000, SR, 1_999_000), 952, "events before the stamp land before it");
    }
}
