//! Small 2×/4× oversampler for nonlinear stages: zero-stuffing upsampler and
//! decimator sharing a 32-tap windowed-sinc half/quarter-band low-pass. Fixed
//! state, allocation-free, bounded work per sample.

const TAPS: usize = 32;

#[derive(Clone, Debug)]
pub struct Oversampler {
    factor: usize,
    up_taps: [f32; TAPS],
    down_taps: [f32; TAPS],
    up_hist: [f32; TAPS],
    down_hist: [f32; TAPS],
    up_pos: usize,
    down_pos: usize,
}

fn design(cutoff: f32) -> [f32; TAPS] {
    let mut t = [0.0f32; TAPS];
    let center = (TAPS as f32 - 1.0) / 2.0;
    let mut sum = 0.0;
    for (k, v) in t.iter_mut().enumerate() {
        let x = k as f32 - center;
        let sinc = if x.abs() < 1e-6 { 2.0 * cutoff } else { (2.0 * std::f32::consts::PI * cutoff * x).sin() / (std::f32::consts::PI * x) };
        let w = 0.5 - 0.5 * (std::f32::consts::TAU * k as f32 / (TAPS as f32 - 1.0)).cos();
        *v = sinc * w;
        sum += *v;
    }
    for v in t.iter_mut() {
        *v /= sum;
    }
    t
}

impl Oversampler {
    pub fn new(factor: usize) -> Self {
        let factor = factor.clamp(1, 4);
        // cutoff relative to the oversampled rate: 0.45 of the base Nyquist
        let cutoff = 0.45 / factor as f32;
        Self { factor, up_taps: design(cutoff), down_taps: design(cutoff), up_hist: [0.0; TAPS], down_hist: [0.0; TAPS], up_pos: 0, down_pos: 0 }
    }
    pub fn factor(&self) -> usize {
        self.factor
    }
    pub fn reset(&mut self) {
        self.up_hist = [0.0; TAPS];
        self.down_hist = [0.0; TAPS];
        self.up_pos = 0;
        self.down_pos = 0;
    }
    /// Run `f` on each oversampled sample derived from `x` and return the decimated result.
    #[inline]
    pub fn process(&mut self, x: f32, mut f: impl FnMut(f32) -> f32) -> f32 {
        if self.factor == 1 {
            return f(x);
        }
        let mut out = 0.0;
        for k in 0..self.factor {
            // upsample: zero-stuff then low-pass (gain compensated by the factor)
            let stuffed = if k == 0 { x * self.factor as f32 } else { 0.0 };
            self.up_hist[self.up_pos] = stuffed;
            self.up_pos = (self.up_pos + 1) % TAPS;
            let mut u = 0.0;
            for (i, t) in self.up_taps.iter().enumerate() {
                u += t * self.up_hist[(self.up_pos + i) % TAPS];
            }
            let y = f(u);
            // decimate: low-pass, keep the last phase
            self.down_hist[self.down_pos] = y;
            self.down_pos = (self.down_pos + 1) % TAPS;
            if k == self.factor - 1 {
                let mut d = 0.0;
                for (i, t) in self.down_taps.iter().enumerate() {
                    d += t * self.down_hist[(self.down_pos + i) % TAPS];
                }
                out = d;
            }
        }
        out
    }
    /// Group delay of the two filters in base-rate frames.
    pub fn latency_frames(&self) -> u32 {
        if self.factor == 1 { 0 } else { ((TAPS as f32 - 1.0) / self.factor as f32).round() as u32 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_through_the_filters_keeps_a_low_frequency_sine_and_delays_it() {
        for factor in [2usize, 4] {
            let mut os = Oversampler::new(factor);
            let sr = 48_000.0;
            let out: Vec<f32> = (0..4_800).map(|i| os.process((std::f32::consts::TAU * 1000.0 * i as f32 / sr).sin(), |v| v)).collect();
            let peak = out[2_000..].iter().fold(0.0f32, |m, v| m.max(v.abs()));
            assert!((peak - 1.0).abs() < 0.03, "factor {factor}: {peak}");
            assert!(os.latency_frames() > 0);
        }
    }
}
