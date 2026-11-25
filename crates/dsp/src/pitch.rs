//! Two-tap granular pitch shifter: two sawtooth-modulated taps on a delay line, half a
//! window apart, crossfaded with sine windows. Exact pitch ratio, constant mean delay,
//! duration preserved. Used by the pitch/formant effect and the shimmer reverb.

use crate::delayline::DelayLine;

pub struct TapShifter {
    line: DelayLine,
    phase: f32,
    /// Grain window in samples.
    pub window: f32,
    /// Output/input frequency ratio (1 = unity).
    pub ratio: f32,
    /// Extra fixed delay in samples added to both taps (keeps the mean delay constant
    /// when the window changes).
    pub offset: f32,
}

impl TapShifter {
    /// `cap` is the delay-line capacity in samples (≥ window + offset + 2).
    pub fn new(cap: usize, window: f32) -> Self {
        Self { line: DelayLine::new(cap.max(4)), phase: 0.0, window: window.max(2.0), ratio: 1.0, offset: 0.0 }
    }

    pub fn reset(&mut self) {
        self.line.reset();
        self.phase = 0.0;
    }

    /// Mean delay through the shifter in samples.
    pub fn mean_delay(&self) -> f32 {
        self.offset + self.window * 0.5
    }

    #[inline]
    pub fn process(&mut self, x: f32) -> f32 {
        self.line.push(x);
        let inc = (1.0 - self.ratio) / self.window; // phase moves backwards for upward shifts
        self.phase = (self.phase + inc).rem_euclid(1.0);
        let p2 = (self.phase + 0.5).rem_euclid(1.0);
        let max_d = (self.line.capacity() - 2) as f32;
        let d1 = (self.offset + self.phase * self.window).min(max_d);
        let d2 = (self.offset + p2 * self.window).min(max_d);
        let w1 = (std::f32::consts::PI * self.phase).sin();
        let w2 = (std::f32::consts::PI * p2).sin();
        self.line.read_frac(d1) * w1 + self.line.read_frac(d2) * w2
    }
}
