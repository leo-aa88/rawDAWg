//! Short-time Fourier transform engine for frame-based effects: Hann analysis and
//! synthesis windows, overlap-add reconstruction, fixed buffers, latency of one frame.
//! A frame is handed to a caller closure as a complex spectrum; the closure says whether
//! it resynthesised the frame (then the synthesis window alone must sum to unity) or
//! left the analysed frame intact (then analysis × synthesis sums to unity).

use realfft::num_complex::Complex;
use realfft::{ComplexToReal, RealFftPlanner, RealToComplex};
use std::sync::Arc;

/// Wrap a phase difference into (-π, π].
#[inline]
pub fn princarg(x: f32) -> f32 {
    let mut q = (x / std::f32::consts::PI) as i64;
    if q >= 0 {
        q += q & 1;
    } else {
        q -= q & 1;
    }
    x - std::f32::consts::PI * q as f32
}

/// Linear interpolation into a table at a fractional index (clamped).
#[inline]
pub fn interp(v: &[f32], pos: f32) -> f32 {
    let i = (pos.max(0.0).floor() as usize).min(v.len() - 1);
    let f = (pos - i as f32).clamp(0.0, 1.0);
    let a = v[i];
    let b = v[(i + 1).min(v.len() - 1)];
    a + (b - a) * f
}

pub struct Stft {
    fft: Arc<dyn RealToComplex<f32>>,
    ifft: Arc<dyn ComplexToReal<f32>>,
    scratch_f: Vec<Complex<f32>>,
    scratch_i: Vec<Complex<f32>>,
    window: Vec<f32>,
    frame: usize,
    hop: usize,
    ibuf: Vec<f32>,
    obuf: Vec<f32>,
    t: usize,
    counter: usize,
    time: Vec<f32>,
    spec: Vec<Complex<f32>>,
    norm_exact: f32,
    norm_synth: f32,
}

impl Stft {
    /// `frame` must be a power of two and a multiple of `hop` (hop ≤ frame / 2).
    pub fn new(frame: usize, hop: usize) -> Self {
        let frame = frame.max(16);
        let hop = hop.clamp(1, frame / 2);
        let mut planner = RealFftPlanner::<f32>::new();
        let fft = planner.plan_fft_forward(frame);
        let ifft = planner.plan_fft_inverse(frame);
        let scratch_f = fft.make_scratch_vec();
        let scratch_i = ifft.make_scratch_vec();
        let window: Vec<f32> = (0..frame).map(|i| 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / frame as f32).cos()).collect();
        // overlap sums of the Hann window (constant for hop = frame / 2^k, k ≥ 1)
        let overlap = frame as f32 / hop as f32;
        let sum_w = 0.5 * overlap;
        let sum_w2 = 0.375 * overlap;
        Self {
            fft,
            ifft,
            scratch_f,
            scratch_i,
            window,
            frame,
            hop,
            ibuf: vec![0.0; frame],
            obuf: vec![0.0; 2 * frame],
            t: 0,
            counter: 0,
            time: vec![0.0; frame],
            spec: vec![Complex::new(0.0, 0.0); frame / 2 + 1],
            norm_exact: 1.0 / (frame as f32 * sum_w2),
            norm_synth: 1.0 / (frame as f32 * sum_w),
        }
    }

    pub fn frame_len(&self) -> usize {
        self.frame
    }
    pub fn hop(&self) -> usize {
        self.hop
    }
    pub fn bins(&self) -> usize {
        self.frame / 2 + 1
    }
    /// Width of one bin in Hz.
    pub fn bin_hz(&self, sample_rate: f32) -> f32 {
        sample_rate / self.frame as f32
    }
    /// Latency in frames (one full frame).
    pub fn latency(&self) -> u32 {
        self.frame as u32
    }

    pub fn reset(&mut self) {
        self.ibuf.fill(0.0);
        self.obuf.fill(0.0);
        self.t = 0;
        self.counter = 0;
    }

    /// Analysis magnitude scale: a full-scale sine shows as `frame / 4` in its bin.
    pub fn sine_scale(&self) -> f32 {
        self.frame as f32 * 0.25
    }

    /// Push one sample. Every `hop` samples the last frame is analysed and `modify` runs
    /// on its spectrum (DC and Nyquist imaginary parts are cleared afterwards); it returns
    /// `true` when it resynthesised the frame from scratch (magnitudes/phases rebuilt),
    /// `false` when the analysed frame is intact (exact reconstruction). Returns the
    /// output sample, one frame behind the input.
    #[inline]
    pub fn push(&mut self, x: f32, mut modify: impl FnMut(&mut [Complex<f32>]) -> bool) -> f32 {
        let n = self.frame;
        self.ibuf[self.t % n] = x;
        self.t += 1;
        self.counter += 1;
        if self.counter == self.hop {
            self.counter = 0;
            for i in 0..n {
                self.time[i] = self.ibuf[(self.t + i) % n] * self.window[i];
            }
            let _ = self.fft.process_with_scratch(&mut self.time, &mut self.spec, &mut self.scratch_f);
            let synth = modify(&mut self.spec);
            let bins = n / 2 + 1;
            self.spec[0].im = 0.0;
            self.spec[bins - 1].im = 0.0;
            let _ = self.ifft.process_with_scratch(&mut self.spec, &mut self.time, &mut self.scratch_i);
            let norm = if synth { self.norm_synth } else { self.norm_exact };
            let start = self.t + n; // == t - n (mod 2n): output time of frame sample 0
            for i in 0..n {
                let idx = (start + i) % (2 * n);
                self.obuf[idx] += self.time[i] * self.window[i] * norm;
            }
        }
        // emit the sample exactly one frame older than the one just pushed; every frame
        // covering it has been added (the last one ends at or before the pushed sample)
        let idx = (self.t + 2 * n - n - 1) % (2 * n);
        let y = self.obuf[idx];
        self.obuf[idx] = 0.0;
        y
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_path_reconstructs_after_one_frame_and_synth_path_keeps_level() {
        let mut s = Stft::new(1024, 256);
        let x: Vec<f32> = (0..8192).map(|i| ((i as f32 * 0.05).sin() * 0.5) + ((i * 7919 % 101) as f32 / 101.0 - 0.5) * 0.3).collect();
        let y: Vec<f32> = x.iter().map(|&v| s.push(v, |_| false)).collect();
        for i in 2048..8192 {
            assert!((y[i] - x[i - 1024]).abs() < 1e-4, "sample {i}: {} vs {}", y[i], x[i - 1024]);
        }
        // the synthesis normalisation assumes an un-windowed resynthesised frame (Σw = 2); an
        // untouched (still analysis-windowed) frame on that path comes out at 1.5 / 2 = 0.75
        let mut s = Stft::new(1024, 256);
        let x: Vec<f32> = (0..8192).map(|i| (i as f32 * 0.1).sin() * 0.5).collect();
        let y: Vec<f32> = x.iter().map(|&v| s.push(v, |_| true)).collect();
        let rms = |b: &[f32]| (b.iter().map(|v| v * v).sum::<f32>() / b.len() as f32).sqrt();
        let (rx, ry) = (rms(&x[4096..]), rms(&y[4096..]));
        assert!((ry / rx - 0.75).abs() < 0.02, "{ry} vs {rx}");
    }
}
