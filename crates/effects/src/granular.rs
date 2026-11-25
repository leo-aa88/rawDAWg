//! Granular processor (`native.fx.granular`): grains are cut from a 2 s buffer of the
//! incoming audio at a density, size, position and jitter, each with its own pitch
//! (spread), direction, pan and window; 64 bounded grain voices, no latency, deterministic
//! (seeded) randomness so renders repeat exactly. Freeze stops the buffer so the grains
//! keep playing the captured audio.

use crate::common::{Params, Preset, mix_wet};
use rawdawg_dsp::processor::{PrepareError, PrepareSpec, ProcessContext, Processor, Tail};
use rawdawg_dsp::rng::DeterministicRng;
use rawdawg_types::events::EventKind;
use rawdawg_types::params::{ParamCategory, ParamMapping, ParamUnit};
use rawdawg_types::{ParamDescriptor, ParamId};

pub const TYPE_ID: &str = "native.fx.granular";
pub const DISPLAY_NAME: &str = "Granular";
pub const STATE_VERSION: u32 = 1;
pub const MAX_GRAINS: usize = 64;
pub const BUFFER_SECONDS: f32 = 2.0;
pub const MAX_SIZE_MS: f32 = 500.0;

pub const P_SIZE: u32 = 1;
pub const P_DENSITY: u32 = 2;
pub const P_POSITION: u32 = 3;
pub const P_JITTER: u32 = 4;
pub const P_PITCH: u32 = 5;
pub const P_SPREAD: u32 = 6;
pub const P_REVERSE: u32 = 7;
pub const P_PAN: u32 = 8;
pub const P_WINDOW: u32 = 9;
pub const P_FREEZE: u32 = 10;
pub const P_MIX: u32 = 11;
pub const WINDOW_LABELS: [&str; 4] = ["Hann", "Triangle", "Sharp attack", "Sharp decay"];

pub static DESCRIPTORS: [ParamDescriptor; 11] = [
    ParamDescriptor::continuous(P_SIZE, "size", "Grain size", ParamUnit::Milliseconds, ParamMapping::Log { min: 5.0, max: 500.0 }, 0.6, ParamCategory::Effects)
        .with_smoothing(50.0),
    ParamDescriptor::continuous(P_DENSITY, "density", "Density", ParamUnit::Hertz, ParamMapping::Log { min: 1.0, max: 200.0 }, 0.565, ParamCategory::Effects).with_smoothing(50.0),
    ParamDescriptor::continuous(P_POSITION, "position", "Position", ParamUnit::Milliseconds, ParamMapping::Linear { min: 0.0, max: 2000.0 }, 0.1, ParamCategory::Effects)
        .with_smoothing(50.0),
    ParamDescriptor::continuous(P_JITTER, "jitter", "Jitter", ParamUnit::Milliseconds, ParamMapping::Linear { min: 0.0, max: 1000.0 }, 0.05, ParamCategory::Effects)
        .with_smoothing(50.0),
    ParamDescriptor::continuous(P_PITCH, "pitch", "Pitch", ParamUnit::Semitones, ParamMapping::Linear { min: -24.0, max: 24.0 }, 0.5, ParamCategory::Pitch).with_smoothing(50.0),
    ParamDescriptor::continuous(P_SPREAD, "spread", "Pitch spread", ParamUnit::Semitones, ParamMapping::Linear { min: 0.0, max: 12.0 }, 0.0, ParamCategory::Pitch)
        .with_smoothing(50.0),
    ParamDescriptor::continuous(P_REVERSE, "reverse", "Reverse", ParamUnit::Percent, ParamMapping::Linear { min: 0.0, max: 100.0 }, 0.0, ParamCategory::Effects)
        .with_smoothing(50.0),
    ParamDescriptor::continuous(P_PAN, "pan", "Pan spread", ParamUnit::Percent, ParamMapping::Linear { min: 0.0, max: 100.0 }, 0.5, ParamCategory::Mix).with_smoothing(50.0),
    ParamDescriptor::continuous(P_WINDOW, "window", "Window", ParamUnit::None, ParamMapping::Discrete { steps: 4 }, 0.0, ParamCategory::Effects)
        .stepped()
        .with_labels(&WINDOW_LABELS),
    ParamDescriptor::continuous(P_FREEZE, "freeze", "Freeze", ParamUnit::None, ParamMapping::Toggle, 0.0, ParamCategory::Effects).stepped(),
    ParamDescriptor::continuous(P_MIX, "mix", "Mix", ParamUnit::Percent, ParamMapping::Linear { min: 0.0, max: 100.0 }, 1.0, ParamCategory::Mix).with_smoothing(20.0),
];

pub static PRESETS: [Preset; 6] = [
    Preset { name: "Cloud", description: "Dense small grains, wide", values: &[("size", 40.0), ("density", 60.0), ("position", 300.0), ("jitter", 250.0), ("pan", 100.0)] },
    Preset {
        name: "Stutter",
        description: "Sparse, tight grains near live",
        values: &[("size", 60.0), ("density", 12.0), ("position", 30.0), ("jitter", 0.0), ("window", 2.0), ("pan", 0.0)],
    },
    Preset {
        name: "Octave shimmer",
        description: "Grains an octave up with spread",
        values: &[("size", 120.0), ("density", 30.0), ("pitch", 12.0), ("spread", 0.3), ("jitter", 100.0), ("mix", 50.0)],
    },
    Preset { name: "Backwards", description: "Every grain reversed", values: &[("size", 250.0), ("density", 8.0), ("position", 400.0), ("reverse", 100.0), ("jitter", 150.0)] },
    Preset { name: "Freeze", description: "Hold the last two seconds", values: &[("size", 150.0), ("density", 25.0), ("position", 500.0), ("jitter", 400.0), ("freeze", 1.0)] },
    Preset {
        name: "Scatter detune",
        description: "Random pitch per grain",
        values: &[("size", 80.0), ("density", 40.0), ("spread", 7.0), ("jitter", 300.0), ("pan", 80.0), ("mix", 60.0)],
    },
];

#[derive(Clone, Copy, Default)]
struct Grain {
    active: bool,
    /// Absolute buffer index (in samples, may wrap through `% len`) at grain start.
    start: f64,
    rate: f32,
    reverse: bool,
    len: u32,
    age: u32,
    gain_l: f32,
    gain_r: f32,
    window: usize,
}

#[inline]
fn window_value(kind: usize, t: f32) -> f32 {
    match kind {
        1 => 1.0 - (2.0 * t - 1.0).abs(),
        2 => {
            if t < 0.05 {
                t / 0.05
            } else {
                (-6.0 * (t - 0.05)).exp()
            }
        }
        3 => {
            let u = 1.0 - t;
            if u < 0.05 { u / 0.05 } else { (-6.0 * (u - 0.05)).exp() }
        }
        _ => {
            let s = (std::f32::consts::PI * t).sin();
            s * s
        }
    }
}

pub struct Granular {
    params: Params,
    sr: f32,
    buf: [Vec<f32>; 2],
    write: usize,
    grains: [Grain; MAX_GRAINS],
    acc: f32,
    rng: DeterministicRng,
}

impl Default for Granular {
    fn default() -> Self {
        Self::new()
    }
}

impl Granular {
    pub fn new() -> Self {
        Self::with_rate(48_000.0)
    }
    fn with_rate(sr: f32) -> Self {
        let len = (BUFFER_SECONDS * sr) as usize + 1;
        Self {
            params: Params::new(&DESCRIPTORS),
            sr,
            buf: [vec![0.0; len], vec![0.0; len]],
            write: 0,
            grains: [Grain::default(); MAX_GRAINS],
            acc: 0.0,
            rng: DeterministicRng::new(0x5eed, 0x6a41),
        }
    }
    pub fn active_grains(&self) -> usize {
        self.grains.iter().filter(|g| g.active).count()
    }

    #[inline]
    fn read(&self, c: usize, pos: f64) -> f32 {
        let len = self.buf[c].len() as f64;
        let p = pos.rem_euclid(len);
        let i = p as usize;
        let f = (p - i as f64) as f32;
        let a = self.buf[c][i];
        let b = self.buf[c][(i + 1) % self.buf[c].len()];
        a + (b - a) * f
    }

    #[allow(clippy::too_many_arguments)]
    fn spawn(&mut self, size_smp: f32, position_smp: f32, jitter_smp: f32, pitch_st: f32, spread_st: f32, reverse_p: f32, pan_spread: f32, window: usize, gain: f32) {
        let Some(slot) = self.grains.iter().position(|g| !g.active) else { return };
        let len = self.buf[0].len() as f32;
        let semis = pitch_st + (self.rng.bipolar_f32()) * spread_st;
        let rate = 2f32.powf(semis / 12.0);
        let reverse = self.rng.unit_f32() < reverse_p;
        let jitter = self.rng.unit_f32() * jitter_smp;
        let span = size_smp * rate;
        // forward grains must end before the newest sample; nothing may reach past the buffer
        let back = if reverse { (position_smp + jitter).min(len - 2.0 - span) } else { (position_smp + jitter).max(span + 1.0).min(len - 2.0) };
        let start = self.write as f64 - back.max(0.0) as f64;
        let pan = self.rng.bipolar_f32() * pan_spread;
        let a = (pan * 0.5 + 0.5) * std::f32::consts::FRAC_PI_2;
        let g = &mut self.grains[slot];
        *g = Grain { active: true, start, rate, reverse, len: size_smp.max(2.0) as u32, age: 0, gain_l: a.cos() * gain, gain_r: a.sin() * gain, window };
    }
}

impl Processor for Granular {
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
        let params = std::mem::replace(&mut self.params, Params::new(&DESCRIPTORS));
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
        let ms = self.sr / 1000.0;
        let size = self.params.phys_smoothed(P_SIZE, frames).clamp(5.0, MAX_SIZE_MS) * ms;
        let density = self.params.phys_smoothed(P_DENSITY, frames).clamp(1.0, 200.0);
        let position = self.params.phys_smoothed(P_POSITION, frames) * ms;
        let jitter = self.params.phys_smoothed(P_JITTER, frames) * ms;
        let pitch = self.params.phys_smoothed(P_PITCH, frames);
        let spread = self.params.phys_smoothed(P_SPREAD, frames);
        let reverse = self.params.phys_smoothed(P_REVERSE, frames) / 100.0;
        let pan = self.params.phys_smoothed(P_PAN, frames) / 100.0;
        let window = self.params.as_index(P_WINDOW).min(3);
        let freeze = self.params.as_bool(P_FREEZE);
        let mix = (self.params.phys_smoothed(P_MIX, frames) / 100.0).clamp(0.0, 1.0);
        // expected overlap keeps dense clouds at a sane level
        let overlap = (density * size / self.sr).max(1.0);
        let gain = 1.0 / overlap.sqrt();
        let inc = density / self.sr;
        let len = self.buf[0].len();
        let (first, rest) = ctx.audio.outputs.split_at_mut(1);
        let (l, r) = (&mut first[0][..frames], &mut rest[0][..frames]);
        let inputs = ctx.audio.inputs;
        for i in 0..frames {
            let xs = [inputs[0][i], inputs[1][i]];
            if !freeze {
                self.buf[0][self.write] = xs[0];
                self.buf[1][self.write] = xs[1];
                self.write = (self.write + 1) % len;
            }
            self.acc += inc;
            while self.acc >= 1.0 {
                self.acc -= 1.0;
                self.spawn(size, position, jitter, pitch, spread, reverse, pan, window, gain);
            }
            let (mut wl, mut wr) = (0.0f32, 0.0f32);
            for gi in 0..MAX_GRAINS {
                let g = self.grains[gi];
                if !g.active {
                    continue;
                }
                let t = g.age as f32 / g.len as f32;
                let w = window_value(g.window, t);
                let offset = g.age as f64 * g.rate as f64;
                let pos = if g.reverse { g.start - offset } else { g.start + offset };
                let sl = self.read(0, pos);
                let sr_ = self.read(1, pos);
                // pan as a balance of the stereo pair (keeps the image, spreads the grain)
                let mid = (sl + sr_) * 0.5;
                wl += (sl + (mid - sl) * 0.5) * w * g.gain_l * std::f32::consts::SQRT_2;
                wr += (sr_ + (mid - sr_) * 0.5) * w * g.gain_r * std::f32::consts::SQRT_2;
                let g = &mut self.grains[gi];
                g.age += 1;
                if g.age >= g.len {
                    g.active = false;
                }
            }
            l[i] = mix_wet(xs[0], wl, mix);
            r[i] = mix_wet(xs[1], wr, mix);
        }
    }
    fn reset(&mut self) {
        for b in &mut self.buf {
            b.fill(0.0);
        }
        self.write = 0;
        self.grains = [Grain::default(); MAX_GRAINS];
        self.acc = 0.0;
        self.rng = DeterministicRng::new(0x5eed, 0x6a41);
        self.params.snap_all();
    }
    fn tail(&self) -> Tail {
        if self.params.as_bool(P_FREEZE) { Tail::Infinite } else { Tail::Frames((BUFFER_SECONDS * self.sr) as u32 + (MAX_SIZE_MS / 1000.0 * self.sr) as u32) }
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
