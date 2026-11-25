//! Schema v1 data types. Field semantics are normative per PROJECT_FORMAT.md.

use rawdawg_types::ids::*;
use rawdawg_types::units::{Db, Meter, Pan, SampleRate, Tick, UsPerQuarter};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Project {
    pub schema_version: u32,
    pub project_id: ProjectId,
    pub name: String,
    pub ppq: i64,
    pub sample_rate_hz: SampleRate,
    pub tempo: Vec<TempoPoint>,
    pub meter: Vec<MeterPoint>,
    #[serde(default)]
    pub assets: Vec<Asset>,
    pub channels: Vec<Channel>,
    pub patterns: Vec<Pattern>,
    #[serde(default)]
    pub tracks: Vec<Track>,
    #[serde(default)]
    pub clips: Vec<Clip>,
    pub mixer: Mixer,
    #[serde(default)]
    pub automation: Vec<AutomationCurve>,
    #[serde(default)]
    pub midi_mappings: Vec<MidiMapping>,
    #[serde(default)]
    pub r#loop: Option<LoopRange>,
    /// Which pattern the rack/piano roll currently show and pattern mode plays.
    #[serde(default)]
    pub selected_pattern: Option<PatternId>,
    /// Unknown/forward-compatible fields are preserved here.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extensions: BTreeMap<String, Value>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TempoPoint {
    pub tick: Tick,
    pub us_per_quarter: UsPerQuarter,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MeterPoint {
    pub tick: Tick,
    pub numerator: u8,
    pub denominator: u8,
}

impl MeterPoint {
    pub fn meter(&self) -> Meter {
        Meter { numerator: self.numerator, denominator: self.denominator }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoopRange {
    pub start_tick: Tick,
    pub end_tick: Tick,
    pub enabled: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssetKind {
    Audio,
    Sf2,
    ImpulseResponse,
    Midi,
    Other,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Asset {
    pub id: AssetId,
    /// Relative portable path (`assets/sha256/<hash>.<ext>`), `/` separators.
    pub path: String,
    pub kind: AssetKind,
    pub source_name: String,
    #[serde(default)]
    pub original_rate_hz: Option<u32>,
    #[serde(default)]
    pub channels: Option<u16>,
    #[serde(default)]
    pub frames: Option<u64>,
    pub bytes: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginFormat {
    Native,
    Clap,
    Vst3,
}

/// A processor instance (instrument or insert).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Instance {
    pub id: InstanceId,
    pub format: PluginFormat,
    pub type_id: TypeId,
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub vendor: String,
    #[serde(default)]
    pub version: String,
    pub state_version: u32,
    /// Native state inline (JSON), or external opaque state as base64 in `opaque_state`.
    #[serde(default)]
    pub state: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opaque_state_path: Option<String>,
    #[serde(default)]
    pub bypass: bool,
    /// Wet/dry 0..1 where meaningful (1 = fully wet). Instruments ignore.
    #[serde(default = "one")]
    pub mix: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path_hint: Option<String>,
}

fn one() -> f32 {
    1.0
}

impl Instance {
    pub fn native(type_id: &str, state: Value) -> Self {
        Self {
            id: InstanceId::new(),
            format: PluginFormat::Native,
            type_id: TypeId::new(type_id),
            display_name: String::new(),
            vendor: String::new(),
            version: String::new(),
            state_version: 1,
            state,
            opaque_state_path: None,
            bypass: false,
            mix: 1.0,
            path_hint: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Channel {
    pub id: ChannelId,
    pub name: String,
    #[serde(default)]
    pub color: Option<[u8; 3]>,
    pub instrument: Instance,
    pub gain_db: Db,
    pub pan: Pan,
    pub mute: bool,
    #[serde(default)]
    pub solo: bool,
    pub mixer_id: MixerChannelId,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Pattern {
    pub id: PatternId,
    pub name: String,
    #[serde(default)]
    pub color: Option<[u8; 3]>,
    pub length_ticks: Tick,
    #[serde(default = "default_steps")]
    pub steps: u16,
    /// Step subdivision as ticks per step (default sixteenth = 240).
    #[serde(default = "default_step_ticks")]
    pub step_ticks: Tick,
    pub lanes: Vec<Lane>,
}

fn default_steps() -> u16 {
    16
}
fn default_step_ticks() -> Tick {
    Tick(240)
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Lane {
    pub channel_id: ChannelId,
    pub notes: Vec<Note>,
    #[serde(default)]
    pub controllers: Vec<ControllerEvent>,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Note {
    pub id: NoteId,
    pub tick: Tick,
    pub duration_ticks: Tick,
    pub pitch: u8,
    /// (0, 1].
    pub velocity: f32,
    #[serde(default)]
    pub channel: u8,
}

impl Note {
    pub fn end_tick(&self) -> Tick {
        self.tick + self.duration_ticks
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ControllerKind {
    Cc { cc: u8, value: f32 },
    PitchBend { value: f32 },
    ChannelPressure { value: f32 },
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ControllerEvent {
    pub id: ControllerEventId,
    pub tick: Tick,
    pub channel: u8,
    #[serde(flatten)]
    pub kind: ControllerKind,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Track {
    pub id: TrackId,
    pub name: String,
    #[serde(default)]
    pub color: Option<[u8; 3]>,
    #[serde(default)]
    pub mute: bool,
    #[serde(default)]
    pub solo: bool,
    #[serde(default)]
    pub height: Option<f32>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClipContent {
    Pattern {
        pattern_id: PatternId,
        #[serde(default)]
        source_offset_ticks: Tick,
        #[serde(default)]
        looped: bool,
    },
    Audio {
        asset_id: AssetId,
        source_offset_frames: u64,
        source_length_frames: u64,
        #[serde(default)]
        gain_db: Db,
        #[serde(default)]
        fade_in_frames: u64,
        #[serde(default)]
        fade_out_frames: u64,
        #[serde(default = "one_f64")]
        playback_rate: f64,
    },
    Automation {
        curve_id: CurveId,
        #[serde(default)]
        source_offset_ticks: Tick,
        #[serde(default)]
        looped: bool,
    },
}

fn one_f64() -> f64 {
    1.0
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Clip {
    pub id: ClipId,
    pub track_id: TrackId,
    pub start_tick: Tick,
    /// For audio clips this is a derived display cache; the source bounds are authoritative.
    pub length_ticks: Tick,
    #[serde(default)]
    pub muted: bool,
    #[serde(flatten)]
    pub content: ClipContent,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SendTap {
    PreFader,
    PostFader,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Send {
    pub target_id: MixerChannelId,
    pub level_db: Db,
    pub tap: SendTap,
    /// `true` routes into the target's sidechain input instead of its main input.
    #[serde(default)]
    pub sidechain: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MixerChannel {
    pub id: MixerChannelId,
    pub name: String,
    #[serde(default)]
    pub color: Option<[u8; 3]>,
    pub gain_db: Db,
    pub pan: Pan,
    #[serde(default)]
    pub mute: bool,
    #[serde(default)]
    pub solo: bool,
    pub inserts: Vec<Instance>,
    pub sends: Vec<Send>,
    /// Main output target; `None` for master only.
    #[serde(default)]
    pub output_id: Option<MixerChannelId>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Mixer {
    pub master_id: MixerChannelId,
    pub channels: Vec<MixerChannel>,
    /// Master output protection (limiter) enabled. Disabling is an explicit user choice.
    #[serde(default = "default_true")]
    pub master_protection: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Interpolation {
    Step,
    Linear,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutomationMode {
    Off,
    Read,
    Touch,
    Write,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct AutomationPoint {
    pub id: PointId,
    pub tick: Tick,
    /// Normalized [0,1].
    pub value: f32,
    pub interpolation: Interpolation,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AutomationTarget {
    pub instance_id: InstanceId,
    pub param_id: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AutomationCurve {
    pub id: CurveId,
    pub name: String,
    pub target: AutomationTarget,
    pub points: Vec<AutomationPoint>,
    #[serde(default = "default_mode")]
    pub mode: AutomationMode,
    /// Orphaned (target missing) curves stay disabled until repaired.
    #[serde(default)]
    pub disabled: bool,
}

fn default_mode() -> AutomationMode {
    AutomationMode::Read
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MidiMapping {
    pub id: MappingId,
    /// Persistent device fingerprint (name), or `None` for any input.
    pub device: Option<String>,
    pub channel: Option<u8>,
    pub cc: u8,
    pub target: AutomationTarget,
    pub min: f32,
    pub max: f32,
    pub invert: bool,
}

impl Project {
    /// A new playable project: one default synth channel, one empty pattern,
    /// master mixer channel, 120 BPM 4/4 48 kHz.
    pub fn new_default(name: &str, synth_state: Value) -> Self {
        let master_id = MixerChannelId::new();
        let channel_mixer_id = MixerChannelId::new();
        let pattern_id = PatternId::new();
        let channel_id = ChannelId::new();
        Self {
            schema_version: crate::SCHEMA_VERSION,
            project_id: ProjectId::new(),
            name: name.to_owned(),
            ppq: rawdawg_types::units::PPQ,
            sample_rate_hz: SampleRate::DEFAULT,
            tempo: vec![TempoPoint { tick: Tick::ZERO, us_per_quarter: UsPerQuarter::DEFAULT }],
            meter: vec![MeterPoint { tick: Tick::ZERO, numerator: 4, denominator: 4 }],
            assets: vec![],
            channels: vec![Channel {
                id: channel_id,
                name: "Synth".to_owned(),
                color: Some([0x87, 0xC8, 0xA2]),
                instrument: Instance::native(rawdawg_types::ids::native_types::SYNTH, synth_state),
                gain_db: Db::UNITY,
                pan: Pan::CENTER,
                mute: false,
                solo: false,
                mixer_id: channel_mixer_id,
            }],
            patterns: vec![Pattern {
                id: pattern_id,
                name: "Pattern 1".to_owned(),
                color: None,
                length_ticks: Tick(3840),
                steps: 16,
                step_ticks: Tick(240),
                lanes: vec![Lane { channel_id, notes: vec![], controllers: vec![] }],
            }],
            tracks: vec![],
            clips: vec![],
            mixer: Mixer {
                master_id,
                channels: vec![
                    MixerChannel {
                        id: master_id,
                        name: "Master".to_owned(),
                        color: None,
                        gain_db: Db::UNITY,
                        pan: Pan::CENTER,
                        mute: false,
                        solo: false,
                        inserts: vec![],
                        sends: vec![],
                        output_id: None,
                    },
                    MixerChannel {
                        id: channel_mixer_id,
                        name: "Insert 1".to_owned(),
                        color: None,
                        gain_db: Db::UNITY,
                        pan: Pan::CENTER,
                        mute: false,
                        solo: false,
                        inserts: vec![],
                        sends: vec![],
                        output_id: Some(master_id),
                    },
                ],
                master_protection: true,
            },
            automation: vec![],
            midi_mappings: vec![],
            r#loop: None,
            selected_pattern: Some(pattern_id),
            extensions: BTreeMap::new(),
        }
    }

    pub fn channel(&self, id: ChannelId) -> Option<&Channel> {
        self.channels.iter().find(|c| c.id == id)
    }
    pub fn channel_mut(&mut self, id: ChannelId) -> Option<&mut Channel> {
        self.channels.iter_mut().find(|c| c.id == id)
    }
    pub fn pattern(&self, id: PatternId) -> Option<&Pattern> {
        self.patterns.iter().find(|p| p.id == id)
    }
    pub fn pattern_mut(&mut self, id: PatternId) -> Option<&mut Pattern> {
        self.patterns.iter_mut().find(|p| p.id == id)
    }
    pub fn mixer_channel(&self, id: MixerChannelId) -> Option<&MixerChannel> {
        self.mixer.channels.iter().find(|c| c.id == id)
    }
    pub fn mixer_channel_mut(&mut self, id: MixerChannelId) -> Option<&mut MixerChannel> {
        self.mixer.channels.iter_mut().find(|c| c.id == id)
    }
    /// Find any instance (instrument or insert) by id.
    pub fn instance(&self, id: InstanceId) -> Option<&Instance> {
        self.channels.iter().map(|c| &c.instrument).chain(self.mixer.channels.iter().flat_map(|m| m.inserts.iter())).find(|i| i.id == id)
    }
    pub fn instance_mut(&mut self, id: InstanceId) -> Option<&mut Instance> {
        if let Some(c) = self.channels.iter_mut().find(|c| c.instrument.id == id) {
            return Some(&mut c.instrument);
        }
        self.mixer.channels.iter_mut().flat_map(|m| m.inserts.iter_mut()).find(|i| i.id == id)
    }
    pub fn tempo_map(&self) -> Result<rawdawg_transport::TempoMap, rawdawg_transport::MapError> {
        rawdawg_transport::TempoMap::new(self.tempo.iter().map(|t| rawdawg_transport::TempoEntry { tick: t.tick, us_per_quarter: t.us_per_quarter }).collect())
    }
    pub fn meter_map(&self) -> Result<rawdawg_transport::MeterMap, rawdawg_transport::MapError> {
        rawdawg_transport::MeterMap::new(self.meter.iter().map(|m| rawdawg_transport::MeterEntry { tick: m.tick, meter: m.meter() }).collect())
    }
}
