//! T-MIDI: SMF export/import round trip (note timing, tempo, meter, CC, pitch bend,
//! program), type 0 split, PPQ conversion, overlapping equal pitches, SMPTE rejection.

use rawdawg_project::smf::{SmfError, export_song, import};
use rawdawg_project::{Clip, ClipContent, ControllerEvent, ControllerKind, MeterPoint, Note, Project, TempoPoint, Track};
use rawdawg_types::units::{PPQ, Tick, UsPerQuarter};
use rawdawg_types::{ClipId, ControllerEventId, NoteId, TrackId};

fn song() -> Project {
    let mut p = Project::new_default("smf-song", serde_json::json!({}));
    let lane = &mut p.patterns[0].lanes[0];
    lane.notes.push(Note { id: NoteId::new(), tick: Tick(0), duration_ticks: Tick(480), pitch: 60, velocity: 0.8, channel: 0 });
    lane.notes.push(Note { id: NoteId::new(), tick: Tick(960), duration_ticks: Tick(1920), pitch: 64, velocity: 0.5, channel: 0 });
    lane.notes.push(Note { id: NoteId::new(), tick: Tick(960), duration_ticks: Tick(240), pitch: 64, velocity: 1.0, channel: 0 });
    lane.controllers.push(ControllerEvent { id: ControllerEventId::new(), tick: Tick(100), channel: 0, kind: ControllerKind::Cc { cc: 1, value: 64.0 / 127.0 } });
    lane.controllers.push(ControllerEvent { id: ControllerEventId::new(), tick: Tick(200), channel: 0, kind: ControllerKind::PitchBend { value: 0.5 } });
    p.tempo = vec![TempoPoint { tick: Tick::ZERO, us_per_quarter: UsPerQuarter(500_000) }, TempoPoint { tick: Tick(3840), us_per_quarter: UsPerQuarter(400_000) }];
    p.meter = vec![MeterPoint { tick: Tick::ZERO, numerator: 4, denominator: 4 }, MeterPoint { tick: Tick(3840), numerator: 3, denominator: 4 }];
    let tid = TrackId::new();
    p.tracks.push(Track { id: tid, name: "T".into(), color: None, mute: false, solo: false, height: None, mixer_id: None });
    // the one-bar pattern looped over two bars
    p.clips.push(Clip {
        id: ClipId::new(),
        track_id: tid,
        start_tick: Tick(0),
        length_ticks: Tick(7680),
        muted: false,
        content: ClipContent::Pattern { pattern_id: p.patterns[0].id, source_offset_ticks: Tick::ZERO, looped: true },
    });
    p
}

#[test]
fn export_type1_then_import_preserves_timing_tempo_meter_and_controllers() {
    let p = song();
    let bytes = export_song(&p).unwrap();
    assert_eq!(&bytes[..4], b"MThd");
    let smf = midly::Smf::parse(&bytes).unwrap();
    assert_eq!(smf.header.format, midly::Format::Parallel);
    assert_eq!(smf.tracks.len(), 2, "conductor + one channel track");
    let imp = import(&bytes, "roundtrip").unwrap();
    assert_eq!(imp.tracks.len(), 1);
    let t = &imp.tracks[0];
    assert_eq!(t.name, p.channels[0].name);
    // 3 notes × 2 loop passes, ticks preserved at PPQ 960
    assert_eq!(t.notes.len(), 6, "{:?}", t.notes);
    let mut ticks: Vec<(i64, i64, u8)> = t.notes.iter().map(|n| (n.tick.0, n.duration_ticks.0, n.pitch)).collect();
    ticks.sort();
    assert_eq!(ticks[0], (0, 480, 60));
    assert!(ticks.contains(&(960, 1920, 64)) && ticks.contains(&(960, 240, 64)), "overlapping equal pitches both survive: {ticks:?}");
    assert!(ticks.contains(&(3840, 480, 60)) && ticks.contains(&(4800, 1920, 64)), "second loop pass: {ticks:?}");
    let v: Vec<f32> = t.notes.iter().filter(|n| n.pitch == 60).map(|n| n.velocity).collect();
    assert!(v.iter().all(|v| (v - 0.8).abs() < 1.0 / 127.0));
    assert_eq!(imp.tempo, p.tempo, "tempo map round-trips");
    assert_eq!(imp.meter, p.meter, "meter map round-trips");
    let cc: Vec<&ControllerEvent> = t.controllers.iter().filter(|c| matches!(c.kind, ControllerKind::Cc { cc: 1, .. })).collect();
    assert_eq!(cc.len(), 2);
    assert_eq!(cc[0].tick, Tick(100));
    let bend = t.controllers.iter().find(|c| matches!(c.kind, ControllerKind::PitchBend { .. })).unwrap();
    if let ControllerKind::PitchBend { value } = bend.kind {
        assert!((value - 0.5).abs() < 1.0 / 8192.0);
    }
    assert_eq!(imp.length_ticks, Tick(7680));
    assert!(imp.warnings.is_empty(), "{:?}", imp.warnings);
}

fn file(format: midly::Format, ppq: u16, tracks: Vec<Vec<(u32, midly::TrackEventKind<'static>)>>) -> Vec<u8> {
    use midly::num::u28;
    let mut smf = midly::Smf::new(midly::Header::new(format, midly::Timing::Metrical(midly::num::u15::new(ppq))));
    for t in tracks {
        let mut track: midly::Track = t.into_iter().map(|(d, kind)| midly::TrackEvent { delta: u28::new(d), kind }).collect();
        track.push(midly::TrackEvent { delta: u28::new(0), kind: midly::TrackEventKind::Meta(midly::MetaMessage::EndOfTrack) });
        smf.tracks.push(track);
    }
    let mut out = Vec::new();
    smf.write_std(&mut out).unwrap();
    out
}

fn note_on(ch: u8, key: u8, vel: u8) -> midly::TrackEventKind<'static> {
    midly::TrackEventKind::Midi { channel: midly::num::u4::new(ch), message: midly::MidiMessage::NoteOn { key: midly::num::u7::new(key), vel: midly::num::u7::new(vel) } }
}
fn note_off(ch: u8, key: u8) -> midly::TrackEventKind<'static> {
    midly::TrackEventKind::Midi { channel: midly::num::u4::new(ch), message: midly::MidiMessage::NoteOff { key: midly::num::u7::new(key), vel: midly::num::u7::new(0) } }
}

#[test]
fn type0_splits_channels_scales_ppq_and_keeps_programs() {
    // PPQ 480: a quarter note is 480 ticks there, 960 here
    let bytes = file(
        midly::Format::SingleTrack,
        480,
        vec![vec![
            (0, midly::TrackEventKind::Meta(midly::MetaMessage::Tempo(midly::num::u24::new(600_000)))),
            (0, midly::TrackEventKind::Midi { channel: midly::num::u4::new(0), message: midly::MidiMessage::ProgramChange { program: midly::num::u7::new(33) } }),
            (0, note_on(0, 36, 100)),
            (0, note_on(9, 42, 90)),
            (240, note_off(9, 42)),
            (240, note_off(0, 36)),
            (480, note_on(0, 36, 100)),
            (480, note_on(0, 36, 0)),
        ]],
    );
    let imp = import(&bytes, "t0").unwrap();
    assert_eq!(imp.tracks.len(), 2, "one project track per MIDI channel");
    let bass = imp.tracks.iter().find(|t| t.midi_channel == Some(0)).unwrap();
    let drums = imp.tracks.iter().find(|t| t.midi_channel == Some(9)).unwrap();
    assert_eq!(bass.program, Some(33));
    assert_eq!(bass.notes[0].tick, Tick(0));
    assert_eq!(bass.notes[0].duration_ticks, Tick(960), "480 ticks at PPQ 480 = one quarter = 960 ticks");
    assert_eq!(bass.notes[1].tick, Tick(1920));
    assert_eq!(bass.notes[1].duration_ticks, Tick(960), "velocity-0 note-on ends the note");
    assert_eq!(drums.notes[0].duration_ticks, Tick(480));
    assert_eq!(imp.tempo[0].us_per_quarter, UsPerQuarter(600_000));
    assert_eq!(imp.length_ticks, Tick(PPQ * 4), "rounded up to a whole bar");
}

#[test]
fn smpte_timing_is_rejected_and_odd_meter_positions_are_snapped_with_warnings() {
    let mut smf = midly::Smf::new(midly::Header::new(midly::Format::SingleTrack, midly::Timing::Timecode(midly::Fps::Fps25, 40)));
    smf.tracks.push(vec![midly::TrackEvent { delta: midly::num::u28::new(0), kind: midly::TrackEventKind::Meta(midly::MetaMessage::EndOfTrack) }]);
    let mut out = Vec::new();
    smf.write_std(&mut out).unwrap();
    assert!(matches!(import(&out, "smpte"), Err(SmfError::SmpteTiming)));
    assert!(matches!(import(b"not midi", "x"), Err(SmfError::Parse(_))));
    // a 3/4 change mid-bar moves to the bar start, with a warning; a late first tempo gets a default at 0
    let bytes = file(
        midly::Format::Parallel,
        960,
        vec![
            vec![
                (1000, midly::TrackEventKind::Meta(midly::MetaMessage::Tempo(midly::num::u24::new(450_000)))),
                (3000, midly::TrackEventKind::Meta(midly::MetaMessage::TimeSignature(3, 2, 24, 8))),
            ],
            vec![(0, note_on(0, 60, 80)), (9600, note_off(0, 60))],
        ],
    );
    let imp = import(&bytes, "warn").unwrap();
    assert_eq!(imp.tempo[0], TempoPoint { tick: Tick::ZERO, us_per_quarter: UsPerQuarter::DEFAULT });
    assert_eq!(imp.tempo[1], TempoPoint { tick: Tick(1000), us_per_quarter: UsPerQuarter(450_000) });
    assert_eq!(imp.meter.len(), 2);
    assert_eq!(imp.meter[1], MeterPoint { tick: Tick(3840), numerator: 3, denominator: 4 }, "snapped to bar 2");
    assert!(imp.warnings.iter().any(|w| w.contains("moved to the bar start")));
    assert!(imp.warnings.iter().any(|w| w.contains("120 BPM assumed")));
    assert_eq!(imp.tracks[0].notes[0].duration_ticks, Tick(9600));
}

#[test]
fn a_pattern_exports_on_its_own_even_when_the_playlist_is_empty() {
    use rawdawg_project::smf::{export_pattern, pattern_note_count, song_note_count};
    let mut p = song();
    p.clips.clear();
    let pid = p.patterns[0].id;
    // nothing is arranged: the song has no notes, the pattern has three
    assert_eq!(song_note_count(&p), 0);
    assert_eq!(pattern_note_count(&p, pid), 3);
    let back = import(&export_pattern(&p, pid).unwrap(), "pat").unwrap();
    let notes: Vec<(i64, u8)> = back.tracks.iter().flat_map(|t| t.notes.iter().map(|n| (n.tick.0, n.pitch))).collect();
    assert_eq!(notes.len(), 3, "every note of the pattern: {notes:?}");
    assert!(notes.contains(&(0, 60)) && notes.contains(&(960, 64)));
    // the pattern plays at the song's starting tempo and meter (the later change is not part of it)
    assert_eq!(back.tempo.len(), 1);
    assert_eq!(back.tempo[0].us_per_quarter, UsPerQuarter(500_000));
    assert_eq!(back.meter.len(), 1);
    assert!(back.tracks.iter().flat_map(|t| &t.controllers).count() >= 2, "its controllers too");
}

#[test]
fn a_range_exports_only_what_starts_inside_it_from_tick_zero_at_the_tempo_in_force() {
    use rawdawg_project::smf::{range_view, song_notes};
    let p = song();
    // the second bar only (the loop repeats the pattern there): its notes from 0, 400 000 µs, 3/4
    let view = range_view(&p, 3840, 7680);
    let mut notes: Vec<(i64, u8)> = song_notes(&view).into_iter().map(|(_, n)| (n.tick.0, n.pitch)).collect();
    notes.sort();
    assert_eq!(notes, vec![(0, 60), (960, 64), (960, 64)], "the bar's notes, shifted to the start");
    assert_eq!(view.tempo.len(), 1);
    assert_eq!((view.tempo[0].tick.0, view.tempo[0].us_per_quarter.0), (0, 400_000));
    assert_eq!((view.meter[0].tick.0, view.meter[0].numerator), (0, 3));
    // a range starting mid-note leaves that note out; the rest of the bar is in
    let mid = range_view(&p, 480, 3840);
    let notes: Vec<i64> = song_notes(&mid).into_iter().map(|(_, n)| n.tick.0).collect();
    assert_eq!(notes.len(), 2, "{notes:?}");
    assert!(notes.iter().all(|t| *t == 480), "{notes:?}");
    // it still exports as a file
    assert!(export_song(&view).is_ok());
}
