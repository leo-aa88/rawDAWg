//! Control messages from UI/MIDI threads to the real-time engine.

use rawdawg_types::ChannelId;
use rawdawg_types::events::TimedEvent;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TransportMode {
    #[default]
    Pattern,
    Song,
}

/// Bounded-size commands; none allocate when handled on the RT thread.
#[derive(Clone, Debug, PartialEq)]
pub enum EngineCommand {
    Play,
    Stop,
    Pause,
    /// Seek to an absolute project tick (applied at the next block boundary).
    SeekTick(i64),
    SetMode(TransportMode),
    SetLoopEnabled(bool),
    /// Route the live audio input (monitor queue) into a mixer channel by plan index;
    /// `None` switches monitoring off (the default).
    SetMonitor(Option<usize>),
    /// Audition a decoded asset through the master (browser preview) at a linear gain;
    /// replaces any running preview. The sender keeps its own `Arc` until the preview
    /// ended so the RT thread never drops the last reference.
    PreviewSample {
        asset: std::sync::Arc<rawdawg_sampler::SampleAsset>,
        gain: f32,
    },
    StopPreview,
    /// Live event (typing keyboard, MIDI, mouse) routed to a channel's instrument.
    /// `frame` is ignored and replaced by the block offset derived from the timestamp.
    Live {
        channel: ChannelId,
        event: TimedEvent,
    },
    /// Release everything from a source (focus loss, device disconnect) or all.
    Panic {
        source: Option<rawdawg_types::events::EventSource>,
    },
    /// Enable/disable the master protection limiter (explicit user choice).
    SetMasterProtection(bool),
    SetMetronome(bool),
    /// Start playing after `beats` metronome clicks (count-in); no-op while playing.
    PlayWithCountIn {
        beats: u32,
    },
    /// Echo applied live events with their project frame to the record queue.
    SetRecording(bool),
    /// Parameter change for any processor instance (instruments or inserts).
    Param {
        instance: rawdawg_types::InstanceId,
        id: rawdawg_types::ParamId,
        value: f32,
    },
}

/// A live event as the engine applied it, with the absolute project frame it landed on.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RecordedEvent {
    pub frame: i64,
    pub event: TimedEvent,
}
