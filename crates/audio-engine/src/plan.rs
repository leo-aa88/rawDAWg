//! Compile a project into a prepared render plan (off the RT thread).

use crate::assets::AssetResolver;
use crate::schedule::Schedule;
use rawdawg_dsp::limiter::PeakLimiter;
use rawdawg_dsp::processor::{PrepareSpec, Processor};
use rawdawg_dsp::smooth::Smoother;
use rawdawg_dsp::{AudioBuffer, BusLayout};
use rawdawg_project::Project;
use rawdawg_synthesizer::Synth;
use rawdawg_synthesizer::TableSender;
use rawdawg_transport::{MeterMap, TempoMap};
use rawdawg_types::limits::EngineLimits;
use rawdawg_types::units::{Db, SampleRate};
use rawdawg_types::{ChannelId, InstanceId, MixerChannelId};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};

#[derive(Debug, thiserror::Error)]
pub enum CompileError {
    #[error("project invalid: {0}")]
    Invalid(String),
    #[error("unsupported instrument type {0}")]
    UnsupportedInstrument(String),
    #[error("instrument state for channel {0} is invalid: {1}")]
    InstrumentState(String, String),
    #[error("prepare failed for {0}: {1}")]
    Prepare(String, String),
    #[error("limit exceeded: {0}")]
    Limit(String),
}

pub struct ChannelNode {
    pub id: ChannelId,
    pub instance_id: InstanceId,
    pub name: String,
    pub processor: Box<dyn Processor>,
    /// Hash of (type id, serialized state) this processor was built from.
    pub fingerprint: u64,
    /// The live instance with this id matches `fingerprint`: migrate its state on swap.
    pub carry_over: bool,
    pub gain: Smoother,
    pub pan_l: f32,
    pub pan_r: f32,
    pub mute: bool,
    pub mixer_index: usize,
    /// Peak meter of the last block (copied to status).
    pub peak: f32,
}

pub struct MixerNode {
    pub id: MixerChannelId,
    pub name: String,
    pub gain: Smoother,
    pub pan_l: f32,
    pub pan_r: f32,
    pub mute: bool,
    pub output_index: Option<usize>,
    pub buffer: AudioBuffer,
    pub peak_l: f32,
    pub peak_r: f32,
}

/// Everything the RT thread needs; immutable topology, mutable DSP state.
pub struct Plan {
    pub revision: u64,
    pub sample_rate: SampleRate,
    pub max_frames: usize,
    pub channels: Vec<ChannelNode>,
    pub mixer: Vec<MixerNode>,
    /// Mixer processing order (sources before their outputs; master last).
    pub mixer_order: Vec<usize>,
    pub master_index: usize,
    pub master_limiter: PeakLimiter,
    pub master_protection: bool,
    pub scratch: AudioBuffer,
    pub pattern_schedule: Schedule,
    pub song_schedule: Schedule,
    pub tempo: TempoMap,
    pub meter: MeterMap,
    pub any_solo: bool,
    pub solo_mixer: Vec<bool>,
}

fn pan_gains(pan: rawdawg_types::units::Pan) -> (f32, f32) {
    pan.equal_power_gains()
}

/// Stable in-process fingerprint of an instance's committed state.
pub fn fingerprint_state(type_id: &str, state: &serde_json::Value) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    type_id.hash(&mut h);
    serde_json::to_vec(state).unwrap_or_default().hash(&mut h);
    h.finish()
}

fn state_bytes(inst: &rawdawg_project::Instance, channel_name: &str) -> Result<Option<Vec<u8>>, CompileError> {
    if inst.state.is_null() || !inst.state.as_object().is_some_and(|o| !o.is_empty()) {
        return Ok(None);
    }
    serde_json::to_vec(&inst.state).map(Some).map_err(|e| CompileError::InstrumentState(channel_name.to_owned(), e.to_string()))
}

/// Build an (unprepared) processor for a native instrument instance, plus its
/// live table channel for synths. Also used by the app for main-thread mirrors.
pub fn make_instrument(inst: &rawdawg_project::Instance, channel_name: &str, assets: &dyn AssetResolver) -> Result<(Box<dyn Processor>, Option<TableSender>), CompileError> {
    match inst.type_id.as_str() {
        rawdawg_types::ids::native_types::SYNTH => {
            let mut synth = Synth::default();
            if let Some(bytes) = state_bytes(inst, channel_name)? {
                synth.load_state(&bytes).map_err(|e| CompileError::InstrumentState(channel_name.to_owned(), e))?;
            }
            let (sender, inbox) = rawdawg_synthesizer::table_channel();
            synth.set_table_inbox(inbox);
            Ok((Box::new(synth), Some(sender)))
        }
        rawdawg_types::ids::native_types::SAMPLER => {
            let patch = match state_bytes(inst, channel_name)? {
                Some(bytes) => rawdawg_sampler::SamplerPatch::from_json(&bytes).map_err(|e| CompileError::InstrumentState(channel_name.to_owned(), e))?,
                None => rawdawg_sampler::SamplerPatch::default(),
            };
            let asset = patch.asset_id.as_ref().and_then(|id| assets.sample(id));
            Ok((Box::new(rawdawg_sampler::Sampler::new(patch, asset)), None))
        }
        rawdawg_types::ids::native_types::SF2 => {
            let patch = match state_bytes(inst, channel_name)? {
                Some(bytes) => serde_json::from_slice::<rawdawg_sampler::Sf2Patch>(&bytes).map_err(|e| CompileError::InstrumentState(channel_name.to_owned(), e.to_string()))?,
                None => rawdawg_sampler::Sf2Patch::default(),
            };
            let font = patch.asset_id.as_ref().and_then(|id| assets.soundfont(id));
            Ok((Box::new(rawdawg_sampler::Sf2Player::new(patch, font)), None))
        }
        other => Err(CompileError::UnsupportedInstrument(other.to_owned())),
    }
}

/// Result of a compile: the plan plus coordinator-side handles.
pub struct CompileOutput {
    pub plan: Plan,
    /// Live table senders for instances that were (re)created; absent for carried-over ones.
    pub table_senders: Vec<(InstanceId, TableSender)>,
    /// Fingerprints of every instrument instance in the plan.
    pub fingerprints: HashMap<InstanceId, u64>,
}

/// Compile and prepare. Allocates; never called on the RT thread.
///
/// `live` maps instance ids to the fingerprint of the state the currently running
/// processor is known to hold; matching nodes are flagged `carry_over` so the
/// audio thread migrates their voices/tables instead of restarting them.
pub fn compile(
    project: &Project,
    revision: u64,
    max_frames: usize,
    limits: &EngineLimits,
    live: &HashMap<InstanceId, u64>,
    assets: &dyn AssetResolver,
) -> Result<CompileOutput, CompileError> {
    rawdawg_project::validate(project).map_err(|e| CompileError::Invalid(e.to_string()))?;
    if project.channels.len() + project.mixer.channels.len() > limits.max_processing_nodes as usize {
        return Err(CompileError::Limit(format!("{} processing nodes (max {})", project.channels.len() + project.mixer.channels.len(), limits.max_processing_nodes)));
    }
    if project.mixer.channels.len() > limits.max_mixer_channels as usize {
        return Err(CompileError::Limit(format!("{} mixer channels (max {})", project.mixer.channels.len(), limits.max_mixer_channels)));
    }
    let rate = project.sample_rate_hz;
    let sr = rate.as_f64();
    let spec = PrepareSpec::new(sr, max_frames, BusLayout::Stereo);

    let mut mixer = Vec::with_capacity(project.mixer.channels.len());
    for m in &project.mixer.channels {
        let (pl, pr) = pan_gains(m.pan);
        mixer.push(MixerNode {
            id: m.id,
            name: m.name.clone(),
            gain: Smoother::new(m.gain_db.to_linear() as f32, 0.01, sr as f32),
            pan_l: pl,
            pan_r: pr,
            mute: m.mute,
            output_index: m.output_id.and_then(|o| project.mixer.channels.iter().position(|c| c.id == o)),
            buffer: AudioBuffer::new(2, max_frames),
            peak_l: 0.0,
            peak_r: 0.0,
        });
    }
    let master_index = project.mixer.channels.iter().position(|c| c.id == project.mixer.master_id).ok_or_else(|| CompileError::Invalid("missing master".into()))?;
    // topological order: depth-first post-order following output edges (validated acyclic)
    let mut order = Vec::with_capacity(mixer.len());
    let mut visited = vec![false; mixer.len()];
    fn visit(i: usize, mixer: &[MixerNode], visited: &mut [bool], order: &mut Vec<usize>) {
        if visited[i] {
            return;
        }
        visited[i] = true;
        // all sources feeding i first
        for (j, m) in mixer.iter().enumerate() {
            if m.output_index == Some(i) {
                visit(j, mixer, visited, order);
            }
        }
        order.push(i);
    }
    for i in 0..mixer.len() {
        visit(i, &mixer, &mut visited, &mut order);
    }

    let mut channels = Vec::with_capacity(project.channels.len());
    let mut table_senders = Vec::new();
    let mut fingerprints = HashMap::new();
    for c in &project.channels {
        let (mut processor, sender) = make_instrument(&c.instrument, &c.name, assets)?;
        processor.prepare(&spec).map_err(|e| CompileError::Prepare(c.name.clone(), e.to_string()))?;
        let fingerprint = fingerprint_state(c.instrument.type_id.as_str(), &c.instrument.state);
        let carry_over = live.get(&c.instrument.id) == Some(&fingerprint);
        fingerprints.insert(c.instrument.id, fingerprint);
        if !carry_over && let Some(s) = sender {
            table_senders.push((c.instrument.id, s));
        }
        let (pl, pr) = pan_gains(c.pan);
        let mixer_index = project.mixer.channels.iter().position(|m| m.id == c.mixer_id).ok_or_else(|| CompileError::Invalid("channel routed to missing mixer channel".into()))?;
        channels.push(ChannelNode {
            id: c.id,
            instance_id: c.instrument.id,
            name: c.name.clone(),
            processor,
            fingerprint,
            carry_over,
            gain: Smoother::new(c.gain_db.to_linear() as f32, 0.01, sr as f32),
            pan_l: pl,
            pan_r: pr,
            mute: c.mute,
            mixer_index,
            peak: 0.0,
        });
    }
    let any_solo = project.channels.iter().any(|c| c.solo) || project.mixer.channels.iter().any(|m| m.solo);
    // A mixer channel is audible under solo if it is soloed, feeds a soloed channel's path,
    // or is downstream of a soloed source. Compute the set of mixer nodes on soloed paths.
    let mut solo_mixer = vec![false; mixer.len()];
    if any_solo {
        let mark = |start: usize, solo_mixer: &mut Vec<bool>| {
            let mut i = Some(start);
            while let Some(idx) = i {
                solo_mixer[idx] = true;
                i = mixer[idx].output_index;
            }
        };
        for c in project.channels.iter().filter(|c| c.solo) {
            if let Some(mi) = project.mixer.channels.iter().position(|m| m.id == c.mixer_id) {
                mark(mi, &mut solo_mixer);
            }
        }
        for (i, m) in project.mixer.channels.iter().enumerate() {
            if m.solo {
                mark(i, &mut solo_mixer);
            }
        }
    }
    let tempo = project.tempo_map().map_err(|e| CompileError::Invalid(e.to_string()))?;
    let meter = project.meter_map().map_err(|e| CompileError::Invalid(e.to_string()))?;
    let pattern_schedule =
        project.selected_pattern.or_else(|| project.patterns.first().map(|p| p.id)).map(|pid| Schedule::for_pattern(project, pid, &tempo, rate)).unwrap_or_default();
    let song_schedule = Schedule::for_song(project, &tempo, rate);
    let _ = Db::UNITY;
    let plan = Plan {
        revision,
        sample_rate: rate,
        max_frames,
        channels,
        mixer,
        mixer_order: order,
        master_index,
        master_limiter: PeakLimiter::new(sr as f32, 0.001, 0.05, rawdawg_dsp::math::db_to_lin(-0.3)),
        master_protection: project.mixer.master_protection,
        scratch: AudioBuffer::new(2, max_frames),
        pattern_schedule,
        song_schedule,
        tempo,
        meter,
        any_solo,
        solo_mixer,
    };
    Ok(CompileOutput { plan, table_senders, fingerprints })
}

impl Plan {
    /// An empty plan (no channels) used before the first compile and in no-project states.
    pub fn empty(rate: SampleRate, max_frames: usize) -> Self {
        let sr = rate.as_f64() as f32;
        Self {
            revision: 0,
            sample_rate: rate,
            max_frames,
            channels: vec![],
            mixer: vec![MixerNode {
                id: MixerChannelId::nil(),
                name: "Master".into(),
                gain: Smoother::new(1.0, 0.01, sr),
                pan_l: std::f32::consts::FRAC_1_SQRT_2,
                pan_r: std::f32::consts::FRAC_1_SQRT_2,
                mute: false,
                output_index: None,
                buffer: AudioBuffer::new(2, max_frames),
                peak_l: 0.0,
                peak_r: 0.0,
            }],
            mixer_order: vec![0],
            master_index: 0,
            master_limiter: PeakLimiter::new(sr, 0.001, 0.05, rawdawg_dsp::math::db_to_lin(-0.3)),
            master_protection: true,
            scratch: AudioBuffer::new(2, max_frames),
            pattern_schedule: Schedule::default(),
            song_schedule: Schedule::default(),
            tempo: TempoMap::default(),
            meter: MeterMap::default(),
            any_solo: false,
            solo_mixer: vec![false],
        }
    }

    pub fn channel_index(&self, id: ChannelId) -> Option<usize> {
        self.channels.iter().position(|c| c.id == id)
    }

    /// RT-safe, bounded: move live state from `old` into this plan's carried-over channels.
    pub fn migrate_from(&mut self, old: &mut Plan) {
        if old.sample_rate != self.sample_rate || old.max_frames != self.max_frames {
            return;
        }
        for node in self.channels.iter_mut().filter(|n| n.carry_over) {
            if let Some(prev) = old.channels.iter_mut().find(|o| o.instance_id == node.instance_id) {
                node.processor.migrate_from(prev.processor.as_mut());
                node.gain.snap(prev.gain.current());
            }
        }
        for node in self.mixer.iter_mut() {
            if let Some(prev) = old.mixer.iter().find(|o| o.id == node.id) {
                node.gain.snap(prev.gain.current());
            }
        }
    }
}
