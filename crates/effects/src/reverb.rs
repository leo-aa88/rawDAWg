//! Reverb (`native.fx.reverb`): room / hall / cathedral (early reflections + an
//! 8-channel feedback delay network), plate (long diffusers + short dense tank),
//! spring (dispersive all-pass chain + short feedback loop; an approximation, not
//! a physical model), shimmer (hall with a pitch-shifted, damped, soft-limited
//! feedback path). A 4-band **decay shape** (RT60 multipliers 0.1–2) is applied
//! per FDN channel through shelving/peak filters whose gains are derived from the
//! channel's base decay gain, so every band stays below unity: stable by
//! construction. Convolution lives in `convolution.rs`.

use crate::common::{Params, Preset};
use rawdawg_dsp::biquad::{Biquad, BiquadKind, Coeffs};
use rawdawg_dsp::delayline::DelayLine;
use rawdawg_dsp::math::fast_tanh;
use rawdawg_dsp::processor::{PrepareError, PrepareSpec, ProcessContext, Processor, Tail};
use rawdawg_types::events::EventKind;
use rawdawg_types::params::{ParamCategory, ParamMapping, ParamUnit};
use rawdawg_types::{ParamDescriptor, ParamId};

pub const TYPE_ID: &str = "native.fx.reverb";
pub const DISPLAY_NAME: &str = "Reverb";
pub const STATE_VERSION: u32 = 1;
pub const CHANNELS: usize = 8;
pub const MODE_LABELS: [&str; 6] = ["Room", "Hall", "Cathedral", "Plate", "Spring", "Shimmer"];
pub const BAND_LABELS: [&str; 4] = ["Low (< 250 Hz)", "Low-mid", "High-mid", "High (> 4 kHz)"];

pub const P_MODE: u32 = 1;
pub const P_SIZE: u32 = 2;
pub const P_DECAY: u32 = 3;
pub const P_PREDELAY: u32 = 4;
pub const P_DAMP: u32 = 5;
pub const P_MIX: u32 = 6;
pub const P_WIDTH: u32 = 7;
pub const P_DIFFUSION: u32 = 8;
pub const P_MODULATION: u32 = 9;
pub const P_EARLY: u32 = 10;
pub const P_LOWCUT: u32 = 11;
pub const P_SHIMMER_PITCH: u32 = 12;
pub const P_SHIMMER_AMOUNT: u32 = 13;
/// Decay shape bands: `P_SHAPE0 + band`.
pub const P_SHAPE0: u32 = 20;

pub static DESCRIPTORS: [ParamDescriptor; 17] = [
    ParamDescriptor::continuous(P_MODE, "mode", "Type", ParamUnit::None, ParamMapping::Discrete { steps: 6 }, 0.2, ParamCategory::Topology).stepped(),
    ParamDescriptor::continuous(P_SIZE, "size", "Size", ParamUnit::Percent, ParamMapping::Linear { min: 50.0, max: 200.0 }, 1.0 / 3.0, ParamCategory::Effects)
        .with_smoothing(200.0),
    ParamDescriptor::continuous(P_DECAY, "decay", "Decay (RT60)", ParamUnit::Seconds, ParamMapping::Log { min: 0.1, max: 30.0 }, 0.5, ParamCategory::Effects).with_smoothing(100.0),
    ParamDescriptor::continuous(P_PREDELAY, "predelay", "Pre-delay", ParamUnit::Milliseconds, ParamMapping::Linear { min: 0.0, max: 200.0 }, 0.1, ParamCategory::Effects)
        .with_smoothing(100.0),
    ParamDescriptor::continuous(P_DAMP, "damp", "Damping", ParamUnit::Hertz, ParamMapping::Log { min: 500.0, max: 20_000.0 }, 0.6, ParamCategory::Effects).with_smoothing(50.0),
    ParamDescriptor::continuous(P_MIX, "mix", "Mix", ParamUnit::Percent, ParamMapping::Linear { min: 0.0, max: 100.0 }, 0.3, ParamCategory::Mix).with_smoothing(20.0),
    ParamDescriptor::continuous(P_WIDTH, "width", "Width", ParamUnit::Percent, ParamMapping::Linear { min: 0.0, max: 100.0 }, 1.0, ParamCategory::Mix).with_smoothing(20.0),
    ParamDescriptor::continuous(P_DIFFUSION, "diffusion", "Diffusion", ParamUnit::Percent, ParamMapping::Linear { min: 0.0, max: 100.0 }, 0.7, ParamCategory::Effects)
        .with_smoothing(50.0),
    ParamDescriptor::continuous(P_MODULATION, "modulation", "Modulation", ParamUnit::Percent, ParamMapping::Linear { min: 0.0, max: 100.0 }, 0.3, ParamCategory::Modulation)
        .with_smoothing(50.0),
    ParamDescriptor::continuous(P_EARLY, "early", "Early reflections", ParamUnit::Percent, ParamMapping::Linear { min: 0.0, max: 100.0 }, 0.5, ParamCategory::Effects)
        .with_smoothing(20.0),
    ParamDescriptor::continuous(P_LOWCUT, "lowcut", "Low cut", ParamUnit::Hertz, ParamMapping::Log { min: 20.0, max: 1000.0 }, 0.3, ParamCategory::Effects).with_smoothing(50.0),
    ParamDescriptor::continuous(
        P_SHIMMER_PITCH,
        "shimmer_pitch",
        "Shimmer pitch",
        ParamUnit::Semitones,
        ParamMapping::Linear { min: -12.0, max: 24.0 },
        2.0 / 3.0,
        ParamCategory::Effects,
    )
    .stepped(),
    ParamDescriptor::continuous(
        P_SHIMMER_AMOUNT,
        "shimmer_amount",
        "Shimmer amount",
        ParamUnit::Percent,
        ParamMapping::Linear { min: 0.0, max: 100.0 },
        0.5,
        ParamCategory::Effects,
    )
    .with_smoothing(50.0),
    ParamDescriptor::continuous(P_SHAPE0, "shape0", "Decay low", ParamUnit::Ratio, ParamMapping::Log { min: 0.1, max: 2.0 }, 0.7686, ParamCategory::Effects).with_smoothing(100.0),
    ParamDescriptor::continuous(P_SHAPE0 + 1, "shape1", "Decay low-mid", ParamUnit::Ratio, ParamMapping::Log { min: 0.1, max: 2.0 }, 0.7686, ParamCategory::Effects)
        .with_smoothing(100.0),
    ParamDescriptor::continuous(P_SHAPE0 + 2, "shape2", "Decay high-mid", ParamUnit::Ratio, ParamMapping::Log { min: 0.1, max: 2.0 }, 0.7686, ParamCategory::Effects)
        .with_smoothing(100.0),
    ParamDescriptor::continuous(P_SHAPE0 + 3, "shape3", "Decay high", ParamUnit::Ratio, ParamMapping::Log { min: 0.1, max: 2.0 }, 0.7686, ParamCategory::Effects)
        .with_smoothing(100.0),
];

pub static PRESETS: [Preset; 9] = [
    Preset { name: "Small room", description: "Short, bright room", values: &[("mode", 0.0), ("size", 70.0), ("decay", 0.6), ("predelay", 5.0), ("damp", 8000.0), ("mix", 25.0)] },
    Preset {
        name: "Concert hall",
        description: "Large hall, 2.4 s",
        values: &[("mode", 1.0), ("size", 120.0), ("decay", 2.4), ("predelay", 25.0), ("damp", 5000.0), ("mix", 30.0)],
    },
    Preset {
        name: "Cathedral",
        description: "Huge, dark, 6 s",
        values: &[("mode", 2.0), ("size", 160.0), ("decay", 6.0), ("predelay", 60.0), ("damp", 2500.0), ("mix", 35.0), ("shape3", 0.5)],
    },
    Preset {
        name: "Plate",
        description: "Dense bright plate",
        values: &[("mode", 3.0), ("size", 100.0), ("decay", 1.8), ("predelay", 10.0), ("damp", 9000.0), ("mix", 30.0), ("diffusion", 90.0)],
    },
    Preset { name: "Spring tank", description: "Boingy spring", values: &[("mode", 4.0), ("size", 90.0), ("decay", 1.5), ("damp", 4000.0), ("mix", 35.0)] },
    Preset {
        name: "Shimmer octave",
        description: "Hall with +12 semitone feedback",
        values: &[("mode", 5.0), ("size", 130.0), ("decay", 5.0), ("damp", 6000.0), ("mix", 40.0), ("shimmer_pitch", 12.0), ("shimmer_amount", 50.0)],
    },
    Preset {
        name: "Dark chamber",
        description: "Room with fast high decay",
        values: &[("mode", 0.0), ("size", 100.0), ("decay", 1.2), ("damp", 3000.0), ("mix", 28.0), ("shape2", 0.6), ("shape3", 0.3)],
    },
    Preset {
        name: "Bright tail",
        description: "Hall where highs linger",
        values: &[("mode", 1.0), ("size", 110.0), ("decay", 2.0), ("damp", 14_000.0), ("mix", 30.0), ("shape0", 0.5), ("shape3", 1.6)],
    },
    Preset { name: "Infinite wash", description: "30 s cathedral", values: &[("mode", 2.0), ("size", 200.0), ("decay", 30.0), ("damp", 4000.0), ("mix", 50.0), ("early", 10.0)] },
];

/// Base delay lengths (ms at size 100 %) per mode; distinct topologies.
fn fdn_lengths(mode: usize) -> [f32; CHANNELS] {
    match mode {
        0 => [11.3, 14.7, 19.1, 23.3, 29.7, 33.1, 37.9, 41.3],
        1 | 5 => [31.1, 37.3, 43.7, 53.1, 61.3, 71.9, 83.1, 97.3],
        2 => [47.1, 59.3, 71.9, 83.3, 101.1, 113.7, 127.9, 149.3],
        3 => [7.9, 9.7, 11.3, 13.1, 15.7, 17.3, 19.9, 23.1],
        _ => [23.1, 29.3, 31.7, 37.1, 41.3, 43.9, 47.3, 53.7],
    }
}
/// Early reflection taps (ms, gain, pan) per mode.
fn early_taps(mode: usize) -> [(f32, f32, f32); 8] {
    match mode {
        0 => [(3.1, 0.9, -0.5), (5.7, 0.8, 0.5), (8.3, 0.7, -0.2), (11.9, 0.6, 0.3), (15.1, 0.5, -0.7), (19.7, 0.4, 0.6), (24.3, 0.3, 0.0), (29.9, 0.25, -0.4)],
        1 | 5 => [(9.1, 0.8, -0.6), (14.3, 0.7, 0.6), (21.7, 0.6, -0.3), (27.9, 0.55, 0.4), (36.1, 0.45, -0.8), (44.3, 0.4, 0.7), (53.7, 0.3, 0.1), (67.9, 0.25, -0.5)],
        2 => [(17.1, 0.8, -0.7), (29.3, 0.7, 0.7), (41.7, 0.6, -0.4), (55.9, 0.55, 0.5), (73.1, 0.45, -0.8), (91.3, 0.4, 0.8), (113.7, 0.3, 0.2), (139.9, 0.25, -0.6)],
        3 => [(1.1, 0.9, 0.0), (2.3, 0.8, -0.3), (3.7, 0.7, 0.3), (5.3, 0.6, -0.6), (7.1, 0.5, 0.6), (9.7, 0.4, -0.2), (12.1, 0.3, 0.2), (14.9, 0.25, 0.0)],
        _ => [(2.1, 0.6, 0.0), (4.3, 0.5, 0.0), (6.9, 0.4, 0.0), (9.1, 0.3, 0.0), (0.0, 0.0, 0.0), (0.0, 0.0, 0.0), (0.0, 0.0, 0.0), (0.0, 0.0, 0.0)],
    }
}
const DIFFUSER_MS: [f32; 4] = [4.3, 6.7, 9.1, 12.3];
const PLATE_DIFFUSER_MS: [f32; 4] = [13.1, 19.7, 27.3, 36.1];
const SPRING_ALLPASS_HZ: [f32; 4] = [220.0, 410.0, 790.0, 1500.0];

struct FdnChannel {
    line: DelayLine,
    damp_z: f32,
    shape: [Biquad; 4],
    len: f32,
    gain: f32,
}

struct Allpass {
    line: DelayLine,
    len: f32,
    g: f32,
}

impl Allpass {
    fn new(cap: usize) -> Self {
        Self { line: DelayLine::new(cap), len: 1.0, g: 0.6 }
    }
    #[inline]
    fn process(&mut self, x: f32) -> f32 {
        let d = self.line.read_frac(self.len);
        let v = x - self.g * d;
        self.line.push(v);
        d + self.g * v
    }
}

/// Granular two-tap pitch shifter (feedback-loop shimmer).
struct PitchShifter {
    line: DelayLine,
    phase: f32,
    window: f32,
    ratio: f32,
}

impl PitchShifter {
    fn new(cap: usize, window: f32) -> Self {
        Self { line: DelayLine::new(cap), phase: 0.0, window, ratio: 2.0 }
    }
    #[inline]
    fn process(&mut self, x: f32) -> f32 {
        self.line.push(x);
        let inc = (1.0 - self.ratio) / self.window; // phase moves backwards for upward shifts
        self.phase = (self.phase + inc).rem_euclid(1.0);
        let p2 = (self.phase + 0.5).rem_euclid(1.0);
        let d1 = self.phase * self.window;
        let d2 = p2 * self.window;
        let w1 = (std::f32::consts::PI * self.phase).sin();
        let w2 = (std::f32::consts::PI * p2).sin();
        self.line.read_frac(d1) * w1 + self.line.read_frac(d2) * w2
    }
}

pub struct Reverb {
    params: Params,
    sr: f32,
    predelay: [DelayLine; 2],
    early: DelayLine,
    diffusers: [Allpass; 4],
    fdn: Vec<FdnChannel>,
    lowcut: [Biquad; 2],
    mod_phase: f32,
    spring_ap: [Biquad; 4],
    spring_loop: DelayLine,
    spring_z: f32,
    shifter: PitchShifter,
    mode_cached: usize,
    /// Scratch for the FDN mixing (fixed size).
    v: [f32; CHANNELS],
}

impl Default for Reverb {
    fn default() -> Self {
        Self::new()
    }
}

impl Reverb {
    pub fn new() -> Self {
        Self::with_rate(48_000.0)
    }
    fn with_rate(sr: f32) -> Self {
        let ms = |m: f32| (m / 1000.0 * sr) as usize + 64;
        Self {
            params: Params::new(&DESCRIPTORS),
            sr,
            predelay: [DelayLine::new(ms(220.0)), DelayLine::new(ms(220.0))],
            early: DelayLine::new(ms(300.0)),
            diffusers: std::array::from_fn(|_| Allpass::new(ms(80.0))),
            fdn: (0..CHANNELS).map(|_| FdnChannel { line: DelayLine::new(ms(320.0)), damp_z: 0.0, shape: [Biquad::new(); 4], len: 100.0, gain: 0.5 }).collect(),
            lowcut: [Biquad::new(); 2],
            mod_phase: 0.0,
            spring_ap: [Biquad::new(); 4],
            spring_loop: DelayLine::new(ms(120.0)),
            spring_z: 0.0,
            shifter: PitchShifter::new(ms(120.0), 0.08 * sr),
            mode_cached: usize::MAX,
            v: [0.0; CHANNELS],
        }
    }

    pub fn mode(&self) -> usize {
        self.params.as_index(P_MODE).min(5)
    }

    /// Per-channel decay gains and band filters for the current decay/size/shape.
    fn update_decay(&mut self, mode: usize, size: f32, rt60: f32, shape: [f32; 4]) {
        let lengths = fdn_lengths(mode);
        for (c, ch) in self.fdn.iter_mut().enumerate() {
            ch.len = (lengths[c] * size / 1000.0 * self.sr).max(2.0).min(ch.line.capacity() as f32 - 8.0);
            // g = 10^(-3 L / (RT60 sr)): −60 dB after RT60 seconds of circulation
            let exponent = -3.0 * ch.len / (rt60.max(0.05) * self.sr);
            ch.gain = 10f32.powf(exponent).min(0.9999);
            // band multipliers → extra gain in dB relative to the base gain; never pushes a band above unity
            let base_db = 20.0 * exponent; // negative
            let band_db = |m: f32| (base_db * (1.0 / m.clamp(0.1, 2.0) - 1.0)).clamp(-40.0, -base_db * 0.5);
            ch.shape[0].coeffs = Coeffs::design(BiquadKind::LowShelf, 250.0, 0.7, band_db(shape[0]), self.sr);
            ch.shape[1].coeffs = Coeffs::design(BiquadKind::Peak, 700.0, 0.8, band_db(shape[1]), self.sr);
            ch.shape[2].coeffs = Coeffs::design(BiquadKind::Peak, 2200.0, 0.8, band_db(shape[2]), self.sr);
            ch.shape[3].coeffs = Coeffs::design(BiquadKind::HighShelf, 4000.0, 0.7, band_db(shape[3]), self.sr);
        }
    }

    fn configure(&mut self, mode: usize, size: f32) {
        let ms = |m: f32| m / 1000.0 * self.sr;
        let dif = if mode == 3 { PLATE_DIFFUSER_MS } else { DIFFUSER_MS };
        for (i, a) in self.diffusers.iter_mut().enumerate() {
            a.len = (ms(dif[i] * size)).max(2.0).min(a.line.capacity() as f32 - 8.0);
        }
        for (i, b) in self.spring_ap.iter_mut().enumerate() {
            b.coeffs = Coeffs::design(BiquadKind::AllPass, SPRING_ALLPASS_HZ[i] / size.max(0.5), 0.5, 0.0, self.sr);
        }
        self.mode_cached = mode;
    }
}

impl Processor for Reverb {
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
        let params = self.params.clone();
        *self = Self::with_rate(spec.sample_rate as f32);
        self.params = params;
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
        let mode = self.mode();
        let size = self.params.phys_smoothed(P_SIZE, frames) / 100.0;
        if mode != self.mode_cached {
            self.configure(mode, size);
        }
        let rt60 = self.params.phys_smoothed(P_DECAY, frames);
        let shape = [
            self.params.phys_smoothed(P_SHAPE0, frames),
            self.params.phys_smoothed(P_SHAPE0 + 1, frames),
            self.params.phys_smoothed(P_SHAPE0 + 2, frames),
            self.params.phys_smoothed(P_SHAPE0 + 3, frames),
        ];
        self.update_decay(mode, size, rt60, shape);
        let pre = (self.params.phys_smoothed(P_PREDELAY, frames) / 1000.0 * self.sr).clamp(0.0, self.predelay[0].capacity() as f32 - 4.0);
        let damp_hz = self.params.phys_smoothed(P_DAMP, frames);
        let damp_c = 1.0 - (-std::f32::consts::TAU * damp_hz / self.sr).exp();
        let mix = (self.params.phys_smoothed(P_MIX, frames) / 100.0).clamp(0.0, 1.0);
        let width = (self.params.phys_smoothed(P_WIDTH, frames) / 100.0).clamp(0.0, 1.0);
        let diffusion = (self.params.phys_smoothed(P_DIFFUSION, frames) / 100.0).clamp(0.0, 1.0) * 0.75;
        let modulation = (self.params.phys_smoothed(P_MODULATION, frames) / 100.0).clamp(0.0, 1.0);
        let early = (self.params.phys_smoothed(P_EARLY, frames) / 100.0).clamp(0.0, 1.0);
        let lowcut_hz = self.params.phys_smoothed(P_LOWCUT, frames);
        let lc = Coeffs::design(BiquadKind::HighPass, lowcut_hz, 0.707, 0.0, self.sr);
        self.lowcut[0].coeffs = lc;
        self.lowcut[1].coeffs = lc;
        for a in &mut self.diffusers {
            a.g = diffusion;
        }
        let shimmer = mode == 5;
        let sh_amount = (self.params.phys_smoothed(P_SHIMMER_AMOUNT, frames) / 100.0).clamp(0.0, 1.0);
        self.shifter.ratio = 2f32.powf(self.params.phys(P_SHIMMER_PITCH) / 12.0);
        let taps = early_taps(mode);
        let tap_smp: [(f32, f32, f32, f32); 8] = std::array::from_fn(|i| {
            let (ms, g, pan) = taps[i];
            let (pl, pr) = rawdawg_types::units::Pan(f64::from(pan)).equal_power_gains();
            ((ms * size / 1000.0 * self.sr).min(self.early.capacity() as f32 - 4.0), g, pl * std::f32::consts::SQRT_2, pr * std::f32::consts::SQRT_2)
        });
        let mod_inc = 0.7 / self.sr;
        let mod_depth = modulation * 8.0 * (self.sr / 48_000.0);
        let spring_len = (40.0 * size / 1000.0 * self.sr).min(self.spring_loop.capacity() as f32 - 4.0);
        let spring_g = 10f32.powf(-3.0 * spring_len / (rt60.max(0.05) * self.sr)).min(0.98);
        let (first, rest) = ctx.audio.outputs.split_at_mut(1);
        let (l, r) = (&mut first[0][..frames], &mut rest[0][..frames]);
        let inputs = ctx.audio.inputs;
        let norm = 1.0 / (CHANNELS as f32).sqrt();
        for i in 0..frames {
            let (xl, xr) = (inputs[0][i], inputs[1][i]);
            self.predelay[0].push(xl);
            self.predelay[1].push(xr);
            let (pl, pr) = (self.predelay[0].read_frac(pre), self.predelay[1].read_frac(pre));
            let mono = 0.5 * (pl + pr);
            let mono = self.lowcut[0].process(mono);
            // early reflections
            self.early.push(mono);
            let mut el = 0.0;
            let mut er = 0.0;
            for (d, g, gl, gr) in &tap_smp {
                if *g > 0.0 {
                    let v = self.early.read_frac(*d) * g;
                    el += v * gl;
                    er += v * gr;
                }
            }
            let (wl, wr) = if mode == 4 {
                // spring: dispersive all-pass chain into a short damped feedback loop
                let mut s = mono + el * 0.3;
                for ap in &mut self.spring_ap {
                    s = ap.process(s);
                }
                let fbk = self.spring_loop.read_frac(spring_len);
                self.spring_z += damp_c * (fbk - self.spring_z);
                let loop_in = s + fast_tanh(self.spring_z * spring_g);
                self.spring_loop.push(loop_in);
                let out = fbk * 0.7 + s * 0.3;
                (out, self.spring_loop.read_frac(spring_len * 0.5) * 0.7 + s * 0.3)
            } else {
                // diffuse the input, feed the FDN
                let mut d = mono * (1.0 - early * 0.5) + (el + er) * 0.25 * early;
                for ap in &mut self.diffusers {
                    d = ap.process(d);
                }
                self.mod_phase = (self.mod_phase + mod_inc).fract();
                // read every channel (modulated), apply damping + decay shape + gain
                for (c, ch) in self.fdn.iter_mut().enumerate() {
                    let m = ((self.mod_phase + c as f32 * 0.125) * std::f32::consts::TAU).sin() * mod_depth;
                    let mut s = ch.line.read_frac((ch.len + m).max(1.0));
                    ch.damp_z += damp_c * (s - ch.damp_z);
                    s = ch.damp_z;
                    for b in &mut ch.shape {
                        s = b.process(s);
                    }
                    self.v[c] = s * ch.gain;
                }
                // Hadamard mix (8×8, orthogonal) and write back with the input
                let v = self.v;
                let h = |a: f32, b: f32| (a + b, a - b);
                let (a0, a1) = h(v[0], v[1]);
                let (a2, a3) = h(v[2], v[3]);
                let (a4, a5) = h(v[4], v[5]);
                let (a6, a7) = h(v[6], v[7]);
                let (b0, b2) = h(a0, a2);
                let (b1, b3) = h(a1, a3);
                let (b4, b6) = h(a4, a6);
                let (b5, b7) = h(a5, a7);
                let (c0, c4) = h(b0, b4);
                let (c1, c5) = h(b1, b5);
                let (c2, c6) = h(b2, b6);
                let (c3, c7) = h(b3, b7);
                let mixed = [c0, c1, c2, c3, c4, c5, c6, c7];
                let mut fb_extra = 0.0;
                if shimmer {
                    // pitch-shifted, damped, soft-limited copy of the tail re-enters two network
                    // channels with a loop gain below unity (bounded: tanh × 0.35 per channel)
                    let tail = (v[0] + v[3] + v[5] + v[6]) * 0.25;
                    fb_extra = fast_tanh(self.shifter.process(tail) * 1.5) * sh_amount * 0.35;
                }
                for (c, ch) in self.fdn.iter_mut().enumerate() {
                    let inj = if c % 2 == 0 { d } else { -d };
                    let extra = match c {
                        0 => fb_extra,
                        4 => -fb_extra,
                        _ => 0.0,
                    };
                    ch.line.push(mixed[c] * norm + inj + extra);
                }
                let wl = (v[0] + v[2] + v[4] + v[6]) * 0.5;
                let wr = (v[1] + v[3] + v[5] + v[7]) * 0.5;
                (wl + el * early, wr + er * early)
            };
            let mid = 0.5 * (wl + wr);
            let side = 0.5 * (wl - wr) * width;
            let (ol, or) = (mid + side, mid - side);
            l[i] = xl + (ol - xl) * mix;
            r[i] = xr + (or - xr) * mix;
        }
    }
    fn reset(&mut self) {
        for d in &mut self.predelay {
            d.reset();
        }
        self.early.reset();
        for a in &mut self.diffusers {
            a.line.reset();
        }
        for ch in &mut self.fdn {
            ch.line.reset();
            ch.damp_z = 0.0;
            for b in &mut ch.shape {
                b.reset();
            }
        }
        for b in &mut self.lowcut {
            b.reset();
        }
        for b in &mut self.spring_ap {
            b.reset();
        }
        self.spring_loop.reset();
        self.spring_z = 0.0;
        self.shifter.line.reset();
        self.mod_phase = 0.0;
        self.params.snap_all();
        let mode = self.mode();
        let size = self.params.phys(P_SIZE) / 100.0;
        self.configure(mode, size);
    }
    fn tail(&self) -> Tail {
        let rt60 = self.params.phys(P_DECAY);
        if self.mode() == 5 && self.params.phys(P_SHIMMER_AMOUNT) > 80.0 {
            return Tail::Unknown;
        }
        Tail::Frames((rt60 * 1.5 * self.sr) as u32 + (self.params.phys(P_PREDELAY) / 1000.0 * self.sr) as u32)
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
        self.params.load(bytes, STATE_VERSION)?;
        let mode = self.mode();
        let size = self.params.phys(P_SIZE) / 100.0;
        self.configure(mode, size);
        Ok(())
    }
}
