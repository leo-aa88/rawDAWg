//! Modulation effects: chorus (`native.fx.chorus`), flanger (`native.fx.flanger`) and
//! phaser (`native.fx.phaser`). A shared LFO (sine/triangle, free-running or tempo
//! synced, stereo phase spread) drives interpolated delay voices, a short comb with
//! bounded feedback, or a chain of first-order all-pass stages whose coefficients stay
//! inside the unit circle for every reachable frequency.

use crate::common::{Params, Preset, mix_wet};
use crate::delay::{SYNC_LABELS, SYNC_QUARTERS};
use rawdawg_dsp::delayline::DelayLine;
use rawdawg_dsp::processor::{PrepareError, PrepareSpec, ProcessContext, Processor, Tail};
use rawdawg_types::events::EventKind;
use rawdawg_types::params::{ParamCategory, ParamMapping, ParamUnit};
use rawdawg_types::{ParamDescriptor, ParamId};

pub const SHAPE_LABELS: [&str; 2] = ["Sine", "Triangle"];
/// Feedback magnitude never reaches unity; the stored sample is also hard-bounded.
pub const MAX_FEEDBACK: f32 = 0.95;
const FEEDBACK_BOUND: f32 = 4.0;

/// LFO value in [-1, 1] for a phase in cycles.
#[inline]
pub fn lfo_value(phase: f32, shape: usize) -> f32 {
    let p = phase.rem_euclid(1.0);
    if shape == 1 { 4.0 * (p - 0.5).abs() - 1.0 } else { (std::f32::consts::TAU * p).sin() }
}

/// LFO rate in Hz: a sync division overrides the free rate.
pub fn lfo_rate_hz(params: &Params, p_rate: u32, p_sync: u32, tempo_bpm: f64) -> f32 {
    let sync = params.as_index(p_sync);
    if sync > 0 && sync < SYNC_QUARTERS.len() {
        let beat_s = 60.0 / tempo_bpm.clamp(20.0, 400.0) as f32;
        (1.0 / (beat_s * SYNC_QUARTERS[sync])).clamp(0.01, 20.0)
    } else {
        params.phys(p_rate).clamp(0.01, 20.0)
    }
}

#[inline]
fn bound(v: f32) -> f32 {
    if v.is_finite() { v.clamp(-FEEDBACK_BOUND, FEEDBACK_BOUND) } else { 0.0 }
}

pub mod chorus {
    use super::*;
    pub const TYPE_ID: &str = "native.fx.chorus";
    pub const DISPLAY_NAME: &str = "Chorus";
    pub const STATE_VERSION: u32 = 1;
    pub const MAX_VOICES: usize = 4;
    pub const MAX_DELAY_MS: f32 = 40.0;
    pub const MAX_DEPTH_MS: f32 = 20.0;
    pub const P_RATE: u32 = 1;
    pub const P_SYNC: u32 = 2;
    pub const P_SHAPE: u32 = 3;
    pub const P_DEPTH: u32 = 4;
    pub const P_DELAY: u32 = 5;
    pub const P_VOICES: u32 = 6;
    pub const P_SPREAD: u32 = 7;
    pub const P_TONE: u32 = 8;
    pub const P_MIX: u32 = 9;
    pub const VOICE_LABELS: [&str; 4] = ["1 voice", "2 voices", "3 voices", "4 voices"];

    pub static DESCRIPTORS: [ParamDescriptor; 9] = [
        ParamDescriptor::continuous(P_RATE, "rate", "Rate", ParamUnit::Hertz, ParamMapping::Log { min: 0.05, max: 10.0 }, 0.52, ParamCategory::Modulation),
        ParamDescriptor::continuous(P_SYNC, "sync", "Sync", ParamUnit::None, ParamMapping::Discrete { steps: 14 }, 0.0, ParamCategory::Modulation)
            .stepped()
            .with_labels(&SYNC_LABELS),
        ParamDescriptor::continuous(P_SHAPE, "shape", "Shape", ParamUnit::None, ParamMapping::Discrete { steps: 2 }, 0.0, ParamCategory::Modulation)
            .stepped()
            .with_labels(&SHAPE_LABELS),
        ParamDescriptor::continuous(P_DEPTH, "depth", "Depth", ParamUnit::Milliseconds, ParamMapping::Linear { min: 0.0, max: 20.0 }, 0.2, ParamCategory::Modulation)
            .with_smoothing(50.0),
        ParamDescriptor::continuous(P_DELAY, "delay", "Delay", ParamUnit::Milliseconds, ParamMapping::Linear { min: 1.0, max: 40.0 }, 0.28, ParamCategory::Effects)
            .with_smoothing(50.0),
        ParamDescriptor::continuous(P_VOICES, "voices", "Voices", ParamUnit::None, ParamMapping::Discrete { steps: 4 }, 1.0 / 3.0, ParamCategory::Topology)
            .stepped()
            .with_labels(&VOICE_LABELS),
        ParamDescriptor::continuous(P_SPREAD, "spread", "Stereo spread", ParamUnit::Percent, ParamMapping::Linear { min: 0.0, max: 100.0 }, 0.7, ParamCategory::Mix)
            .with_smoothing(50.0),
        ParamDescriptor::continuous(P_TONE, "tone", "Tone", ParamUnit::Hertz, ParamMapping::Log { min: 1000.0, max: 20_000.0 }, 1.0, ParamCategory::Effects).with_smoothing(30.0),
        ParamDescriptor::continuous(P_MIX, "mix", "Mix", ParamUnit::Percent, ParamMapping::Linear { min: 0.0, max: 100.0 }, 0.5, ParamCategory::Mix).with_smoothing(20.0),
    ];

    pub static PRESETS: [Preset; 5] = [
        Preset {
            name: "Soft ensemble",
            description: "Two voices, slow and wide",
            values: &[("rate", 0.6), ("depth", 4.0), ("delay", 12.0), ("voices", 1.0), ("spread", 80.0), ("mix", 45.0)],
        },
        Preset {
            name: "80s stereo",
            description: "Three voices, medium rate",
            values: &[("rate", 1.2), ("depth", 3.0), ("delay", 9.0), ("voices", 2.0), ("spread", 100.0), ("mix", 50.0)],
        },
        Preset {
            name: "Thick quad",
            description: "Four voices, deep",
            values: &[("rate", 0.4), ("depth", 7.0), ("delay", 18.0), ("voices", 3.0), ("spread", 60.0), ("tone", 8000.0), ("mix", 55.0)],
        },
        Preset {
            name: "Vibrato",
            description: "Single voice, fully wet",
            values: &[("rate", 5.5), ("depth", 1.5), ("delay", 3.0), ("voices", 0.0), ("spread", 0.0), ("mix", 100.0)],
        },
        Preset {
            name: "Synced 1/2 wobble",
            description: "Tempo-synced slow chorus",
            values: &[("sync", 12.0), ("shape", 1.0), ("depth", 6.0), ("delay", 14.0), ("voices", 1.0), ("spread", 50.0), ("mix", 50.0)],
        },
    ];

    pub struct Chorus {
        params: Params,
        sr: f32,
        lines: [DelayLine; 2],
        tone_z: [f32; 2],
        phase: f32,
    }

    impl Default for Chorus {
        fn default() -> Self {
            Self::new()
        }
    }

    impl Chorus {
        pub fn new() -> Self {
            Self { params: Params::new(&DESCRIPTORS), sr: 48_000.0, lines: [DelayLine::new(4096), DelayLine::new(4096)], tone_z: [0.0; 2], phase: 0.0 }
        }
        pub fn voices(&self) -> usize {
            self.params.as_index(P_VOICES).clamp(0, MAX_VOICES - 1) + 1
        }
    }

    impl Processor for Chorus {
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
            let cap = ((MAX_DELAY_MS + MAX_DEPTH_MS) / 1000.0 * self.sr) as usize + 64;
            self.lines = [DelayLine::new(cap), DelayLine::new(cap)];
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
            let rate = lfo_rate_hz(&self.params, P_RATE, P_SYNC, ctx.transport.tempo_bpm);
            let shape = self.params.as_index(P_SHAPE);
            let voices = self.voices();
            let depth = self.params.phys_smoothed(P_DEPTH, frames) / 1000.0 * self.sr;
            let base = self.params.phys_smoothed(P_DELAY, frames) / 1000.0 * self.sr;
            let spread = (self.params.phys_smoothed(P_SPREAD, frames) / 100.0).clamp(0.0, 1.0);
            let tone_hz = self.params.phys_smoothed(P_TONE, frames);
            let tone_on = tone_hz < 19_900.0;
            let tone_c = 1.0 - (-std::f32::consts::TAU * tone_hz / self.sr).exp();
            let mix = (self.params.phys_smoothed(P_MIX, frames) / 100.0).clamp(0.0, 1.0);
            let max_d = (self.lines[0].capacity() - 2) as f32;
            let inc = rate / self.sr;
            let norm = 1.0 / voices as f32;
            let (first, rest) = ctx.audio.outputs.split_at_mut(1);
            let (l, r) = (&mut first[0][..frames], &mut rest[0][..frames]);
            let inputs = ctx.audio.inputs;
            for i in 0..frames {
                let xs = [inputs[0][i], inputs[1][i]];
                let mut ys = [0.0f32; 2];
                for c in 0..2 {
                    self.lines[c].push(xs[c]);
                    let mut acc = 0.0;
                    for v in 0..voices {
                        let ph = self.phase + v as f32 / voices as f32 + c as f32 * spread * 0.5;
                        let d = (base + depth * 0.5 * (1.0 + lfo_value(ph, shape))).clamp(1.0, max_d);
                        acc += self.lines[c].read_frac(d);
                    }
                    let mut w = acc * norm;
                    if tone_on {
                        self.tone_z[c] += tone_c * (w - self.tone_z[c]);
                        w = self.tone_z[c];
                    }
                    ys[c] = mix_wet(xs[c], w, mix);
                }
                self.phase = (self.phase + inc).rem_euclid(1.0);
                l[i] = ys[0];
                r[i] = ys[1];
            }
        }
        fn reset(&mut self) {
            for line in &mut self.lines {
                line.reset();
            }
            self.tone_z = [0.0; 2];
            self.phase = 0.0;
            self.params.snap_all();
        }
        fn tail(&self) -> Tail {
            Tail::Frames((MAX_DELAY_MS + MAX_DEPTH_MS) as u32 * 192)
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

pub mod flanger {
    use super::*;
    pub const TYPE_ID: &str = "native.fx.flanger";
    pub const DISPLAY_NAME: &str = "Flanger";
    pub const STATE_VERSION: u32 = 1;
    pub const MAX_DELAY_MS: f32 = 20.0;
    pub const MAX_DEPTH_MS: f32 = 10.0;
    pub const P_RATE: u32 = 1;
    pub const P_SYNC: u32 = 2;
    pub const P_SHAPE: u32 = 3;
    pub const P_DEPTH: u32 = 4;
    pub const P_DELAY: u32 = 5;
    pub const P_FEEDBACK: u32 = 6;
    pub const P_SPREAD: u32 = 7;
    pub const P_MIX: u32 = 8;

    pub static DESCRIPTORS: [ParamDescriptor; 8] = [
        ParamDescriptor::continuous(P_RATE, "rate", "Rate", ParamUnit::Hertz, ParamMapping::Log { min: 0.02, max: 10.0 }, 0.45, ParamCategory::Modulation),
        ParamDescriptor::continuous(P_SYNC, "sync", "Sync", ParamUnit::None, ParamMapping::Discrete { steps: 14 }, 0.0, ParamCategory::Modulation)
            .stepped()
            .with_labels(&SYNC_LABELS),
        ParamDescriptor::continuous(P_SHAPE, "shape", "Shape", ParamUnit::None, ParamMapping::Discrete { steps: 2 }, 0.0, ParamCategory::Modulation)
            .stepped()
            .with_labels(&SHAPE_LABELS),
        ParamDescriptor::continuous(P_DEPTH, "depth", "Depth", ParamUnit::Milliseconds, ParamMapping::Linear { min: 0.0, max: 10.0 }, 0.2, ParamCategory::Modulation)
            .with_smoothing(50.0),
        ParamDescriptor::continuous(P_DELAY, "delay", "Delay", ParamUnit::Milliseconds, ParamMapping::Log { min: 0.1, max: 20.0 }, 0.43, ParamCategory::Effects)
            .with_smoothing(50.0),
        ParamDescriptor::continuous(P_FEEDBACK, "feedback", "Feedback", ParamUnit::Percent, ParamMapping::Linear { min: -95.0, max: 95.0 }, 0.7, ParamCategory::Effects)
            .with_smoothing(20.0),
        ParamDescriptor::continuous(P_SPREAD, "spread", "Stereo spread", ParamUnit::Percent, ParamMapping::Linear { min: 0.0, max: 100.0 }, 0.5, ParamCategory::Mix)
            .with_smoothing(50.0),
        ParamDescriptor::continuous(P_MIX, "mix", "Mix", ParamUnit::Percent, ParamMapping::Linear { min: 0.0, max: 100.0 }, 0.5, ParamCategory::Mix).with_smoothing(20.0),
    ];

    pub static PRESETS: [Preset; 5] = [
        Preset {
            name: "Jet",
            description: "Classic slow sweep with feedback",
            values: &[("rate", 0.25), ("depth", 2.5), ("delay", 0.8), ("feedback", 60.0), ("spread", 50.0), ("mix", 50.0)],
        },
        Preset {
            name: "Through-zero-ish",
            description: "Negative feedback, hollow",
            values: &[("rate", 0.15), ("depth", 3.0), ("delay", 0.4), ("feedback", -75.0), ("spread", 30.0), ("mix", 50.0)],
        },
        Preset {
            name: "Metallic",
            description: "Short delay, high feedback, fast",
            values: &[("rate", 2.0), ("depth", 0.6), ("delay", 0.3), ("feedback", 90.0), ("spread", 0.0), ("mix", 50.0)],
        },
        Preset {
            name: "Wide subtle",
            description: "Low feedback, wide stereo",
            values: &[("rate", 0.5), ("depth", 1.5), ("delay", 2.0), ("feedback", 20.0), ("spread", 100.0), ("mix", 35.0)],
        },
        Preset {
            name: "Synced 1 bar",
            description: "One sweep per bar",
            values: &[("sync", 13.0), ("shape", 1.0), ("depth", 4.0), ("delay", 0.5), ("feedback", 55.0), ("spread", 50.0), ("mix", 50.0)],
        },
    ];

    pub struct Flanger {
        params: Params,
        sr: f32,
        lines: [DelayLine; 2],
        phase: f32,
    }

    impl Default for Flanger {
        fn default() -> Self {
            Self::new()
        }
    }

    impl Flanger {
        pub fn new() -> Self {
            Self { params: Params::new(&DESCRIPTORS), sr: 48_000.0, lines: [DelayLine::new(4096), DelayLine::new(4096)], phase: 0.0 }
        }
    }

    impl Processor for Flanger {
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
            let cap = ((MAX_DELAY_MS + MAX_DEPTH_MS) / 1000.0 * self.sr) as usize + 64;
            self.lines = [DelayLine::new(cap), DelayLine::new(cap)];
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
            let rate = lfo_rate_hz(&self.params, P_RATE, P_SYNC, ctx.transport.tempo_bpm);
            let shape = self.params.as_index(P_SHAPE);
            let depth = self.params.phys_smoothed(P_DEPTH, frames) / 1000.0 * self.sr;
            let base = self.params.phys_smoothed(P_DELAY, frames) / 1000.0 * self.sr;
            let fb = (self.params.phys_smoothed(P_FEEDBACK, frames) / 100.0).clamp(-MAX_FEEDBACK, MAX_FEEDBACK);
            let spread = (self.params.phys_smoothed(P_SPREAD, frames) / 100.0).clamp(0.0, 1.0);
            let mix = (self.params.phys_smoothed(P_MIX, frames) / 100.0).clamp(0.0, 1.0);
            let max_d = (self.lines[0].capacity() - 2) as f32;
            let inc = rate / self.sr;
            let (first, rest) = ctx.audio.outputs.split_at_mut(1);
            let (l, r) = (&mut first[0][..frames], &mut rest[0][..frames]);
            let inputs = ctx.audio.inputs;
            for i in 0..frames {
                let xs = [inputs[0][i], inputs[1][i]];
                let mut ys = [0.0f32; 2];
                for c in 0..2 {
                    let ph = self.phase + c as f32 * spread * 0.5;
                    // the loop needs at least one sample of delay to stay causal; reading before
                    // the push means index 0 is already one sample old
                    let d = (base + depth * 0.5 * (1.0 + lfo_value(ph, shape))).clamp(1.0, max_d);
                    let w = self.lines[c].read_frac(d - 1.0);
                    self.lines[c].push(bound(xs[c] + fb * w));
                    ys[c] = mix_wet(xs[c], w, mix);
                }
                self.phase = (self.phase + inc).rem_euclid(1.0);
                l[i] = ys[0];
                r[i] = ys[1];
            }
        }
        fn reset(&mut self) {
            for line in &mut self.lines {
                line.reset();
            }
            self.phase = 0.0;
            self.params.snap_all();
        }
        fn tail(&self) -> Tail {
            Tail::Unknown
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

pub mod phaser {
    use super::*;
    pub const TYPE_ID: &str = "native.fx.phaser";
    pub const DISPLAY_NAME: &str = "Phaser";
    pub const STATE_VERSION: u32 = 1;
    pub const MAX_STAGES: usize = 12;
    pub const P_RATE: u32 = 1;
    pub const P_SYNC: u32 = 2;
    pub const P_SHAPE: u32 = 3;
    pub const P_DEPTH: u32 = 4;
    pub const P_CENTER: u32 = 5;
    pub const P_STAGES: u32 = 6;
    pub const P_FEEDBACK: u32 = 7;
    pub const P_SPREAD: u32 = 8;
    pub const P_MIX: u32 = 9;
    pub const STAGE_LABELS: [&str; 6] = ["2 stages", "4 stages", "6 stages", "8 stages", "10 stages", "12 stages"];
    /// Sweep range at 100 % depth, in octaves either side of the centre.
    pub const SWEEP_OCTAVES: f32 = 2.5;

    pub static DESCRIPTORS: [ParamDescriptor; 9] = [
        ParamDescriptor::continuous(P_RATE, "rate", "Rate", ParamUnit::Hertz, ParamMapping::Log { min: 0.02, max: 10.0 }, 0.5, ParamCategory::Modulation),
        ParamDescriptor::continuous(P_SYNC, "sync", "Sync", ParamUnit::None, ParamMapping::Discrete { steps: 14 }, 0.0, ParamCategory::Modulation)
            .stepped()
            .with_labels(&SYNC_LABELS),
        ParamDescriptor::continuous(P_SHAPE, "shape", "Shape", ParamUnit::None, ParamMapping::Discrete { steps: 2 }, 0.0, ParamCategory::Modulation)
            .stepped()
            .with_labels(&SHAPE_LABELS),
        ParamDescriptor::continuous(P_DEPTH, "depth", "Depth", ParamUnit::Percent, ParamMapping::Linear { min: 0.0, max: 100.0 }, 0.7, ParamCategory::Modulation)
            .with_smoothing(50.0),
        ParamDescriptor::continuous(P_CENTER, "center", "Centre", ParamUnit::Hertz, ParamMapping::Log { min: 100.0, max: 5000.0 }, 0.53, ParamCategory::Effects)
            .with_smoothing(50.0),
        ParamDescriptor::continuous(P_STAGES, "stages", "Stages", ParamUnit::None, ParamMapping::Discrete { steps: 6 }, 0.2, ParamCategory::Topology)
            .stepped()
            .with_labels(&STAGE_LABELS),
        ParamDescriptor::continuous(P_FEEDBACK, "feedback", "Feedback", ParamUnit::Percent, ParamMapping::Linear { min: 0.0, max: 95.0 }, 0.3, ParamCategory::Effects)
            .with_smoothing(20.0),
        ParamDescriptor::continuous(P_SPREAD, "spread", "Stereo spread", ParamUnit::Percent, ParamMapping::Linear { min: 0.0, max: 100.0 }, 0.5, ParamCategory::Mix)
            .with_smoothing(50.0),
        ParamDescriptor::continuous(P_MIX, "mix", "Mix", ParamUnit::Percent, ParamMapping::Linear { min: 0.0, max: 100.0 }, 0.5, ParamCategory::Mix).with_smoothing(20.0),
    ];

    pub static PRESETS: [Preset; 5] = [
        Preset {
            name: "Classic 4-stage",
            description: "Slow, medium feedback",
            values: &[("rate", 0.4), ("depth", 70.0), ("center", 800.0), ("stages", 1.0), ("feedback", 30.0), ("spread", 50.0), ("mix", 50.0)],
        },
        Preset {
            name: "Deep 8-stage",
            description: "Many notches, resonant",
            values: &[("rate", 0.2), ("depth", 85.0), ("center", 600.0), ("stages", 3.0), ("feedback", 65.0), ("spread", 70.0), ("mix", 50.0)],
        },
        Preset {
            name: "Fast shimmer",
            description: "12 stages, quick and shallow",
            values: &[("rate", 4.0), ("depth", 30.0), ("center", 2000.0), ("stages", 5.0), ("feedback", 20.0), ("spread", 100.0), ("mix", 45.0)],
        },
        Preset {
            name: "Hollow sweep",
            description: "2 stages, no feedback, full depth",
            values: &[("rate", 0.1), ("depth", 100.0), ("center", 1000.0), ("stages", 0.0), ("feedback", 0.0), ("spread", 0.0), ("mix", 50.0)],
        },
        Preset {
            name: "Synced 1/4",
            description: "One sweep per beat, triangle",
            values: &[("sync", 9.0), ("shape", 1.0), ("depth", 60.0), ("center", 900.0), ("stages", 2.0), ("feedback", 45.0), ("spread", 50.0), ("mix", 50.0)],
        },
    ];

    #[derive(Clone, Copy, Default)]
    struct Stage {
        x1: f32,
        y1: f32,
    }

    impl Stage {
        /// First-order all-pass `H(z) = (c + z^-1) / (1 + c z^-1)`, |c| < 1 by construction.
        #[inline]
        fn process(&mut self, x: f32, c: f32) -> f32 {
            let y = c * x + self.x1 - c * self.y1;
            self.x1 = x;
            self.y1 = y;
            y
        }
    }

    pub struct Phaser {
        params: Params,
        sr: f32,
        stages: [[Stage; MAX_STAGES]; 2],
        last: [f32; 2],
        phase: f32,
    }

    impl Default for Phaser {
        fn default() -> Self {
            Self::new()
        }
    }

    impl Phaser {
        pub fn new() -> Self {
            Self { params: Params::new(&DESCRIPTORS), sr: 48_000.0, stages: [[Stage::default(); MAX_STAGES]; 2], last: [0.0; 2], phase: 0.0 }
        }
        pub fn stage_count(&self) -> usize {
            (self.params.as_index(P_STAGES).min(5) + 1) * 2
        }
        /// All-pass coefficient for a break frequency, bounded inside the unit circle.
        #[inline]
        pub fn coefficient(hz: f32, sr: f32) -> f32 {
            let f = hz.clamp(10.0, sr * 0.49);
            let t = (std::f32::consts::PI * f / sr).tan();
            ((t - 1.0) / (t + 1.0)).clamp(-0.999, 0.999)
        }
    }

    impl Processor for Phaser {
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
            let rate = lfo_rate_hz(&self.params, P_RATE, P_SYNC, ctx.transport.tempo_bpm);
            let shape = self.params.as_index(P_SHAPE);
            let n = self.stage_count();
            let depth = (self.params.phys_smoothed(P_DEPTH, frames) / 100.0).clamp(0.0, 1.0);
            let center = self.params.phys_smoothed(P_CENTER, frames);
            let fb = (self.params.phys_smoothed(P_FEEDBACK, frames) / 100.0).clamp(0.0, MAX_FEEDBACK);
            let spread = (self.params.phys_smoothed(P_SPREAD, frames) / 100.0).clamp(0.0, 1.0);
            let mix = (self.params.phys_smoothed(P_MIX, frames) / 100.0).clamp(0.0, 1.0);
            let inc = rate / self.sr;
            let sr = self.sr;
            let (first, rest) = ctx.audio.outputs.split_at_mut(1);
            let (l, r) = (&mut first[0][..frames], &mut rest[0][..frames]);
            let inputs = ctx.audio.inputs;
            for i in 0..frames {
                let xs = [inputs[0][i], inputs[1][i]];
                let mut ys = [0.0f32; 2];
                for c in 0..2 {
                    let ph = self.phase + c as f32 * spread * 0.5;
                    let hz = center * (lfo_value(ph, shape) * depth * SWEEP_OCTAVES).exp2();
                    let coef = Self::coefficient(hz, sr);
                    let mut s = bound(xs[c] + fb * self.last[c]);
                    for stage in self.stages[c].iter_mut().take(n) {
                        s = stage.process(s, coef);
                    }
                    self.last[c] = s;
                    ys[c] = mix_wet(xs[c], s, mix);
                }
                self.phase = (self.phase + inc).rem_euclid(1.0);
                l[i] = ys[0];
                r[i] = ys[1];
            }
        }
        fn reset(&mut self) {
            self.stages = [[Stage::default(); MAX_STAGES]; 2];
            self.last = [0.0; 2];
            self.phase = 0.0;
            self.params.snap_all();
        }
        fn tail(&self) -> Tail {
            Tail::Frames(4096)
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
