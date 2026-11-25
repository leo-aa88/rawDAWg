//! RBJ-cookbook biquads (direct form II transposed) with coefficient interpolation
//! for stable parameter changes. Used by the parametric EQ and other effects.

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Coeffs {
    pub b0: f32,
    pub b1: f32,
    pub b2: f32,
    pub a1: f32,
    pub a2: f32,
}

impl Default for Coeffs {
    fn default() -> Self {
        Self::IDENTITY
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BiquadKind {
    Peak,
    LowShelf,
    HighShelf,
    LowPass,
    HighPass,
    Notch,
    BandPass,
    AllPass,
}

impl BiquadKind {
    pub const ALL: [BiquadKind; 8] =
        [BiquadKind::Peak, BiquadKind::LowShelf, BiquadKind::HighShelf, BiquadKind::LowPass, BiquadKind::HighPass, BiquadKind::Notch, BiquadKind::BandPass, BiquadKind::AllPass];
    pub fn label(self) -> &'static str {
        match self {
            BiquadKind::Peak => "Bell",
            BiquadKind::LowShelf => "Low shelf",
            BiquadKind::HighShelf => "High shelf",
            BiquadKind::LowPass => "Low-pass",
            BiquadKind::HighPass => "High-pass",
            BiquadKind::Notch => "Notch",
            BiquadKind::BandPass => "Band-pass",
            BiquadKind::AllPass => "All-pass",
        }
    }
    pub fn from_index(i: usize) -> Self {
        Self::ALL[i.min(Self::ALL.len() - 1)]
    }
}

impl Coeffs {
    pub const IDENTITY: Coeffs = Coeffs { b0: 1.0, b1: 0.0, b2: 0.0, a1: 0.0, a2: 0.0 };

    /// Design a biquad (gain in dB is used by peak/shelf kinds only). Frequencies are
    /// clamped below Nyquist; Q to a stable range.
    pub fn design(kind: BiquadKind, freq_hz: f32, q: f32, gain_db: f32, sample_rate: f32) -> Coeffs {
        let f = freq_hz.clamp(1.0, sample_rate * 0.49);
        let q = q.clamp(0.025, 40.0);
        let w0 = std::f32::consts::TAU * f / sample_rate;
        let (sin, cos) = w0.sin_cos();
        let alpha = sin / (2.0 * q);
        let a = 10f32.powf(gain_db / 40.0);
        let (b0, b1, b2, a0, a1, a2) = match kind {
            BiquadKind::Peak => (1.0 + alpha * a, -2.0 * cos, 1.0 - alpha * a, 1.0 + alpha / a, -2.0 * cos, 1.0 - alpha / a),
            BiquadKind::LowShelf => {
                let sq = 2.0 * a.sqrt() * alpha;
                (
                    (a * ((a + 1.0) - (a - 1.0) * cos + sq)),
                    2.0 * a * ((a - 1.0) - (a + 1.0) * cos),
                    a * ((a + 1.0) - (a - 1.0) * cos - sq),
                    (a + 1.0) + (a - 1.0) * cos + sq,
                    -2.0 * ((a - 1.0) + (a + 1.0) * cos),
                    (a + 1.0) + (a - 1.0) * cos - sq,
                )
            }
            BiquadKind::HighShelf => {
                let sq = 2.0 * a.sqrt() * alpha;
                (
                    (a * ((a + 1.0) + (a - 1.0) * cos + sq)),
                    -2.0 * a * ((a - 1.0) + (a + 1.0) * cos),
                    a * ((a + 1.0) + (a - 1.0) * cos - sq),
                    (a + 1.0) - (a - 1.0) * cos + sq,
                    2.0 * ((a - 1.0) - (a + 1.0) * cos),
                    (a + 1.0) - (a - 1.0) * cos - sq,
                )
            }
            BiquadKind::LowPass => ((1.0 - cos) / 2.0, 1.0 - cos, (1.0 - cos) / 2.0, 1.0 + alpha, -2.0 * cos, 1.0 - alpha),
            BiquadKind::HighPass => ((1.0 + cos) / 2.0, -(1.0 + cos), (1.0 + cos) / 2.0, 1.0 + alpha, -2.0 * cos, 1.0 - alpha),
            BiquadKind::Notch => (1.0, -2.0 * cos, 1.0, 1.0 + alpha, -2.0 * cos, 1.0 - alpha),
            BiquadKind::BandPass => (alpha, 0.0, -alpha, 1.0 + alpha, -2.0 * cos, 1.0 - alpha),
            BiquadKind::AllPass => (1.0 - alpha, -2.0 * cos, 1.0 + alpha, 1.0 + alpha, -2.0 * cos, 1.0 - alpha),
        };
        let inv = 1.0 / a0;
        Coeffs { b0: b0 * inv, b1: b1 * inv, b2: b2 * inv, a1: a1 * inv, a2: a2 * inv }
    }

    /// Magnitude response in dB at `freq_hz`.
    pub fn response_db(&self, freq_hz: f32, sample_rate: f32) -> f32 {
        let w = std::f32::consts::TAU * freq_hz / sample_rate;
        let (c1, s1) = (w.cos(), w.sin());
        let (c2, s2) = ((2.0 * w).cos(), (2.0 * w).sin());
        // H(z) = (b0 + b1 z^-1 + b2 z^-2) / (1 + a1 z^-1 + a2 z^-2), z = e^{jw}
        let nr = self.b0 + self.b1 * c1 + self.b2 * c2;
        let ni = -(self.b1 * s1 + self.b2 * s2);
        let dr = 1.0 + self.a1 * c1 + self.a2 * c2;
        let di = -(self.a1 * s1 + self.a2 * s2);
        let num = nr * nr + ni * ni;
        let den = (dr * dr + di * di).max(1e-20);
        10.0 * (num / den).max(1e-20).log10()
    }

    pub fn lerp(&self, other: &Coeffs, t: f32) -> Coeffs {
        Coeffs {
            b0: self.b0 + (other.b0 - self.b0) * t,
            b1: self.b1 + (other.b1 - self.b1) * t,
            b2: self.b2 + (other.b2 - self.b2) * t,
            a1: self.a1 + (other.a1 - self.a1) * t,
            a2: self.a2 + (other.a2 - self.a2) * t,
        }
    }
}

/// One biquad channel state (TDF-II) with coefficient ramping over a control block.
#[derive(Clone, Copy, Debug, Default)]
pub struct Biquad {
    pub coeffs: Coeffs,
    z1: f32,
    z2: f32,
}

impl Biquad {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn reset(&mut self) {
        self.z1 = 0.0;
        self.z2 = 0.0;
    }
    #[inline]
    pub fn process(&mut self, x: f32) -> f32 {
        let c = self.coeffs;
        let y = c.b0 * x + self.z1;
        self.z1 = c.b1 * x - c.a1 * y + self.z2;
        self.z2 = c.b2 * x - c.a2 * y;
        y
    }
    /// Process a block while ramping linearly from the current coefficients to `target`.
    pub fn process_block_ramped(&mut self, buf: &mut [f32], target: Coeffs) {
        let start = self.coeffs;
        let n = buf.len().max(1) as f32;
        for (i, s) in buf.iter_mut().enumerate() {
            self.coeffs = start.lerp(&target, (i + 1) as f32 / n);
            *s = self.process(*s);
        }
        self.coeffs = target;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peak_boost_and_shelves_have_the_designed_gain() {
        let sr = 48_000.0;
        let c = Coeffs::design(BiquadKind::Peak, 1000.0, 1.0, 6.0, sr);
        assert!((c.response_db(1000.0, sr) - 6.0).abs() < 0.05);
        assert!(c.response_db(50.0, sr).abs() < 0.2 && c.response_db(15_000.0, sr).abs() < 0.3);
        let ls = Coeffs::design(BiquadKind::LowShelf, 200.0, 0.707, -9.0, sr);
        assert!((ls.response_db(20.0, sr) + 9.0).abs() < 0.2);
        assert!(ls.response_db(5000.0, sr).abs() < 0.2);
        let hs = Coeffs::design(BiquadKind::HighShelf, 4000.0, 0.707, 4.0, sr);
        assert!((hs.response_db(18_000.0, sr) - 4.0).abs() < 0.3);
        let lp = Coeffs::design(BiquadKind::LowPass, 1000.0, 0.707, 0.0, sr);
        assert!((lp.response_db(1000.0, sr) + 3.0).abs() < 0.2, "{}", lp.response_db(1000.0, sr));
        assert!(lp.response_db(8000.0, sr) < -30.0, "12 dB/oct: three octaves ≈ −36 dB");
        let notch = Coeffs::design(BiquadKind::Notch, 60.0, 10.0, 0.0, sr);
        assert!(notch.response_db(60.0, sr) < -40.0 && notch.response_db(1000.0, sr).abs() < 0.1);
    }

    #[test]
    fn processing_matches_response_and_ramping_is_finite() {
        let sr = 48_000.0;
        let mut bq = Biquad { coeffs: Coeffs::design(BiquadKind::Peak, 1000.0, 2.0, 12.0, sr), ..Default::default() };
        // steady-state sine at 1 kHz gains 12 dB
        let mut out = Vec::new();
        for i in 0..48_000 {
            let x = (std::f32::consts::TAU * 1000.0 * i as f32 / sr).sin();
            out.push(bq.process(x));
        }
        let peak = out[40_000..].iter().fold(0.0f32, |m, v| m.max(v.abs()));
        assert!((20.0 * peak.log10() - 12.0).abs() < 0.1, "{peak}");
        let mut buf = vec![0.5f32; 64];
        bq.process_block_ramped(&mut buf, Coeffs::design(BiquadKind::HighPass, 8000.0, 0.5, 0.0, sr));
        assert!(buf.iter().all(|v| v.is_finite()));
        assert_eq!(bq.coeffs, Coeffs::design(BiquadKind::HighPass, 8000.0, 0.5, 0.0, sr));
    }
}
