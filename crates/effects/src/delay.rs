//! Delay (`native.fx.delay`): stereo, ping-pong, tape (wow + saturation), multitap
//! (16 taps with time/gain/pan) and reverse (segment buffers, latency reported).
//! Time in ms or tempo-synced; feedback bounded below unity with a soft clip;
//! damping low-pass and high-pass in the feedback path.

use crate::common::{Params, Preset};
use rawdawg_dsp::delayline::DelayLine;
use rawdawg_dsp::math::{db_to_lin, fast_tanh};
use rawdawg_dsp::processor::{PrepareError, PrepareSpec, ProcessContext, Processor, Tail};
use rawdawg_types::events::EventKind;
use rawdawg_types::params::{ParamCategory, ParamMapping, ParamUnit};
use rawdawg_types::{ParamDescriptor, ParamId};

pub const TYPE_ID: &str = "native.fx.delay";
pub const DISPLAY_NAME: &str = "Delay";
pub const STATE_VERSION: u32 = 1;
pub const MAX_TIME_MS: f32 = 2000.0;
pub const MAX_TAPS: usize = 16;
pub const MAX_FEEDBACK: f32 = 0.98;

pub const P_MODE: u32 = 1;
pub const P_TIME: u32 = 2;
pub const P_SYNC: u32 = 3;
pub const P_FEEDBACK: u32 = 4;
pub const P_MIX: u32 = 5;
pub const P_DAMP: u32 = 6;
pub const P_HIGHPASS: u32 = 7;
pub const P_OFFSET: u32 = 8;
pub const P_WOW_RATE: u32 = 9;
pub const P_WOW_DEPTH: u32 = 10;
pub const P_DRIVE: u32 = 11;
pub const P_WIDTH: u32 = 12;
/// Multitap: `tap_param(t, T_*)`.
pub const T_ON: u32 = 0;
pub const T_TIME: u32 = 1;
pub const T_GAIN: u32 = 2;
pub const T_PAN: u32 = 3;
pub const fn tap_param(tap: usize, offset: u32) -> u32 {
    100 + tap as u32 * 4 + offset
}

pub const MODE_LABELS: [&str; 5] = ["Stereo", "Ping-pong", "Tape", "Multitap", "Reverse"];
/// Sync divisions in quarter notes: off, 1/32, 1/16T, 1/16, 1/8T, 1/16., 1/8, 1/4T, 1/8., 1/4, 1/2T, 1/4., 1/2, 1/1.
pub const SYNC_LABELS: [&str; 14] = ["Off (ms)", "1/32", "1/16T", "1/16", "1/8T", "1/16.", "1/8", "1/4T", "1/8.", "1/4", "1/2T", "1/4.", "1/2", "1/1"];
const SYNC_QUARTERS: [f32; 14] = [0.0, 0.125, 1.0 / 6.0, 0.25, 1.0 / 3.0, 0.375, 0.5, 2.0 / 3.0, 0.75, 1.0, 4.0 / 3.0, 1.5, 2.0, 4.0];

macro_rules! descriptors {
    ($($t:expr),*) => {
        [
            ParamDescriptor::continuous(P_MODE, "mode", "Mode", ParamUnit::None, ParamMapping::Discrete { steps: 5 }, 0.0, ParamCategory::Topology).stepped(),
            ParamDescriptor::continuous(P_TIME, "time", "Time", ParamUnit::Milliseconds, ParamMapping::Log { min: 1.0, max: 2000.0 }, 0.7598, ParamCategory::Effects).with_smoothing(60.0),
            ParamDescriptor::continuous(P_SYNC, "sync", "Sync", ParamUnit::None, ParamMapping::Discrete { steps: 14 }, 0.0, ParamCategory::Effects).stepped(),
            ParamDescriptor::continuous(P_FEEDBACK, "feedback", "Feedback", ParamUnit::Percent, ParamMapping::Linear { min: 0.0, max: 98.0 }, 0.4, ParamCategory::Effects).with_smoothing(20.0),
            ParamDescriptor::continuous(P_MIX, "mix", "Mix", ParamUnit::Percent, ParamMapping::Linear { min: 0.0, max: 100.0 }, 0.35, ParamCategory::Mix).with_smoothing(20.0),
            ParamDescriptor::continuous(P_DAMP, "damp", "Damping", ParamUnit::Hertz, ParamMapping::Log { min: 500.0, max: 20_000.0 }, 0.7, ParamCategory::Effects).with_smoothing(30.0),
            ParamDescriptor::continuous(P_HIGHPASS, "highpass", "Low cut", ParamUnit::Hertz, ParamMapping::Log { min: 20.0, max: 2000.0 }, 0.3, ParamCategory::Effects).with_smoothing(30.0),
            ParamDescriptor::continuous(P_OFFSET, "offset", "L/R offset", ParamUnit::Percent, ParamMapping::Linear { min: -50.0, max: 50.0 }, 0.5, ParamCategory::Effects).with_smoothing(60.0),
            ParamDescriptor::continuous(P_WOW_RATE, "wow_rate", "Wow rate", ParamUnit::Hertz, ParamMapping::Log { min: 0.05, max: 8.0 }, 0.4, ParamCategory::Modulation),
            ParamDescriptor::continuous(P_WOW_DEPTH, "wow_depth", "Wow depth", ParamUnit::Milliseconds, ParamMapping::Linear { min: 0.0, max: 10.0 }, 0.15, ParamCategory::Modulation).with_smoothing(50.0),
            ParamDescriptor::continuous(P_DRIVE, "drive", "Tape drive", ParamUnit::Decibel, ParamMapping::Linear { min: 0.0, max: 24.0 }, 0.25, ParamCategory::Effects).with_smoothing(20.0),
            ParamDescriptor::continuous(P_WIDTH, "width", "Width", ParamUnit::Percent, ParamMapping::Linear { min: 0.0, max: 100.0 }, 1.0, ParamCategory::Mix).with_smoothing(20.0),
            $(
                ParamDescriptor::continuous(tap_param($t, T_ON), concat!("tap", $t, ".on"), concat!("Tap ", $t, " on"), ParamUnit::None, ParamMapping::Toggle, if $t < 4 { 1.0 } else { 0.0 }, ParamCategory::Effects).stepped(),
                ParamDescriptor::continuous(tap_param($t, T_TIME), concat!("tap", $t, ".time"), concat!("Tap ", $t, " time"), ParamUnit::Ratio, ParamMapping::Linear { min: 0.0, max: 4.0 }, ($t as f32 + 1.0) / 16.0, ParamCategory::Effects).with_smoothing(60.0),
                ParamDescriptor::continuous(tap_param($t, T_GAIN), concat!("tap", $t, ".gain"), concat!("Tap ", $t, " gain"), ParamUnit::Decibel, ParamMapping::Linear { min: -48.0, max: 6.0 }, (48.0 - 3.0 * $t as f32) / 54.0, ParamCategory::Effects).with_smoothing(20.0),
                ParamDescriptor::continuous(tap_param($t, T_PAN), concat!("tap", $t, ".pan"), concat!("Tap ", $t, " pan"), ParamUnit::None, ParamMapping::Linear { min: -1.0, max: 1.0 }, if $t % 2 == 0 { 0.3 } else { 0.7 }, ParamCategory::Effects).with_smoothing(20.0),
            )*
        ]
    };
}
pub static DESCRIPTORS: [ParamDescriptor; 12 + MAX_TAPS * 4] = descriptors!(0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15);

pub static PRESETS: [Preset; 7] = [
    Preset { name: "Slapback", description: "Stereo 110 ms, little feedback", values: &[("mode", 0.0), ("time", 110.0), ("feedback", 12.0), ("mix", 30.0), ("offset", 8.0)] },
    Preset { name: "Dotted eighth", description: "Synced 1/8. with damping", values: &[("mode", 0.0), ("sync", 8.0), ("feedback", 45.0), ("mix", 35.0), ("damp", 4000.0)] },
    Preset { name: "Ping-pong 1/4", description: "Alternating left/right quarter notes", values: &[("mode", 1.0), ("sync", 9.0), ("feedback", 55.0), ("mix", 40.0)] },
    Preset {
        name: "Tape wobble",
        description: "Tape mode with wow and saturation, dark repeats",
        values: &[("mode", 2.0), ("time", 380.0), ("feedback", 60.0), ("mix", 35.0), ("damp", 2500.0), ("wow_rate", 0.6), ("wow_depth", 3.0), ("drive", 9.0)],
    },
    Preset {
        name: "Multitap rhythm",
        description: "Four taps at 1/4, 1/2, 3/4 and 1× the time, panned",
        values: &[
            ("mode", 3.0),
            ("time", 600.0),
            ("feedback", 20.0),
            ("mix", 40.0),
            ("tap0.time", 0.25),
            ("tap1.time", 0.5),
            ("tap2.time", 0.75),
            ("tap3.time", 1.0),
            ("tap0.gain", -2.0),
            ("tap1.gain", -5.0),
            ("tap2.gain", -8.0),
            ("tap3.gain", -11.0),
        ],
    },
    Preset { name: "Reverse 1/2", description: "Half-bar segments played backwards", values: &[("mode", 4.0), ("sync", 12.0), ("feedback", 0.0), ("mix", 50.0)] },
    Preset {
        name: "Dub throw",
        description: "Long feedback, heavy damping and drive",
        values: &[("mode", 1.0), ("time", 500.0), ("feedback", 80.0), ("mix", 45.0), ("damp", 1500.0), ("drive", 6.0)],
    },
];

struct Channel {
    line: DelayLine,
    damp_z: f32,
    hp_z: f32,
}

pub struct Delay {
    params: Params,
    sr: f32,
    ch: [Channel; 2],
    /// Current (smoothed) delay times in samples.
    time_smp: [f32; 2],
    wow_phase: f32,
    // reverse mode: segment buffers
    seg: [Vec<f32>; 2],
    seg_prev: [Vec<f32>; 2],
    seg_len: usize,
    seg_pos: usize,
    latency: u32,
}

impl Default for Delay {
    fn default() -> Self {
        Self::new()
    }
}

fn new_channel(cap: usize) -> Channel {
    Channel { line: DelayLine::new(cap), damp_z: 0.0, hp_z: 0.0 }
}

impl Delay {
    pub fn new() -> Self {
        Self {
            params: Params::new(&DESCRIPTORS),
            sr: 48_000.0,
            ch: [new_channel(96_000 + 4096), new_channel(96_000 + 4096)],
            time_smp: [24_000.0; 2],
            wow_phase: 0.0,
            seg: [vec![0.0; 1], vec![0.0; 1]],
            seg_prev: [vec![0.0; 1], vec![0.0; 1]],
            seg_len: 1,
            seg_pos: 0,
            latency: 0,
        }
    }

    pub fn mode(&self) -> usize {
        self.params.as_index(P_MODE).min(4)
    }

    /// Base delay time in ms for the current settings and tempo.
    pub fn base_time_ms(&self, tempo_bpm: f64) -> f32 {
        let sync = self.params.as_index(P_SYNC);
        if sync > 0 && sync < SYNC_QUARTERS.len() {
            let beat_ms = 60_000.0 / tempo_bpm.clamp(20.0, 400.0) as f32;
            (beat_ms * SYNC_QUARTERS[sync]).clamp(1.0, MAX_TIME_MS)
        } else {
            self.params.phys(P_TIME).clamp(1.0, MAX_TIME_MS)
        }
    }

    fn configure_reverse(&mut self, tempo_bpm: f64) {
        let len = ((self.base_time_ms(tempo_bpm) / 1000.0 * self.sr) as usize).clamp(32, self.seg[0].len().max(32));
        if len != self.seg_len {
            self.seg_len = len;
            self.seg_pos = 0;
            for s in self.seg.iter_mut().chain(self.seg_prev.iter_mut()) {
                s[..].fill(0.0);
            }
        }
        self.latency = if self.mode() == 4 { self.seg_len as u32 } else { 0 };
    }

    /// Tap definitions (on, time ratio, gain dB, pan) for the UI.
    pub fn taps(&self) -> [(bool, f32, f32, f32); MAX_TAPS] {
        let mut out = [(false, 0.0f32, 0.0f32, 0.0f32); MAX_TAPS];
        for (t, o) in out.iter_mut().enumerate() {
            *o = (self.params.as_bool(tap_param(t, T_ON)), self.params.phys(tap_param(t, T_TIME)), self.params.phys(tap_param(t, T_GAIN)), self.params.phys(tap_param(t, T_PAN)));
        }
        out
    }
}

impl Processor for Delay {
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
        let cap = (MAX_TIME_MS / 1000.0 * self.sr) as usize + 4096;
        self.ch = [new_channel(cap), new_channel(cap)];
        let seg_cap = (MAX_TIME_MS / 1000.0 * self.sr) as usize + 1;
        self.seg = [vec![0.0; seg_cap], vec![0.0; seg_cap]];
        self.seg_prev = [vec![0.0; seg_cap], vec![0.0; seg_cap]];
        self.seg_len = 0;
        self.configure_reverse(120.0);
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
        let tempo = ctx.transport.tempo_bpm;
        let mode = self.mode();
        let base_ms = if self.params.as_index(P_SYNC) > 0 { self.base_time_ms(tempo) } else { self.params.phys_smoothed(P_TIME, frames).clamp(1.0, MAX_TIME_MS) };
        let fb = (self.params.phys_smoothed(P_FEEDBACK, frames) / 100.0).clamp(0.0, MAX_FEEDBACK);
        let mix = (self.params.phys_smoothed(P_MIX, frames) / 100.0).clamp(0.0, 1.0);
        let damp_hz = self.params.phys_smoothed(P_DAMP, frames);
        let hp_hz = self.params.phys_smoothed(P_HIGHPASS, frames);
        let offset = self.params.phys_smoothed(P_OFFSET, frames) / 100.0;
        let width = (self.params.phys_smoothed(P_WIDTH, frames) / 100.0).clamp(0.0, 1.0);
        let drive = db_to_lin(self.params.phys_smoothed(P_DRIVE, frames));
        let wow_rate = self.params.phys(P_WOW_RATE);
        let wow_depth = self.params.phys_smoothed(P_WOW_DEPTH, frames);
        let damp_c = 1.0 - (-std::f32::consts::TAU * damp_hz / self.sr).exp();
        let hp_c = 1.0 - (-std::f32::consts::TAU * hp_hz / self.sr).exp();
        let base_smp = base_ms / 1000.0 * self.sr;
        let max_smp = (self.ch[0].line.capacity() - 4096) as f32;
        let (first, rest) = ctx.audio.outputs.split_at_mut(1);
        let (l, r) = (&mut first[0][..frames], &mut rest[0][..frames]);
        let inputs = ctx.audio.inputs;
        if mode == 4 {
            // reverse: play the previous segment backwards while recording the current one
            if self.seg_len == 0 || (base_smp as usize).clamp(32, self.seg[0].len()) != self.seg_len {
                self.configure_reverse(tempo);
            }
            for i in 0..frames {
                let (xl, xr) = (inputs[0][i], inputs[1][i]);
                let p = self.seg_pos;
                let ridx = self.seg_len - 1 - p;
                let (wl, wr) = (self.seg_prev[0][ridx], self.seg_prev[1][ridx]);
                // feedback: the reversed output re-enters the segment
                self.seg[0][p] = xl + wl * fb;
                self.seg[1][p] = xr + wr * fb;
                self.seg_pos += 1;
                if self.seg_pos >= self.seg_len {
                    self.seg_pos = 0;
                    for c in 0..2 {
                        let (a, b) = (&mut self.seg[c], &mut self.seg_prev[c]);
                        b[..self.seg_len].copy_from_slice(&a[..self.seg_len]);
                    }
                }
                l[i] = xl + (wl - xl) * mix;
                r[i] = xr + (wr - xr) * mix;
            }
            return;
        }
        let wow_inc = wow_rate / self.sr;
        let time_c = 1.0 - (-1.0 / (0.02 * self.sr)).exp(); // 20 ms glide on time changes
        // positive offset delays the right channel, negative the left
        let target_l = (base_smp * (1.0 + (-offset).max(0.0))).clamp(1.0, max_smp);
        let target_r = (base_smp * (1.0 + offset.max(0.0))).clamp(1.0, max_smp);
        let mut tap_gains: [(f32, f32, f32); MAX_TAPS] = [(1.0, 0.0, 0.0); MAX_TAPS];
        let mut n_taps = 0usize;
        if mode == 3 {
            for t in self.taps().iter().filter(|t| t.0) {
                let (pl, pr) = rawdawg_types::units::Pan(f64::from(t.3)).equal_power_gains();
                tap_gains[n_taps] = ((t.1 * base_smp).clamp(1.0, max_smp), db_to_lin(t.2) * pl * std::f32::consts::SQRT_2, db_to_lin(t.2) * pr * std::f32::consts::SQRT_2);
                n_taps += 1;
            }
        }
        for i in 0..frames {
            let (xl, xr) = (inputs[0][i], inputs[1][i]);
            self.time_smp[0] += time_c * (target_l - self.time_smp[0]);
            self.time_smp[1] += time_c * (target_r - self.time_smp[1]);
            let mut wow = 0.0;
            if mode == 2 {
                self.wow_phase = (self.wow_phase + wow_inc).fract();
                wow = (self.wow_phase * std::f32::consts::TAU).sin() * wow_depth / 1000.0 * self.sr;
            }
            let (dl, dr) = match mode {
                3 => {
                    let mut al = 0.0;
                    let mut ar = 0.0;
                    for (t, gl, gr) in &tap_gains[..n_taps] {
                        let v = self.ch[0].line.read_frac(*t);
                        al += v * gl;
                        ar += v * gr;
                    }
                    (al, ar)
                }
                _ => (self.ch[0].line.read_frac((self.time_smp[0] + wow).max(1.0)), self.ch[1].line.read_frac((self.time_smp[1] + wow).max(1.0))),
            };
            // feedback path: damping low-pass, low cut, bounded gain with a soft clip
            let (fl, fr) = match mode {
                1 => (dr, dl), // ping-pong crosses
                3 => {
                    let f = self.ch[0].line.read_frac(self.time_smp[0]);
                    (f, f)
                }
                _ => (dl, dr),
            };
            let mut wet_in_l = fl * fb;
            let mut wet_in_r = fr * fb;
            for (c, v) in [(0usize, &mut wet_in_l), (1, &mut wet_in_r)] {
                let ch = &mut self.ch[c];
                ch.damp_z += damp_c * (*v - ch.damp_z);
                let lp = ch.damp_z;
                ch.hp_z += hp_c * (lp - ch.hp_z);
                *v = lp - ch.hp_z;
            }
            if mode == 2 {
                wet_in_l = fast_tanh(wet_in_l * drive) / drive.max(1.0).sqrt();
                wet_in_r = fast_tanh(wet_in_r * drive) / drive.max(1.0).sqrt();
            } else {
                wet_in_l = fast_tanh(wet_in_l);
                wet_in_r = fast_tanh(wet_in_r);
            }
            match mode {
                1 => {
                    let mono = 0.5 * (xl + xr);
                    self.ch[0].line.push(mono + wet_in_l);
                    self.ch[1].line.push(wet_in_r);
                }
                3 => {
                    let mono = 0.5 * (xl + xr);
                    self.ch[0].line.push(mono + wet_in_l);
                    self.ch[1].line.push(0.0);
                }
                _ => {
                    self.ch[0].line.push(xl + wet_in_l);
                    self.ch[1].line.push(xr + wet_in_r);
                }
            }
            // width on the wet signal
            let mid = 0.5 * (dl + dr);
            let side = 0.5 * (dl - dr) * width;
            let (wl, wr) = (mid + side, mid - side);
            l[i] = xl + (wl - xl) * mix;
            r[i] = xr + (wr - xr) * mix;
        }
    }
    fn reset(&mut self) {
        for c in &mut self.ch {
            c.line.reset();
            c.damp_z = 0.0;
            c.hp_z = 0.0;
        }
        self.wow_phase = 0.0;
        self.seg_pos = 0;
        for s in self.seg.iter_mut().chain(self.seg_prev.iter_mut()) {
            s.fill(0.0);
        }
        self.params.snap_all();
        let t = self.params.phys(P_TIME) / 1000.0 * self.sr;
        self.time_smp = [t, t];
    }
    fn latency_frames(&self) -> u32 {
        self.latency
    }
    fn tail(&self) -> Tail {
        let fb = self.params.phys(P_FEEDBACK) / 100.0;
        if fb >= 0.97 {
            return Tail::Unknown;
        }
        // repeats until −60 dB
        let reps = if fb <= 0.0 { 1.0 } else { (-60.0 / (20.0 * fb.max(1e-3).log10())).ceil().clamp(1.0, 200.0) };
        Tail::Frames((self.params.phys(P_TIME) / 1000.0 * self.sr * reps * 4.0) as u32)
    }
    fn param(&self, id: ParamId) -> Option<f32> {
        self.params.param(id)
    }
    fn set_param(&mut self, id: ParamId, normalized: f32) -> bool {
        let ok = self.params.set(id, normalized);
        if ok && (id.0 == P_MODE || id.0 == P_SYNC || id.0 == P_TIME) {
            self.configure_reverse(120.0);
        }
        ok
    }
    fn save_state(&self) -> Vec<u8> {
        self.params.save(STATE_VERSION)
    }
    fn load_state(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.params.load(bytes, STATE_VERSION)?;
        self.seg_len = 0;
        self.configure_reverse(120.0);
        let t = self.params.phys(P_TIME) / 1000.0 * self.sr;
        self.time_smp = [t, t];
        Ok(())
    }
}
