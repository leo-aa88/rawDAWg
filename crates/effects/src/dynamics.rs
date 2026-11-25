//! Dynamics: compressor (`native.fx.compressor`), lookahead limiter with a
//! true-peak detector (`native.fx.limiter`) and noise gate (`native.fx.gate`).
//! All stereo-linked; the compressor and gate accept an external sidechain.

use crate::common::{Params, Preset};
use rawdawg_dsp::delayline::DelayLine;
use rawdawg_dsp::math::{db_to_lin, lin_to_db};
use rawdawg_dsp::processor::{PrepareError, PrepareSpec, ProcessContext, Processor, Tail};
use rawdawg_types::events::EventKind;
use rawdawg_types::params::{ParamCategory, ParamMapping, ParamUnit};
use rawdawg_types::{ParamDescriptor, ParamId};

fn coeff(time_s: f32, sr: f32) -> f32 {
    if time_s <= 0.0 { 1.0 } else { 1.0 - (-1.0 / (time_s * sr)).exp() }
}

// ---------------------------------------------------------------------------
// Compressor

pub mod compressor {
    use super::*;
    pub const TYPE_ID: &str = "native.fx.compressor";
    pub const DISPLAY_NAME: &str = "Compressor";
    pub const STATE_VERSION: u32 = 1;
    pub const P_THRESHOLD: u32 = 1;
    pub const P_RATIO: u32 = 2;
    pub const P_ATTACK: u32 = 3;
    pub const P_RELEASE: u32 = 4;
    pub const P_KNEE: u32 = 5;
    pub const P_MAKEUP: u32 = 6;
    pub const P_DETECTOR: u32 = 7;
    pub const P_SIDECHAIN: u32 = 8;
    pub const P_MIX: u32 = 9;

    pub static DESCRIPTORS: [ParamDescriptor; 9] = [
        ParamDescriptor::continuous(P_THRESHOLD, "threshold", "Threshold", ParamUnit::Decibel, ParamMapping::Linear { min: -60.0, max: 0.0 }, 42.0 / 60.0, ParamCategory::Effects)
            .with_smoothing(10.0),
        ParamDescriptor::continuous(P_RATIO, "ratio", "Ratio", ParamUnit::Ratio, ParamMapping::Log { min: 1.0, max: 20.0 }, 0.4627, ParamCategory::Effects).with_smoothing(10.0),
        ParamDescriptor::continuous(P_ATTACK, "attack", "Attack", ParamUnit::Milliseconds, ParamMapping::Log { min: 0.1, max: 300.0 }, 0.5, ParamCategory::Effects),
        ParamDescriptor::continuous(P_RELEASE, "release", "Release", ParamUnit::Milliseconds, ParamMapping::Log { min: 5.0, max: 2000.0 }, 0.5, ParamCategory::Effects),
        ParamDescriptor::continuous(P_KNEE, "knee", "Knee", ParamUnit::Decibel, ParamMapping::Linear { min: 0.0, max: 24.0 }, 0.25, ParamCategory::Effects),
        ParamDescriptor::continuous(P_MAKEUP, "makeup", "Makeup", ParamUnit::Decibel, ParamMapping::Linear { min: 0.0, max: 24.0 }, 0.0, ParamCategory::Effects)
            .with_smoothing(10.0),
        ParamDescriptor::continuous(P_DETECTOR, "detector", "Detector", ParamUnit::None, ParamMapping::Discrete { steps: 2 }, 0.0, ParamCategory::Effects)
            .stepped()
            .with_labels(&DETECTOR_LABELS),
        ParamDescriptor::continuous(P_SIDECHAIN, "sidechain", "External sidechain", ParamUnit::None, ParamMapping::Toggle, 0.0, ParamCategory::Effects).stepped(),
        ParamDescriptor::continuous(P_MIX, "mix", "Mix", ParamUnit::Percent, ParamMapping::Linear { min: 0.0, max: 100.0 }, 1.0, ParamCategory::Mix).with_smoothing(10.0),
    ];
    pub const DETECTOR_LABELS: [&str; 2] = ["Peak", "RMS"];

    pub static PRESETS: [Preset; 4] = [
        Preset {
            name: "Gentle glue",
            description: "2:1, soft knee, slow",
            values: &[("threshold", -20.0), ("ratio", 2.0), ("attack", 30.0), ("release", 300.0), ("knee", 12.0), ("makeup", 2.0), ("detector", 1.0)],
        },
        Preset {
            name: "Punchy drums",
            description: "4:1 fast attack, medium release",
            values: &[("threshold", -18.0), ("ratio", 4.0), ("attack", 2.0), ("release", 80.0), ("knee", 3.0), ("makeup", 4.0)],
        },
        Preset {
            name: "Sidechain duck",
            description: "Pumps from the sidechain input",
            values: &[("threshold", -30.0), ("ratio", 8.0), ("attack", 1.0), ("release", 150.0), ("knee", 2.0), ("sidechain", 1.0)],
        },
        Preset {
            name: "Brick",
            description: "20:1 near-limiting",
            values: &[("threshold", -10.0), ("ratio", 20.0), ("attack", 0.3), ("release", 40.0), ("knee", 1.0), ("makeup", 6.0)],
        },
    ];

    pub struct Compressor {
        params: Params,
        sr: f32,
        env: f32,
        rms: f32,
        gain_db: f32,
        /// Max gain reduction (dB, positive) in the last block (meter 0).
        gr_db: f32,
    }

    impl Default for Compressor {
        fn default() -> Self {
            Self::new()
        }
    }

    impl Compressor {
        pub fn new() -> Self {
            Self { params: Params::new(&DESCRIPTORS), sr: 48_000.0, env: 0.0, rms: 0.0, gain_db: 0.0, gr_db: 0.0 }
        }

        /// Static gain curve (dB out of the gain computer for a level in dB).
        pub fn static_gain_db(level_db: f32, threshold: f32, ratio: f32, knee: f32) -> f32 {
            let over = level_db - threshold;
            if knee > 0.0 && over.abs() <= knee * 0.5 {
                let t = over + knee * 0.5;
                -(1.0 - 1.0 / ratio) * t * t / (2.0 * knee)
            } else if over > 0.0 {
                -over * (1.0 - 1.0 / ratio)
            } else {
                0.0
            }
        }
    }

    impl Processor for Compressor {
        fn type_id(&self) -> &'static str {
            TYPE_ID
        }
        fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
            self
        }
        fn descriptors(&self) -> &'static [ParamDescriptor] {
            &DESCRIPTORS
        }
        fn prepare(&mut self, spec: &PrepareSpec) -> Result<(), PrepareError> {
            if !spec.is_valid() {
                return Err(PrepareError::UnsupportedSampleRate);
            }
            self.sr = spec.sample_rate as f32;
            self.params.set_sample_rate(self.sr);
            self.reset();
            Ok(())
        }
        fn process(&mut self, ctx: &mut ProcessContext<'_>) {
            let frames = ctx.audio.frames;
            for ev in ctx.events {
                if let EventKind::Param { id, value } = ev.kind {
                    self.params.set(id, value);
                }
            }
            ctx.audio.copy_input_to_output();
            if ctx.audio.outputs.len() < 2 {
                return;
            }
            let threshold = self.params.phys_smoothed(P_THRESHOLD, frames);
            let ratio = self.params.phys_smoothed(P_RATIO, frames).max(1.0);
            let knee = self.params.phys(P_KNEE);
            let makeup = self.params.phys_smoothed(P_MAKEUP, frames);
            let mix = self.params.phys_smoothed(P_MIX, frames) / 100.0;
            let rms_mode = self.params.as_index(P_DETECTOR) == 1;
            let use_sc = self.params.as_bool(P_SIDECHAIN) && ctx.audio.sidechain.len() >= 2;
            let att = coeff(self.params.phys(P_ATTACK) / 1000.0, self.sr);
            let rel = coeff(self.params.phys(P_RELEASE) / 1000.0, self.sr);
            let rms_c = coeff(0.03, self.sr);
            let (first, rest) = ctx.audio.outputs.split_at_mut(1);
            let (l, r) = (&mut first[0][..frames], &mut rest[0][..frames]);
            let (sc_l, sc_r) =
                if use_sc { (&ctx.audio.sidechain[0][..frames], &ctx.audio.sidechain[1][..frames]) } else { (&ctx.audio.inputs[0][..frames], &ctx.audio.inputs[1][..frames]) };
            let mut gr_max = 0.0f32;
            let makeup_lin = db_to_lin(makeup);
            for i in 0..frames {
                let key = sc_l[i].abs().max(sc_r[i].abs());
                let level = if rms_mode {
                    let sq = 0.5 * (sc_l[i] * sc_l[i] + sc_r[i] * sc_r[i]);
                    self.rms += rms_c * (sq - self.rms);
                    self.rms.sqrt()
                } else {
                    key
                };
                // envelope follower on the detected level: attack while rising, release while falling
                let c = if level > self.env { att } else { rel };
                self.env += c * (level - self.env);
                let level_db = lin_to_db(self.env.max(1e-6));
                self.gain_db = Self::static_gain_db(level_db, threshold, ratio, knee);
                let g = db_to_lin(self.gain_db) * makeup_lin;
                gr_max = gr_max.max(-self.gain_db);
                let (dl, dr) = (ctx.audio.inputs[0][i], ctx.audio.inputs[1][i]);
                l[i] = dl + (dl * g - dl) * mix;
                r[i] = dr + (dr * g - dr) * mix;
            }
            self.gr_db = gr_max;
        }
        fn reset(&mut self) {
            self.env = 0.0;
            self.rms = 0.0;
            self.gain_db = 0.0;
            self.gr_db = 0.0;
            self.params.snap_all();
        }
        fn meter(&self, index: usize) -> Option<f32> {
            (index == 0).then_some(self.gr_db)
        }
        fn param(&self, id: ParamId) -> Option<f32> {
            self.params.param(id)
        }
        fn set_param(&mut self, id: ParamId, normalized: f32) -> bool {
            self.params.set(id, normalized)
        }
        fn save_state(&self) -> Vec<u8> {
            self.params.save(STATE_VERSION)
        }
        fn load_state(&mut self, bytes: &[u8]) -> Result<(), String> {
            self.params.load(bytes, STATE_VERSION)
        }
        fn tail(&self) -> Tail {
            Tail::None
        }
    }
}

// ---------------------------------------------------------------------------
// Limiter

pub mod limiter {
    use super::*;
    pub const TYPE_ID: &str = "native.fx.limiter";
    pub const DISPLAY_NAME: &str = "Limiter";
    pub const STATE_VERSION: u32 = 1;
    pub const P_CEILING: u32 = 1;
    pub const P_RELEASE: u32 = 2;
    pub const P_LOOKAHEAD: u32 = 3;
    pub const P_TRUE_PEAK: u32 = 4;
    pub const P_INPUT: u32 = 5;
    pub const MAX_LOOKAHEAD_MS: f32 = 10.0;
    pub const OVERSAMPLE: usize = 4;
    const FIR_TAPS: usize = 32;

    pub static DESCRIPTORS: [ParamDescriptor; 5] = [
        ParamDescriptor::continuous(P_CEILING, "ceiling", "Ceiling", ParamUnit::Decibel, ParamMapping::Linear { min: -24.0, max: 0.0 }, 23.0 / 24.0, ParamCategory::Effects)
            .with_smoothing(10.0),
        ParamDescriptor::continuous(P_RELEASE, "release", "Release", ParamUnit::Milliseconds, ParamMapping::Log { min: 5.0, max: 1000.0 }, 0.45, ParamCategory::Effects),
        ParamDescriptor::continuous(P_LOOKAHEAD, "lookahead", "Lookahead", ParamUnit::Milliseconds, ParamMapping::Linear { min: 1.0, max: 10.0 }, 0.0, ParamCategory::Effects)
            .stepped(),
        ParamDescriptor::continuous(P_TRUE_PEAK, "true_peak", "True peak", ParamUnit::None, ParamMapping::Toggle, 1.0, ParamCategory::Effects).stepped(),
        ParamDescriptor::continuous(P_INPUT, "input", "Input gain", ParamUnit::Decibel, ParamMapping::Linear { min: -12.0, max: 24.0 }, 1.0 / 3.0, ParamCategory::Effects)
            .with_smoothing(10.0),
    ];

    pub static PRESETS: [Preset; 3] = [
        Preset { name: "Transparent −1 dBTP", description: "True-peak ceiling −1 dB, medium release", values: &[("ceiling", -1.0), ("release", 100.0), ("true_peak", 1.0)] },
        Preset { name: "Loud", description: "+6 dB in, ceiling −0.3 dB, fast release", values: &[("input", 6.0), ("ceiling", -0.3), ("release", 30.0), ("true_peak", 1.0)] },
        Preset { name: "Safety −6", description: "Sample-peak ceiling at −6 dB", values: &[("ceiling", -6.0), ("release", 200.0), ("true_peak", 0.0)] },
    ];

    /// 4× oversampled peak estimate: polyphase windowed-sinc interpolation of |x|.
    pub struct TruePeakDetector {
        hist: [f32; FIR_TAPS],
        pos: usize,
        taps: [[f32; FIR_TAPS]; OVERSAMPLE],
    }

    impl TruePeakDetector {
        pub fn new() -> Self {
            let mut taps = [[0.0f32; FIR_TAPS]; OVERSAMPLE];
            let center = FIR_TAPS as f32 / 2.0;
            for (ph, row) in taps.iter_mut().enumerate() {
                let frac = ph as f32 / OVERSAMPLE as f32;
                let mut sum = 0.0;
                for (k, t) in row.iter_mut().enumerate() {
                    let x = k as f32 - center + frac;
                    let sinc = if x.abs() < 1e-6 { 1.0 } else { (std::f32::consts::PI * x).sin() / (std::f32::consts::PI * x) };
                    let w = 0.5 * (1.0 - (std::f32::consts::TAU * k as f32 / (FIR_TAPS as f32 - 1.0)).cos());
                    *t = sinc * w;
                    sum += *t;
                }
                for t in row.iter_mut() {
                    *t /= sum;
                }
            }
            Self { hist: [0.0; FIR_TAPS], pos: 0, taps }
        }
        pub fn reset(&mut self) {
            self.hist = [0.0; FIR_TAPS];
            self.pos = 0;
        }
        /// Push one sample and return the largest absolute value among the 4 interpolated points.
        #[inline]
        pub fn push(&mut self, x: f32) -> f32 {
            self.hist[self.pos] = x;
            self.pos = (self.pos + 1) % FIR_TAPS;
            let mut peak = x.abs();
            for row in &self.taps[1..] {
                let mut acc = 0.0f32;
                for (k, t) in row.iter().enumerate() {
                    acc += t * self.hist[(self.pos + k) % FIR_TAPS];
                }
                peak = peak.max(acc.abs());
            }
            peak
        }
    }

    impl Default for TruePeakDetector {
        fn default() -> Self {
            Self::new()
        }
    }

    pub struct Limiter {
        params: Params,
        sr: f32,
        delay: [DelayLine; 2],
        /// Sliding window maximum over the lookahead: a small ring with brute-force max (lookahead ≤ 480 frames).
        win: Vec<f32>,
        win_pos: usize,
        lookahead: usize,
        gain: f32,
        tp: [TruePeakDetector; 2],
        gr_db: f32,
        in_peak_db: f32,
    }

    impl Default for Limiter {
        fn default() -> Self {
            Self::new()
        }
    }

    impl Limiter {
        pub fn new() -> Self {
            Self {
                params: Params::new(&DESCRIPTORS),
                sr: 48_000.0,
                delay: [DelayLine::new(512), DelayLine::new(512)],
                win: vec![0.0; 512],
                win_pos: 0,
                lookahead: 48,
                gain: 1.0,
                tp: [TruePeakDetector::new(), TruePeakDetector::new()],
                gr_db: 0.0,
                in_peak_db: -100.0,
            }
        }
        fn configure(&mut self) {
            let ms = self.params.phys(P_LOOKAHEAD).clamp(1.0, MAX_LOOKAHEAD_MS);
            self.lookahead = ((ms / 1000.0 * self.sr).round() as usize).clamp(1, self.win.len() - 1);
        }
    }

    impl Processor for Limiter {
        fn type_id(&self) -> &'static str {
            TYPE_ID
        }
        fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
            self
        }
        fn descriptors(&self) -> &'static [ParamDescriptor] {
            &DESCRIPTORS
        }
        fn prepare(&mut self, spec: &PrepareSpec) -> Result<(), PrepareError> {
            if !spec.is_valid() {
                return Err(PrepareError::UnsupportedSampleRate);
            }
            self.sr = spec.sample_rate as f32;
            self.params.set_sample_rate(self.sr);
            let cap = ((MAX_LOOKAHEAD_MS / 1000.0 * self.sr) as usize + 2).next_power_of_two();
            self.delay = [DelayLine::new(cap), DelayLine::new(cap)];
            self.win = vec![0.0; cap];
            self.configure();
            self.reset();
            Ok(())
        }
        fn process(&mut self, ctx: &mut ProcessContext<'_>) {
            let frames = ctx.audio.frames;
            for ev in ctx.events {
                if let EventKind::Param { id, value } = ev.kind {
                    self.params.set(id, value);
                }
            }
            ctx.audio.copy_input_to_output();
            if ctx.audio.outputs.len() < 2 {
                return;
            }
            let ceiling = db_to_lin(self.params.phys_smoothed(P_CEILING, frames));
            let input = db_to_lin(self.params.phys_smoothed(P_INPUT, frames));
            let rel = coeff(self.params.phys(P_RELEASE) / 1000.0, self.sr);
            let true_peak = self.params.as_bool(P_TRUE_PEAK);
            let la = self.lookahead;
            let (first, rest) = ctx.audio.outputs.split_at_mut(1);
            let (l, r) = (&mut first[0][..frames], &mut rest[0][..frames]);
            let mut gr_max = 0.0f32;
            let mut in_peak = 0.0f32;
            for i in 0..frames {
                let (xl, xr) = (l[i] * input, r[i] * input);
                let peak = if true_peak { self.tp[0].push(xl).max(self.tp[1].push(xr)) } else { xl.abs().max(xr.abs()) };
                in_peak = in_peak.max(peak);
                // window maximum over the lookahead (+1 so the sample leaving the delay is covered)
                let span = (la + 1).min(self.win.len());
                self.win[self.win_pos] = peak;
                self.win_pos = (self.win_pos + 1) % span;
                let mut wmax = 0.0f32;
                for v in &self.win[..span] {
                    wmax = wmax.max(*v);
                }
                let target = if wmax > ceiling { ceiling / wmax } else { 1.0 };
                // instantaneous attack (the signal is delayed by the lookahead), exponential release
                if target < self.gain {
                    self.gain = target;
                } else {
                    self.gain += rel * (target - self.gain);
                }
                self.delay[0].push(xl);
                self.delay[1].push(xr);
                let g = self.gain;
                l[i] = self.delay[0].read(la) * g;
                r[i] = self.delay[1].read(la) * g;
                gr_max = gr_max.max(-lin_to_db(g.max(1e-6)));
            }
            self.gr_db = gr_max;
            self.in_peak_db = lin_to_db(in_peak.max(1e-6));
        }
        fn reset(&mut self) {
            for d in &mut self.delay {
                d.reset();
            }
            self.win.fill(0.0);
            self.win_pos = 0;
            self.gain = 1.0;
            for t in &mut self.tp {
                t.reset();
            }
            self.gr_db = 0.0;
            self.in_peak_db = -100.0;
            self.params.snap_all();
            self.configure();
        }
        fn latency_frames(&self) -> u32 {
            self.lookahead as u32
        }
        fn meter(&self, index: usize) -> Option<f32> {
            match index {
                0 => Some(self.gr_db),
                1 => Some(self.in_peak_db),
                _ => None,
            }
        }
        fn param(&self, id: ParamId) -> Option<f32> {
            self.params.param(id)
        }
        fn set_param(&mut self, id: ParamId, normalized: f32) -> bool {
            // lookahead changes take effect at the next prepare (fixed latency per plan)
            self.params.set(id, normalized)
        }
        fn save_state(&self) -> Vec<u8> {
            self.params.save(STATE_VERSION)
        }
        fn load_state(&mut self, bytes: &[u8]) -> Result<(), String> {
            self.params.load(bytes, STATE_VERSION)?;
            self.configure();
            Ok(())
        }
        fn tail(&self) -> Tail {
            Tail::Frames(self.lookahead as u32)
        }
    }
}

// ---------------------------------------------------------------------------
// Gate

pub mod gate {
    use super::*;
    pub const TYPE_ID: &str = "native.fx.gate";
    pub const DISPLAY_NAME: &str = "Gate";
    pub const STATE_VERSION: u32 = 1;
    pub const P_THRESHOLD: u32 = 1;
    pub const P_HYSTERESIS: u32 = 2;
    pub const P_ATTACK: u32 = 3;
    pub const P_HOLD: u32 = 4;
    pub const P_RELEASE: u32 = 5;
    pub const P_RANGE: u32 = 6;
    pub const P_SIDECHAIN: u32 = 7;

    pub static DESCRIPTORS: [ParamDescriptor; 7] = [
        ParamDescriptor::continuous(P_THRESHOLD, "threshold", "Threshold", ParamUnit::Decibel, ParamMapping::Linear { min: -80.0, max: 0.0 }, 0.5, ParamCategory::Effects)
            .with_smoothing(10.0),
        ParamDescriptor::continuous(P_HYSTERESIS, "hysteresis", "Hysteresis", ParamUnit::Decibel, ParamMapping::Linear { min: 0.0, max: 24.0 }, 0.25, ParamCategory::Effects),
        ParamDescriptor::continuous(P_ATTACK, "attack", "Attack", ParamUnit::Milliseconds, ParamMapping::Log { min: 0.1, max: 100.0 }, 0.33, ParamCategory::Effects),
        ParamDescriptor::continuous(P_HOLD, "hold", "Hold", ParamUnit::Milliseconds, ParamMapping::Log { min: 1.0, max: 1000.0 }, 0.5, ParamCategory::Effects),
        ParamDescriptor::continuous(P_RELEASE, "release", "Release", ParamUnit::Milliseconds, ParamMapping::Log { min: 1.0, max: 2000.0 }, 0.5, ParamCategory::Effects),
        ParamDescriptor::continuous(P_RANGE, "range", "Range", ParamUnit::Decibel, ParamMapping::Linear { min: -80.0, max: 0.0 }, 0.0, ParamCategory::Effects).with_smoothing(10.0),
        ParamDescriptor::continuous(P_SIDECHAIN, "sidechain", "External sidechain", ParamUnit::None, ParamMapping::Toggle, 0.0, ParamCategory::Effects).stepped(),
    ];

    pub static PRESETS: [Preset; 3] = [
        Preset { name: "Drum gate", description: "Tight, short hold", values: &[("threshold", -30.0), ("hysteresis", 4.0), ("attack", 0.3), ("hold", 20.0), ("release", 60.0)] },
        Preset {
            name: "Noise cleanup",
            description: "Soft gate with slow release and −30 dB floor",
            values: &[("threshold", -50.0), ("hysteresis", 6.0), ("attack", 5.0), ("hold", 100.0), ("release", 300.0), ("range", -30.0)],
        },
        Preset {
            name: "Chopper (sidechain)",
            description: "Opens with the sidechain signal",
            values: &[("threshold", -24.0), ("hysteresis", 3.0), ("attack", 1.0), ("hold", 10.0), ("release", 40.0), ("sidechain", 1.0)],
        },
    ];

    pub struct Gate {
        params: Params,
        sr: f32,
        env: f32,
        open: bool,
        hold_left: u32,
        gain: f32,
        gr_db: f32,
    }

    impl Default for Gate {
        fn default() -> Self {
            Self::new()
        }
    }

    impl Gate {
        pub fn new() -> Self {
            Self { params: Params::new(&DESCRIPTORS), sr: 48_000.0, env: 0.0, open: false, hold_left: 0, gain: 0.0, gr_db: 0.0 }
        }
        pub fn is_open(&self) -> bool {
            self.open
        }
    }

    impl Processor for Gate {
        fn type_id(&self) -> &'static str {
            TYPE_ID
        }
        fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
            self
        }
        fn descriptors(&self) -> &'static [ParamDescriptor] {
            &DESCRIPTORS
        }
        fn prepare(&mut self, spec: &PrepareSpec) -> Result<(), PrepareError> {
            if !spec.is_valid() {
                return Err(PrepareError::UnsupportedSampleRate);
            }
            self.sr = spec.sample_rate as f32;
            self.params.set_sample_rate(self.sr);
            self.reset();
            Ok(())
        }
        fn process(&mut self, ctx: &mut ProcessContext<'_>) {
            let frames = ctx.audio.frames;
            for ev in ctx.events {
                if let EventKind::Param { id, value } = ev.kind {
                    self.params.set(id, value);
                }
            }
            ctx.audio.copy_input_to_output();
            if ctx.audio.outputs.len() < 2 {
                return;
            }
            let thr = db_to_lin(self.params.phys_smoothed(P_THRESHOLD, frames));
            let close = thr * db_to_lin(-self.params.phys(P_HYSTERESIS));
            let att = coeff(self.params.phys(P_ATTACK) / 1000.0, self.sr);
            let rel = coeff(self.params.phys(P_RELEASE) / 1000.0, self.sr);
            let hold = (self.params.phys(P_HOLD) / 1000.0 * self.sr) as u32;
            let floor = db_to_lin(self.params.phys_smoothed(P_RANGE, frames));
            let det_c = coeff(0.0005, self.sr);
            let use_sc = self.params.as_bool(P_SIDECHAIN) && ctx.audio.sidechain.len() >= 2;
            let (first, rest) = ctx.audio.outputs.split_at_mut(1);
            let (l, r) = (&mut first[0][..frames], &mut rest[0][..frames]);
            let (sc_l, sc_r) =
                if use_sc { (&ctx.audio.sidechain[0][..frames], &ctx.audio.sidechain[1][..frames]) } else { (&ctx.audio.inputs[0][..frames], &ctx.audio.inputs[1][..frames]) };
            let mut gr_max = 0.0f32;
            for i in 0..frames {
                let key = sc_l[i].abs().max(sc_r[i].abs());
                // fast peak follower with a 50 ms decay
                self.env = if key > self.env { self.env + det_c * (key - self.env) * 40.0 } else { self.env * (1.0 - 1.0 / (0.05 * self.sr)) };
                if self.env > thr {
                    self.open = true;
                    self.hold_left = hold;
                } else if self.env < close {
                    if self.hold_left > 0 {
                        self.hold_left -= 1;
                    } else {
                        self.open = false;
                    }
                }
                let target = if self.open { 1.0 } else { floor };
                let c = if target > self.gain { att } else { rel };
                self.gain += c * (target - self.gain);
                l[i] *= self.gain;
                r[i] *= self.gain;
                gr_max = gr_max.max(-lin_to_db(self.gain.max(1e-6)));
            }
            self.gr_db = gr_max;
        }
        fn reset(&mut self) {
            self.env = 0.0;
            self.open = false;
            self.hold_left = 0;
            self.gain = 0.0;
            self.gr_db = 0.0;
            self.params.snap_all();
        }
        fn meter(&self, index: usize) -> Option<f32> {
            (index == 0).then_some(self.gr_db)
        }
        fn param(&self, id: ParamId) -> Option<f32> {
            self.params.param(id)
        }
        fn set_param(&mut self, id: ParamId, normalized: f32) -> bool {
            self.params.set(id, normalized)
        }
        fn save_state(&self) -> Vec<u8> {
            self.params.save(STATE_VERSION)
        }
        fn load_state(&mut self, bytes: &[u8]) -> Result<(), String> {
            self.params.load(bytes, STATE_VERSION)
        }
        fn tail(&self) -> Tail {
            Tail::None
        }
    }
}
