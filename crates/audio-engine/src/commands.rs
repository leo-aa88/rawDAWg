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
}

/// A live event as the engine applied it, with the absolute project frame it landed on.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RecordedEvent {
    pub frame: i64,
    pub event: TimedEvent,
}
