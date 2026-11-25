//! `native.sampler`: one sample, pitched relative to a root note, one-shot or
//! gated, start/end/loop bounds with forward loop and crossfade, ADSR, polyphony.

use crate::asset::SampleAsset;
use rawdawg_dsp::envelope::{Adsr, AdsrParams};
use rawdawg_dsp::math::{db_to_lin, sanitize};
use rawdawg_dsp::processor::{PrepareError, PrepareSpec, ProcessContext, Processor, Tail, find_descriptor};
use rawdawg_synthesizer::registry::{HeldNotes, NoteKey, SustainTable, source_slot};
use rawdawg_types::events::{EventKind, TimedEvent};
use rawdawg_types::params::{ParamCategory, ParamDescriptor, ParamMapping, ParamUnit};
use rawdawg_types::{AssetId, ParamId};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum LoopMode {
    #[default]
    Off,
    Forward,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct SamplerPatch {
    pub version: u32,
    pub name: String,
    /// Content hash of the sample asset in the project (resolved by the host).
    pub asset_id: Option<AssetId>,
    pub root_pitch: u8,
    pub fine_cents: f32,
    /// `true`: the note plays to the end regardless of note-off.
    pub one_shot: bool,
    /// Playback bounds as fractions of the sample 0..1.
    pub start: f32,
    pub end: f32,
    pub loop_mode: LoopMode,
    pub loop_start: f32,
    pub loop_end: f32,
    /// Loop crossfade in milliseconds.
    pub loop_crossfade_ms: f32,
    pub gain_db: f32,
    pub pan: f32,
    pub env: AdsrParams,
    pub max_voices: u32,
    pub velocity_sensitivity: f32,
}

impl Default for SamplerPatch {
    fn default() -> Self {
        Self {
            version: 1,
            name: "Sample".into(),
            asset_id: None,
            root_pitch: 60,
            fine_cents: 0.0,
            one_shot: false,
            start: 0.0,
            end: 1.0,
            loop_mode: LoopMode::Off,
            loop_start: 0.0,
            loop_end: 1.0,
            loop_crossfade_ms: 5.0,
            gain_db: 0.0,
            pan: 0.0,
            env: AdsrParams { attack_s: 0.002, decay_s: 0.1, sustain: 1.0, release_s: 0.08 },
            max_voices: 16,
            velocity_sensitivity: 0.8,
        }
    }
}

impl SamplerPatch {
    pub fn validate(&self) -> Result<(), String> {
        let unit = |v: f32| v.is_finite() && (0.0..=1.0).contains(&v);
        if self.version != 1 {
            return Err("unsupported sampler patch version".into());
        }
        if self.root_pitch > 127 || !self.fine_cents.is_finite() || !(-100.0..=100.0).contains(&self.fine_cents) {
            return Err("root pitch/fine out of range".into());
        }
        if !unit(self.start) || !unit(self.end) || self.end <= self.start {
            return Err("start/end bounds invalid".into());
        }
        if !unit(self.loop_start) || !unit(self.loop_end) || (self.loop_mode == LoopMode::Forward && self.loop_end <= self.loop_start) {
            return Err("loop bounds invalid".into());
        }
        if !self.loop_crossfade_ms.is_finite() || !(0.0..=200.0).contains(&self.loop_crossfade_ms) {
            return Err("loop crossfade out of range".into());
        }
        if !self.gain_db.is_finite() || !(-60.0..=12.0).contains(&self.gain_db) || !self.pan.is_finite() || !(-1.0..=1.0).contains(&self.pan) {
            return Err("gain/pan out of range".into());
        }
        if !(1..=64).contains(&self.max_voices) || !unit(self.velocity_sensitivity) {
            return Err("voices/velocity out of range".into());
        }
        if self.env != self.env.clamped() {
            return Err("envelope out of range".into());
        }
        Ok(())
    }
    pub fn to_json(&self) -> Vec<u8> {
        serde_json::to_vec_pretty(self).expect("serializes")
    }
    pub fn from_json(bytes: &[u8]) -> Result<Self, String> {
        let p: Self = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
        p.validate()?;
        Ok(p)
    }
}

pub const P_GAIN: u32 = 1;
pub const P_PAN: u32 = 2;
pub const P_ROOT: u32 = 3;
pub const P_FINE: u32 = 4;
pub const P_ONE_SHOT: u32 = 5;
pub const P_START: u32 = 6;
pub const P_END: u32 = 7;
pub const P_LOOP_MODE: u32 = 8;
pub const P_LOOP_START: u32 = 9;
pub const P_LOOP_END: u32 = 10;
pub const P_LOOP_XFADE: u32 = 11;
pub const P_ATTACK: u32 = 12;
pub const P_DECAY: u32 = 13;
pub const P_SUSTAIN: u32 = 14;
pub const P_RELEASE: u32 = 15;
pub const P_VELOCITY: u32 = 16;

pub static DESCRIPTORS: [ParamDescriptor; 16] = [
    ParamDescriptor::continuous(P_GAIN, "gain", "Gain", ParamUnit::Decibel, ParamMapping::Linear { min: -60.0, max: 12.0 }, 60.0 / 72.0, ParamCategory::Mix),
    ParamDescriptor::continuous(P_PAN, "pan", "Pan", ParamUnit::None, ParamMapping::Linear { min: -1.0, max: 1.0 }, 0.5, ParamCategory::Mix),
    ParamDescriptor::continuous(P_ROOT, "root", "Root Note", ParamUnit::None, ParamMapping::Discrete { steps: 128 }, 60.0 / 127.0, ParamCategory::Pitch).stepped(),
    ParamDescriptor::continuous(P_FINE, "fine", "Fine Tune", ParamUnit::Cents, ParamMapping::Linear { min: -100.0, max: 100.0 }, 0.5, ParamCategory::Pitch),
    ParamDescriptor::continuous(P_ONE_SHOT, "one_shot", "One-shot", ParamUnit::None, ParamMapping::Toggle, 0.0, ParamCategory::Envelope).stepped(),
    ParamDescriptor::continuous(P_START, "start", "Start", ParamUnit::Percent, ParamMapping::Linear { min: 0.0, max: 1.0 }, 0.0, ParamCategory::Wave).with_smoothing(0.0),
    ParamDescriptor::continuous(P_END, "end", "End", ParamUnit::Percent, ParamMapping::Linear { min: 0.0, max: 1.0 }, 1.0, ParamCategory::Wave).with_smoothing(0.0),
    ParamDescriptor::continuous(P_LOOP_MODE, "loop.mode", "Loop", ParamUnit::None, ParamMapping::Discrete { steps: 2 }, 0.0, ParamCategory::Wave).stepped(),
    ParamDescriptor::continuous(P_LOOP_START, "loop.start", "Loop Start", ParamUnit::Percent, ParamMapping::Linear { min: 0.0, max: 1.0 }, 0.0, ParamCategory::Wave)
        .with_smoothing(0.0),
    ParamDescriptor::continuous(P_LOOP_END, "loop.end", "Loop End", ParamUnit::Percent, ParamMapping::Linear { min: 0.0, max: 1.0 }, 1.0, ParamCategory::Wave).with_smoothing(0.0),
    ParamDescriptor::continuous(
        P_LOOP_XFADE,
        "loop.crossfade",
        "Loop Crossfade",
        ParamUnit::Milliseconds,
        ParamMapping::Linear { min: 0.0, max: 200.0 },
        0.025,
        ParamCategory::Wave,
    )
    .with_smoothing(0.0),
    ParamDescriptor::continuous(P_ATTACK, "env.attack", "Attack", ParamUnit::Seconds, ParamMapping::LogWithZero { min: 0.001, max: 30.0 }, 0.067, ParamCategory::Envelope)
        .with_smoothing(0.0),
    ParamDescriptor::continuous(P_DECAY, "env.decay", "Decay", ParamUnit::Seconds, ParamMapping::LogWithZero { min: 0.001, max: 30.0 }, 0.45, ParamCategory::Envelope)
        .with_smoothing(0.0),
    ParamDescriptor::continuous(P_SUSTAIN, "env.sustain", "Sustain", ParamUnit::Percent, ParamMapping::Linear { min: 0.0, max: 1.0 }, 1.0, ParamCategory::Envelope)
        .with_smoothing(0.0),
    ParamDescriptor::continuous(P_RELEASE, "env.release", "Release", ParamUnit::Seconds, ParamMapping::LogWithZero { min: 0.001, max: 30.0 }, 0.43, ParamCategory::Envelope)
        .with_smoothing(0.0),
    ParamDescriptor::continuous(P_VELOCITY, "velocity", "Velocity", ParamUnit::Percent, ParamMapping::Linear { min: 0.0, max: 1.0 }, 0.8, ParamCategory::Envelope),
];

#[derive(Clone, Debug)]
struct Voice {
    key: Option<NoteKey>,
    age: u64,
    released: bool,
    sustained: bool,
    velocity: f32,
    pos: f64,
    inc: f64,
    env: Adsr,
    bend_semitones: f32,
    /// One-shot notes ignore note-off; they end when the sample ends.
    one_shot: bool,
}

pub struct Sampler {
    patch: SamplerPatch,
    asset: Option<Arc<SampleAsset>>,
    voices: Vec<Voice>,
    sustain: SustainTable,
    held: HeldNotes,
    bend: [f32; 16],
    age: u64,
    sample_rate: f64,
    prepared: bool,
    gain: f32,
    pan_l: f32,
    pan_r: f32,
    nonfinite: u64,
}

impl Sampler {
    pub fn new(patch: SamplerPatch, asset: Option<Arc<SampleAsset>>) -> Self {
        Self {
            patch,
            asset,
            voices: Vec::new(),
            sustain: SustainTable::new(),
            held: HeldNotes::new(),
            bend: [0.0; 16],
            age: 0,
            sample_rate: 48_000.0,
            prepared: false,
            gain: 1.0,
            pan_l: 0.707,
            pan_r: 0.707,
            nonfinite: 0,
        }
    }
    pub fn patch(&self) -> &SamplerPatch {
        &self.patch
    }
    pub fn asset(&self) -> Option<&Arc<SampleAsset>> {
        self.asset.as_ref()
    }
    /// Non-RT: replace the sample (host resolves assets).
    pub fn set_asset(&mut self, asset: Option<Arc<SampleAsset>>) {
        self.asset = asset.filter(|a| a.is_valid());
        for v in &mut self.voices {
            v.key = None;
            v.env.reset();
        }
    }
    pub fn set_patch(&mut self, patch: SamplerPatch) -> Result<(), String> {
        patch.validate()?;
        self.patch = patch;
        self.refresh();
        if self.prepared && self.voices.len() != self.patch.max_voices as usize + 4 {
            self.build_voices();
        }
        Ok(())
    }
    fn refresh(&mut self) {
        self.gain = db_to_lin(self.patch.gain_db);
        let (l, r) = rawdawg_types::units::Pan(f64::from(self.patch.pan)).equal_power_gains();
        self.pan_l = l;
        self.pan_r = r;
        for v in &mut self.voices {
            v.env.set_params(self.patch.env);
        }
    }
    fn build_voices(&mut self) {
        let n = self.patch.max_voices as usize + 4;
        let sr = self.sample_rate as f32;
        self.voices = (0..n)
            .map(|_| Voice {
                key: None,
                age: 0,
                released: false,
                sustained: false,
                velocity: 0.0,
                pos: 0.0,
                inc: 0.0,
                env: Adsr::new(sr, self.patch.env),
                bend_semitones: 0.0,
                one_shot: false,
            })
            .collect();
    }
    pub fn active_voice_count(&self) -> usize {
        self.voices.iter().filter(|v| v.key.is_some()).count()
    }

    fn bounds(&self, asset: &SampleAsset) -> (f64, f64, usize, usize) {
        let n = asset.frames() as f64;
        let start = (f64::from(self.patch.start) * n).floor();
        let end = (f64::from(self.patch.end) * n).ceil().min(n).max(start + 1.0);
        let ls = (f64::from(self.patch.loop_start) * n).floor() as usize;
        let le = ((f64::from(self.patch.loop_end) * n).ceil() as usize).min(asset.frames()).max(ls + 2);
        (start, end, ls, le)
    }

    fn increment(&self, pitch: u8, bend: f32, asset: &SampleAsset) -> f64 {
        let semis = f64::from(pitch) - f64::from(self.patch.root_pitch) + f64::from(self.patch.fine_cents) / 100.0 + f64::from(bend);
        2f64.powf(semis / 12.0) * f64::from(asset.sample_rate) / self.sample_rate
    }

    fn allocate(&mut self) -> usize {
        if let Some(i) = self.voices.iter().position(|v| v.key.is_none()) {
            return i;
        }
        let mut best = 0;
        let mut best_age = u64::MAX;
        for (i, v) in self.voices.iter().enumerate() {
            let a = if v.released { v.age / 2 } else { v.age };
            if a < best_age {
                best_age = a;
                best = i;
            }
        }
        best
    }

    fn note_on(&mut self, key: NoteKey, velocity: f32) {
        let Some(asset) = self.asset.clone() else { return };
        self.age += 1;
        let (start, _, _, _) = self.bounds(&asset);
        let bend = self.bend[usize::from(key.channel) % 16] * 2.0;
        let i = self.allocate();
        let inc = self.increment(key.pitch, bend, &asset);
        let v = &mut self.voices[i];
        v.key = Some(key);
        v.age = self.age;
        v.released = false;
        v.sustained = false;
        v.velocity = velocity;
        v.pos = start;
        v.inc = inc;
        v.bend_semitones = bend;
        v.one_shot = self.patch.one_shot;
        v.env.note_on();
    }

    fn note_off(&mut self, key: NoteKey) {
        let pedal = self.sustain.is_down(key.source, key.channel);
        for v in &mut self.voices {
            if v.key == Some(key) && !v.released && !v.one_shot {
                if pedal {
                    v.sustained = true;
                } else {
                    v.released = true;
                    v.env.note_off();
                }
            }
        }
    }

    fn all_notes_off(&mut self, source: Option<u8>) {
        for v in &mut self.voices {
            if let Some(k) = v.key
                && source.is_none_or(|s| s == k.source)
            {
                v.released = true;
                v.env.note_off();
            }
        }
    }

    fn handle_event(&mut self, ev: &TimedEvent) {
        let source = source_slot(ev.source);
        match ev.kind {
            EventKind::NoteOn { note_id, pitch, velocity } => self.note_on(NoteKey { source, channel: ev.channel, note_id, pitch }, velocity),
            EventKind::NoteOff { note_id, pitch } => self.note_off(NoteKey { source, channel: ev.channel, note_id, pitch }),
            EventKind::Control { cc: 64, value } => {
                let down = value >= 0.5;
                self.sustain.set(source, ev.channel, down);
                if !down {
                    for v in &mut self.voices {
                        if let Some(k) = v.key
                            && v.sustained
                            && k.source == source
                            && k.channel == ev.channel
                        {
                            v.sustained = false;
                            v.released = true;
                            v.env.note_off();
                        }
                    }
                }
            }
            EventKind::Control { cc: 123, .. } => self.all_notes_off(Some(source)),
            EventKind::Control { cc: 120, .. } | EventKind::Reset => self.reset(),
            EventKind::PitchBend { value } => {
                self.bend[usize::from(ev.channel) % 16] = value;
                if let Some(asset) = self.asset.clone() {
                    for v in &mut self.voices {
                        if let Some(k) = v.key
                            && k.channel == ev.channel
                        {
                            v.bend_semitones = value * 2.0;
                            v.inc = {
                                let semis = f64::from(k.pitch) - f64::from(self.patch.root_pitch) + f64::from(self.patch.fine_cents) / 100.0 + f64::from(v.bend_semitones);
                                2f64.powf(semis / 12.0) * f64::from(asset.sample_rate) / self.sample_rate
                            };
                        }
                    }
                }
            }
            EventKind::AllNotesOff { source: s } => self.all_notes_off(s.map(source_slot)),
            EventKind::Param { id, value } => {
                self.apply_param(id, value);
            }
            _ => {}
        }
    }

    fn apply_param(&mut self, id: ParamId, n: f32) -> bool {
        let Some(d) = find_descriptor(&DESCRIPTORS, id) else { return false };
        let phys = d.to_physical(n);
        let p = &mut self.patch;
        match id.0 {
            P_GAIN => p.gain_db = phys as f32,
            P_PAN => p.pan = phys as f32,
            P_ROOT => p.root_pitch = (phys as u8).min(127),
            P_FINE => p.fine_cents = phys as f32,
            P_ONE_SHOT => p.one_shot = phys >= 0.5,
            P_START => p.start = (phys as f32).min(p.end - 0.001).max(0.0),
            P_END => p.end = (phys as f32).max(p.start + 0.001).min(1.0),
            P_LOOP_MODE => p.loop_mode = if phys >= 0.5 { LoopMode::Forward } else { LoopMode::Off },
            P_LOOP_START => p.loop_start = (phys as f32).min(p.loop_end - 0.001).max(0.0),
            P_LOOP_END => p.loop_end = (phys as f32).max(p.loop_start + 0.001).min(1.0),
            P_LOOP_XFADE => p.loop_crossfade_ms = phys as f32,
            P_ATTACK => p.env.attack_s = phys as f32,
            P_DECAY => p.env.decay_s = phys as f32,
            P_SUSTAIN => p.env.sustain = phys as f32,
            P_RELEASE => p.env.release_s = phys as f32,
            P_VELOCITY => p.velocity_sensitivity = phys as f32,
            _ => return false,
        }
        self.refresh();
        true
    }

    fn render(&mut self, out_l: &mut [f32], out_r: &mut [f32]) {
        let Some(asset) = self.asset.as_ref() else { return };
        let (start, end, ls, le) = self.bounds(asset);
        let looped = self.patch.loop_mode == LoopMode::Forward;
        let xfade = (f64::from(self.patch.loop_crossfade_ms) * 0.001 * f64::from(asset.sample_rate)).min((le - ls) as f64 * 0.5);
        let stereo = asset.is_stereo();
        let sens = self.patch.velocity_sensitivity;
        for v in self.voices.iter_mut() {
            if v.key.is_none() {
                continue;
            }
            let vel = 1.0 - sens * (1.0 - v.velocity);
            for i in 0..out_l.len() {
                let e = v.env.next();
                if !v.env.is_active() {
                    v.key = None;
                    break;
                }
                let (l, r) = if looped && v.pos >= ls as f64 {
                    // forward loop with crossfade near the loop end
                    let mut rel = v.pos - ls as f64;
                    let len = (le - ls) as f64;
                    if rel >= len {
                        rel -= len;
                        v.pos = ls as f64 + rel;
                    }
                    let p = ls as f64 + rel;
                    let a_l = asset.read_looped(0, p, ls, le);
                    let a_r = if stereo { asset.read_looped(1, p, ls, le) } else { a_l };
                    if xfade > 0.0 && rel > len - xfade {
                        let t = ((rel - (len - xfade)) / xfade) as f32;
                        let q = ls as f64 + (rel - len);
                        let b_l = asset.read(0, q.max(start));
                        let b_r = if stereo { asset.read(1, q.max(start)) } else { b_l };
                        (a_l + (b_l - a_l) * t, a_r + (b_r - a_r) * t)
                    } else {
                        (a_l, a_r)
                    }
                } else {
                    if v.pos >= end {
                        v.key = None;
                        break;
                    }
                    let l = asset.read(0, v.pos);
                    (l, if stereo { asset.read(1, v.pos) } else { l })
                };
                v.pos += v.inc;
                let g = e * vel * self.gain;
                out_l[i] += l * g * self.pan_l * std::f32::consts::SQRT_2;
                out_r[i] += r * g * self.pan_r * std::f32::consts::SQRT_2;
            }
        }
    }
}

impl Processor for Sampler {
    fn type_id(&self) -> &'static str {
        crate::SAMPLER_TYPE_ID
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
        self.patch.validate().map_err(PrepareError::InvalidState)?;
        self.sample_rate = spec.sample_rate;
        self.prepared = true;
        self.build_voices();
        self.refresh();
        Ok(())
    }
    fn process(&mut self, ctx: &mut ProcessContext<'_>) {
        let frames = ctx.audio.frames;
        ctx.audio.clear_outputs();
        if !self.prepared || ctx.audio.outputs.len() < 2 {
            return;
        }
        let (first, rest) = ctx.audio.outputs.split_at_mut(1);
        let out_l: &mut [f32] = &mut first[0][..frames];
        let out_r: &mut [f32] = &mut rest[0][..frames];
        let mut pos = 0usize;
        for ev in ctx.events {
            let at = (ev.frame as usize).min(frames);
            if at > pos {
                self.render(&mut out_l[pos..at], &mut out_r[pos..at]);
                pos = at;
            }
            self.handle_event(ev);
        }
        if pos < frames {
            self.render(&mut out_l[pos..], &mut out_r[pos..]);
        }
        let bad = sanitize(out_l) + sanitize(out_r);
        if bad > 0 {
            self.nonfinite += u64::from(bad);
            self.reset();
        }
    }
    fn reset(&mut self) {
        for v in &mut self.voices {
            v.key = None;
            v.env.reset();
        }
        self.held.clear();
        self.sustain.clear();
        self.bend = [0.0; 16];
    }
    fn tail(&self) -> Tail {
        Tail::Frames((self.patch.env.release_s * self.sample_rate as f32).ceil() as u32 + 64)
    }
    fn active_voices(&self) -> u32 {
        self.active_voice_count() as u32
    }
    fn param(&self, id: ParamId) -> Option<f32> {
        let d = find_descriptor(&DESCRIPTORS, id)?;
        let p = &self.patch;
        let phys: f64 = match id.0 {
            P_GAIN => p.gain_db.into(),
            P_PAN => p.pan.into(),
            P_ROOT => p.root_pitch.into(),
            P_FINE => p.fine_cents.into(),
            P_ONE_SHOT => f64::from(u8::from(p.one_shot)),
            P_START => p.start.into(),
            P_END => p.end.into(),
            P_LOOP_MODE => f64::from(u8::from(p.loop_mode == LoopMode::Forward)),
            P_LOOP_START => p.loop_start.into(),
            P_LOOP_END => p.loop_end.into(),
            P_LOOP_XFADE => p.loop_crossfade_ms.into(),
            P_ATTACK => p.env.attack_s.into(),
            P_DECAY => p.env.decay_s.into(),
            P_SUSTAIN => p.env.sustain.into(),
            P_RELEASE => p.env.release_s.into(),
            P_VELOCITY => p.velocity_sensitivity.into(),
            _ => return None,
        };
        Some(d.to_normalized(phys))
    }
    fn set_param(&mut self, id: ParamId, normalized: f32) -> bool {
        normalized.is_finite() && self.apply_param(id, normalized.clamp(0.0, 1.0))
    }
    fn save_state(&self) -> Vec<u8> {
        self.patch.to_json()
    }
    fn load_state(&mut self, bytes: &[u8]) -> Result<(), String> {
        let p = SamplerPatch::from_json(bytes)?;
        self.set_patch(p)
    }
}
