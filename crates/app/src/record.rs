//! Live note/controller recording (IO-03): collects events the engine actually
//! applied (with their project frames) into a take, maps frames to pattern
//! ticks through the capture tempo map, and turns the take into notes and
//! controller events. Raw timing is preserved; quantization is a separate,
//! explicit step.

use rawdawg_audio_engine::RecordedEvent;
use rawdawg_project::{ControllerEvent, ControllerKind, Note};
use rawdawg_transport::TempoMap;
use rawdawg_types::events::{EventKind, EventSource};
use rawdawg_types::units::{SampleRate, Tick};
use rawdawg_types::{ChannelId, ControllerEventId, NoteId, PatternId};
use std::collections::HashMap;

/// A recording in progress.
#[derive(Debug)]
pub struct Take {
    pub pattern: PatternId,
    pub channel: ChannelId,
    /// Pattern length in ticks (recording wraps inside it in pattern mode).
    pub length_ticks: i64,
    /// Song mode: ticks are made relative to this project tick.
    pub origin_tick: i64,
    pub wrap: bool,
    tempo: TempoMap,
    rate: SampleRate,
    /// Latency compensation subtracted from every event (frames).
    offset_frames: i64,
    open: HashMap<(EventSource, u32), (i64, u8, f32, u8)>,
    pub notes: Vec<Note>,
    pub controllers: Vec<ControllerEvent>,
    pub late_events: u32,
}

impl Take {
    #[allow(clippy::too_many_arguments, reason = "constructor mirrors the take's fields")]
    pub fn new(pattern: PatternId, channel: ChannelId, length_ticks: i64, origin_tick: i64, wrap: bool, tempo: TempoMap, rate: SampleRate, offset_ms: f32) -> Self {
        let offset_frames = (offset_ms as f64 * rate.as_f64() / 1000.0).round() as i64;
        Self {
            pattern,
            channel,
            length_ticks: length_ticks.max(1),
            origin_tick,
            wrap,
            tempo,
            rate,
            offset_frames,
            open: HashMap::new(),
            notes: Vec::new(),
            controllers: Vec::new(),
            late_events: 0,
        }
    }

    fn tick_of(&self, frame: i64) -> i64 {
        let t = self.tempo.frame_to_tick_floor((frame - self.offset_frames).max(0), self.rate).0 - self.origin_tick;
        if self.wrap { t.rem_euclid(self.length_ticks) } else { t.max(0) }
    }

    /// Feed one applied event. Returns true when it changed the take.
    pub fn push(&mut self, ev: RecordedEvent) -> bool {
        let frame = ev.frame;
        let e = ev.event;
        // mouse/preview auditions are not performances
        if matches!(e.source, EventSource::Mouse | EventSource::Preview) {
            return false;
        }
        match e.kind {
            EventKind::NoteOn { note_id, pitch, velocity } => {
                self.open.insert((e.source, note_id), (frame, pitch, velocity, e.channel));
                true
            }
            EventKind::NoteOff { note_id, pitch } => {
                let key = (e.source, note_id);
                let Some((on_frame, on_pitch, velocity, channel)) = self.open.remove(&key) else { return false };
                // the note-on pitch is authoritative (a note-off always targets the id)
                let _ = pitch;
                self.close(on_frame, frame, on_pitch, velocity, channel);
                true
            }
            EventKind::AllNotesOff { source } => {
                let keys: Vec<_> = self.open.keys().filter(|(s, _)| source.is_none_or(|src| src == *s)).copied().collect();
                for k in keys {
                    if let Some((on_frame, pitch, velocity, channel)) = self.open.remove(&k) {
                        self.close(on_frame, frame, pitch, velocity, channel);
                    }
                }
                !self.open.is_empty()
            }
            EventKind::Control { cc, value } => self.controller(frame, e.channel, ControllerKind::Cc { cc, value }),
            EventKind::PitchBend { value } => self.controller(frame, e.channel, ControllerKind::PitchBend { value }),
            EventKind::ChannelPressure { value } => self.controller(frame, e.channel, ControllerKind::ChannelPressure { value }),
            _ => false,
        }
    }

    fn controller(&mut self, frame: i64, channel: u8, kind: ControllerKind) -> bool {
        let tick = self.tick_of(frame);
        self.controllers.push(ControllerEvent { id: ControllerEventId::new(), tick: Tick(tick), channel: channel.min(15), kind });
        true
    }

    fn close(&mut self, on_frame: i64, off_frame: i64, pitch: u8, velocity: f32, channel: u8) {
        let on_tick = self.tick_of(on_frame);
        let raw_dur = (self.tick_of_unwrapped(off_frame) - self.tick_of_unwrapped(on_frame)).max(1);
        // a note reaching past the pattern end is truncated there (the loop retriggers it)
        let dur = if self.wrap { raw_dur.min(self.length_ticks - on_tick).max(1) } else { raw_dur };
        self.notes.push(Note { id: NoteId::new(), tick: Tick(on_tick), duration_ticks: Tick(dur), pitch, velocity: velocity.clamp(0.01, 1.0), channel: channel.min(15) });
    }

    fn tick_of_unwrapped(&self, frame: i64) -> i64 {
        self.tempo.frame_to_tick_floor((frame - self.offset_frames).max(0), self.rate).0
    }

    /// Notes still held, as (tick, pitch) for display.
    pub fn open_notes(&self) -> Vec<(i64, u8)> {
        self.open.values().map(|(f, p, _, _)| (self.tick_of(*f), *p)).collect()
    }

    /// Close everything still open at `frame` (recording stopped).
    pub fn finish(&mut self, frame: i64) {
        let keys: Vec<_> = self.open.keys().copied().collect();
        for k in keys {
            if let Some((on_frame, pitch, velocity, channel)) = self.open.remove(&k) {
                self.close(on_frame, frame.max(on_frame + 1), pitch, velocity, channel);
            }
        }
    }

    pub fn is_empty(&self) -> bool {
        self.notes.is_empty() && self.controllers.is_empty() && self.open.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rawdawg_types::events::TimedEvent;
    use rawdawg_types::units::UsPerQuarter;

    fn take(wrap: bool, offset_ms: f32) -> Take {
        Take::new(PatternId::new(), ChannelId::new(), 3840, 0, wrap, TempoMap::constant(UsPerQuarter(500_000)), SampleRate(48_000), offset_ms)
    }
    fn on(frame: i64, id: u32, pitch: u8) -> RecordedEvent {
        RecordedEvent { frame, event: TimedEvent::note_on(0, EventSource::MidiDevice(0), 0, id, pitch, 0.9) }
    }
    fn off(frame: i64, id: u32, pitch: u8) -> RecordedEvent {
        RecordedEvent { frame, event: TimedEvent::note_off(0, EventSource::MidiDevice(0), 0, id, pitch) }
    }

    #[test]
    fn frames_map_to_ticks_through_the_tempo_map_and_raw_timing_is_kept() {
        let mut t = take(false, 0.0);
        // 120 BPM: 24 000 frames per beat (960 ticks) → 25 frames per tick
        t.push(on(24_000 + 250, 1, 60));
        t.push(off(24_000 + 250 + 12_000, 1, 60));
        assert_eq!(t.notes.len(), 1);
        assert_eq!(t.notes[0].tick.0, 970, "off-grid timing preserved (not snapped)");
        assert_eq!(t.notes[0].duration_ticks.0, 480);
        assert_eq!(t.notes[0].pitch, 60);
    }

    #[test]
    fn pattern_mode_wraps_at_the_loop_and_truncates_crossing_notes() {
        let mut t = take(true, 0.0);
        // one bar = 96 000 frames; a note starting in the second pass at beat 1 lands at tick 0..
        t.push(on(96_000 + 24_000, 1, 64));
        t.push(off(96_000 + 36_000, 1, 64));
        assert_eq!(t.notes[0].tick.0, 960);
        assert_eq!(t.notes[0].duration_ticks.0, 480);
        // a note crossing the loop end is cut at the end of the pattern
        t.push(on(90_000, 2, 67));
        t.push(off(100_000, 2, 67));
        assert_eq!(t.notes[1].tick.0, 3600);
        assert_eq!(t.notes[1].duration_ticks.0, 240);
    }

    #[test]
    fn offset_compensation_and_finish_close_open_notes() {
        let mut t = take(false, 10.0); // 10 ms = 480 frames earlier
        t.push(on(24_000 + 480, 1, 60));
        assert_eq!(t.open_notes(), vec![(960, 60)]);
        t.finish(24_000 + 480 + 2_400);
        assert_eq!(t.notes[0].tick.0, 960);
        assert_eq!(t.notes[0].duration_ticks.0, 96);
        assert!(t.open_notes().is_empty());
        // controllers are kept as raw events
        let mut t = take(false, 0.0);
        t.push(RecordedEvent { frame: 1_000, event: TimedEvent::new(0, EventSource::MidiDevice(0), 0, EventKind::Control { cc: 1, value: 0.5 }) });
        t.push(RecordedEvent { frame: 2_000, event: TimedEvent::new(0, EventSource::MidiDevice(0), 0, EventKind::PitchBend { value: -0.25 }) });
        assert_eq!(t.controllers.len(), 2);
        assert_eq!(t.controllers[0].tick.0, 40);
        assert!(matches!(t.controllers[1].kind, ControllerKind::PitchBend { value } if (value + 0.25).abs() < 1e-6));
    }
}
