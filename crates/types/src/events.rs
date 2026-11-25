//! Immutable timed events exchanged between sequencer, input adapters and processors.
//!
//! Ordering within a block follows ARCHITECTURE.md §4: frame offset first,
//! then kind rank (reset/all-notes-off, note-off, controller/parameter,
//! note-on), then deterministic source/ID order.

use crate::params::ParamId;
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;

/// Identifies where an event came from so that releases never cross sources.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Serialize, Deserialize)]
pub enum EventSource {
    /// Scheduled from the transport (pattern/song playback).
    Transport,
    /// Live typing keyboard.
    TypingKeyboard,
    /// Live mouse audition (piano ruler, on-screen keyboard).
    Mouse,
    /// Physical MIDI input device, identified by a small stable index.
    MidiDevice(u16),
    /// Internal preview (browser/evolution audition).
    Preview,
    /// Plugin host adapter (events supplied by an external host).
    Host,
}

/// Kinds of timed events.
#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub enum EventKind {
    /// Hard reset of the receiving processor (all voices off immediately).
    Reset,
    /// Release every voice belonging to the given source (or all when `None`).
    AllNotesOff {
        source: Option<EventSource>,
    },
    NoteOff {
        note_id: u32,
        pitch: u8,
    },
    /// Controller change, value normalized to [0,1] (MIDI 0..127 maps linearly).
    Control {
        cc: u8,
        value: f32,
    },
    /// Pitch bend in [-1, 1].
    PitchBend {
        value: f32,
    },
    /// Channel pressure (aftertouch) in [0, 1].
    ChannelPressure {
        value: f32,
    },
    /// Polyphonic pressure for one note in [0, 1].
    NotePressure {
        note_id: u32,
        value: f32,
    },
    /// Per-note expression (CLAP-style) in the given expression's documented range.
    NoteExpression {
        note_id: u32,
        expression: NoteExpression,
        value: f32,
    },
    /// Normalized parameter change ([0,1]) for a processor parameter.
    Param {
        id: ParamId,
        value: f32,
    },
    /// Parameter modulation offset added to the base value (normalized).
    ParamMod {
        id: ParamId,
        amount: f32,
    },
    NoteOn {
        note_id: u32,
        pitch: u8,
        velocity: f32,
    },
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum NoteExpression {
    Volume,
    Pan,
    Tuning,
    Vibrato,
    Expression,
    Brightness,
    Pressure,
}

impl EventKind {
    /// Rank used for deterministic tie-breaking at equal frame offsets.
    pub fn rank(&self) -> u8 {
        match self {
            EventKind::Reset => 0,
            EventKind::AllNotesOff { .. } => 1,
            EventKind::NoteOff { .. } => 2,
            EventKind::Control { .. }
            | EventKind::PitchBend { .. }
            | EventKind::ChannelPressure { .. }
            | EventKind::NotePressure { .. }
            | EventKind::NoteExpression { .. }
            | EventKind::Param { .. }
            | EventKind::ParamMod { .. } => 3,
            EventKind::NoteOn { .. } => 4,
        }
    }

    pub fn is_note_on(&self) -> bool {
        matches!(self, EventKind::NoteOn { .. })
    }
    pub fn is_note_off(&self) -> bool {
        matches!(self, EventKind::NoteOff { .. })
    }
    /// Validate value ranges and finiteness.
    pub fn is_valid(&self) -> bool {
        fn unit(v: f32) -> bool {
            v.is_finite() && (0.0..=1.0).contains(&v)
        }
        match self {
            EventKind::Reset | EventKind::AllNotesOff { .. } => true,
            EventKind::NoteOff { pitch, .. } => *pitch <= 127,
            EventKind::NoteOn { pitch, velocity, .. } => *pitch <= 127 && velocity.is_finite() && *velocity > 0.0 && *velocity <= 1.0,
            EventKind::Control { cc, value } => *cc <= 127 && unit(*value),
            EventKind::PitchBend { value } => value.is_finite() && (-1.0..=1.0).contains(value),
            EventKind::ChannelPressure { value } => unit(*value),
            EventKind::NotePressure { value, .. } => unit(*value),
            EventKind::NoteExpression { value, .. } => value.is_finite(),
            EventKind::Param { value, .. } => unit(*value),
            EventKind::ParamMod { amount, .. } => amount.is_finite() && (-1.0..=1.0).contains(amount),
        }
    }
}

/// An event scheduled at a sample offset inside the current processing block.
#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub struct TimedEvent {
    /// Offset in frames from the block start; always `< frames`.
    pub frame: u32,
    pub source: EventSource,
    /// MIDI channel 0..=15.
    pub channel: u8,
    pub kind: EventKind,
}

impl TimedEvent {
    pub fn new(frame: u32, source: EventSource, channel: u8, kind: EventKind) -> Self {
        Self { frame, source, channel, kind }
    }

    pub fn note_on(frame: u32, source: EventSource, channel: u8, note_id: u32, pitch: u8, velocity: f32) -> Self {
        Self::new(frame, source, channel, EventKind::NoteOn { note_id, pitch, velocity })
    }

    pub fn note_off(frame: u32, source: EventSource, channel: u8, note_id: u32, pitch: u8) -> Self {
        Self::new(frame, source, channel, EventKind::NoteOff { note_id, pitch })
    }

    /// Deterministic total order: frame, then kind rank, then source, channel and note/param id.
    pub fn schedule_cmp(&self, other: &Self) -> Ordering {
        self.frame
            .cmp(&other.frame)
            .then_with(|| self.kind.rank().cmp(&other.kind.rank()))
            .then_with(|| self.source.cmp(&other.source))
            .then_with(|| self.channel.cmp(&other.channel))
            .then_with(|| self.secondary_key().cmp(&other.secondary_key()))
    }

    fn secondary_key(&self) -> u64 {
        match self.kind {
            EventKind::NoteOn { note_id, pitch, .. } | EventKind::NoteOff { note_id, pitch } => (u64::from(pitch) << 32) | u64::from(note_id),
            EventKind::NotePressure { note_id, .. } | EventKind::NoteExpression { note_id, .. } => u64::from(note_id),
            EventKind::Control { cc, .. } => u64::from(cc),
            EventKind::Param { id, .. } | EventKind::ParamMod { id, .. } => u64::from(id.0),
            _ => 0,
        }
    }

    pub fn is_valid(&self, frames: u32) -> bool {
        self.frame < frames && self.channel <= 15 && self.kind.is_valid()
    }
}

/// Sort a block of events into schedule order (stable, deterministic).
pub fn sort_events(events: &mut [TimedEvent]) {
    events.sort_by(TimedEvent::schedule_cmp);
}

/// Normalize a MIDI 1.0 byte triple into an [`EventKind`] (velocity-0 note-on → note-off).
/// `note_id` is assigned by the caller's note registry.
pub fn midi_message_to_kind(status: u8, data1: u8, data2: u8, note_id: u32) -> Option<EventKind> {
    match status & 0xF0 {
        0x90 if data2 == 0 => Some(EventKind::NoteOff { note_id, pitch: data1 & 0x7F }),
        0x90 => Some(EventKind::NoteOn { note_id, pitch: data1 & 0x7F, velocity: f32::from(data2 & 0x7F) / 127.0 }),
        0x80 => Some(EventKind::NoteOff { note_id, pitch: data1 & 0x7F }),
        0xB0 => Some(EventKind::Control { cc: data1 & 0x7F, value: f32::from(data2 & 0x7F) / 127.0 }),
        0xE0 => {
            let raw = (u16::from(data2 & 0x7F) << 7) | u16::from(data1 & 0x7F);
            let v = (f32::from(raw) - 8192.0) / 8192.0;
            Some(EventKind::PitchBend { value: v.clamp(-1.0, 1.0) })
        }
        0xD0 => Some(EventKind::ChannelPressure { value: f32::from(data1 & 0x7F) / 127.0 }),
        0xA0 => Some(EventKind::NotePressure { note_id, value: f32::from(data2 & 0x7F) / 127.0 }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schedule_order_puts_offs_before_ons_at_same_frame() {
        let on = TimedEvent::note_on(10, EventSource::Transport, 0, 1, 60, 0.5);
        let off = TimedEvent::note_off(10, EventSource::Transport, 0, 2, 60);
        let cc = TimedEvent::new(10, EventSource::Transport, 0, EventKind::Control { cc: 1, value: 0.2 });
        let early_on = TimedEvent::note_on(3, EventSource::Transport, 0, 9, 72, 1.0);
        let reset = TimedEvent::new(10, EventSource::Transport, 0, EventKind::Reset);
        let mut evs = vec![on, cc, off, early_on, reset];
        sort_events(&mut evs);
        assert_eq!(evs[0], early_on);
        assert_eq!(evs[1], reset);
        assert_eq!(evs[2], off);
        assert_eq!(evs[3], cc);
        assert_eq!(evs[4], on);
    }

    #[test]
    fn same_pitch_note_ons_order_by_source_then_id() {
        let a = TimedEvent::note_on(0, EventSource::MidiDevice(1), 0, 7, 60, 0.5);
        let b = TimedEvent::note_on(0, EventSource::MidiDevice(0), 0, 9, 60, 0.5);
        let c = TimedEvent::note_on(0, EventSource::MidiDevice(0), 0, 3, 60, 0.5);
        let mut evs = vec![a, b, c];
        sort_events(&mut evs);
        assert_eq!(evs, vec![c, b, a]);
    }

    #[test]
    fn midi_velocity_zero_is_note_off() {
        assert_eq!(midi_message_to_kind(0x90, 60, 0, 5), Some(EventKind::NoteOff { note_id: 5, pitch: 60 }));
        assert!(matches!(midi_message_to_kind(0x91, 60, 127, 5), Some(EventKind::NoteOn { pitch: 60, velocity, .. }) if (velocity - 1.0).abs() < 1e-6));
        assert!(matches!(midi_message_to_kind(0xE0, 0x00, 0x40, 0), Some(EventKind::PitchBend { value }) if value.abs() < 1e-6));
        assert!(matches!(midi_message_to_kind(0xE0, 0x7F, 0x7F, 0), Some(EventKind::PitchBend { value }) if (value - 1.0).abs() < 1e-3));
        assert!(matches!(midi_message_to_kind(0xE0, 0x00, 0x00, 0), Some(EventKind::PitchBend { value }) if (value + 1.0).abs() < 1e-6));
        assert_eq!(midi_message_to_kind(0xF8, 0, 0, 0), None);
    }

    #[test]
    fn validation_rejects_out_of_range() {
        assert!(!TimedEvent::note_on(0, EventSource::Transport, 16, 0, 60, 0.5).is_valid(64));
        assert!(!TimedEvent::note_on(64, EventSource::Transport, 0, 0, 60, 0.5).is_valid(64));
        assert!(!TimedEvent::note_on(0, EventSource::Transport, 0, 0, 60, 0.0).is_valid(64));
        assert!(!TimedEvent::note_on(0, EventSource::Transport, 0, 0, 128, 0.5).is_valid(64));
        assert!(!TimedEvent::new(0, EventSource::Transport, 0, EventKind::Param { id: ParamId(1), value: f32::NAN }).is_valid(64));
        assert!(TimedEvent::note_on(63, EventSource::Transport, 15, 0, 127, 1.0).is_valid(64));
    }
}
