//! Lookahead peak limiter used for master/preview output protection.
//!
//! Gain computer with instantaneous attack over a short lookahead delay and
//! exponential release. Sample-peak (not true-peak) in this primitive; the
//! native limiter effect adds oversampled true-peak detection on top.

pub struct PeakLimiter {
    delay_l: Vec<f32>,
    delay_r: Vec<f32>,
    /// Sliding window maximum of |x| over the lookahead (monotonic deque indices).
    win: Vec<f32>,
    pos: usize,
    lookahead: usize,
    gain: f32,
    release_coeff: f32,
    ceiling: f32,
    reduction_db_max: f32,
    sample_rate: f32,
}

impl PeakLimiter {
    /// `lookahead_s` default 1 ms, `release_s` default 50 ms.
    pub fn new(sample_rate: f32, lookahead_s: f32, release_s: f32, ceiling_lin: f32) -> Self {
        let lookahead = ((lookahead_s * sample_rate).round() as usize).clamp(1, 4096);
        let mut l = Self {
            delay_l: vec![0.0; lookahead],
            delay_r: vec![0.0; lookahead],
            win: vec![0.0; lookahead],
            pos: 0,
            lookahead,
            gain: 1.0,
            release_coeff: 0.0,
            ceiling: ceiling_lin.max(1e-3),
            reduction_db_max: 0.0,
            sample_rate,
        };
        l.set_release(release_s);
        l
    }
    pub fn set_release(&mut self, release_s: f32) {
        self.release_coeff = crate::math::one_pole_coeff(release_s.max(0.001), self.sample_rate);
    }
    pub fn set_ceiling(&mut self, ceiling_lin: f32) {
        self.ceiling = ceiling_lin.clamp(1e-3, 4.0);
    }
    pub fn latency_frames(&self) -> u32 {
        self.lookahead as u32
    }
    pub fn reset(&mut self) {
        self.delay_l.fill(0.0);
        self.delay_r.fill(0.0);
        self.win.fill(0.0);
        self.gain = 1.0;
        self.pos = 0;
        self.reduction_db_max = 0.0;
    }
    /// Maximum gain reduction (dB, positive) since the last call; resets the hold.
    pub fn take_reduction_db(&mut self) -> f32 {
        std::mem::take(&mut self.reduction_db_max)
    }

    /// In-place stereo processing. RT-safe.
    pub fn process(&mut self, l: &mut [f32], r: &mut [f32]) {
        let n = self.lookahead;
        for i in 0..l.len() {
            let inl = l[i];
            let inr = r[i];
            let peak_in = inl.abs().max(inr.abs());
            // store incoming peak in window, compute window max (bounded scan of lookahead)
            self.win[self.pos] = peak_in;
            let mut wmax = 0.0f32;
            for w in &self.win {
                wmax = wmax.max(*w);
            }
            let target = if wmax > self.ceiling { self.ceiling / wmax } else { 1.0 };
            if target < self.gain {
                self.gain = target; // instantaneous attack (we have lookahead)
            } else {
                self.gain += (target - self.gain) * self.release_coeff;
            }
            let outl = self.delay_l[self.pos] * self.gain;
            let outr = self.delay_r[self.pos] * self.gain;
            self.delay_l[self.pos] = inl;
            self.delay_r[self.pos] = inr;
            self.pos += 1;
            if self.pos >= n {
                self.pos = 0;
            }
            // final safety clip (should not engage for sample peaks)
            l[i] = outl.clamp(-self.ceiling, self.ceiling);
            r[i] = outr.clamp(-self.ceiling, self.ceiling);
        }
        let red = -20.0 * self.gain.max(1e-6).log10();
        if red > self.reduction_db_max {
            self.reduction_db_max = red;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_never_exceeds_ceiling_and_passes_quiet_signal() {
        let sr = 48_000.0;
        let mut lim = PeakLimiter::new(sr, 0.001, 0.05, 0.9);
        let n = 48_000;
        let mut l: Vec<f32> = (0..n).map(|i| 3.0 * (i as f32 * 0.05).sin()).collect();
        let mut r = l.clone();
        lim.process(&mut l, &mut r);
        assert!(l.iter().all(|s| s.abs() <= 0.9 + 1e-6));
        assert!(lim.take_reduction_db() > 9.0);
        let mut q: Vec<f32> = (0..n).map(|i| 0.5 * (i as f32 * 0.05).sin()).collect();
        let mut q2 = q.clone();
        let orig = q.clone();
        let mut lim2 = PeakLimiter::new(sr, 0.001, 0.05, 0.9);
        lim2.process(&mut q, &mut q2);
        // delayed by lookahead but otherwise unchanged
        let d = lim2.latency_frames() as usize;
        for i in d..n {
            assert!((q[i] - orig[i - d]).abs() < 1e-6);
        }
        assert_eq!(lim2.take_reduction_db(), 0.0);
    }

    #[test]
    fn transient_is_caught_by_lookahead() {
        let mut lim = PeakLimiter::new(48_000.0, 0.001, 0.05, 1.0);
        let mut l = vec![0.0f32; 1000];
        l[500] = 5.0;
        let mut r = l.clone();
        lim.process(&mut l, &mut r);
        assert!(l.iter().all(|s| s.abs() <= 1.0 + 1e-6));
        assert!(l.iter().any(|s| *s > 0.9), "transient should pass at the ceiling, not be removed");
    }
}
