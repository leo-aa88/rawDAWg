//! The keys being played right now (typing keys, MIDI devices, the on-screen piano, the piano
//! roll's keys): their names (C4 is middle C) and, when they form a chord, its name. The toolbar
//! shows them; the piano roll and the on-screen keyboards light them up.

use rawdawg_types::events::{EventKind, EventSource, TimedEvent};

/// Bound on remembered keys (a stuck controller cannot grow it).
const MAX_HELD: usize = 128;

#[derive(Default)]
pub(crate) struct HeldNotes {
    /// (source, note id, pitch) of every key down.
    notes: Vec<(EventSource, u32, u8)>,
}

impl HeldNotes {
    /// Follow one live event: a note-on adds its key, a note-off (or a note-on with velocity 0)
    /// removes it, all-notes-off and reset clear the source (or everything).
    pub(crate) fn track(&mut self, ev: &TimedEvent) {
        match ev.kind {
            EventKind::NoteOn { note_id, pitch, velocity } if velocity > 0.0 => {
                self.notes.retain(|n| !(n.0 == ev.source && n.1 == note_id));
                if self.notes.len() < MAX_HELD {
                    self.notes.push((ev.source, note_id, pitch));
                }
            }
            EventKind::NoteOn { note_id, pitch, .. } | EventKind::NoteOff { note_id, pitch } => {
                // by id; by key only when no note has that id (a release that carries another
                // id), so the same key held twice from one source loses just one
                let before = self.notes.len();
                self.notes.retain(|n| !(n.0 == ev.source && n.1 == note_id));
                if self.notes.len() == before
                    && let Some(i) = self.notes.iter().position(|n| n.0 == ev.source && n.2 == pitch)
                {
                    self.notes.remove(i);
                }
            }
            EventKind::AllNotesOff { source: Some(s) } => self.notes.retain(|n| n.0 != s),
            EventKind::AllNotesOff { source: None } | EventKind::Reset => self.notes.clear(),
            _ => {}
        }
    }

    pub(crate) fn clear(&mut self) {
        self.notes.clear();
    }

    /// The held keys, lowest first, each once.
    pub(crate) fn pitches(&self) -> Vec<u8> {
        let mut p: Vec<u8> = self.notes.iter().map(|n| n.2).collect();
        p.sort_unstable();
        p.dedup();
        p
    }

    pub(crate) fn contains(&self, pitch: u8) -> bool {
        self.notes.iter().any(|n| n.2 == pitch)
    }
}

/// Letters with their natural pitch classes.
const LETTERS: [(char, i32); 7] = [('C', 0), ('D', 2), ('E', 4), ('F', 5), ('G', 7), ('A', 9), ('B', 11)];

/// Spelling of a pitch class with no chord to follow: the usual mixed choice.
const PLAIN: [&str; 12] = ["C", "C#", "D", "Eb", "E", "F", "F#", "G", "Ab", "A", "Bb", "B"];

/// Chord shapes: intervals above the root, the letter step of each interval (so a minor third
/// is spelled as a third: C–Eb, not C–D#), and the suffix.
const CHORDS: &[(&[i32], &[usize], &str)] = &[
    (&[0, 4, 7], &[0, 2, 4], ""),
    (&[0, 3, 7], &[0, 2, 4], "m"),
    (&[0, 3, 6], &[0, 2, 4], "dim"),
    (&[0, 4, 8], &[0, 2, 4], "aug"),
    (&[0, 2, 7], &[0, 1, 4], "sus2"),
    (&[0, 5, 7], &[0, 3, 4], "sus4"),
    (&[0, 7], &[0, 4], " power"),
    (&[0, 4, 7, 10], &[0, 2, 4, 6], "7"),
    (&[0, 4, 7, 11], &[0, 2, 4, 6], "maj7"),
    (&[0, 3, 7, 10], &[0, 2, 4, 6], "m7"),
    (&[0, 3, 7, 11], &[0, 2, 4, 6], "m(maj7)"),
    (&[0, 3, 6, 10], &[0, 2, 4, 6], "m7b5"),
    (&[0, 3, 6, 9], &[0, 2, 4, 6], "dim7"),
    (&[0, 4, 8, 10], &[0, 2, 4, 6], "aug7"),
    (&[0, 4, 7, 9], &[0, 2, 4, 5], "6"),
    (&[0, 3, 7, 9], &[0, 2, 4, 5], "m6"),
    (&[0, 5, 7, 10], &[0, 3, 4, 6], "7sus4"),
    (&[0, 2, 4, 7], &[0, 1, 2, 4], "add9"),
    (&[0, 2, 3, 7], &[0, 1, 2, 4], "m(add9)"),
    (&[0, 2, 4, 7, 10], &[0, 1, 2, 4, 6], "9"),
    (&[0, 2, 4, 7, 11], &[0, 1, 2, 4, 6], "maj9"),
    (&[0, 2, 3, 7, 10], &[0, 1, 2, 4, 6], "m9"),
];

fn accidental(diff: i32) -> Option<&'static str> {
    match diff {
        -2 => Some("bb"),
        -1 => Some("b"),
        0 => Some(""),
        1 => Some("#"),
        2 => Some("##"),
        _ => None,
    }
}

/// `pc` spelled on `letter` (index into [`LETTERS`]): the name and the accidental's offset.
fn on_letter(pc: i32, letter: usize) -> Option<(String, i32)> {
    let (l, natural) = LETTERS[letter % 7];
    let mut diff = (pc - natural).rem_euclid(12);
    if diff > 6 {
        diff -= 12;
    }
    accidental(diff).map(|a| (format!("{l}{a}"), diff))
}

/// The ways to spell a root: a white key by its own letter (never B#, E#, Cb or Fb), a black
/// key as the sharp of the letter below or the flat of the letter above.
fn root_letters(pc: i32) -> Vec<usize> {
    if let Some(natural) = LETTERS.iter().position(|(_, n)| *n == pc.rem_euclid(12)) {
        return vec![natural];
    }
    (0..7).filter(|&i| on_letter(pc, i).is_some_and(|(_, d)| d.abs() == 1)).collect()
}

/// A recognised chord: its name ("Cm", "C/E") and the spelling of each pitch class in it.
struct Chord {
    name: String,
    spelled: Vec<(i32, String, i32)>,
}

fn find_chord(pitches: &[u8]) -> Option<Chord> {
    let mut pcs: Vec<i32> = pitches.iter().map(|p| i32::from(*p) % 12).collect();
    pcs.sort_unstable();
    pcs.dedup();
    if pcs.len() < 2 {
        return None;
    }
    let bass = i32::from(*pitches.iter().min()?) % 12;
    // the bass as root first, so C–E–G–A is C6 and A–C–E–G is Am7
    let roots = std::iter::once(bass).chain(pcs.iter().copied().filter(|p| *p != bass));
    for root in roots {
        let mut ivs: Vec<i32> = pcs.iter().map(|p| (p - root).rem_euclid(12)).collect();
        ivs.sort_unstable();
        let Some((shape, steps, suffix)) = CHORDS.iter().find(|(s, ..)| *s == ivs.as_slice()) else { continue };
        // the root spelling with the fewest accidentals over the whole chord (Eb–G–Bb, not D#–F##–A#)
        let best = root_letters(root)
            .into_iter()
            .filter_map(|letter| {
                let tones: Option<Vec<(i32, String, i32)>> =
                    shape.iter().zip(steps.iter()).map(|(iv, step)| on_letter(root + iv, letter + step).map(|(n, d)| ((root + iv).rem_euclid(12), n, d))).collect();
                tones.map(|t| (t.iter().map(|x| x.2.abs()).sum::<i32>(), t))
            })
            .min_by_key(|(cost, _)| *cost)?
            .1;
        let name_of = |pc: i32| best.iter().find(|t| t.0 == pc).map(|t| t.1.clone()).unwrap_or_else(|| PLAIN[pc as usize].to_owned());
        let mut name = format!("{}{suffix}", name_of(root));
        // a power chord's "inversion" is the same two notes: no slash
        if bass != root && shape.len() > 2 {
            name += &format!("/{}", name_of(bass));
        }
        return Some(Chord { name, spelled: best });
    }
    None
}

/// "C4 Eb4 G4 · Cm": the held keys with octave numbers (C4 is middle C, MIDI 60), spelled to
/// fit the chord they form, and the chord's name. Empty when nothing is held.
pub(crate) fn describe(pitches: &[u8]) -> String {
    let chord = find_chord(pitches);
    let names: Vec<String> = pitches
        .iter()
        .map(|&p| {
            let pc = i32::from(p) % 12;
            let (name, offset) =
                chord.as_ref().and_then(|c| c.spelled.iter().find(|t| t.0 == pc)).map(|t| (t.1.clone(), t.2)).unwrap_or_else(|| (PLAIN[pc as usize].to_owned(), 0));
            // the octave of the letter (B#3 is the C4 key, Cb4 the B3 key)
            let octave = (i32::from(p) - offset).div_euclid(12) - 1;
            format!("{name}{octave}")
        })
        .collect();
    match chord {
        Some(c) => format!("{} · {}", names.join(" "), c.name),
        None => names.join(" "),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notes_are_named_with_middle_c_as_c4_and_chords_are_named_and_spelled() {
        assert_eq!(describe(&[]), "");
        assert_eq!(describe(&[60]), "C4");
        assert_eq!(describe(&[63]), "Eb4");
        assert_eq!(describe(&[21, 108]), "A0 C8");
        assert_eq!(describe(&[60, 63, 67]), "C4 Eb4 G4 · Cm");
        assert_eq!(describe(&[60, 64, 67]), "C4 E4 G4 · C");
        assert_eq!(describe(&[64, 67, 72]), "E4 G4 C5 · C/E");
        assert_eq!(describe(&[63, 67, 70]), "Eb4 G4 Bb4 · Eb", "flats, not D#–F##–A#");
        assert_eq!(describe(&[61, 65, 68]), "Db4 F4 Ab4 · Db");
        assert_eq!(describe(&[66, 70, 73]), "F#4 A#4 C#5 · F#");
        assert_eq!(describe(&[60, 64, 67, 70]), "C4 E4 G4 Bb4 · C7");
        assert_eq!(describe(&[57, 60, 64, 67]), "A3 C4 E4 G4 · Am7");
        assert_eq!(describe(&[60, 64, 67, 69]), "C4 E4 G4 A4 · C6");
        assert_eq!(describe(&[59, 62, 65]), "B3 D4 F4 · Bdim");
        assert_eq!(describe(&[60, 67]), "C4 G4 · C power");
        assert_eq!(describe(&[55, 60]), "G3 C4 · C power", "the fourth below is the same power chord");
        // white-key roots keep their letter: Cdim7, Fdim, Bdim, Baug (not B#dim7, E#dim, Cbaug)
        assert_eq!(describe(&[60, 63, 66, 69]), "C4 Eb4 Gb4 Bbb4 · Cdim7");
        assert_eq!(describe(&[65, 68, 71]), "F4 Ab4 Cb5 · Fdim");
        assert_eq!(describe(&[68, 71, 77]), "Ab4 Cb5 F5 · Fdim/Ab");
        assert_eq!(describe(&[59, 63, 67]), "B3 D#4 F##4 · Baug");
        assert_eq!(describe(&[65, 68, 71, 75]), "F4 Ab4 Cb5 Eb5 · Fm7b5");
        // the octave number belongs to the letter: B#4 is the C5 key, Cb5 the B4 key
        assert_eq!(describe(&[64, 68, 72]), "E4 G#4 B#4 · Eaug");
        assert_eq!(describe(&[0, 127]), "C-1 G9 · C power");
        assert_eq!(describe(&[48, 60, 63, 67]), "C3 C4 Eb4 G4 · Cm", "doubled root");
        assert_eq!(describe(&[60, 61, 62]), "C4 C#4 D4", "a cluster is just its notes");
    }

    #[test]
    fn keys_are_tracked_per_source_until_released() {
        let mut h = HeldNotes::default();
        let on = |src, id, p| TimedEvent::note_on(0, src, 0, id, p, 0.8);
        h.track(&on(EventSource::TypingKeyboard, 1, 60));
        h.track(&on(EventSource::TypingKeyboard, 2, 63));
        h.track(&on(EventSource::MidiDevice(0), 7, 67));
        h.track(&on(EventSource::Mouse, 9, 60));
        assert_eq!(h.pitches(), vec![60, 63, 67]);
        assert!(h.contains(63) && !h.contains(64));
        h.track(&TimedEvent::note_off(0, EventSource::TypingKeyboard, 0, 2, 63));
        assert_eq!(h.pitches(), vec![60, 67]);
        // one source's release leaves the other's key down
        h.track(&TimedEvent::note_off(0, EventSource::TypingKeyboard, 0, 1, 60));
        assert_eq!(h.pitches(), vec![60, 67], "the mouse still holds C4");
        // a device's release with another id still lets go of its key; velocity 0 is a release
        h.track(&TimedEvent::note_off(0, EventSource::MidiDevice(0), 0, 99, 67));
        h.track(&on(EventSource::MidiDevice(1), 3, 70));
        h.track(&TimedEvent::note_on(0, EventSource::MidiDevice(1), 0, 3, 70, 0.0));
        assert_eq!(h.pitches(), vec![60]);
        h.track(&TimedEvent::new(0, EventSource::Mouse, 0, EventKind::AllNotesOff { source: Some(EventSource::Mouse) }));
        assert!(h.pitches().is_empty());
        h.track(&on(EventSource::TypingKeyboard, 4, 62));
        h.clear();
        assert!(h.pitches().is_empty());
        // the same key held twice from one source (another octave's key, two MIDI channels):
        // one release leaves the other held
        h.track(&on(EventSource::TypingKeyboard, 5, 60));
        h.track(&on(EventSource::TypingKeyboard, 6, 60));
        h.track(&TimedEvent::note_off(0, EventSource::TypingKeyboard, 0, 6, 60));
        assert_eq!(h.pitches(), vec![60], "one C4 is still down");
        h.track(&TimedEvent::note_off(0, EventSource::TypingKeyboard, 0, 99, 60));
        assert!(h.pitches().is_empty(), "a release with an unknown id lets go of one by key");
    }
}
