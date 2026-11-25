//! Note identity, source isolation and sustain bookkeeping (ARCHITECTURE.md §4).

use rawdawg_types::events::EventSource;

/// Number of distinct source slots tracked for sustain/isolation.
pub const SOURCE_SLOTS: usize = 32;

/// Map an [`EventSource`] onto a bounded slot index.
pub fn source_slot(src: EventSource) -> u8 {
    match src {
        EventSource::Transport => 0,
        EventSource::TypingKeyboard => 1,
        EventSource::Mouse => 2,
        EventSource::Preview => 3,
        EventSource::Host => 4,
        EventSource::MidiDevice(n) => 5 + (n % (SOURCE_SLOTS as u16 - 5)) as u8,
    }
}

/// Identity of a sounding note.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct NoteKey {
    pub source: u8,
    pub channel: u8,
    pub note_id: u32,
    pub pitch: u8,
}

/// Per-(source, channel) sustain pedal state.
#[derive(Clone, Debug)]
pub struct SustainTable {
    down: [[bool; 16]; SOURCE_SLOTS],
}

impl Default for SustainTable {
    fn default() -> Self {
        Self::new()
    }
}

impl SustainTable {
    pub fn new() -> Self {
        Self { down: [[false; 16]; SOURCE_SLOTS] }
    }
    pub fn set(&mut self, source: u8, channel: u8, down: bool) {
        self.down[usize::from(source) % SOURCE_SLOTS][usize::from(channel) % 16] = down;
    }
    pub fn is_down(&self, source: u8, channel: u8) -> bool {
        self.down[usize::from(source) % SOURCE_SLOTS][usize::from(channel) % 16]
    }
    pub fn clear(&mut self) {
        self.down = [[false; 16]; SOURCE_SLOTS];
    }
}

/// Bounded last-note-priority stack for mono modes.
#[derive(Clone, Debug)]
pub struct HeldNotes {
    keys: [Option<(NoteKey, f32)>; 32],
    len: usize,
}

impl Default for HeldNotes {
    fn default() -> Self {
        Self::new()
    }
}

impl HeldNotes {
    pub fn new() -> Self {
        Self { keys: [None; 32], len: 0 }
    }
    pub fn clear(&mut self) {
        self.len = 0;
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
    pub fn push(&mut self, key: NoteKey, velocity: f32) {
        self.remove(key);
        if self.len == self.keys.len() {
            // drop the oldest to stay bounded
            self.keys.copy_within(1.., 0);
            self.len -= 1;
        }
        self.keys[self.len] = Some((key, velocity));
        self.len += 1;
    }
    pub fn remove(&mut self, key: NoteKey) -> bool {
        let mut i = 0;
        let mut removed = false;
        while i < self.len {
            if let Some((k, _)) = self.keys[i]
                && k == key
            {
                self.keys.copy_within(i + 1..self.len, i);
                self.len -= 1;
                removed = true;
                continue;
            }
            i += 1;
        }
        removed
    }
    /// Remove every note from a source (device disconnect / focus loss).
    pub fn remove_source(&mut self, source: u8) {
        let mut i = 0;
        while i < self.len {
            if let Some((k, _)) = self.keys[i]
                && k.source == source
            {
                self.keys.copy_within(i + 1..self.len, i);
                self.len -= 1;
                continue;
            }
            i += 1;
        }
    }
    pub fn top(&self) -> Option<(NoteKey, f32)> {
        if self.len == 0 { None } else { self.keys[self.len - 1] }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(id: u32) -> NoteKey {
        NoteKey { source: 1, channel: 0, note_id: id, pitch: 60 }
    }

    #[test]
    fn held_notes_last_priority() {
        let mut h = HeldNotes::new();
        h.push(key(1), 0.5);
        h.push(key(2), 0.6);
        h.push(key(3), 0.7);
        assert_eq!(h.top().unwrap().0, key(3));
        assert!(h.remove(key(3)));
        assert_eq!(h.top().unwrap().0, key(2));
        assert!(h.remove(key(1)));
        assert_eq!(h.top().unwrap().0, key(2));
        assert!(!h.remove(key(9)));
        h.remove(key(2));
        assert!(h.is_empty());
    }

    #[test]
    fn held_notes_bounded() {
        let mut h = HeldNotes::new();
        for i in 0..100 {
            h.push(key(i), 0.5);
        }
        assert_eq!(h.top().unwrap().0, key(99));
        h.remove_source(1);
        assert!(h.is_empty());
    }

    #[test]
    fn sustain_isolated_per_source_and_channel() {
        let mut s = SustainTable::new();
        s.set(source_slot(EventSource::MidiDevice(0)), 3, true);
        assert!(s.is_down(source_slot(EventSource::MidiDevice(0)), 3));
        assert!(!s.is_down(source_slot(EventSource::MidiDevice(1)), 3));
        assert!(!s.is_down(source_slot(EventSource::MidiDevice(0)), 4));
        s.clear();
        assert!(!s.is_down(5, 3));
    }

    #[test]
    fn source_slots_are_bounded() {
        for n in 0..1000u16 {
            assert!((source_slot(EventSource::MidiDevice(n)) as usize) < SOURCE_SLOTS);
        }
        assert_ne!(source_slot(EventSource::TypingKeyboard), source_slot(EventSource::Transport));
    }
}
