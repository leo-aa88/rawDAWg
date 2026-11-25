//! Spectral processor (`native.fx.spectral`): STFT (frame 2048, hop 512, Hann/Hann
//! overlap-add) with freeze (phase-vocoder sustain of the captured frame), blur (magnitude
//! smoothing over time), spectral gate (bins under a threshold are removed), tilt
//! (dB/octave around 1 kHz), linear frequency shift (bins moved with a phase ramp, quantised
//! to the bin width) and robotise (phases discarded). Neutral settings are an exact
//! reconstruction one frame late; the latency is reported.

use crate::common::{Params, Preset, mix_wet};
use rawdawg_dsp::math::db_to_lin;
use rawdawg_dsp::processor::{PrepareError, PrepareSpec, ProcessContext, Processor, Tail};
use rawdawg_dsp::stft::{Stft, princarg};
use rawdawg_types::events::EventKind;
use rawdawg_types::params::{ParamCategory, ParamMapping, ParamUnit};
use rawdawg_types::{ParamDescriptor, ParamId};
use realfft::num_complex::Complex;

pub const TYPE_ID: &str = "native.fx.spectral";
pub const DISPLAY_NAME: &str = "Spectral";
pub const STATE_VERSION: u32 = 1;
pub const FRAME: usize = 2048;
pub const HOP: usize = 512;
const BINS: usize = FRAME / 2 + 1;

pub const P_FREEZE: u32 = 1;
pub const P_BLUR: u32 = 2;
pub const P_SHIFT: u32 = 3;
pub const P_THRESHOLD: u32 = 4;
pub const P_TILT: u32 = 5;
pub const P_ROBOT: u32 = 6;
pub const P_MIX: u32 = 7;

pub static DESCRIPTORS: [ParamDescriptor; 7] = [
    ParamDescriptor::continuous(P_FREEZE, "freeze", "Freeze", ParamUnit::None, ParamMapping::Toggle, 0.0, ParamCategory::Effects).stepped(),
    ParamDescriptor::continuous(P_BLUR, "blur", "Blur", ParamUnit::Percent, ParamMapping::Linear { min: 0.0, max: 100.0 }, 0.0, ParamCategory::Effects).with_smoothing(30.0),
    ParamDescriptor::continuous(P_SHIFT, "shift", "Frequency shift", ParamUnit::Hertz, ParamMapping::Linear { min: -2000.0, max: 2000.0 }, 0.5, ParamCategory::Effects)
        .with_smoothing(30.0),
    ParamDescriptor::continuous(P_THRESHOLD, "threshold", "Gate threshold", ParamUnit::Decibel, ParamMapping::Linear { min: -96.0, max: 0.0 }, 0.0, ParamCategory::Effects)
        .with_smoothing(30.0),
    ParamDescriptor::continuous(P_TILT, "tilt", "Tilt", ParamUnit::Decibel, ParamMapping::Linear { min: -12.0, max: 12.0 }, 0.5, ParamCategory::Effects).with_smoothing(30.0),
    ParamDescriptor::continuous(P_ROBOT, "robot", "Robotise", ParamUnit::None, ParamMapping::Toggle, 0.0, ParamCategory::Effects).stepped(),
    ParamDescriptor::continuous(P_MIX, "mix", "Mix", ParamUnit::Percent, ParamMapping::Linear { min: 0.0, max: 100.0 }, 1.0, ParamCategory::Mix).with_smoothing(20.0),
];

pub static PRESETS: [Preset; 6] = [
    Preset { name: "Freeze pad", description: "Hold the current spectrum", values: &[("freeze", 1.0), ("mix", 70.0)] },
    Preset { name: "Smear", description: "Slow magnitude blur", values: &[("blur", 85.0)] },
    Preset { name: "Spectral gate", description: "Only the loudest partials pass", values: &[("threshold", -30.0)] },
    Preset { name: "Shift up 300 Hz", description: "Inharmonic frequency shift", values: &[("shift", 300.0), ("mix", 60.0)] },
    Preset { name: "Bright tilt", description: "+6 dB/octave", values: &[("tilt", 6.0)] },
    Preset { name: "Robot", description: "Phases discarded: monotone buzz", values: &[("robot", 1.0), ("blur", 20.0)] },
];

struct State {
    last_phase: Vec<f32>,
    phase_acc: Vec<f32>,
    mag: Vec<f32>,
    freq: Vec<f32>,
    mag_f: Vec<f32>,
    freq_f: Vec<f32>,
    mag_blur: Vec<f32>,
    freq_hold: Vec<f32>,
    work: Vec<Complex<f32>>,
    frozen: bool,
    shift_phase: f32,
}

struct Channel {
    stft: Stft,
    st: State,
}

fn channel() -> Channel {
    Channel {
        stft: Stft::new(FRAME, HOP),
        st: State {
            last_phase: vec![0.0; BINS],
            phase_acc: vec![0.0; BINS],
            mag: vec![0.0; BINS],
            freq: vec![0.0; BINS],
            mag_f: vec![0.0; BINS],
            freq_f: vec![0.0; BINS],
            mag_blur: vec![0.0; BINS],
            freq_hold: vec![0.0; BINS],
            work: vec![Complex::new(0.0, 0.0); BINS],
            frozen: false,
            shift_phase: 0.0,
        },
    }
}

#[derive(Clone, Copy)]
struct Settings {
    freeze: bool,
    blur: f32,
    shift_bins: i32,
    threshold: f32,
    tilt_db: f32,
    robot: bool,
    sine_scale: f32,
    bin_hz: f32,
}

#[allow(clippy::needless_range_loop, reason = "bin index addresses several parallel spectra")]
fn process_frame(st: &mut State, spec: &mut [Complex<f32>], s: Settings) -> bool {
    let neutral = !s.freeze && s.blur <= 0.0 && s.shift_bins == 0 && s.threshold <= 0.0 && s.tilt_db.abs() < 1e-3 && !s.robot;
    let expct = std::f32::consts::TAU * HOP as f32 / FRAME as f32;
    let osamp = (FRAME / HOP) as f32;
    // analysis (always: freeze needs the previous phases when it engages)
    for k in 0..BINS {
        let phase = spec[k].arg();
        let mut d = phase - st.last_phase[k];
        st.last_phase[k] = phase;
        d -= k as f32 * expct;
        st.freq[k] = k as f32 + osamp * princarg(d) / std::f32::consts::TAU;
        st.mag[k] = spec[k].norm();
    }
    if neutral {
        st.frozen = false;
        st.mag_blur.copy_from_slice(&st.mag);
        return false;
    }
    let blur = s.blur.clamp(0.0, 1.0) * 0.98;
    let synth = s.freeze || s.robot || blur > 0.0;
    if s.freeze {
        if !st.frozen {
            st.mag_f.copy_from_slice(&st.mag);
            st.freq_f.copy_from_slice(&st.freq);
            // continue from the analysed phases so the freeze does not click in
            st.phase_acc.copy_from_slice(&st.last_phase);
            st.frozen = true;
        }
        for k in 0..BINS {
            st.phase_acc[k] += st.freq_f[k] * expct;
            st.mag[k] = st.mag_f[k];
        }
        st.mag_blur.copy_from_slice(&st.mag);
    } else if blur > 0.0 {
        st.frozen = false;
        // smeared magnitudes carry on with the frequency of whichever frame dominated the bin:
        // a live bin resyncs to its analysed phase, a fading one keeps advancing
        for k in 0..BINS {
            st.mag_blur[k] = blur * st.mag_blur[k] + (1.0 - blur) * st.mag[k];
            if st.mag[k] >= 0.5 * st.mag_blur[k] {
                st.freq_hold[k] = st.freq[k];
                st.phase_acc[k] = st.last_phase[k];
            } else {
                st.phase_acc[k] += st.freq_hold[k] * expct;
            }
            st.mag[k] = st.mag_blur[k];
        }
    } else {
        st.frozen = false;
        st.phase_acc.copy_from_slice(&st.last_phase);
        st.mag_blur.copy_from_slice(&st.mag);
    }
    if s.threshold > 0.0 {
        let thr = s.threshold * s.sine_scale;
        for m in st.mag.iter_mut() {
            if *m < thr {
                *m = 0.0;
            }
        }
    }
    if s.tilt_db.abs() >= 1e-3 {
        for (k, m) in st.mag.iter_mut().enumerate().skip(1) {
            let oct = (k as f32 * s.bin_hz / 1000.0).log2();
            *m *= db_to_lin(s.tilt_db * oct);
        }
    }
    // rebuild the spectrum: synthesised phases (freeze/robot) or the analysed ones
    for k in 0..BINS {
        let ph = if s.robot { 0.0 } else { st.phase_acc[k] };
        st.work[k] = Complex::new(st.mag[k] * ph.cos(), st.mag[k] * ph.sin());
    }
    if s.shift_bins != 0 {
        // bins move by a whole number of bins; a phase ramp of shift·hop turns the move
        // into an exact frequency shift for the exact path too
        st.shift_phase = (st.shift_phase + s.shift_bins as f32 * expct).rem_euclid(std::f32::consts::TAU);
        let rot = Complex::new(st.shift_phase.cos(), st.shift_phase.sin());
        for v in spec.iter_mut() {
            *v = Complex::new(0.0, 0.0);
        }
        for k in 0..BINS {
            let dst = k as i32 + s.shift_bins;
            if (0..BINS as i32).contains(&dst) {
                spec[dst as usize] = st.work[k] * rot;
            }
        }
    } else {
        spec.copy_from_slice(&st.work);
    }
    synth
}

pub struct Spectral {
    params: Params,
    sr: f32,
    ch: [Channel; 2],
}

impl Default for Spectral {
    fn default() -> Self {
        Self::new()
    }
}

impl Spectral {
    pub fn new() -> Self {
        Self { params: Params::new(&DESCRIPTORS), sr: 48_000.0, ch: [channel(), channel()] }
    }
    /// The frequency shift actually applied (quantised to the bin width).
    pub fn shift_hz(&self) -> f32 {
        let bin = self.sr / FRAME as f32;
        (self.params.phys(P_SHIFT) / bin).round() * bin
    }
}

impl Processor for Spectral {
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
        self.ch = [channel(), channel()];
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
        let bin_hz = self.sr / FRAME as f32;
        let threshold_db = self.params.phys_smoothed(P_THRESHOLD, frames);
        let s = Settings {
            freeze: self.params.as_bool(P_FREEZE),
            blur: self.params.phys_smoothed(P_BLUR, frames) / 100.0,
            shift_bins: (self.params.phys_smoothed(P_SHIFT, frames) / bin_hz).round() as i32,
            threshold: if threshold_db <= -95.0 { 0.0 } else { db_to_lin(threshold_db) },
            tilt_db: self.params.phys_smoothed(P_TILT, frames),
            robot: self.params.as_bool(P_ROBOT),
            sine_scale: FRAME as f32 * 0.25,
            bin_hz,
        };
        let mix = (self.params.phys_smoothed(P_MIX, frames) / 100.0).clamp(0.0, 1.0);
        let (first, rest) = ctx.audio.outputs.split_at_mut(1);
        let (l, r) = (&mut first[0][..frames], &mut rest[0][..frames]);
        let inputs = ctx.audio.inputs;
        for i in 0..frames {
            let xs = [inputs[0][i], inputs[1][i]];
            let mut ys = [0.0f32; 2];
            for c in 0..2 {
                let Channel { stft, st } = &mut self.ch[c];
                let w = stft.push(xs[c], |spec| process_frame(st, spec, s));
                ys[c] = mix_wet(xs[c], w, mix);
            }
            l[i] = ys[0];
            r[i] = ys[1];
        }
    }
    fn reset(&mut self) {
        for c in &mut self.ch {
            c.stft.reset();
            c.st.last_phase.fill(0.0);
            c.st.phase_acc.fill(0.0);
            c.st.mag_blur.fill(0.0);
            c.st.freq_hold.fill(0.0);
            c.st.frozen = false;
            c.st.shift_phase = 0.0;
        }
        self.params.snap_all();
    }
    fn latency_frames(&self) -> u32 {
        FRAME as u32
    }
    fn tail(&self) -> Tail {
        if self.params.as_bool(P_FREEZE) { Tail::Infinite } else { Tail::Frames(2 * FRAME as u32 + 48_000) }
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
