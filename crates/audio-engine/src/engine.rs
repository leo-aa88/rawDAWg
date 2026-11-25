//! Real-time engine core. Owns the live plan; renders blocks; applies commands
//! at block boundaries; publishes status through atomics.

use crate::commands::{EngineCommand, RecordedEvent, TransportMode};
use crate::plan::Plan;
use crate::schedule::{Schedule, ScheduledNote};
use rawdawg_dsp::AudioBlock;
use rawdawg_dsp::AudioBuffer;
use rawdawg_dsp::math::sanitize;
use rawdawg_dsp::processor::{ProcessContext, TransportView};
use rawdawg_rt::handoff::{Handoff, HandoffConsumer, HandoffProducer};
use rawdawg_rt::queue::{ControlQueue, QueueConsumer, QueueProducer};
use rawdawg_rt::shared::{AtomicF32, AtomicF64, Counter};
use rawdawg_types::events::{EventKind, EventSource, TimedEvent, sort_events};
use rawdawg_types::limits::EngineLimits;
use rawdawg_types::units::SampleRate;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU8, AtomicU64, Ordering};

/// Status visible to the UI (atomics only; no locks).
#[derive(Debug)]
pub struct EngineStatus {
    pub playing: AtomicBool,
    pub mode: AtomicU8,
    /// Playhead position in project frames.
    pub position_frame: AtomicI64,
    pub position_tick: AtomicI64,
    pub plan_revision: AtomicU64,
    pub blocks: Counter,
    pub frames: AtomicU64,
    pub master_peak_l: AtomicF32,
    pub master_peak_r: AtomicF32,
    pub master_reduction_db: AtomicF32,
    pub nonfinite_blocks: Counter,
    pub dropped_events: Counter,
    pub command_overflows: Counter,
    pub refused_swaps: Counter,
    pub active_voices: AtomicU64,
    pub last_block_ns: AtomicU64,
    pub max_block_ns: AtomicU64,
    pub tempo_bpm: AtomicF64,
    pub metronome: AtomicBool,
    pub recording: AtomicBool,
    pub record_overflows: Counter,
    /// Latency from sources to the master output, frames.
    pub output_latency: std::sync::atomic::AtomicU32,
    /// Per processor slot meter (effects: gain reduction / correlation), −1000 = none.
    pub slot_meters: Vec<AtomicF32>,
    /// Per mixer channel peak (fetch_max; the UI resets).
    pub mixer_peak_l: Vec<AtomicF32>,
    pub mixer_peak_r: Vec<AtomicF32>,
    /// Count-in frames still to go (0 = none).
    pub count_in_frames: AtomicI64,
    /// Master output (mono sum) for the live scope.
    pub scope: rawdawg_rt::ScopeBuffer,
}

impl Default for EngineStatus {
    fn default() -> Self {
        Self {
            playing: AtomicBool::new(false),
            mode: AtomicU8::new(0),
            position_frame: AtomicI64::new(0),
            position_tick: AtomicI64::new(0),
            plan_revision: AtomicU64::new(0),
            blocks: Counter::new(),
            frames: AtomicU64::new(0),
            master_peak_l: AtomicF32::new(0.0),
            master_peak_r: AtomicF32::new(0.0),
            master_reduction_db: AtomicF32::new(0.0),
            nonfinite_blocks: Counter::new(),
            dropped_events: Counter::new(),
            command_overflows: Counter::new(),
            refused_swaps: Counter::new(),
            active_voices: AtomicU64::new(0),
            last_block_ns: AtomicU64::new(0),
            max_block_ns: AtomicU64::new(0),
            tempo_bpm: AtomicF64::new(120.0),
            recording: AtomicBool::new(false),
            record_overflows: Counter::new(),
            output_latency: std::sync::atomic::AtomicU32::new(0),
            slot_meters: (0..512).map(|_| AtomicF32::new(-1000.0)).collect(),
            mixer_peak_l: (0..128).map(|_| AtomicF32::new(0.0)).collect(),
            mixer_peak_r: (0..128).map(|_| AtomicF32::new(0.0)).collect(),
            count_in_frames: AtomicI64::new(0),
            metronome: AtomicBool::new(false),
            scope: rawdawg_rt::ScopeBuffer::new(4096),
        }
    }
}

/// Non-RT handle: sends commands and plans, reads status.
pub struct EngineHandle {
    commands: QueueProducer<EngineCommand>,
    plans: HandoffProducer<Plan>,
    record: QueueConsumer<RecordedEvent>,
    pub status: Arc<EngineStatus>,
}

impl EngineHandle {
    /// Next recorded live event (UI thread), if any.
    pub fn poll_recorded(&mut self) -> Option<RecordedEvent> {
        self.record.pop()
    }

    pub fn send(&mut self, cmd: EngineCommand) -> Result<(), EngineCommand> {
        let r = self.commands.push(cmd);
        if r.is_err() {
            self.status.command_overflows.incr();
        }
        r
    }
    /// Publish a new plan; returns it back (boxed) if the handoff is full (retry later).
    pub fn publish_plan(&mut self, plan: Plan) -> Result<u64, Box<Plan>> {
        self.plans.publish(plan).map_err(|rawdawg_rt::handoff::PublishError::Full(p)| Box::new(p))
    }
    /// Collect and drop retired plans (deallocation happens here, off RT).
    pub fn collect_retired(&mut self) -> usize {
        self.plans.collect_retired().len()
    }
    pub fn is_engine_alive(&self) -> bool {
        self.commands.is_consumer_alive()
    }
}

/// Bounded per-channel event staging.
struct EventStage {
    events: Vec<Vec<TimedEvent>>, // one preallocated Vec per channel (capacity fixed)
    capacity: usize,
}

impl EventStage {
    fn new(channels: usize, capacity: usize) -> Self {
        Self { events: (0..channels).map(|_| Vec::with_capacity(capacity)).collect(), capacity }
    }
    fn ensure_channels(&mut self, channels: usize) {
        // Called off-RT (on plan compile); RT path only uses existing Vecs.
        while self.events.len() < channels {
            self.events.push(Vec::with_capacity(self.capacity));
        }
    }
    fn clear(&mut self) {
        for e in &mut self.events {
            e.clear();
        }
    }
    /// Push without growing; returns false when full.
    #[inline]
    fn push(&mut self, channel: usize, ev: TimedEvent) -> bool {
        match self.events.get_mut(channel) {
            Some(v) if v.len() < v.capacity() => {
                v.push(ev);
                true
            }
            _ => false,
        }
    }
}

const MAX_ACTIVE_SCHEDULED: usize = 2048;

/// The real-time engine. Lives on the audio thread (or the offline render loop).
pub struct Engine {
    plan: Plan,
    commands: QueueConsumer<EngineCommand>,
    plans: HandoffConsumer<Plan>,
    status: Arc<EngineStatus>,
    stage: EventStage,
    /// Scheduled notes currently sounding: (note, channel) for note-off generation.
    active: Vec<ScheduledNote>,
    cursor: usize,
    param_cursor: usize,
    /// Emit the automation values in force at the playhead on the next block (after seek/start/swap).
    chase_params: bool,
    playing: bool,
    mode: TransportMode,
    position: i64,
    loop_enabled_override: Option<bool>,
    pending_seek: Option<i64>,
    pending_stop_release: bool,
    sample_rate: SampleRate,
    max_frames: usize,
    metronome_enabled: bool,
    metronome: rawdawg_dsp::oscillator::SineOscillator,
    metronome_env: f32,
    recording: bool,
    record: QueueProducer<RecordedEvent>,
    /// Count-in: frames left and the beat length; playback starts when it reaches zero.
    count_in_left: i64,
    count_in_total: i64,
    count_in_beat_frames: i64,
    /// Transport spans rendered this block: (project start frame, block offset, length).
    /// At most one loop wrap per block is expected; more spans are dropped (counted).
    spans: [(i64, usize, usize); MAX_SPANS],
    span_count: usize,
    /// Post-fader copies of every mixer channel's block (offline stem export only; `None` live).
    stems: Option<Vec<AudioBuffer>>,
}

const MAX_SPANS: usize = 8;

impl Engine {
    /// Create an engine with an empty plan. Non-RT.
    pub fn new(rate: SampleRate, max_frames: usize, limits: &EngineLimits) -> (Engine, EngineHandle) {
        let (cmd_p, cmd_c) = ControlQueue::create::<EngineCommand>(limits.max_control_messages_per_queue as usize);
        let (plan_p, plan_c) = Handoff::create::<Plan>(4);
        let (rec_p, rec_c) = ControlQueue::create::<RecordedEvent>(limits.max_control_messages_per_queue as usize);
        let status = Arc::new(EngineStatus::default());
        let engine = Engine {
            plan: Plan::empty(rate, max_frames),
            commands: cmd_c,
            plans: plan_c,
            status: status.clone(),
            stage: EventStage::new(limits.max_mixer_channels as usize, limits.max_scheduled_events_per_block as usize),
            active: Vec::with_capacity(MAX_ACTIVE_SCHEDULED),
            cursor: 0,
            param_cursor: 0,
            chase_params: false,
            playing: false,
            mode: TransportMode::Pattern,
            position: 0,
            loop_enabled_override: None,
            pending_seek: None,
            pending_stop_release: false,
            sample_rate: rate,
            max_frames,
            metronome_enabled: false,
            metronome: rawdawg_dsp::oscillator::SineOscillator::new(rate.as_f64()),
            metronome_env: 0.0,
            recording: false,
            record: rec_p,
            count_in_left: 0,
            count_in_total: 0,
            count_in_beat_frames: 1,
            spans: [(0, 0, 0); MAX_SPANS],
            span_count: 0,
            stems: None,
        };
        (engine, EngineHandle { commands: cmd_p, plans: plan_p, record: rec_c, status })
    }

    pub fn status(&self) -> &Arc<EngineStatus> {
        &self.status
    }
    pub fn plan(&self) -> &Plan {
        &self.plan
    }
    pub fn plan_mut(&mut self) -> &mut Plan {
        &mut self.plan
    }
    /// Install a plan directly (offline renderer / tests; not used by the RT path).
    pub fn install_plan(&mut self, plan: Plan) -> Plan {
        self.stage.ensure_channels(plan.slots.len().max(1));
        let old = std::mem::replace(&mut self.plan, plan);
        self.on_plan_changed();
        old
    }

    fn on_plan_changed(&mut self) {
        self.status.plan_revision.store(self.plan.revision, Ordering::Relaxed);
        self.status.output_latency.store(self.plan.output_latency, Ordering::Relaxed);
        self.status.tempo_bpm.store(self.plan.tempo.tempo_at(rawdawg_types::units::Tick(self.current_tick())).bpm().0);
        // Keep playing and keep the list of sounding scheduled notes: carried-over
        // processors still hold those voices and need their note-offs. Channels that
        // were rebuilt start silent and simply ignore the eventual note-off.
        self.reseek_cursor();
    }

    fn schedule(&self) -> &Schedule {
        match self.mode {
            TransportMode::Pattern => &self.plan.pattern_schedule,
            TransportMode::Song => &self.plan.song_schedule,
        }
    }

    fn loop_enabled(&self) -> bool {
        self.loop_enabled_override.unwrap_or(self.schedule().loop_enabled)
    }

    fn reseek_cursor(&mut self) {
        self.param_cursor = self.schedule().params.partition_point(|p| p.frame < self.position);
        self.chase_params = true;
        let pos = self.position;
        let sched = self.schedule();
        self.cursor = sched.notes.partition_point(|n| n.on_frame < pos);
    }

    fn current_tick(&self) -> i64 {
        self.plan.tempo.frame_to_tick_floor(self.position, self.sample_rate).0
    }

    fn handle_command(&mut self, cmd: EngineCommand) {
        match cmd {
            EngineCommand::Play => {
                if !self.playing {
                    self.playing = true;
                    self.reseek_cursor();
                }
            }
            EngineCommand::Stop => {
                self.playing = false;
                self.pending_stop_release = true;
                self.position = 0;
                self.reseek_cursor();
            }
            EngineCommand::Pause => {
                self.playing = false;
                self.pending_stop_release = true;
            }
            EngineCommand::SeekTick(t) => {
                let f = self.plan.tempo.tick_to_frame(rawdawg_types::units::Tick(t.max(0)), self.sample_rate);
                self.pending_seek = Some(f);
            }
            EngineCommand::SetMode(m) => {
                if m != self.mode {
                    self.mode = m;
                    self.pending_stop_release = true;
                    self.reseek_cursor();
                }
            }
            EngineCommand::SetLoopEnabled(b) => self.loop_enabled_override = Some(b),
            EngineCommand::Param { instance, id, value } => {
                if let Some(slot) = self.plan.slot_of(instance) {
                    let ev = TimedEvent::new(0, EventSource::Mouse, 0, EventKind::Param { id, value });
                    if !self.stage.push(slot, ev) {
                        self.status.dropped_events.incr();
                    }
                } else {
                    self.status.dropped_events.incr();
                }
            }
            EngineCommand::SetRecording(b) => {
                self.recording = b;
                self.status.recording.store(b, Ordering::Relaxed);
            }
            EngineCommand::PlayWithCountIn { beats } => {
                if !self.playing && self.count_in_left <= 0 {
                    let tick = rawdawg_types::units::Tick(self.current_tick());
                    let beat_ticks = self.plan.meter.meter_at(tick).beat_ticks();
                    let f0 = self.plan.tempo.tick_to_frame(tick, self.sample_rate);
                    let f1 = self.plan.tempo.tick_to_frame(rawdawg_types::units::Tick(tick.0 + beat_ticks.0), self.sample_rate);
                    self.count_in_beat_frames = (f1 - f0).max(1);
                    self.count_in_total = self.count_in_beat_frames * i64::from(beats.min(64));
                    self.count_in_left = self.count_in_total;
                    if beats == 0 {
                        self.playing = true;
                        self.reseek_cursor();
                    }
                }
            }
            EngineCommand::Live { channel, event } => {
                if let Some(ci) = self.plan.channel_index(channel) {
                    let mut ev = event;
                    ev.frame = 0;
                    if !self.stage.push(ci, ev) {
                        self.status.dropped_events.incr();
                    } else if self.recording && self.playing && self.record.push(RecordedEvent { frame: self.position, event: ev }).is_err() {
                        self.status.record_overflows.incr();
                    }
                } else {
                    self.status.dropped_events.incr();
                }
            }
            EngineCommand::Panic { source } => {
                for ci in 0..self.plan.channels.len() {
                    let ev = TimedEvent::new(0, source.unwrap_or(EventSource::Transport), 0, EventKind::AllNotesOff { source });
                    if !self.stage.push(ci, ev) {
                        self.status.dropped_events.incr();
                    }
                }
                if source.is_none() {
                    self.active.clear();
                }
            }
            EngineCommand::SetMasterProtection(b) => self.plan.master_protection = b,
            EngineCommand::SetMetronome(b) => {
                self.metronome_enabled = b;
                self.status.metronome.store(b, Ordering::Relaxed);
            }
        }
    }

    fn release_all_scheduled(&mut self) {
        // note-offs for every active scheduled note at frame 0
        while let Some(n) = self.active.pop() {
            let ev = TimedEvent::note_off(0, EventSource::Transport, n.midi_channel, n.id, n.pitch);
            if !self.stage.push(n.channel_index as usize, ev) {
                self.status.dropped_events.incr();
            }
        }
    }

    /// Schedule transport note events for `[self.position, self.position + frames)`.
    /// Handles loop wrap inside the block (notes crossing the end are truncated).
    fn schedule_block(&mut self, frames: usize) {
        self.span_count = 0;
        if !self.playing {
            return;
        }
        let mut block_offset = 0usize;
        let mut remaining = frames;
        while remaining > 0 {
            let loop_on = self.loop_enabled();
            let sched_end = self.schedule().loop_end_frame;
            let span_end = if loop_on && self.position < sched_end { (sched_end - self.position).min(remaining as i64) as usize } else { remaining };
            let start = self.position;
            let end = start + span_end as i64;
            // note-offs of active notes within the span
            let mut i = 0;
            while i < self.active.len() {
                let n = self.active[i];
                if n.off_frame < end {
                    let off_at = (n.off_frame - start).max(0) as u32 + block_offset as u32;
                    let ev = TimedEvent::note_off(off_at, EventSource::Transport, n.midi_channel, n.id, n.pitch);
                    if !self.stage.push(n.channel_index as usize, ev) {
                        self.status.dropped_events.incr();
                    }
                    self.active.swap_remove(i);
                } else {
                    i += 1;
                }
            }
            // automation chase: the latest value per target before the playhead (bounded scan)
            if self.chase_params {
                self.chase_params = false;
                let mut seen: [(u16, u32); 64] = [(u16::MAX, u32::MAX); 64];
                let mut n_seen = 0usize;
                let mut i = self.param_cursor;
                while i > 0 && n_seen < seen.len() {
                    i -= 1;
                    let p = self.schedule().params[i];
                    let key = (p.slot, p.id.0);
                    if seen[..n_seen].contains(&key) {
                        continue;
                    }
                    seen[n_seen] = key;
                    n_seen += 1;
                    let ev = TimedEvent::new(block_offset as u32, EventSource::Transport, 0, EventKind::Param { id: p.id, value: p.value });
                    if !self.stage.push(p.slot as usize, ev) {
                        self.status.dropped_events.incr();
                    }
                }
            }
            // automation events in [start, end)
            while let Some(p) = self.schedule().params.get(self.param_cursor).copied() {
                if p.frame >= end {
                    break;
                }
                self.param_cursor += 1;
                if p.frame < start {
                    continue;
                }
                let at = (p.frame - start) as u32 + block_offset as u32;
                let ev = TimedEvent::new(at, EventSource::Transport, 0, EventKind::Param { id: p.id, value: p.value });
                if !self.stage.push(p.slot as usize, ev) {
                    self.status.dropped_events.incr();
                }
            }
            // note-ons in [start, end)
            while let Some(n) = self.schedule().notes.get(self.cursor).copied() {
                if n.on_frame >= end {
                    break;
                }
                self.cursor += 1;
                if n.on_frame < start {
                    continue; // behind the playhead (after a seek): skip, no retroactive attack
                }
                let on_at = (n.on_frame - start) as u32 + block_offset as u32;
                let ev = TimedEvent::note_on(on_at, EventSource::Transport, n.midi_channel, n.id, n.pitch, n.velocity);
                if !self.stage.push(n.channel_index as usize, ev) {
                    self.status.dropped_events.incr();
                    continue;
                }
                if n.off_frame < end {
                    // note ends within this span as well
                    let off_at = (n.off_frame - start) as u32 + block_offset as u32;
                    let off = TimedEvent::note_off(off_at.max(on_at), EventSource::Transport, n.midi_channel, n.id, n.pitch);
                    if !self.stage.push(n.channel_index as usize, off) {
                        self.status.dropped_events.incr();
                    }
                } else if self.active.len() < self.active.capacity() {
                    self.active.push(n);
                } else {
                    self.status.dropped_events.incr();
                }
            }
            if span_end > 0 {
                if self.span_count < MAX_SPANS {
                    self.spans[self.span_count] = (start, block_offset, span_end);
                    self.span_count += 1;
                } else {
                    self.status.dropped_events.incr();
                }
            }
            self.position = end;
            block_offset += span_end;
            remaining -= span_end;
            if loop_on && self.position >= sched_end {
                // loop boundary: truncate everything still active, wrap
                let at = block_offset as u32;
                while let Some(n) = self.active.pop() {
                    let ev = TimedEvent::note_off(at.saturating_sub(1), EventSource::Transport, n.midi_channel, n.id, n.pitch);
                    if !self.stage.push(n.channel_index as usize, ev) {
                        self.status.dropped_events.incr();
                    }
                }
                self.position = self.schedule().loop_start_frame;
                self.reseek_cursor();
                if span_end == 0 {
                    break;
                }
            }
        }
    }

    /// Capture post-fader stems of every mixer channel from the next block on (non-RT:
    /// allocates one buffer per channel of the current plan; offline export only).
    pub fn enable_stems(&mut self) {
        let n = self.plan.mixer.len();
        self.stems = Some((0..n).map(|_| AudioBuffer::new(2, self.max_frames)).collect());
    }
    /// Post-fader stem of mixer node `index` for the last rendered block (`frames` long).
    pub fn stem(&self, index: usize, frames: usize) -> Option<(&[f32], &[f32])> {
        let st = self.stems.as_ref()?.get(index)?;
        Some((&st.channel(0)[..frames], &st.channel(1)[..frames]))
    }
    /// Mixer channel ids in plan order (stems are addressed by this index).
    pub fn mixer_ids(&self) -> Vec<rawdawg_types::MixerChannelId> {
        self.plan.mixer.iter().map(|m| m.id).collect()
    }
    pub fn master_index(&self) -> usize {
        self.plan.master_index
    }

    /// Play the plan's audio clips over this block's transport spans: resident decoded
    /// assets read with Hermite interpolation at the clip's rate, clip gain and linear
    /// fades, summed into the track's mixer buffer. Bounded by the clips overlapping the
    /// spans; no allocation.
    fn render_audio_clips(&mut self) {
        let plan = &mut self.plan;
        for si in 0..self.span_count {
            let (start, offset, len) = self.spans[si];
            let end = start + len as i64;
            for clip in &plan.audio_clips {
                if clip.start_frame >= end {
                    break; // sorted by start
                }
                let clip_end = clip.start_frame + clip.len_frames;
                if clip_end <= start {
                    continue;
                }
                let from = clip.start_frame.max(start);
                let to = clip_end.min(end);
                let node = &mut plan.mixer[clip.mixer_index];
                let (bl, br) = node.buffer.stereo_mut(offset + len);
                let stereo = clip.asset.is_stereo();
                for f in from..to {
                    let local = (f - clip.start_frame) as f64;
                    let src = clip.source_offset + local * clip.rate;
                    let mut g = clip.gain;
                    if clip.fade_in > 0.0 && local < clip.fade_in {
                        g *= (local / clip.fade_in) as f32;
                    }
                    let remaining = clip.len_frames as f64 - local;
                    if clip.fade_out > 0.0 && remaining < clip.fade_out {
                        g *= (remaining / clip.fade_out).max(0.0) as f32;
                    }
                    let i = offset + (f - start) as usize;
                    let l = clip.asset.read(0, src) * g;
                    let r = if stereo { clip.asset.read(1, src) * g } else { l };
                    bl[i] += l;
                    br[i] += r;
                }
            }
        }
    }

    /// Copy a mixer channel's buffer into its send targets (main input or sidechain), with
    /// per-send gain and latency compensation. `pre` selects the pre- or post-fader taps.
    fn run_sends(plan: &mut Plan, mi: usize, frames: usize, pre: bool, audible: bool, feed: bool) {
        let n_sends = plan.mixer[mi].sends.len();
        for si in 0..n_sends {
            let (target, is_pre, sidechain, comp) = {
                let snd = &plan.mixer[mi].sends[si];
                (snd.target_index, snd.pre_fader, snd.sidechain, snd.comp_frames)
            };
            if is_pre != pre || target == mi {
                continue;
            }
            // under solo a feeding node delivers sidechain sends to soloed/feeding targets and
            // ordinary sends to feeding targets only
            let deliver = audible || (feed && if sidechain { plan.solo_mixer[target] || plan.solo_feed[target] } else { plan.solo_feed[target] });
            if !deliver {
                continue;
            }
            let (src, dst) = if mi < target {
                let (a, b) = plan.mixer.split_at_mut(target);
                (&mut a[mi], &mut b[0])
            } else {
                let (a, b) = plan.mixer.split_at_mut(mi);
                (&mut b[0], &mut a[target])
            };
            let snd = &mut src.sends[si];
            let (sl, sr) = src.buffer.stereo_mut(frames);
            let (dl, dr) = if sidechain { dst.sidechain.stereo_mut(frames) } else { dst.buffer.stereo_mut(frames) };
            let mute = src.mute && !pre; // a muted channel still feeds pre-fader sends
            for i in 0..frames {
                // `deliver` already covers the solo gating for this edge
                let g = snd.gain.next() * if mute { 0.0 } else { 1.0 };
                let (mut l, mut r) = (sl[i] * g, sr[i] * g);
                if comp > 0 {
                    snd.comp[0].push(l);
                    snd.comp[1].push(r);
                    l = snd.comp[0].read(comp);
                    r = snd.comp[1].read(comp);
                }
                dl[i] += l;
                dr[i] += r;
            }
        }
    }

    /// Count-in clicks before playback starts (forced on, independent of the metronome toggle).
    fn count_in_block(&mut self, l: &mut [f32], r: &mut [f32]) {
        if self.count_in_left <= 0 {
            return;
        }
        let frames = l.len();
        let decay = (-1.0 / (0.03 * self.sample_rate.as_f64() as f32)).exp();
        let beat = self.count_in_beat_frames.max(1);
        let beats_total = self.count_in_total / beat;
        let meter_beats = i64::from(self.plan.meter.meter_at(rawdawg_types::units::Tick(self.current_tick())).numerator.max(1));
        for i in 0..frames {
            if self.count_in_left <= 0 {
                break;
            }
            let elapsed = self.count_in_total - self.count_in_left;
            if elapsed % beat == 0 {
                let beat_index = elapsed / beat;
                let accent = (beats_total - beat_index) % meter_beats == 0;
                self.metronome.set_frequency(if accent { 1760.0 } else { 1320.0 });
                self.metronome.reset_phase(0.0);
                self.metronome_env = 0.5;
            }
            if self.metronome_env > 1e-4 {
                let s = self.metronome.next() * self.metronome_env;
                self.metronome_env *= decay;
                l[i] += s;
                r[i] += s;
            }
            self.count_in_left -= 1;
            if self.count_in_left == 0 {
                self.playing = true;
                self.reseek_cursor();
            }
        }
        self.status.count_in_frames.store(self.count_in_left.max(0), Ordering::Relaxed);
    }

    fn metronome_block(&mut self, l: &mut [f32], r: &mut [f32]) {
        if !self.metronome_enabled || !self.playing {
            self.metronome_env = 0.0;
            return;
        }
        let frames = l.len();
        let start = self.position - frames as i64; // position already advanced
        let beat_ticks = self.plan.meter.meter_at(rawdawg_types::units::Tick(self.current_tick())).beat_ticks();
        let mut first_tick = self.plan.tempo.first_tick_at_or_after_frame(start, self.sample_rate);
        // round up to beat boundary
        let rem = first_tick.0.rem_euclid(beat_ticks.0);
        if rem != 0 {
            first_tick = rawdawg_types::units::Tick(first_tick.0 - rem + beat_ticks.0);
        }
        let mut click_frames: [i64; 8] = [i64::MAX; 8];
        let mut n_clicks = 0;
        let mut t = first_tick;
        while n_clicks < 8 {
            let f = self.plan.tempo.tick_to_frame(t, self.sample_rate);
            if f >= start + frames as i64 {
                break;
            }
            if f >= start {
                click_frames[n_clicks] = f - start;
                n_clicks += 1;
            }
            t = rawdawg_types::units::Tick(t.0 + beat_ticks.0);
        }
        let decay = (-1.0 / (0.03 * self.sample_rate.as_f64() as f32)).exp();
        let mut ci = 0;
        for i in 0..frames {
            if ci < n_clicks && click_frames[ci] as usize == i {
                let tick_pos = self.plan.tempo.frame_to_tick_floor(start + i as i64, self.sample_rate);
                let bar_start = self.plan.meter.position(tick_pos).beat == 0;
                self.metronome.set_frequency(if bar_start { 1760.0 } else { 1320.0 });
                self.metronome.reset_phase(0.0);
                self.metronome_env = 0.5;
                ci += 1;
            }
            if self.metronome_env > 1e-4 {
                let s = self.metronome.next() * self.metronome_env;
                self.metronome_env *= decay;
                l[i] += s;
                r[i] += s;
            }
        }
    }

    /// Render one block into `out_l`/`out_r` (already zeroed, equal length ≤ max_frames).
    /// This is the single render path for both real-time and offline operation.
    pub fn render_block(&mut self, out_l: &mut [f32], out_r: &mut [f32]) {
        let t0 = std::time::Instant::now();
        let frames = out_l.len().min(out_r.len()).min(self.max_frames);
        // 1. plan swap at block boundary (refused when retirement is full)
        if self.plans.try_swap_with(&mut self.plan, |new, old| new.migrate_from(old)) {
            self.on_plan_changed();
        }
        let refused = self.plans.refused_swaps();
        if refused > self.status.refused_swaps.get() {
            self.status.refused_swaps.add(refused - self.status.refused_swaps.get());
        }
        // 2. commands
        self.stage.clear();
        while let Some(cmd) = self.commands.pop() {
            self.handle_command(cmd);
        }
        if self.pending_stop_release {
            self.pending_stop_release = false;
            self.release_all_scheduled();
        }
        if let Some(f) = self.pending_seek.take() {
            self.release_all_scheduled();
            self.position = f;
            self.reseek_cursor();
        }
        // 3. transport schedule
        self.schedule_block(frames);
        // 4. channels → mixer buffers
        for m in &mut self.plan.mixer {
            m.buffer.clear(frames);
            if m.sidechain_fed {
                m.sidechain.clear(frames);
            }
        }
        // 4a. audio clips (song mode) straight into their track's mixer buffer
        if self.mode == TransportMode::Song {
            self.render_audio_clips();
        }
        let plan = &mut self.plan;
        let transport = TransportView {
            playing: self.playing,
            block_start_frame: self.position - frames as i64,
            tempo_bpm: self.status.tempo_bpm.load(),
            position_quarters: (self.position - frames as i64) as f64 / plan.sample_rate.as_f64() * self.status.tempo_bpm.load() / 60.0,
            meter_numerator: 4,
            meter_denominator: 4,
            sample_rate: plan.sample_rate.as_f64(),
        };
        let mut voices = 0u64;
        for (ci, ch) in plan.channels.iter_mut().enumerate() {
            let events = self.stage.events.get_mut(ci).map(|v| v.as_mut_slice()).unwrap_or(&mut []);
            sort_events(events);
            let (sl, sr) = plan.scratch.stereo_mut(frames);
            {
                let mut outs: [&mut [f32]; 2] = [sl, sr];
                let mut ctx = ProcessContext { audio: AudioBlock::output_only(frames, &mut outs), events, transport: &transport };
                ch.processor.process(&mut ctx);
            }
            let (sl, sr) = plan.scratch.stereo_mut(frames);
            let bad = sanitize(sl) + sanitize(sr);
            if bad > 0 {
                self.status.nonfinite_blocks.incr();
                ch.processor.reset();
            }
            if ch.comp_frames > 0 {
                ch.comp[0].delay_block(sl, ch.comp_frames);
                ch.comp[1].delay_block(sr, ch.comp_frames);
            }
            let audible = !ch.mute && (!plan.any_solo || plan.solo_mixer[ch.mixer_index] || plan.solo_feed[ch.mixer_index]);
            let mut peak = 0.0f32;
            let dest = &mut plan.mixer[ch.mixer_index].buffer;
            let (dl, dr) = dest.stereo_mut(frames);
            for i in 0..frames {
                let g = ch.gain.next() * if audible { 1.0 } else { 0.0 };
                let l = sl[i] * g;
                let r = sr[i] * g;
                // channel pan: equal-power applied as balance between L/R feeds
                let pl = ch.pan_l * std::f32::consts::SQRT_2;
                let pr = ch.pan_r * std::f32::consts::SQRT_2;
                dl[i] += l * pl;
                dr[i] += r * pr;
                peak = peak.max(l.abs()).max(r.abs());
            }
            ch.peak = peak;
            voices += u64::from(ch.processor.active_voices());
        }
        // 5. mixer in topological order (sources before outputs): inserts → pre sends → fader → post sends → output
        for oi in 0..plan.mixer_order.len() {
            let mi = plan.mixer_order[oi];
            let audible = !plan.any_solo || plan.solo_mixer[mi];
            // a sidechain dependency of a soloed node keeps running; only its deliveries are gated
            let feed = !audible && plan.solo_feed[mi];
            // inserts
            for ii in 0..plan.mixer[mi].inserts.len() {
                let slot = plan.mixer[mi].inserts[ii].slot;
                let events = self.stage.events.get_mut(slot).map(|v| v.as_mut_slice()).unwrap_or(&mut []);
                sort_events(events);
                let (node, fx_in) = (&mut plan.mixer[mi], &mut plan.fx_in);
                let ins = &mut node.inserts[ii];
                let (bl, br) = node.buffer.stereo_mut(frames);
                if ins.bypass {
                    // true bypass keeps the chain's latency so parallel paths stay aligned
                    if ins.latency > 0 {
                        ins.dry[0].delay_block(bl, ins.latency as usize);
                        ins.dry[1].delay_block(br, ins.latency as usize);
                    }
                    // parameters still arrive (bypassed effects keep their state current)
                    for ev in events.iter() {
                        if let EventKind::Param { id, value } = ev.kind {
                            ins.processor.set_param(id, value);
                        }
                    }
                    continue;
                }
                let (il, ir) = fx_in.stereo_mut(frames);
                il.copy_from_slice(bl);
                ir.copy_from_slice(br);
                {
                    let inputs: [&[f32]; 2] = [il, ir];
                    let (scl, scr) = node.sidechain.stereo_mut(frames);
                    let sidechain: [&[f32]; 2] = [scl, scr];
                    let mut outputs: [&mut [f32]; 2] = [bl, br];
                    let mut ctx = ProcessContext {
                        audio: AudioBlock { frames, inputs: &inputs, outputs: &mut outputs, sidechain: if node.sidechain_fed { &sidechain } else { &[] } },
                        events,
                        transport: &transport,
                    };
                    ins.processor.process(&mut ctx);
                }
                let (bl, br) = node.buffer.stereo_mut(frames);
                let bad = sanitize(bl) + sanitize(br);
                if bad > 0 {
                    self.status.nonfinite_blocks.incr();
                    ins.processor.reset();
                }
                if ins.has_mix {
                    let (il, ir) = fx_in.stereo_mut(frames);
                    let lat = ins.latency as usize;
                    for i in 0..frames {
                        let mix = ins.mix.next();
                        let (dl, dr) = if lat > 0 {
                            ins.dry[0].push(il[i]);
                            ins.dry[1].push(ir[i]);
                            (ins.dry[0].read(lat), ins.dry[1].read(lat))
                        } else {
                            (il[i], ir[i])
                        };
                        bl[i] = dl + (bl[i] - dl) * mix;
                        br[i] = dr + (br[i] - dr) * mix;
                    }
                } else if ins.latency > 0 {
                    // keep the dry line in step for a later mix change
                    let (il, ir) = fx_in.stereo_mut(frames);
                    for i in 0..frames {
                        ins.dry[0].push(il[i]);
                        ins.dry[1].push(ir[i]);
                    }
                }
                if let Some(m) = ins.processor.meter(0)
                    && let Some(a) = self.status.slot_meters.get(slot)
                {
                    a.store(m);
                }
            }
            // pre-fader sends (tap after the inserts)
            Self::run_sends(plan, mi, frames, true, audible, feed);
            // fader / pan / meters
            let mute = plan.mixer[mi].mute;
            let gain_pl = plan.mixer[mi].pan_l * std::f32::consts::SQRT_2;
            let gain_pr = plan.mixer[mi].pan_r * std::f32::consts::SQRT_2;
            let mut pk_l = 0.0f32;
            let mut pk_r = 0.0f32;
            {
                let m = &mut plan.mixer[mi];
                let (bl, br) = m.buffer.stereo_mut(frames);
                for i in 0..frames {
                    let g = m.gain.next() * if mute || !(audible || feed) { 0.0 } else { 1.0 };
                    bl[i] *= g * gain_pl;
                    br[i] *= g * gain_pr;
                    pk_l = pk_l.max(bl[i].abs());
                    pk_r = pk_r.max(br[i].abs());
                }
                m.peak_l = pk_l;
                m.peak_r = pk_r;
                // stems: the post-fader signal of every channel (offline export)
                if let Some(stems) = self.stems.as_mut()
                    && let Some(st) = stems.get_mut(mi)
                {
                    let (sl, sr) = st.stereo_mut(frames);
                    sl.copy_from_slice(bl);
                    sr.copy_from_slice(br);
                }
            }
            if let (Some(a), Some(b)) = (self.status.mixer_peak_l.get(mi), self.status.mixer_peak_r.get(mi)) {
                a.fetch_max(pk_l);
                b.fetch_max(pk_r);
            }
            // post-fader sends
            Self::run_sends(plan, mi, frames, false, audible, feed);
            // main output
            let out_idx = plan.mixer[mi].output_index;
            // a feeding node's main output reaches only other feeding nodes, never the mix
            let deliver_out = out_idx.is_some_and(|oi| audible || (feed && plan.solo_feed[oi]));
            if let Some(oi) = out_idx
                && oi != mi
                && deliver_out
            {
                let (src, dst) = if mi < oi {
                    let (a, b) = plan.mixer.split_at_mut(oi);
                    (&mut a[mi], &mut b[0])
                } else {
                    let (a, b) = plan.mixer.split_at_mut(mi);
                    (&mut b[0], &mut a[oi])
                };
                let comp = src.comp_frames;
                let (sl, sr) = src.buffer.stereo_mut(frames);
                if comp > 0 {
                    src.comp[0].delay_block(sl, comp);
                    src.comp[1].delay_block(sr, comp);
                }
                let (dl, dr) = dst.buffer.stereo_mut(frames);
                for i in 0..frames {
                    dl[i] += sl[i];
                    dr[i] += sr[i];
                }
            }
        }
        // 6. master → output, protection, meters
        {
            let master = &mut plan.mixer[plan.master_index];
            let (ml, mr) = master.buffer.stereo_mut(frames);
            out_l[..frames].copy_from_slice(ml);
            out_r[..frames].copy_from_slice(mr);
        }
        self.count_in_block(&mut out_l[..frames], &mut out_r[..frames]);
        self.metronome_block(&mut out_l[..frames], &mut out_r[..frames]);
        let plan = &mut self.plan;
        if plan.master_protection {
            plan.master_limiter.process(&mut out_l[..frames], &mut out_r[..frames]);
            self.status.master_reduction_db.store(plan.master_limiter.take_reduction_db());
        } else {
            self.status.master_reduction_db.store(0.0);
        }
        let bad = sanitize(&mut out_l[..frames]) + sanitize(&mut out_r[..frames]);
        if bad > 0 {
            self.status.nonfinite_blocks.incr();
        }
        {
            // scope: mono sum in 64-sample chunks through a stack buffer (no allocation)
            let mut tmp = [0.0f32; 64];
            let mut i = 0;
            while i < frames {
                let n = (frames - i).min(64);
                for k in 0..n {
                    tmp[k] = 0.5 * (out_l[i + k] + out_r[i + k]);
                }
                self.status.scope.push(&tmp[..n]);
                i += n;
            }
        }
        self.status.master_peak_l.fetch_max(rawdawg_dsp::math::peak(&out_l[..frames]));
        self.status.master_peak_r.fetch_max(rawdawg_dsp::math::peak(&out_r[..frames]));
        self.status.active_voices.store(voices, Ordering::Relaxed);
        self.status.playing.store(self.playing, Ordering::Relaxed);
        self.status.mode.store(self.mode as u8, Ordering::Relaxed);
        self.status.position_frame.store(self.position, Ordering::Relaxed);
        self.status.position_tick.store(self.current_tick(), Ordering::Relaxed);
        self.status.blocks.incr();
        self.status.frames.fetch_add(frames as u64, Ordering::Relaxed);
        let ns = t0.elapsed().as_nanos() as u64;
        self.status.last_block_ns.store(ns, Ordering::Relaxed);
        self.status.max_block_ns.fetch_max(ns, Ordering::Relaxed);
    }

    pub fn position_frame(&self) -> i64 {
        self.position
    }
    pub fn is_playing(&self) -> bool {
        self.playing
    }
}
