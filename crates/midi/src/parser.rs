//! Raw MIDI 1.0 byte parsing with running status, plus per-device note identity.

use rawdawg_types::events::{EventKind, midi_message_to_kind};

/// Assigns stable note ids so overlapping equal pitches on one channel match FIFO.
#[derive(Debug, Default, Clone)]
pub struct NoteRegistry {
    /// (channel, pitch, note_id) for every sounding note, in press order.
    active: Vec<(u8, u8, u32)>,
    next_id: u32,
}

impl NoteRegistry {
    pub fn new() -> Self {
        Self { active: Vec::with_capacity(128), next_id: 1 }
    }
    /// Register a note-on and return its id.
    pub fn note_on(&mut self, channel: u8, pitch: u8) -> u32 {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1).max(1);
        if self.active.len() < 1024 {
            self.active.push((channel, pitch, id));
        }
        id
    }
    /// Resolve a note-off to the oldest sounding note with that channel/pitch.
    pub fn note_off(&mut self, channel: u8, pitch: u8) -> Option<u32> {
        let i = self.active.iter().position(|(c, p, _)| *c == channel && *p == pitch)?;
        Some(self.active.remove(i).2)
    }
    /// Drain all sounding notes (device disconnect / disable).
    pub fn drain(&mut self) -> Vec<(u8, u8, u32)> {
        std::mem::take(&mut self.active)
    }
    pub fn active_count(&self) -> usize {
        self.active.len()
    }
}

/// Incremental parser for a byte stream from one device.
#[derive(Debug, Default, Clone)]
pub struct MidiParser {
    running_status: Option<u8>,
    data: [u8; 2],
    have: u8,
    in_sysex: bool,
    registry: NoteRegistry,
}

impl MidiParser {
    pub fn new() -> Self {
        Self { registry: NoteRegistry::new(), ..Default::default() }
    }

    pub fn registry_mut(&mut self) -> &mut NoteRegistry {
        &mut self.registry
    }

    fn data_len(status: u8) -> u8 {
        match status & 0xF0 {
            0xC0 | 0xD0 => 1,
            0x80..=0xB0 | 0xE0 => 2,
            _ => 0,
        }
    }

    /// Feed bytes; returns `(channel, kind)` for every complete channel message.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<(u8, EventKind)> {
        let mut out = Vec::new();
        for &b in bytes {
            if b == 0xF0 {
                self.in_sysex = true;
                continue;
            }
            if b == 0xF7 {
                self.in_sysex = false;
                continue;
            }
            if self.in_sysex {
                continue;
            }
            if b >= 0xF8 {
                continue; // realtime messages do not affect running status
            }
            if b >= 0x80 {
                if b >= 0xF0 {
                    // system common: cancels running status
                    self.running_status = None;
                    self.have = 0;
                    continue;
                }
                self.running_status = Some(b);
                self.have = 0;
                continue;
            }
            let Some(status) = self.running_status else { continue };
            let need = Self::data_len(status);
            if need == 0 {
                continue;
            }
            self.data[self.have as usize] = b;
            self.have += 1;
            if self.have == need {
                self.have = 0;
                let channel = status & 0x0F;
                let d1 = self.data[0];
                let d2 = if need == 2 { self.data[1] } else { 0 };
                let kind = match status & 0xF0 {
                    0x90 if d2 > 0 => {
                        let id = self.registry.note_on(channel, d1 & 0x7F);
                        Some(EventKind::NoteOn { note_id: id, pitch: d1 & 0x7F, velocity: f32::from(d2 & 0x7F) / 127.0 })
                    }
                    0x80 | 0x90 => self.registry.note_off(channel, d1 & 0x7F).map(|id| EventKind::NoteOff { note_id: id, pitch: d1 & 0x7F }),
                    0xA0 => {
                        // poly pressure applies to the newest sounding note of that pitch
                        let id = self.registry.active.iter().rev().find(|(c, p, _)| *c == channel && *p == (d1 & 0x7F)).map(|x| x.2);
                        id.and_then(|id| midi_message_to_kind(status, d1, d2, id))
                    }
                    _ => midi_message_to_kind(status, d1, d2, 0),
                };
                if let Some(k) = kind {
                    out.push((channel, k));
                }
            }
        }
        out
    }

    /// Release every sounding note (returns note-off events).
    pub fn release_all(&mut self) -> Vec<(u8, EventKind)> {
        self.registry.drain().into_iter().map(|(c, p, id)| (c, EventKind::NoteOff { note_id: id, pitch: p })).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_note_on_off_with_running_status() {
        let mut p = MidiParser::new();
        let evs = p.feed(&[0x90, 60, 100, 64, 90, 0x80, 60, 0]);
        assert_eq!(evs.len(), 3);
        assert!(matches!(evs[0].1, EventKind::NoteOn { note_id: 1, pitch: 60, .. }));
        assert!(matches!(evs[1].1, EventKind::NoteOn { note_id: 2, pitch: 64, .. }));
        assert!(matches!(evs[2].1, EventKind::NoteOff { note_id: 1, pitch: 60 }));
        assert_eq!(p.registry.active_count(), 1);
    }

    #[test]
    fn velocity_zero_is_note_off_and_overlapping_pitches_match_fifo() {
        let mut p = MidiParser::new();
        let evs = p.feed(&[0x91, 60, 100, 0x91, 60, 100, 0x91, 60, 0, 0x91, 60, 0]);
        assert!(matches!(evs[2].1, EventKind::NoteOff { note_id: 1, .. }));
        assert!(matches!(evs[3].1, EventKind::NoteOff { note_id: 2, .. }));
        assert_eq!(evs[2].0, 1);
        assert!(p.feed(&[0x91, 60, 0]).is_empty(), "stray note-off without a sounding note is dropped");
    }

    #[test]
    fn sysex_and_realtime_bytes_are_skipped_without_breaking_running_status() {
        let mut p = MidiParser::new();
        let evs = p.feed(&[0x90, 60, 100, 0xF8, 62, 100, 0xF0, 0x7E, 0x7F, 0x09, 0x01, 0xF7, 64, 100]);
        assert_eq!(evs.len(), 3);
        assert!(matches!(evs[2].1, EventKind::NoteOn { pitch: 64, .. }));
        // system common cancels running status
        let evs = p.feed(&[0xF2, 0x00, 0x00, 65, 100]);
        assert!(evs.is_empty());
    }

    #[test]
    fn controllers_bend_pressure() {
        let mut p = MidiParser::new();
        let evs = p.feed(&[0xB0, 64, 127, 0xE0, 0x00, 0x60, 0xD0, 64, 0x90, 60, 100, 0xA0, 60, 50, 0xC0, 5]);
        assert!(matches!(evs[0].1, EventKind::Control { cc: 64, value } if (value - 1.0).abs() < 1e-6));
        assert!(matches!(evs[1].1, EventKind::PitchBend { value } if value > 0.4 && value < 0.6));
        assert!(matches!(evs[2].1, EventKind::ChannelPressure { .. }));
        assert!(matches!(evs[4].1, EventKind::NotePressure { note_id: 1, .. }));
        assert_eq!(evs.len(), 5, "program change produces no engine event");
    }

    #[test]
    fn release_all_on_disconnect() {
        let mut p = MidiParser::new();
        p.feed(&[0x90, 60, 100, 0x90, 64, 100]);
        let offs = p.release_all();
        assert_eq!(offs.len(), 2);
        assert!(offs.iter().all(|(_, k)| k.is_note_off()));
        assert_eq!(p.registry.active_count(), 0);
    }
}
