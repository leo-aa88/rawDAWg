//! MIDI input (IO-01). The midir callback thread only copies bytes and a
//! timestamp into a bounded queue; parsing, note-identity assignment and
//! routing happen on the coordinator thread.

pub mod input;
pub mod parser;

pub use input::{MidiClock, MidiDeviceInfo, MidiInputManager, MidiMessage, MidiTime};
pub use parser::{MidiParser, NoteRegistry};
