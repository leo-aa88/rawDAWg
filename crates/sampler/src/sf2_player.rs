//! `native.sf2`: SoundFont player with bank/preset selection, key/velocity
//! zones, global/local zone merging, loops, volume envelope, attenuation, pan,
//! tuning and a basic low-pass per voice.

use crate::sf2::{Gen, SoundFont, Zone};
use rawdawg_dsp::envelope::{Adsr, AdsrParams};
use rawdawg_dsp::filter::{FilterMode, Svf};
use rawdawg_dsp::math::{db_to_lin, sanitize};
use rawdawg_dsp::processor::{PrepareError, PrepareSpec, ProcessContext, Processor, Tail, find_descriptor};
use rawdawg_synthesizer::registry::{NoteKey, SustainTable, source_slot};
use rawdawg_types::events::{EventKind, TimedEvent};
use rawdawg_types::params::{ParamCategory, ParamDescriptor, ParamMapping, ParamUnit};
use rawdawg_types::{AssetId, ParamId};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct Sf2Patch {
    pub version: u32,
    pub asset_id: Option<AssetId>,
    pub bank: u16,
    pub preset: u16,
    pub gain_db: f32,
    pub max_voices: u32,
    /// Pitch bend range in semitones.
    pub bend_range: f32,
}

impl Default for Sf2Patch {
    fn default() -> Self {
        Self { version: 1, asset_id: None, bank: 0, preset: 0, gain_db: 0.0, max_voices: 32, bend_range: 2.0 }
    }
}

impl Sf2Patch {
    pub fn validate(&self) -> Result<(), String> {
        if self.version != 1 {
            return Err("unsupported sf2 patch version".into());
        }
        if !self.gain_db.is_finite()
            || !(-60.0..=12.0).contains(&self.gain_db)
            || !(1..=64).contains(&self.max_voices)
            || !self.bend_range.is_finite()
            || !(0.0..=24.0).contains(&self.bend_range)
        {
            return Err("sf2 settings out of range".into());
        }
        Ok(())
    }
}

pub const P_GAIN: u32 = 1;
pub const P_BANK: u32 = 2;
pub const P_PRESET: u32 = 3;
pub const P_BEND: u32 = 4;

pub static DESCRIPTORS: [ParamDescriptor; 4] = [
    ParamDescriptor::continuous(P_GAIN, "gain", "Gain", ParamUnit::Decibel, ParamMapping::Linear { min: -60.0, max: 12.0 }, 60.0 / 72.0, ParamCategory::Mix),
    ParamDescriptor::continuous(P_BANK, "bank", "Bank", ParamUnit::None, ParamMapping::Discrete { steps: 129 }, 0.0, ParamCategory::Topology).stepped(),
    ParamDescriptor::continuous(P_PRESET, "preset", "Program", ParamUnit::None, ParamMapping::Discrete { steps: 128 }, 0.0, ParamCategory::Topology).stepped(),
    ParamDescriptor::continuous(P_BEND, "bend_range", "Bend Range", ParamUnit::Semitones, ParamMapping::Linear { min: 0.0, max: 24.0 }, 2.0 / 24.0, ParamCategory::Pitch),
];

/// Effective generator values for one sounding zone (preset + instrument merged).
#[derive(Clone, Copy, Debug)]
struct Region {
    #[allow(dead_code)]
    sample: usize,
    start: usize,
    end: usize,
    loop_start: usize,
    loop_end: usize,
    looped: bool,
    root_key: u8,
    coarse: i16,
    fine: i16,
    scale_tuning: i16,
    attenuation_cb: i16,
    pan: i16,
    env: AdsrParams,
    filter_fc_cents: i16,
    filter_q_cb: i16,
    exclusive_class: i16,
    pitch_correction: i8,
    sample_rate: u32,
}

/// SF2 timecents → seconds.
fn timecents(v: i16) -> f32 {
    2f32.powf(f32::from(v) / 1200.0)
}

fn region_for(sf: &SoundFont, preset: &crate::sf2::Preset, key: u8, vel: u8) -> Vec<Region> {
    let mut out = Vec::new();
    let pglobal = preset.global.as_ref();
    for pz in &preset.zones {
        let (klo, khi) = pz.key_range();
        let (vlo, vhi) = pz.vel_range();
        if key < klo || key > khi || vel < vlo || vel > vhi {
            continue;
        }
        let Some(inst_idx) = pz.get(Gen::Instrument) else { continue };
        let Some(inst) = sf.instruments.get(inst_idx as usize) else { continue };
        let iglobal = inst.global.as_ref();
        for iz in &inst.zones {
            let (klo, khi) = iz.key_range();
            let (vlo, vhi) = iz.vel_range();
            if key < klo || key > khi || vel < vlo || vel > vhi {
                continue;
            }
            let Some(sample_idx) = iz.get(Gen::SampleId) else { continue };
            let Some(sh) = sf.samples.get(sample_idx as usize) else { continue };
            // instrument-level value: local overrides global; preset-level values are added
            let ig = |g: Gen, default: i16| -> i16 { iz.get(g).or_else(|| iglobal.and_then(|z| z.get(g))).unwrap_or(default) };
            let pg = |g: Gen| -> i16 { pz.get(g).or_else(|| pglobal.and_then(|z: &Zone| z.get(g))).unwrap_or(0) };
            let add = |g: Gen, default: i16| -> i16 { ig(g, default).saturating_add(pg(g)) };
            let start = (sh.start as i64 + i64::from(ig(Gen::StartAddrsOffset, 0)) + 32768 * i64::from(ig(Gen::StartAddrsCoarseOffset, 0))).max(0) as usize;
            let end = (sh.end as i64 + i64::from(ig(Gen::EndAddrsOffset, 0)) + 32768 * i64::from(ig(Gen::EndAddrsCoarseOffset, 0))).max(start as i64 + 1) as usize;
            let loop_start = (sh.loop_start as i64 + i64::from(ig(Gen::StartloopAddrsOffset, 0)) + 32768 * i64::from(ig(Gen::StartloopAddrsCoarseOffset, 0))).max(0) as usize;
            let loop_end =
                (sh.loop_end as i64 + i64::from(ig(Gen::EndloopAddrsOffset, 0)) + 32768 * i64::from(ig(Gen::EndloopAddrsCoarseOffset, 0))).max(loop_start as i64 + 1) as usize;
            let modes = ig(Gen::SampleModes, 0);
            let root_key = match ig(Gen::OverridingRootKey, -1) {
                v if (0..=127).contains(&v) => v as u8,
                _ => sh.original_pitch.min(127),
            };
            let sustain_cb = add(Gen::SustainVolEnv, 0).clamp(0, 1440);
            let env = AdsrParams {
                attack_s: timecents(add(Gen::AttackVolEnv, -12000)),
                decay_s: timecents(add(Gen::DecayVolEnv, -12000)) + timecents(add(Gen::HoldVolEnv, -12000)),
                sustain: db_to_lin(-f32::from(sustain_cb) / 10.0),
                release_s: timecents(add(Gen::ReleaseVolEnv, -12000)),
            };
            out.push(Region {
                sample: sample_idx as usize,
                start: start.min(sf.sample_data.len()),
                end: end.min(sf.sample_data.len()),
                loop_start: loop_start.min(sf.sample_data.len()),
                loop_end: loop_end.min(sf.sample_data.len()),
                looped: modes & 3 == 1 || modes & 3 == 3,
                root_key,
                coarse: add(Gen::CoarseTune, 0),
                fine: add(Gen::FineTune, 0),
                scale_tuning: ig(Gen::ScaleTuning, 100),
                attenuation_cb: add(Gen::InitialAttenuation, 0).clamp(0, 1440),
                pan: add(Gen::Pan, 0).clamp(-500, 500),
                env: env.clamped(),
                filter_fc_cents: add(Gen::InitialFilterFc, 13500).clamp(1500, 13500),
                filter_q_cb: add(Gen::InitialFilterQ, 0).clamp(0, 960),
                exclusive_class: ig(Gen::ExclusiveClass, 0),
                pitch_correction: sh.pitch_correction,
                sample_rate: sh.sample_rate.max(1000),
            });
        }
    }
    out
}

#[derive(Clone, Debug)]
struct Voice {
    key: Option<NoteKey>,
    age: u64,
    released: bool,
    sustained: bool,
    region: Option<Region>,
    pos: f64,
    inc: f64,
    env: Adsr,
    filter: Svf,
    gain: f32,
    pan_l: f32,
    pan_r: f32,
    exclusive_class: i16,
}

pub struct Sf2Player {
    patch: Sf2Patch,
    font: Option<Arc<SoundFont>>,
    voices: Vec<Voice>,
    sustain: SustainTable,
    bend: [f32; 16],
    age: u64,
    sample_rate: f64,
    prepared: bool,
    gain: f32,
    nonfinite: u64,
    /// Notes that matched no zone (reported to the UI as a count).
    pub unmapped_notes: u64,
}

impl Sf2Player {
    pub fn new(patch: Sf2Patch, font: Option<Arc<SoundFont>>) -> Self {
        Self {
            patch,
            font,
            voices: Vec::new(),
            sustain: SustainTable::new(),
            bend: [0.0; 16],
            age: 0,
            sample_rate: 48_000.0,
            prepared: false,
            gain: 1.0,
            nonfinite: 0,
            unmapped_notes: 0,
        }
    }
    pub fn patch(&self) -> &Sf2Patch {
        &self.patch
    }
    pub fn font(&self) -> Option<&Arc<SoundFont>> {
        self.font.as_ref()
    }
    pub fn set_font(&mut self, font: Option<Arc<SoundFont>>) {
        self.font = font;
        self.reset();
    }
    pub fn set_patch(&mut self, patch: Sf2Patch) -> Result<(), String> {
        patch.validate()?;
        self.patch = patch;
        self.gain = db_to_lin(self.patch.gain_db);
        if self.prepared && self.voices.len() != self.patch.max_voices as usize + 4 {
            self.build_voices();
        }
        Ok(())
    }
    fn build_voices(&mut self) {
        let sr = self.sample_rate as f32;
        self.voices = (0..self.patch.max_voices as usize + 4)
            .map(|_| Voice {
                key: None,
                age: 0,
                released: false,
                sustained: false,
                region: None,
                pos: 0.0,
                inc: 0.0,
                env: Adsr::new(sr, AdsrParams::DEFAULT),
                filter: Svf::new(sr),
                gain: 0.0,
                pan_l: 0.0,
                pan_r: 0.0,
                exclusive_class: 0,
            })
            .collect();
    }
    pub fn active_voice_count(&self) -> usize {
        self.voices.iter().filter(|v| v.key.is_some()).count()
    }
    /// Presets available in the loaded font as (bank, program, name).
    pub fn presets(&self) -> Vec<(u16, u16, String)> {
        self.font.as_ref().map(|f| f.presets.iter().map(|p| (p.bank, p.preset, p.name.clone())).collect()).unwrap_or_default()
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

    fn increment(&self, r: &Region, key: u8, bend: f32) -> f64 {
        let semis = (f64::from(key) - f64::from(r.root_key)) * f64::from(r.scale_tuning) / 100.0
            + f64::from(r.coarse)
            + (f64::from(r.fine) + f64::from(r.pitch_correction)) / 100.0
            + f64::from(bend);
        2f64.powf(semis / 12.0) * f64::from(r.sample_rate) / self.sample_rate
    }

    fn note_on(&mut self, key: NoteKey, velocity: f32) {
        let Some(font) = self.font.clone() else { return };
        let Some(preset) = font.find_preset(self.patch.bank, self.patch.preset).or_else(|| font.presets.first()) else { return };
        let vel = (velocity * 127.0).round().clamp(1.0, 127.0) as u8;
        let regions = region_for(&font, preset, key.pitch, vel);
        if regions.is_empty() {
            self.unmapped_notes += 1;
            return;
        }
        let bend = self.bend[usize::from(key.channel) % 16] * self.patch.bend_range;
        for r in regions {
            // exclusive class: cut other voices of the same class
            if r.exclusive_class != 0 {
                for v in &mut self.voices {
                    if v.key.is_some() && v.exclusive_class == r.exclusive_class {
                        v.env.fade_out();
                        v.released = true;
                    }
                }
            }
            self.age += 1;
            let i = self.allocate();
            let inc = self.increment(&r, key.pitch, bend);
            let v = &mut self.voices[i];
            v.key = Some(key);
            v.age = self.age;
            v.released = false;
            v.sustained = false;
            v.pos = r.start as f64;
            v.inc = inc;
            // default modulator: velocity → attenuation (concave), plus initialAttenuation
            let vel_db = 40.0 * (f32::from(vel) / 127.0).max(1e-3).log10().abs().min(96.0);
            v.gain = db_to_lin(-f32::from(r.attenuation_cb) / 10.0 - vel_db * 0.5);
            let (pl, pr) = rawdawg_types::units::Pan(f64::from(r.pan) / 500.0).equal_power_gains();
            v.pan_l = pl;
            v.pan_r = pr;
            v.env.set_params(r.env);
            v.env.note_on();
            let fc = 8.176 * 2f32.powf(f32::from(r.filter_fc_cents) / 1200.0);
            v.filter.set_mode(FilterMode::LowPass);
            v.filter.set(fc.min(20_000.0), (0.707 * 10f32.powf(f32::from(r.filter_q_cb) / 200.0)).clamp(0.5, 20.0));
            v.filter.reset();
            v.exclusive_class = r.exclusive_class;
            v.region = Some(r);
        }
    }

    fn note_off(&mut self, key: NoteKey) {
        let pedal = self.sustain.is_down(key.source, key.channel);
        for v in &mut self.voices {
            if v.key == Some(key) && !v.released {
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
                let bend = value * self.patch.bend_range;
                let sr = self.sample_rate;
                for v in &mut self.voices {
                    if let (Some(k), Some(r)) = (v.key, v.region)
                        && k.channel == ev.channel
                    {
                        let semis = (f64::from(k.pitch) - f64::from(r.root_key)) * f64::from(r.scale_tuning) / 100.0
                            + f64::from(r.coarse)
                            + (f64::from(r.fine) + f64::from(r.pitch_correction)) / 100.0
                            + f64::from(bend);
                        v.inc = 2f64.powf(semis / 12.0) * f64::from(r.sample_rate) / sr;
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
        match id.0 {
            P_GAIN => {
                self.patch.gain_db = phys as f32;
                self.gain = db_to_lin(self.patch.gain_db);
            }
            P_BANK => self.patch.bank = phys as u16,
            P_PRESET => self.patch.preset = phys as u16,
            P_BEND => self.patch.bend_range = phys as f32,
            _ => return false,
        }
        true
    }

    fn render(&mut self, out_l: &mut [f32], out_r: &mut [f32]) {
        let Some(font) = self.font.as_ref() else { return };
        let data = &font.sample_data;
        for v in self.voices.iter_mut() {
            let (Some(_), Some(r)) = (v.key, v.region) else { continue };
            for i in 0..out_l.len() {
                let e = v.env.next();
                if !v.env.is_active() {
                    v.key = None;
                    break;
                }
                // read position with loop handling (loop only while the note is held or always for mode 1)
                let mut p = v.pos;
                if r.looped && p >= r.loop_end as f64 {
                    let len = (r.loop_end - r.loop_start).max(1) as f64;
                    p = r.loop_start as f64 + (p - r.loop_start as f64).rem_euclid(len);
                    v.pos = p;
                }
                if p >= r.end as f64 {
                    v.key = None;
                    break;
                }
                let s = if r.looped && p >= r.loop_start as f64 {
                    let len = (r.loop_end - r.loop_start).max(1);
                    let rel = p - r.loop_start as f64;
                    let i0 = rel.floor() as usize;
                    let frac = (rel - i0 as f64) as f32;
                    let at = |k: isize| -> f32 { data[(r.loop_start + ((i0 as isize + k).rem_euclid(len as isize)) as usize).min(data.len() - 1)] };
                    rawdawg_dsp::wavetable::hermite(at(-1), at(0), at(1), at(2), frac)
                } else {
                    let i0 = p.floor() as usize;
                    let frac = (p - i0 as f64) as f32;
                    let at = |k: isize| -> f32 { data[((i0 as isize + k).clamp(r.start as isize, r.end as isize - 1)) as usize] };
                    rawdawg_dsp::wavetable::hermite(at(-1), at(0), at(1), at(2), frac)
                };
                let s = if r.filter_fc_cents < 13500 { v.filter.process(s) } else { s };
                v.pos += v.inc;
                let g = e * v.gain * self.gain;
                out_l[i] += s * g * v.pan_l * std::f32::consts::SQRT_2;
                out_r[i] += s * g * v.pan_r * std::f32::consts::SQRT_2;
            }
        }
    }
}

impl Processor for Sf2Player {
    fn type_id(&self) -> &'static str {
        crate::SF2_TYPE_ID
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
        self.gain = db_to_lin(self.patch.gain_db);
        self.build_voices();
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
        self.sustain.clear();
        self.bend = [0.0; 16];
    }
    fn tail(&self) -> Tail {
        Tail::Unknown
    }
    fn active_voices(&self) -> u32 {
        self.active_voice_count() as u32
    }
    fn param(&self, id: ParamId) -> Option<f32> {
        let d = find_descriptor(&DESCRIPTORS, id)?;
        let phys: f64 = match id.0 {
            P_GAIN => self.patch.gain_db.into(),
            P_BANK => self.patch.bank.into(),
            P_PRESET => self.patch.preset.into(),
            P_BEND => self.patch.bend_range.into(),
            _ => return None,
        };
        Some(d.to_normalized(phys))
    }
    fn set_param(&mut self, id: ParamId, normalized: f32) -> bool {
        normalized.is_finite() && self.apply_param(id, normalized.clamp(0.0, 1.0))
    }
    fn save_state(&self) -> Vec<u8> {
        serde_json::to_vec_pretty(&self.patch).expect("serializes")
    }
    fn load_state(&mut self, bytes: &[u8]) -> Result<(), String> {
        let p: Sf2Patch = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
        self.set_patch(p)
    }
}
