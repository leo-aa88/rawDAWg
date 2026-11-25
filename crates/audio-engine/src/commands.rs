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
}
