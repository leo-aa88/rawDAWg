//! Parameter smoothing: one-pole exponential smoother with snap-to-target.

/// One-pole smoother toward a target value. `set_time` is non-RT or RT (cheap).
#[derive(Clone, Copy, Debug)]
pub struct Smoother {
    current: f64,
    target: f64,
    coeff: f64,
}

impl Smoother {
    /// Create at `value` with the given smoothing time (seconds) at `sample_rate`.
    pub fn new(value: f32, time_s: f32, sample_rate: f32) -> Self {
        Self { current: f64::from(value), target: f64::from(value), coeff: f64::from(crate::math::one_pole_coeff(time_s, sample_rate)) }
    }
    pub fn set_time(&mut self, time_s: f32, sample_rate: f32) {
        self.coeff = f64::from(crate::math::one_pole_coeff(time_s, sample_rate));
    }
    pub fn set_target(&mut self, target: f32) {
        if target.is_finite() {
            self.target = f64::from(target);
        }
    }
    /// Jump immediately (reset / stepped parameters).
    pub fn snap(&mut self, value: f32) {
        self.current = f64::from(value);
        self.target = self.current;
    }
    pub fn target(&self) -> f32 {
        self.target as f32
    }
    pub fn current(&self) -> f32 {
        self.current as f32
    }
    pub fn is_settled(&self) -> bool {
        self.current == self.target
    }
    #[inline]
    pub fn next(&mut self) -> f32 {
        let diff = self.target - self.current;
        if diff.abs() <= 1e-9 * self.target.abs().max(1.0) {
            self.current = self.target;
        } else {
            self.current += diff * self.coeff;
        }
        self.current as f32
    }
    /// Advance by `n` samples without producing intermediate values.
    pub fn skip(&mut self, n: usize) {
        if n == 0 {
            return;
        }
        let remain = (1.0 - self.coeff).powi(n.min(i32::MAX as usize) as i32);
        self.current = self.target - (self.target - self.current) * remain;
        if (self.current - self.target).abs() <= 1e-9 * self.target.abs().max(1.0) {
            self.current = self.target;
        }
    }
}

/// Linear ramp over a fixed number of samples (used for crossfades / bypass).
#[derive(Clone, Copy, Debug)]
pub struct LinearRamp {
    current: f32,
    target: f32,
    step: f32,
    remaining: u32,
}

impl LinearRamp {
    pub fn new(value: f32) -> Self {
        Self { current: value, target: value, step: 0.0, remaining: 0 }
    }
    pub fn ramp_to(&mut self, target: f32, samples: u32) {
        self.target = target;
        if samples == 0 {
            self.current = target;
            self.remaining = 0;
            self.step = 0.0;
        } else {
            self.remaining = samples;
            self.step = (target - self.current) / samples as f32;
        }
    }
    pub fn is_done(&self) -> bool {
        self.remaining == 0
    }
    pub fn current(&self) -> f32 {
        self.current
    }
    #[inline]
    pub fn next(&mut self) -> f32 {
        if self.remaining > 0 {
            self.remaining -= 1;
            self.current += self.step;
            if self.remaining == 0 {
                self.current = self.target;
            }
        }
        self.current
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smoother_converges_within_expected_time() {
        let sr = 48_000.0;
        let mut s = Smoother::new(0.0, 0.010, sr);
        s.set_target(1.0);
        // after one time constant (10 ms) ≈ 63.2%
        for _ in 0..480 {
            s.next();
        }
        assert!((s.current() - 0.632).abs() < 0.01, "{}", s.current());
        for _ in 0..48_000 {
            s.next();
        }
        assert!(s.is_settled());
        assert_eq!(s.current(), 1.0);
    }

    #[test]
    fn skip_matches_stepping() {
        let sr = 48_000.0;
        let mut a = Smoother::new(0.0, 0.005, sr);
        let mut b = a;
        a.set_target(1.0);
        b.set_target(1.0);
        for _ in 0..1000 {
            a.next();
        }
        b.skip(1000);
        assert!((a.current() - b.current()).abs() < 1e-4);
    }

    #[test]
    fn nan_target_ignored() {
        let mut s = Smoother::new(0.5, 0.01, 48_000.0);
        s.set_target(f32::NAN);
        assert_eq!(s.target(), 0.5);
    }

    #[test]
    fn linear_ramp_hits_target_exactly() {
        let mut r = LinearRamp::new(0.0);
        r.ramp_to(1.0, 10);
        let mut last = 0.0;
        for _ in 0..10 {
            last = r.next();
        }
        assert_eq!(last, 1.0);
        assert!(r.is_done());
        assert_eq!(r.next(), 1.0);
    }
}
