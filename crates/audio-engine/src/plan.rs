//! Compile a project into a prepared render plan (off the RT thread).

use crate::assets::AssetResolver;
use crate::schedule::Schedule;
use rawdawg_dsp::delayline::DelayLine;
use rawdawg_dsp::limiter::PeakLimiter;
use rawdawg_dsp::processor::{PrepareSpec, Processor};
use rawdawg_dsp::smooth::Smoother;
use rawdawg_dsp::{AudioBuffer, BusLayout};
use rawdawg_project::{ClipContent, Project};
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
    pub latency: u32,
    pub comp_frames: usize,
    pub comp: [DelayLine; 2],
}

/// What a processor slot refers to (slots: channels first, then inserts in mixer order).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Slot {
    Channel(usize),
    Insert { mixer: usize, insert: usize },
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
    pub inserts: Vec<InsertNode>,
    pub sends: Vec<SendNode>,
    /// External sidechain input summed from `sidechain` sends.
    pub sidechain: AudioBuffer,
    pub sidechain_fed: bool,
    /// Compensation delay on the main-output contribution (frames).
    pub comp_frames: usize,
    pub comp: [DelayLine; 2],
    /// Latency at this node's input (max over its sources) and output (+ inserts).
    pub latency_in: u32,
    pub latency_out: u32,
}

/// One effect in a mixer channel's insert chain.
pub struct InsertNode {
    pub instance_id: InstanceId,
    pub processor: Box<dyn Processor>,
    pub fingerprint: u64,
    pub carry_over: bool,
    pub bypass: bool,
    pub has_mix: bool,
    pub mix: Smoother,
    pub latency: u32,
    /// Dry copy delayed by `latency` for wet/dry mixing and true bypass alignment.
    pub dry: [DelayLine; 2],
    /// Processor slot (event stage / meter index).
    pub slot: usize,
}

/// A send edge into another mixer channel (main input or sidechain).
pub struct SendNode {
    pub target_index: usize,
    pub gain: Smoother,
    pub pre_fader: bool,
    pub sidechain: bool,
    pub comp_frames: usize,
    pub comp: [DelayLine; 2],
}

/// Sends per mixer node whose state carries over a recompile (more keep working, fresh).
const MAX_SENDS_MATCHED: usize = 32;

/// Continue a running processor in the new plan (see [`Plan::migrate_from`]).
fn carry_processor(new: &mut Box<dyn Processor>, old: &mut Box<dyn Processor>) {
    if !new.carries_live_state() && Processor::type_id(new.as_ref()) == Processor::type_id(old.as_ref()) {
        // same type and state, prepared with the same spec: the running instance continues
        std::mem::swap(new, old);
    } else {
        new.migrate_from(old.as_mut());
    }
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
    /// Mixer nodes that are not on a soloed path but feed one through a sidechain send
    /// (and everything upstream of them): they keep processing and delivering to those
    /// dependencies, while their own output to the mix stays silent.
    pub solo_feed: Vec<bool>,
    /// Processor slots (events and meters are addressed by slot index).
    pub slots: Vec<Slot>,
    pub slot_instances: Vec<InstanceId>,
    /// Input copy for inserts (frames × 2).
    pub fx_in: AudioBuffer,
    /// Latency from the master's sources to its output, frames (published for UI/export).
    pub output_latency: u32,
    /// Audio clips of unmuted tracks (song mode), sorted by start frame.
    pub audio_clips: Vec<AudioClipNode>,
}

/// An audio clip resolved for playback (song mode): resident decoded asset, project-frame
/// bounds and the mixer node it plays into. Rates and fades are in output frames.
/// Where an audio clip's samples come from.
pub enum ClipSource {
    Resident(std::sync::Arc<rawdawg_sampler::SampleAsset>),
    Streamed(crate::stream::ClipStream),
}

pub struct AudioClipNode {
    pub clip_id: rawdawg_types::ClipId,
    pub source: ClipSource,
    /// First project frame of the clip and its length in output frames.
    pub start_frame: i64,
    pub len_frames: i64,
    /// Source frame of the clip's first output frame and source frames per output frame.
    pub source_offset: f64,
    pub rate: f64,
    pub gain: f32,
    pub fade_in: f64,
    pub fade_out: f64,
    pub mixer_index: usize,
}

/// Maximum audio clips a plan plays (bounded per-block work).
pub const MAX_AUDIO_CLIPS: usize = 4096;

/// Processor slot layout shared by the compiler and the scheduler: channels in project
/// order, then every insert of every mixer channel in project order.
pub fn slot_layout(project: &Project) -> Vec<InstanceId> {
    let mut v: Vec<InstanceId> = project.channels.iter().map(|c| c.instrument.id).collect();
    for m in &project.mixer.channels {
        v.extend(m.inserts.iter().map(|i| i.id));
    }
    v
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
/// Stand-in for a third-party plugin that is not available (format not hosted yet, or the
/// plugin is missing): instruments stay silent, inserts pass audio through; the project's
/// state (inline or opaque file) is never touched. Reported in `CompileOutput::missing_plugins`.
pub struct MissingPlugin {
    type_id: &'static str,
    insert: bool,
}

impl Processor for MissingPlugin {
    fn type_id(&self) -> &'static str {
        self.type_id
    }
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
    fn descriptors(&self) -> &'static [rawdawg_types::ParamDescriptor] {
        &[]
    }
    fn prepare(&mut self, _spec: &rawdawg_dsp::processor::PrepareSpec) -> Result<(), rawdawg_dsp::processor::PrepareError> {
        Ok(())
    }
    fn process(&mut self, ctx: &mut rawdawg_dsp::processor::ProcessContext<'_>) {
        if self.insert {
            ctx.audio.copy_input_to_output();
        } else {
            for out in ctx.audio.outputs.iter_mut() {
                out[..ctx.audio.frames].fill(0.0);
            }
        }
    }
    fn reset(&mut self) {}
    fn param(&self, _id: rawdawg_types::ParamId) -> Option<f32> {
        None
    }
    fn set_param(&mut self, _id: rawdawg_types::ParamId, _v: f32) -> bool {
        false
    }
    fn save_state(&self) -> Vec<u8> {
        Vec::new()
    }
    fn load_state(&mut self, _bytes: &[u8]) -> Result<(), String> {
        Ok(())
    }
}

fn missing_label(inst: &rawdawg_project::Instance, owner: &str) -> String {
    format!(
        "{owner}: {} ({:?}) is not available — its state is preserved",
        if inst.display_name.is_empty() { inst.type_id.as_str() } else { inst.display_name.as_str() },
        inst.format
    )
}

pub fn make_instrument(inst: &rawdawg_project::Instance, channel_name: &str, assets: &dyn AssetResolver) -> Result<(Box<dyn Processor>, Option<TableSender>), CompileError> {
    if inst.format != rawdawg_project::PluginFormat::Native {
        return Ok((Box::new(MissingPlugin { type_id: "missing.instrument", insert: false }), None));
    }
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
    /// Fingerprints of every instrument and insert instance in the plan.
    pub fingerprints: HashMap<InstanceId, u64>,
    /// Processor slot of every instance (meters, parameter events).
    pub slots: HashMap<InstanceId, usize>,
    pub output_latency: u32,
    /// Decoding worker for streamed audio clips; keep it alive while the plan plays.
    pub stream_worker: Option<crate::stream::StreamWorker>,
    /// Third-party instances replaced by silent/pass-through stand-ins (state preserved).
    pub missing_plugins: Vec<String>,
}

/// Build an (unprepared) native effect processor for an insert instance. Effects that
/// reference project assets (convolution impulse responses) get them resolved here, off-thread.
pub fn make_effect(inst: &rawdawg_project::Instance, owner: &str, _assets: &dyn AssetResolver) -> Result<Box<dyn Processor>, CompileError> {
    if inst.format != rawdawg_project::PluginFormat::Native {
        return Ok(Box::new(MissingPlugin { type_id: "missing.effect", insert: true }));
    }
    if !inst.type_id.as_str().starts_with(rawdawg_types::ids::native_types::FX_PREFIX) {
        return Err(CompileError::UnsupportedInstrument(inst.type_id.as_str().to_owned()));
    }
    let bytes = state_bytes(inst, owner)?;
    rawdawg_effects::make_effect(inst.type_id.as_str(), bytes.as_deref()).map_err(|e| CompileError::InstrumentState(owner.to_owned(), e))
}

/// Resolve asset-backed effect data after `prepare` (the IR is partitioned at the final rate).
pub fn attach_effect_assets(p: &mut dyn Processor, assets: &dyn AssetResolver) {
    if let Some(c) = p.as_any_mut().downcast_mut::<rawdawg_effects::convolution::Convolution>() {
        let ir = c
            .ir_asset_id()
            .and_then(rawdawg_types::AssetId::from_hex)
            .and_then(|id| assets.sample(&id))
            .map(|s| std::sync::Arc::new(rawdawg_effects::convolution::ImpulseResponse { name: s.name.clone(), sample_rate: s.sample_rate, channels: s.channels.clone() }));
        c.set_ir(ir);
    }
}

/// Compile and prepare. Allocates; never called on the RT thread.
///
/// `live` maps instance ids to the fingerprint of the state the currently running
/// processor is known to hold; matching nodes are flagged `carry_over` so the
/// audio thread migrates their voices/tables instead of restarting them.
/// Resolve the project's audio clips against the asset store. Clips on muted tracks and
/// clips whose asset is missing are skipped (missing assets are reported by the resolver).
fn audio_clip_nodes(
    project: &Project,
    tempo: &TempoMap,
    rate: SampleRate,
    master_index: usize,
    assets: &dyn AssetResolver,
) -> (Vec<AudioClipNode>, Option<crate::stream::StreamWorker>) {
    let mut out = Vec::new();
    let mut stream_requests: Vec<(rawdawg_sampler::stream::StreamInfo, u64)> = Vec::new();
    for clip in &project.clips {
        let ClipContent::Audio { asset_id, source_offset_frames, source_length_frames, gain_db, fade_in_frames, fade_out_frames, playback_rate } = &clip.content else { continue };
        if clip.muted || out.len() >= MAX_AUDIO_CLIPS {
            continue;
        }
        let Some(track) = project.tracks.iter().find(|t| t.id == clip.track_id) else { continue };
        if track.mute {
            continue;
        }
        // resident when the cache allows it, otherwise streamed from disk
        let (source, asset_rate, asset_frames) = match assets.sample(asset_id) {
            Some(asset) if asset.is_valid() && asset.frames() > 0 => {
                let (r, f) = (asset.sample_rate, asset.frames() as u64);
                (ClipSource::Resident(asset), r, f)
            }
            _ => match assets.stream_info(asset_id) {
                Some(info) if info.frames > 0 => {
                    stream_requests.push((info.clone(), *source_offset_frames));
                    (ClipSource::Streamed(crate::stream::ClipStream::placeholder(info.frames)), info.sample_rate, info.frames)
                }
                _ => continue,
            },
        };
        let mixer_index = track.mixer_id.and_then(|m| project.mixer.channels.iter().position(|c| c.id == m)).unwrap_or(master_index);
        let eff_rate = (playback_rate * f64::from(asset_rate) / rate.as_f64()).max(1e-6);
        let src_len = (*source_length_frames).min(asset_frames - (*source_offset_frames).min(asset_frames - 1)) as f64;
        let len_frames = (src_len / eff_rate).ceil().max(1.0) as i64;
        out.push(AudioClipNode {
            clip_id: clip.id,
            source,
            start_frame: tempo.tick_to_frame(clip.start_tick, rate),
            len_frames,
            source_offset: *source_offset_frames as f64,
            rate: eff_rate,
            gain: gain_db.to_linear() as f32,
            fade_in: *fade_in_frames as f64 / eff_rate,
            fade_out: *fade_out_frames as f64 / eff_rate,
            mixer_index,
        });
    }
    // start the streams (one worker for the plan) and hand the RT ends to their nodes
    let mut worker = None;
    if !stream_requests.is_empty() {
        match crate::stream::start_streams(&stream_requests) {
            Ok((rts, w)) => {
                let mut it = rts.into_iter();
                for node in out.iter_mut() {
                    if let ClipSource::Streamed(slot) = &mut node.source
                        && let Some(rt) = it.next()
                    {
                        *slot = rt;
                    }
                }
                worker = Some(w);
            }
            Err(e) => {
                log::warn!("audio clip streaming unavailable: {e}");
                out.retain(|n| matches!(n.source, ClipSource::Resident(_)));
            }
        }
    }
    out.sort_by_key(|c| c.start_frame);
    (out, worker)
}

pub fn compile(
    project: &Project,
    revision: u64,
    max_frames: usize,
    limits: &EngineLimits,
    live: &HashMap<InstanceId, u64>,
    assets: &dyn AssetResolver,
) -> Result<CompileOutput, CompileError> {
    compile_with(project, revision, max_frames, limits, live, assets, &mut crate::externals::NoExternals)
}

/// [`compile`] with a provider for hosted third-party plugins (the app's plugin host).
pub fn compile_with(
    project: &Project,
    revision: u64,
    max_frames: usize,
    limits: &EngineLimits,
    live: &HashMap<InstanceId, u64>,
    assets: &dyn AssetResolver,
    externals: &mut dyn crate::externals::ExternalPlugins,
) -> Result<CompileOutput, CompileError> {
    let mut missing_plugins: Vec<String> = Vec::new();
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

    let n_channels = project.channels.len();
    let mut fingerprints = HashMap::new();
    let mut slots: Vec<Slot> = (0..n_channels).map(Slot::Channel).collect();
    let mut slot_instances: Vec<InstanceId> = project.channels.iter().map(|c| c.instrument.id).collect();
    let mut mixer = Vec::with_capacity(project.mixer.channels.len());
    for (mi, m) in project.mixer.channels.iter().enumerate() {
        let (pl, pr) = pan_gains(m.pan);
        let mut inserts = Vec::with_capacity(m.inserts.len());
        for (ii, inst) in m.inserts.iter().enumerate() {
            let owner = format!("{} / {}", m.name, inst.display_name);
            let external = if inst.format != rawdawg_project::PluginFormat::Native {
                externals.instantiate(inst, crate::externals::PluginRole::Insert, &spec).map_err(|e| CompileError::Prepare(owner.clone(), e))?
            } else {
                None
            };
            let mut processor = match external {
                Some(p) => p,
                None => {
                    if inst.format != rawdawg_project::PluginFormat::Native {
                        missing_plugins.push(missing_label(inst, &owner));
                    }
                    make_effect(inst, &owner, assets)?
                }
            };
            processor.prepare(&spec).map_err(|e| CompileError::Prepare(owner.clone(), e.to_string()))?;
            attach_effect_assets(processor.as_mut(), assets);
            let fingerprint = fingerprint_state(inst.type_id.as_str(), &inst.state);
            let carry_over = live.get(&inst.id) == Some(&fingerprint) || processor.carries_live_state();
            fingerprints.insert(inst.id, fingerprint);
            let latency = processor.latency_frames();
            let has_mix = rawdawg_effects::info(inst.type_id.as_str()).is_some_and(|i| i.has_mix);
            let slot = slots.len();
            slots.push(Slot::Insert { mixer: mi, insert: ii });
            slot_instances.push(inst.id);
            inserts.push(InsertNode {
                instance_id: inst.id,
                processor,
                fingerprint,
                carry_over,
                bypass: inst.bypass,
                has_mix,
                mix: Smoother::new(if has_mix { inst.mix } else { 1.0 }, 0.01, sr as f32),
                latency,
                dry: [DelayLine::new(latency as usize), DelayLine::new(latency as usize)],
                slot,
            });
        }
        let sends = m
            .sends
            .iter()
            .filter_map(|snd| {
                let target_index = project.mixer.channels.iter().position(|c| c.id == snd.target_id)?;
                Some(SendNode {
                    target_index,
                    gain: Smoother::new(snd.level_db.to_linear() as f32, 0.01, sr as f32),
                    pre_fader: snd.tap == rawdawg_project::SendTap::PreFader,
                    sidechain: snd.sidechain,
                    comp_frames: 0,
                    comp: [DelayLine::new(0), DelayLine::new(0)],
                })
            })
            .collect();
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
            inserts,
            sends,
            sidechain: AudioBuffer::new(2, max_frames),
            sidechain_fed: false,
            comp_frames: 0,
            comp: [DelayLine::new(0), DelayLine::new(0)],
            latency_in: 0,
            latency_out: 0,
        });
    }
    for mi in 0..mixer.len() {
        let targets: Vec<usize> = mixer[mi].sends.iter().filter(|s| s.sidechain).map(|s| s.target_index).collect();
        for t in targets {
            mixer[t].sidechain_fed = true;
        }
    }
    let master_index = project.mixer.channels.iter().position(|c| c.id == project.mixer.master_id).ok_or_else(|| CompileError::Invalid("missing master".into()))?;
    // topological order: depth-first post-order following output and send edges (validated acyclic)
    let mut order = Vec::with_capacity(mixer.len());
    let mut visited = vec![false; mixer.len()];
    fn feeds(m: &MixerNode, i: usize) -> bool {
        m.output_index == Some(i) || m.sends.iter().any(|s| s.target_index == i)
    }
    fn visit(i: usize, mixer: &[MixerNode], visited: &mut [bool], order: &mut Vec<usize>) {
        if visited[i] {
            return;
        }
        visited[i] = true;
        for (j, m) in mixer.iter().enumerate() {
            if j != i && feeds(m, i) {
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
    for c in &project.channels {
        let external = if c.instrument.format != rawdawg_project::PluginFormat::Native {
            externals.instantiate(&c.instrument, crate::externals::PluginRole::Instrument, &spec).map_err(|e| CompileError::Prepare(c.name.clone(), e))?
        } else {
            None
        };
        let (mut processor, sender) = match external {
            Some(p) => (p, None),
            None => {
                if c.instrument.format != rawdawg_project::PluginFormat::Native {
                    missing_plugins.push(missing_label(&c.instrument, &c.name));
                }
                make_instrument(&c.instrument, &c.name, assets)?
            }
        };
        processor.prepare(&spec).map_err(|e| CompileError::Prepare(c.name.clone(), e.to_string()))?;
        let fingerprint = fingerprint_state(c.instrument.type_id.as_str(), &c.instrument.state);
        let carry_over = live.get(&c.instrument.id) == Some(&fingerprint) || processor.carries_live_state();
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
            latency: 0,
            comp_frames: 0,
            comp: [DelayLine::new(0), DelayLine::new(0)],
        });
    }
    for ch in channels.iter_mut() {
        ch.latency = ch.processor.latency_frames();
    }
    // Latency compensation: longest path. Every source feeding a node is delayed up to the
    // slowest source at that summing point; intentional delays inside inserts are not compensated away.
    let max_comp = (f64::from(limits.max_compensated_latency_seconds) * sr) as u32;
    for &mi in &order {
        let mut lat_in = 0u32;
        for ch in channels.iter().filter(|c| c.mixer_index == mi) {
            lat_in = lat_in.max(ch.latency);
        }
        for (j, m) in mixer.iter().enumerate() {
            if j != mi && feeds(m, mi) {
                lat_in = lat_in.max(m.latency_out);
            }
        }
        let inserts_latency: u32 = mixer[mi].inserts.iter().map(|i| i.latency).sum();
        mixer[mi].latency_in = lat_in;
        mixer[mi].latency_out = lat_in.saturating_add(inserts_latency);
        if mixer[mi].latency_out > max_comp {
            return Err(CompileError::Limit(format!("compensated latency {} frames on {} exceeds {} frames", mixer[mi].latency_out, mixer[mi].name, max_comp)));
        }
    }
    for ch in channels.iter_mut() {
        let need = mixer[ch.mixer_index].latency_in.saturating_sub(ch.latency) as usize;
        ch.comp_frames = need;
        if need > 0 {
            ch.comp = [DelayLine::new(need), DelayLine::new(need)];
        }
    }
    for mi in 0..mixer.len() {
        let lat_out = mixer[mi].latency_out;
        if let Some(oi) = mixer[mi].output_index {
            let need = mixer[oi].latency_in.saturating_sub(lat_out) as usize;
            mixer[mi].comp_frames = need;
            if need > 0 {
                mixer[mi].comp = [DelayLine::new(need), DelayLine::new(need)];
            }
        }
        let targets: Vec<usize> = mixer[mi].sends.iter().map(|s| s.target_index).collect();
        for (si, t) in targets.into_iter().enumerate() {
            let need = mixer[t].latency_in.saturating_sub(lat_out) as usize;
            let snd = &mut mixer[mi].sends[si];
            snd.comp_frames = need;
            if need > 0 {
                snd.comp = [DelayLine::new(need), DelayLine::new(need)];
            }
        }
    }
    let output_latency = mixer[master_index].latency_out;
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
    // Solo keeps sidechain dependencies: a node sidechain-sending into a soloed node feeds it,
    // and so does anything whose main output or send lands in a feeding node (bounded fixpoint).
    let mut solo_feed = vec![false; mixer.len()];
    if any_solo {
        let index_of = |id: rawdawg_types::MixerChannelId| project.mixer.channels.iter().position(|c| c.id == id);
        for _ in 0..mixer.len() + 1 {
            let mut changed = false;
            for (i, m) in project.mixer.channels.iter().enumerate() {
                if solo_mixer[i] || solo_feed[i] {
                    continue;
                }
                let via_sends = m.sends.iter().any(|snd| index_of(snd.target_id).is_some_and(|t| if snd.sidechain { solo_mixer[t] || solo_feed[t] } else { solo_feed[t] }));
                let via_output = mixer[i].output_index.is_some_and(|o| solo_feed[o]);
                if via_sends || via_output {
                    solo_feed[i] = true;
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
    }
    let tempo = project.tempo_map().map_err(|e| CompileError::Invalid(e.to_string()))?;
    let meter = project.meter_map().map_err(|e| CompileError::Invalid(e.to_string()))?;
    let pattern_schedule =
        project.selected_pattern.or_else(|| project.patterns.first().map(|p| p.id)).map(|pid| Schedule::for_pattern(project, pid, &tempo, rate)).unwrap_or_default();
    let song_schedule = Schedule::for_song(project, &tempo, rate);
    let (audio_clips, stream_worker) = audio_clip_nodes(project, &tempo, rate, master_index, assets);
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
        solo_feed,
        audio_clips,
        slots,
        slot_instances: slot_instances.clone(),
        fx_in: AudioBuffer::new(2, max_frames),
        output_latency,
    };
    let slot_map: HashMap<InstanceId, usize> = slot_instances.iter().enumerate().map(|(i, id)| (*id, i)).collect();
    Ok(CompileOutput { stream_worker, missing_plugins, plan, table_senders, fingerprints, slots: slot_map, output_latency })
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
                inserts: vec![],
                sends: vec![],
                sidechain: AudioBuffer::new(2, max_frames),
                sidechain_fed: false,
                comp_frames: 0,
                comp: [DelayLine::new(0), DelayLine::new(0)],
                latency_in: 0,
                latency_out: 0,
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
            solo_feed: vec![false],
            audio_clips: Vec::new(),
            slots: vec![],
            slot_instances: vec![],
            fx_in: AudioBuffer::new(2, max_frames),
            output_latency: 0,
        }
    }

    pub fn channel_index(&self, id: ChannelId) -> Option<usize> {
        self.channels.iter().position(|c| c.id == id)
    }

    /// RT-safe, bounded linear scan: processor slot of an instance.
    pub fn slot_of(&self, id: InstanceId) -> Option<usize> {
        self.slot_instances.iter().position(|x| *x == id)
    }

    /// RT-safe, bounded: move live state from `old` into this plan, so a recompile changes
    /// only what was edited.
    ///
    /// - A processor whose state did not change (`carry_over`) continues running: native
    ///   processors are swapped in whole (voices, delay lines, reverb tails, envelopes and
    ///   smoothers continue sample for sample); hosted plugins move their running audio half
    ///   through `migrate_from`.
    /// - Latency compensation and dry lines, gain and mix smoothers of channels, mixer nodes,
    ///   inserts and sends that still exist carry their contents when their lengths match.
    /// - The master limiter keeps its envelope and lookahead.
    ///
    /// Only pointer swaps and smoother snaps; `old` ends up holding the fresh state and is
    /// retired off the audio thread.
    pub fn migrate_from(&mut self, old: &mut Plan) {
        if old.sample_rate != self.sample_rate || old.max_frames != self.max_frames {
            return;
        }
        for node in self.channels.iter_mut() {
            let Some(prev) = old.channels.iter_mut().find(|o| o.instance_id == node.instance_id) else { continue };
            if node.carry_over {
                carry_processor(&mut node.processor, &mut prev.processor);
            }
            node.gain.snap(prev.gain.current());
            if node.comp_frames == prev.comp_frames {
                std::mem::swap(&mut node.comp, &mut prev.comp);
            }
        }
        for mi in 0..self.mixer.len() {
            let id = self.mixer[mi].id;
            let Some(pi) = old.mixer.iter().position(|o| o.id == id) else { continue };
            // send targets by id (mixer indices may differ between plans)
            let targets: [Option<MixerChannelId>; MAX_SENDS_MATCHED] = std::array::from_fn(|k| self.mixer[mi].sends.get(k).map(|s| self.mixer[s.target_index].id));
            let prev_targets: [Option<MixerChannelId>; MAX_SENDS_MATCHED] = std::array::from_fn(|k| old.mixer[pi].sends.get(k).map(|s| old.mixer[s.target_index].id));
            let (node, prev) = (&mut self.mixer[mi], &mut old.mixer[pi]);
            node.gain.snap(prev.gain.current());
            if node.comp_frames == prev.comp_frames {
                std::mem::swap(&mut node.comp, &mut prev.comp);
            }
            for ins in node.inserts.iter_mut() {
                let Some(p) = prev.inserts.iter_mut().find(|o| o.instance_id == ins.instance_id) else { continue };
                if ins.carry_over {
                    carry_processor(&mut ins.processor, &mut p.processor);
                }
                ins.mix.snap(p.mix.current());
                if ins.latency == p.latency {
                    std::mem::swap(&mut ins.dry, &mut p.dry);
                }
            }
            let mut used = [false; MAX_SENDS_MATCHED];
            for (k, send) in node.sends.iter_mut().enumerate().take(MAX_SENDS_MATCHED) {
                let found = (0..prev.sends.len().min(MAX_SENDS_MATCHED))
                    .find(|&j| !used[j] && prev_targets[j] == targets[k] && prev.sends[j].sidechain == send.sidechain && prev.sends[j].pre_fader == send.pre_fader);
                if let Some(j) = found {
                    used[j] = true;
                    let p = &mut prev.sends[j];
                    send.gain.snap(p.gain.current());
                    if send.comp_frames == p.comp_frames {
                        std::mem::swap(&mut send.comp, &mut p.comp);
                    }
                }
            }
        }
        std::mem::swap(&mut self.master_limiter, &mut old.master_limiter);
    }
}
