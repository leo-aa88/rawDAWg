//! Distortion (`native.fx.distortion`): tube (asymmetric), tape (pre-emphasis +
//! soft saturation + roll-off), soft clip, hard clip, wavefold and bitcrush
//! (bit depth + rate reduction), with 1×/2×/4× oversampling for the nonlinear
//! modes, DC blocking, tone and output trim. Per-band distortion
//! (`native.fx.multiband`) splits into four Linkwitz–Riley bands, each with its
//! own drive/mix/gain, and recombines at unity.

use crate::common::{Params, Preset};
use crate::oversample::Oversampler;
use rawdawg_dsp::biquad::{Biquad, BiquadKind, Coeffs};
use rawdawg_dsp::math::{db_to_lin, fast_tanh};
use rawdawg_dsp::processor::{PrepareError, PrepareSpec, ProcessContext, Processor, Tail};
use rawdawg_types::events::EventKind;
use rawdawg_types::params::{ParamCategory, ParamMapping, ParamUnit};
use rawdawg_types::{ParamDescriptor, ParamId};

pub const MODE_LABELS: [&str; 6] = ["Tube", "Tape", "Soft clip", "Hard clip", "Wavefold", "Bitcrush"];
/// The per-band shaper modes (no bitcrusher inside a crossover).
pub const SHAPER_LABELS: [&str; 5] = ["Tube", "Tape", "Soft clip", "Hard clip", "Wavefold"];

/// Shape one sample (nonlinear core shared by both effects).
#[inline]
pub fn shape(mode: usize, x: f32, bias: f32) -> f32 {
    match mode {
        0 => {
            // asymmetric tube: more compression on the positive half
            let b = x + bias * 0.75;
            let y = if b >= 0.0 { fast_tanh(b) } else { fast_tanh(b * 1.5) / 1.5 };
            y - fast_tanh(bias * 0.75)
        }
        1 => fast_tanh(x * 0.8) * 1.15, // tape core; emphasis handled around it
        2 => fast_tanh(x),
        3 => x.clamp(-1.0, 1.0),
        4 => {
            let t = (x + 1.0).rem_euclid(4.0);
            if t < 2.0 { t - 1.0 } else { 3.0 - t }
        }
        _ => x,
    }
}

struct DcBlocker {
    x1: f32,
    y1: f32,
    r: f32,
}

impl DcBlocker {
    fn new(sr: f32) -> Self {
        Self { x1: 0.0, y1: 0.0, r: 1.0 - std::f32::consts::TAU * 10.0 / sr }
    }
    #[inline]
    fn process(&mut self, x: f32) -> f32 {
        let y = x - self.x1 + self.r * self.y1;
        self.x1 = x;
        self.y1 = y;
        y
    }
    fn reset(&mut self) {
        self.x1 = 0.0;
        self.y1 = 0.0;
    }
}

pub mod single {
    use super::*;
    pub const TYPE_ID: &str = "native.fx.distortion";
    pub const DISPLAY_NAME: &str = "Distortion";
    pub const STATE_VERSION: u32 = 1;
    pub const P_MODE: u32 = 1;
    pub const P_DRIVE: u32 = 2;
    pub const P_BIAS: u32 = 3;
    pub const P_TONE: u32 = 4;
    pub const P_OUTPUT: u32 = 5;
    pub const P_MIX: u32 = 6;
    pub const P_OVERSAMPLE: u32 = 7;
    pub const P_BITS: u32 = 8;
    pub const P_RATE_DIV: u32 = 9;
    pub const OVERSAMPLE_LABELS: [&str; 3] = ["1×", "2×", "4×"];
    pub const BITS_LABELS: [&str; 15] =
        ["2 bit", "3 bit", "4 bit", "5 bit", "6 bit", "7 bit", "8 bit", "9 bit", "10 bit", "11 bit", "12 bit", "13 bit", "14 bit", "15 bit", "16 bit"];
    pub const RATE_LABELS: [&str; 50] = [
        "1×", "2×", "3×", "4×", "5×", "6×", "7×", "8×", "9×", "10×", "11×", "12×", "13×", "14×", "15×", "16×", "17×", "18×", "19×", "20×", "21×", "22×", "23×", "24×", "25×",
        "26×", "27×", "28×", "29×", "30×", "31×", "32×", "33×", "34×", "35×", "36×", "37×", "38×", "39×", "40×", "41×", "42×", "43×", "44×", "45×", "46×", "47×", "48×", "49×",
        "50×",
    ];

    pub static DESCRIPTORS: [ParamDescriptor; 9] = [
        ParamDescriptor::continuous(P_MODE, "mode", "Mode", ParamUnit::None, ParamMapping::Discrete { steps: 6 }, 0.0, ParamCategory::Topology).stepped().with_labels(&MODE_LABELS),
        ParamDescriptor::continuous(P_DRIVE, "drive", "Drive", ParamUnit::Decibel, ParamMapping::Linear { min: 0.0, max: 40.0 }, 0.3, ParamCategory::Effects).with_smoothing(20.0),
        ParamDescriptor::continuous(P_BIAS, "bias", "Bias", ParamUnit::None, ParamMapping::Linear { min: -1.0, max: 1.0 }, 0.5, ParamCategory::Effects).with_smoothing(20.0),
        ParamDescriptor::continuous(P_TONE, "tone", "Tone", ParamUnit::Hertz, ParamMapping::Log { min: 500.0, max: 20_000.0 }, 1.0, ParamCategory::Effects).with_smoothing(30.0),
        ParamDescriptor::continuous(P_OUTPUT, "output", "Output", ParamUnit::Decibel, ParamMapping::Linear { min: -36.0, max: 12.0 }, 0.75, ParamCategory::Mix)
            .with_smoothing(20.0),
        ParamDescriptor::continuous(P_MIX, "mix", "Mix", ParamUnit::Percent, ParamMapping::Linear { min: 0.0, max: 100.0 }, 1.0, ParamCategory::Mix).with_smoothing(20.0),
        ParamDescriptor::continuous(P_OVERSAMPLE, "oversample", "Oversampling", ParamUnit::None, ParamMapping::Discrete { steps: 3 }, 0.5, ParamCategory::Effects)
            .stepped()
            .with_labels(&OVERSAMPLE_LABELS),
        ParamDescriptor::continuous(P_BITS, "bits", "Bit depth", ParamUnit::None, ParamMapping::Discrete { steps: 15 }, 1.0, ParamCategory::Effects)
            .stepped()
            .with_labels(&BITS_LABELS),
        ParamDescriptor::continuous(P_RATE_DIV, "rate_div", "Rate divider", ParamUnit::None, ParamMapping::Discrete { steps: 50 }, 0.0, ParamCategory::Effects)
            .stepped()
            .with_labels(&RATE_LABELS),
    ];

    pub static PRESETS: [Preset; 6] = [
        Preset {
            name: "Warm tube",
            description: "Asymmetric, gentle",
            values: &[("mode", 0.0), ("drive", 10.0), ("bias", 0.3), ("tone", 8000.0), ("output", -4.0), ("oversample", 1.0)],
        },
        Preset { name: "Tape heat", description: "Tape with roll-off", values: &[("mode", 1.0), ("drive", 12.0), ("tone", 6000.0), ("output", -5.0), ("oversample", 1.0)] },
        Preset { name: "Fuzz", description: "Hard clip, 30 dB", values: &[("mode", 3.0), ("drive", 30.0), ("tone", 4000.0), ("output", -14.0), ("oversample", 2.0)] },
        Preset { name: "Fold", description: "Wavefolder", values: &[("mode", 4.0), ("drive", 18.0), ("output", -8.0), ("oversample", 2.0)] },
        Preset { name: "8-bit", description: "8 bits, 4× rate reduction", values: &[("mode", 5.0), ("bits", 6.0), ("rate_div", 3.0), ("output", -3.0)] },
        Preset { name: "Lo-fi grit", description: "6 bits, 12× rate", values: &[("mode", 5.0), ("bits", 4.0), ("rate_div", 11.0), ("tone", 5000.0), ("output", -3.0)] },
    ];

    pub struct Distortion {
        params: Params,
        sr: f32,
        os: [Oversampler; 2],
        dc: [DcBlocker; 2],
        tone: [Biquad; 2],
        emph: [Biquad; 2],
        deemph: [Biquad; 2],
        hold: [f32; 2],
        hold_count: u32,
        os_factor: usize,
    }

    impl Default for Distortion {
        fn default() -> Self {
            Self::new()
        }
    }

    impl Distortion {
        pub fn new() -> Self {
            Self {
                params: Params::new(&DESCRIPTORS),
                sr: 48_000.0,
                os: [Oversampler::new(2), Oversampler::new(2)],
                dc: [DcBlocker::new(48_000.0), DcBlocker::new(48_000.0)],
                tone: [Biquad::new(); 2],
                emph: [Biquad::new(); 2],
                deemph: [Biquad::new(); 2],
                hold: [0.0; 2],
                hold_count: 0,
                os_factor: 2,
            }
        }
        fn configure(&mut self) {
            let f = match self.params.as_index(P_OVERSAMPLE) {
                0 => 1,
                1 => 2,
                _ => 4,
            };
            if f != self.os_factor {
                self.os = [Oversampler::new(f), Oversampler::new(f)];
                self.os_factor = f;
            }
            let e = Coeffs::design(BiquadKind::HighShelf, 3000.0, 0.7, 6.0, self.sr);
            let d = Coeffs::design(BiquadKind::HighShelf, 3000.0, 0.7, -6.0, self.sr);
            for c in 0..2 {
                self.emph[c].coeffs = e;
                self.deemph[c].coeffs = d;
            }
        }
        pub fn mode(&self) -> usize {
            self.params.as_index(P_MODE).min(5)
        }
    }

    impl Processor for Distortion {
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
            self.dc = [DcBlocker::new(self.sr), DcBlocker::new(self.sr)];
            self.os_factor = 0;
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
            let mode = self.mode();
            let drive = db_to_lin(self.params.phys_smoothed(P_DRIVE, frames));
            let bias = self.params.phys_smoothed(P_BIAS, frames);
            let tone_hz = self.params.phys_smoothed(P_TONE, frames);
            let tone_on = tone_hz < 19_900.0;
            let tc = Coeffs::design(BiquadKind::LowPass, tone_hz.min(self.sr * 0.45), 0.707, 0.0, self.sr);
            let out_gain = db_to_lin(self.params.phys_smoothed(P_OUTPUT, frames));
            let mix = (self.params.phys_smoothed(P_MIX, frames) / 100.0).clamp(0.0, 1.0);
            let bits = self.params.as_index(P_BITS) as f32 + 2.0;
            let levels = 2f32.powf(bits) * 0.5;
            let rate_div = self.params.as_index(P_RATE_DIV) as u32 + 1;
            let (first, rest) = ctx.audio.outputs.split_at_mut(1);
            let (l, r) = (&mut first[0][..frames], &mut rest[0][..frames]);
            let inputs = ctx.audio.inputs;
            for c in 0..2 {
                self.tone[c].coeffs = tc;
            }
            for i in 0..frames {
                let xs = [inputs[0][i], inputs[1][i]];
                let mut ys = [0.0f32; 2];
                let crush_now = self.hold_count == 0;
                for c in 0..2 {
                    let x = xs[c];
                    let mut y = if mode == 5 {
                        // bitcrush: quantize and hold (aliasing intended, no oversampling)
                        if crush_now {
                            let q = (x * drive * levels).round() / levels;
                            self.hold[c] = q.clamp(-1.0, 1.0);
                        }
                        self.hold[c]
                    } else if mode == 1 {
                        let e = self.emph[c].process(x * drive);
                        let s = self.os[c].process(e, |v| shape(1, v, 0.0));
                        self.deemph[c].process(s)
                    } else {
                        self.os[c].process(x * drive, |v| shape(mode, v, bias))
                    };
                    // the DC blocker follows the analog-style stages; the crusher keeps its steps intact
                    if mode != 5 {
                        y = self.dc[c].process(y);
                    }
                    if tone_on {
                        y = self.tone[c].process(y);
                    }
                    ys[c] = y * out_gain;
                }
                self.hold_count = if crush_now { rate_div - 1 } else { self.hold_count - 1 };
                l[i] = xs[0] + (ys[0] - xs[0]) * mix;
                r[i] = xs[1] + (ys[1] - xs[1]) * mix;
            }
        }
        fn reset(&mut self) {
            for c in 0..2 {
                self.os[c].reset();
                self.dc[c].reset();
                self.tone[c].reset();
                self.emph[c].reset();
                self.deemph[c].reset();
            }
            self.hold = [0.0; 2];
            self.hold_count = 0;
            self.params.snap_all();
            self.configure();
        }
        fn latency_frames(&self) -> u32 {
            if self.mode() == 5 { 0 } else { self.os[0].latency_frames() }
        }
        fn tail(&self) -> Tail {
            Tail::Frames(64)
        }
        fn param(&self, id: ParamId) -> Option<f32> {
            self.params.param(id)
        }
        fn set_param(&mut self, id: ParamId, normalized: f32) -> bool {
            let ok = self.params.set(id, normalized);
            if ok && id.0 == P_OVERSAMPLE {
                self.configure();
            }
            ok
        }
        fn save_state(&self) -> Vec<u8> {
            self.params.save(STATE_VERSION)
        }
        fn load_state(&mut self, bytes: &[u8]) -> Result<(), String> {
            self.params.load(bytes, STATE_VERSION)?;
            self.configure();
            Ok(())
        }
    }
}

pub mod multiband {
    use super::*;
    pub const TYPE_ID: &str = "native.fx.multiband";
    pub const DISPLAY_NAME: &str = "Per-band distortion";
    pub const STATE_VERSION: u32 = 1;
    pub const BANDS: usize = 4;
    pub const P_MODE: u32 = 1;
    pub const P_LOW_X: u32 = 2;
    pub const P_MID_X: u32 = 3;
    pub const P_HIGH_X: u32 = 4;
    pub const P_OUTPUT: u32 = 5;
    pub const P_MIX: u32 = 6;
    /// Per band: `band_param(b, B_*)`.
    pub const B_DRIVE: u32 = 0;
    pub const B_MIX: u32 = 1;
    pub const B_GAIN: u32 = 2;
    pub const fn band_param(band: usize, offset: u32) -> u32 {
        20 + band as u32 * 10 + offset
    }
    pub const BAND_LABELS: [&str; 4] = ["Low", "Low-mid", "High-mid", "High"];

    macro_rules! descs {
        ($($b:expr),*) => {
            [
                ParamDescriptor::continuous(P_MODE, "mode", "Mode", ParamUnit::None, ParamMapping::Discrete { steps: 5 }, 0.5, ParamCategory::Topology).stepped().with_labels(&SHAPER_LABELS),
                ParamDescriptor::continuous(P_LOW_X, "low_x", "Low / low-mid split", ParamUnit::Hertz, ParamMapping::Log { min: 40.0, max: 800.0 }, 0.5, ParamCategory::Effects).with_smoothing(50.0),
                ParamDescriptor::continuous(P_MID_X, "mid_x", "Low-mid / high-mid split", ParamUnit::Hertz, ParamMapping::Log { min: 300.0, max: 4000.0 }, 0.5, ParamCategory::Effects).with_smoothing(50.0),
                ParamDescriptor::continuous(P_HIGH_X, "high_x", "High-mid / high split", ParamUnit::Hertz, ParamMapping::Log { min: 2000.0, max: 16000.0 }, 0.5, ParamCategory::Effects).with_smoothing(50.0),
                ParamDescriptor::continuous(P_OUTPUT, "output", "Output", ParamUnit::Decibel, ParamMapping::Linear { min: -36.0, max: 12.0 }, 0.75, ParamCategory::Mix).with_smoothing(20.0),
                ParamDescriptor::continuous(P_MIX, "mix", "Mix", ParamUnit::Percent, ParamMapping::Linear { min: 0.0, max: 100.0 }, 1.0, ParamCategory::Mix).with_smoothing(20.0),
                $(
                    ParamDescriptor::continuous(band_param($b, B_DRIVE), concat!("band", $b, ".drive"), concat!("Band ", $b, " drive"), ParamUnit::Decibel, ParamMapping::Linear { min: 0.0, max: 40.0 }, 0.0, ParamCategory::Effects).with_smoothing(20.0),
                    ParamDescriptor::continuous(band_param($b, B_MIX), concat!("band", $b, ".mix"), concat!("Band ", $b, " mix"), ParamUnit::Percent, ParamMapping::Linear { min: 0.0, max: 100.0 }, 1.0, ParamCategory::Effects).with_smoothing(20.0),
                    ParamDescriptor::continuous(band_param($b, B_GAIN), concat!("band", $b, ".gain"), concat!("Band ", $b, " gain"), ParamUnit::Decibel, ParamMapping::Linear { min: -24.0, max: 12.0 }, 2.0 / 3.0, ParamCategory::Effects).with_smoothing(20.0),
                )*
            ]
        };
    }
    pub static DESCRIPTORS: [ParamDescriptor; 6 + BANDS * 3] = descs!(0, 1, 2, 3);

    pub static PRESETS: [Preset; 3] = [
        Preset { name: "Bass warmth", description: "Drive the lows, leave the rest", values: &[("band0.drive", 14.0), ("band1.drive", 4.0)] },
        Preset { name: "Crunchy mids", description: "Mid bands into tube", values: &[("mode", 0.0), ("band1.drive", 18.0), ("band2.drive", 14.0), ("band3.gain", -3.0)] },
        Preset { name: "Air fold", description: "Wavefold the highs only", values: &[("mode", 4.0), ("band3.drive", 16.0), ("band3.mix", 60.0)] },
    ];

    /// Linkwitz–Riley 4th-order split (two cascaded Butterworth biquads per side).
    struct Split {
        lp: [Biquad; 2],
        hp: [Biquad; 2],
    }
    impl Split {
        fn new() -> Self {
            Self { lp: [Biquad::new(); 2], hp: [Biquad::new(); 2] }
        }
        fn set(&mut self, hz: f32, sr: f32) {
            let lp = Coeffs::design(BiquadKind::LowPass, hz, std::f32::consts::FRAC_1_SQRT_2, 0.0, sr);
            let hp = Coeffs::design(BiquadKind::HighPass, hz, std::f32::consts::FRAC_1_SQRT_2, 0.0, sr);
            for b in &mut self.lp {
                b.coeffs = lp;
            }
            for b in &mut self.hp {
                b.coeffs = hp;
            }
        }
        #[inline]
        fn process(&mut self, x: f32) -> (f32, f32) {
            let lo1 = self.lp[0].process(x);
            let lo = self.lp[1].process(lo1);
            let hi1 = self.hp[0].process(x);
            let hi = self.hp[1].process(hi1);
            (lo, hi)
        }
        fn reset(&mut self) {
            for b in self.lp.iter_mut().chain(self.hp.iter_mut()) {
                b.reset();
            }
        }
    }

    pub struct Multiband {
        params: Params,
        sr: f32,
        splits: [[Split; 3]; 2],
        os: [[Oversampler; BANDS]; 2],
        dc: [[DcBlocker; BANDS]; 2],
    }

    impl Default for Multiband {
        fn default() -> Self {
            Self::new()
        }
    }

    impl Multiband {
        pub fn new() -> Self {
            Self {
                params: Params::new(&DESCRIPTORS),
                sr: 48_000.0,
                splits: [[Split::new(), Split::new(), Split::new()], [Split::new(), Split::new(), Split::new()]],
                os: std::array::from_fn(|_| std::array::from_fn(|_| Oversampler::new(2))),
                dc: std::array::from_fn(|_| std::array::from_fn(|_| DcBlocker::new(48_000.0))),
            }
        }
        pub fn mode(&self) -> usize {
            self.params.as_index(P_MODE).min(4)
        }
        /// (drive dB, mix %, gain dB) per band for the UI.
        pub fn bands(&self) -> [(f32, f32, f32); BANDS] {
            let mut out = [(0.0, 0.0, 0.0); BANDS];
            for (b, o) in out.iter_mut().enumerate() {
                *o = (self.params.phys(band_param(b, B_DRIVE)), self.params.phys(band_param(b, B_MIX)), self.params.phys(band_param(b, B_GAIN)));
            }
            out
        }
    }

    impl Processor for Multiband {
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
            self.dc = std::array::from_fn(|_| std::array::from_fn(|_| DcBlocker::new(self.sr)));
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
            let mode = self.mode();
            let xs_hz = [self.params.phys_smoothed(P_LOW_X, frames), self.params.phys_smoothed(P_MID_X, frames), self.params.phys_smoothed(P_HIGH_X, frames)];
            for ch in &mut self.splits {
                for (i, s) in ch.iter_mut().enumerate() {
                    s.set(xs_hz[i].min(self.sr * 0.45), self.sr);
                }
            }
            let mut drive = [0.0f32; BANDS];
            let mut bmix = [0.0f32; BANDS];
            let mut bgain = [0.0f32; BANDS];
            for b in 0..BANDS {
                drive[b] = db_to_lin(self.params.phys_smoothed(band_param(b, B_DRIVE), frames));
                bmix[b] = (self.params.phys_smoothed(band_param(b, B_MIX), frames) / 100.0).clamp(0.0, 1.0);
                bgain[b] = db_to_lin(self.params.phys_smoothed(band_param(b, B_GAIN), frames));
            }
            let out_gain = db_to_lin(self.params.phys_smoothed(P_OUTPUT, frames));
            let mix = (self.params.phys_smoothed(P_MIX, frames) / 100.0).clamp(0.0, 1.0);
            let (first, rest) = ctx.audio.outputs.split_at_mut(1);
            let (l, r) = (&mut first[0][..frames], &mut rest[0][..frames]);
            let inputs = ctx.audio.inputs;
            for i in 0..frames {
                let xs = [inputs[0][i], inputs[1][i]];
                let mut ys = [0.0f32; 2];
                for c in 0..2 {
                    let x = xs[c];
                    let (b0, rest1) = self.splits[c][0].process(x);
                    let (b1, rest2) = self.splits[c][1].process(rest1);
                    let (b2, b3) = self.splits[c][2].process(rest2);
                    let bands = [b0, b1, b2, b3];
                    let mut sum = 0.0;
                    for (b, band) in bands.iter().enumerate() {
                        let wet = if drive[b] > 1.0001 {
                            let d = drive[b];
                            let s = self.os[c][b].process(band * d, |v| shape(mode, v, 0.0));
                            self.dc[c][b].process(s)
                        } else {
                            *band
                        };
                        sum += (band + (wet - band) * bmix[b]) * bgain[b];
                    }
                    ys[c] = sum * out_gain;
                }
                l[i] = xs[0] + (ys[0] - xs[0]) * mix;
                r[i] = xs[1] + (ys[1] - xs[1]) * mix;
            }
        }
        fn reset(&mut self) {
            for ch in &mut self.splits {
                for s in ch {
                    s.reset();
                }
            }
            for ch in &mut self.os {
                for o in ch {
                    o.reset();
                }
            }
            for ch in &mut self.dc {
                for d in ch {
                    d.reset();
                }
            }
            self.params.snap_all();
        }
        fn tail(&self) -> Tail {
            Tail::Frames(64)
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
    }
}
