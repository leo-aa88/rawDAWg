//! Standard MIDI File import (type 0/1) and export (type 1) — IO-05/IO-06.
//!
//! Import keeps tempo, meter, channel/program and supported controller data (CC, pitch
//! bend, channel pressure); PPQ is converted to the project's 960; SMPTE timing is
//! rejected with an actionable error; overlapping equal pitches pair first-on/first-off.
//! Export writes the song arrangement (pattern clips expanded, looped clips unrolled) as
//! one conductor track plus one track per channel at PPQ 960.

use crate::model::*;
use midly::num::{u4, u7, u15, u24, u28};
use midly::{Format, Header, MetaMessage, MidiMessage, Smf, Timing, TrackEvent, TrackEventKind};
use rawdawg_types::units::{PPQ, Tick, UsPerQuarter};
use rawdawg_types::{ControllerEventId, NoteId};
use std::collections::VecDeque;

#[derive(Debug, thiserror::Error)]
pub enum SmfError {
    #[error("not a Standard MIDI File: {0}")]
    Parse(String),
    #[error("SMPTE (timecode) timing is not supported; re-export the file with metrical (PPQ) timing")]
    SmpteTiming,
    #[error("the file has no note events")]
    Empty,
    #[error("could not write MIDI: {0}")]
    Write(String),
}

/// One imported track: notes and controllers in project ticks (PPQ 960).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ImportedTrack {
    pub name: String,
    pub midi_channel: Option<u8>,
    pub program: Option<u8>,
    pub notes: Vec<Note>,
    pub controllers: Vec<ControllerEvent>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MidiImport {
    pub name: String,
    pub tempo: Vec<TempoPoint>,
    pub meter: Vec<MeterPoint>,
    pub tracks: Vec<ImportedTrack>,
    /// Whole bars covering every event.
    pub length_ticks: Tick,
    pub warnings: Vec<String>,
}

fn scale_tick(t: u64, ppq: u64) -> i64 {
    ((t as i128 * PPQ as i128 + (ppq as i128 / 2)) / ppq as i128) as i64
}

/// Parse an SMF. Type 0 files are split into one track per MIDI channel.
pub fn import(bytes: &[u8], name: &str) -> Result<MidiImport, SmfError> {
    // SMPTE timing is unsupported; reject it before the parser sees the division word
    // (midly 0.5.3 negates the frame-rate byte and overflows on 0x80 — found by fuzzing).
    if bytes.len() >= 14 && bytes.starts_with(b"MThd") && bytes[12] & 0x80 != 0 {
        return Err(SmfError::SmpteTiming);
    }
    // the parser is third-party code on untrusted input: contain any other panic as an error
    let smf = match std::panic::catch_unwind(|| Smf::parse(bytes)) {
        Ok(r) => r.map_err(|e| SmfError::Parse(e.to_string()))?,
        Err(_) => return Err(SmfError::Parse("the MIDI parser failed on malformed data".into())),
    };
    let ppq = match smf.header.timing {
        Timing::Metrical(p) => u64::from(p.as_int()).max(1),
        Timing::Timecode(..) => return Err(SmfError::SmpteTiming),
    };
    let mut warnings = Vec::new();
    let mut tempo: Vec<TempoPoint> = Vec::new();
    let mut meter_raw: Vec<(i64, u8, u8)> = Vec::new();
    // per source track: absolute-tick events
    struct Raw {
        name: String,
        events: Vec<(i64, u8, MidiMessage)>,
        program: Option<u8>,
    }
    let mut raws: Vec<Raw> = Vec::new();
    let mut sysex = 0usize;
    for (ti, track) in smf.tracks.iter().enumerate() {
        let mut abs = 0u64;
        let mut raw = Raw { name: String::new(), events: Vec::new(), program: None };
        for ev in track {
            abs += u64::from(ev.delta.as_int());
            let tick = scale_tick(abs, ppq);
            match &ev.kind {
                TrackEventKind::Midi { channel, message } => {
                    if let MidiMessage::ProgramChange { program } = message
                        && raw.program.is_none()
                    {
                        raw.program = Some(program.as_int());
                    }
                    raw.events.push((tick, channel.as_int(), *message));
                }
                TrackEventKind::Meta(MetaMessage::Tempo(us)) => {
                    let us = us.as_int().max(1);
                    if let Some(last) = tempo.last_mut()
                        && last.tick.0 == tick
                    {
                        last.us_per_quarter = UsPerQuarter(us);
                    } else {
                        tempo.push(TempoPoint { tick: Tick(tick), us_per_quarter: UsPerQuarter(us) });
                    }
                }
                TrackEventKind::Meta(MetaMessage::TimeSignature(n, d, _, _)) => meter_raw.push((tick, (*n).max(1), 1u8 << (*d).min(5))),
                TrackEventKind::Meta(MetaMessage::TrackName(b)) => raw.name = String::from_utf8_lossy(b).trim().to_owned(),
                TrackEventKind::SysEx(_) | TrackEventKind::Escape(_) => sysex += 1,
                _ => {}
            }
        }
        if raw.name.is_empty() {
            raw.name = format!("Track {}", ti + 1);
        }
        raws.push(raw);
    }
    if sysex > 0 {
        warnings.push(format!("{sysex} SysEx/escape events ignored"));
    }
    // tempo map: a point at tick 0 is required (default 120 BPM when the file starts later)
    tempo.sort_by_key(|t| t.tick.0);
    if tempo.first().is_none_or(|t| t.tick.0 != 0) {
        tempo.insert(0, TempoPoint { tick: Tick::ZERO, us_per_quarter: UsPerQuarter::DEFAULT });
        if tempo.len() > 1 {
            warnings.push("no tempo at the start: 120 BPM assumed until the first tempo change".into());
        }
    }
    tempo.dedup_by_key(|t| t.tick.0);
    // meter map: changes land on bar boundaries (snapped back with a warning otherwise)
    meter_raw.sort_by_key(|m| m.0);
    let mut meter: Vec<MeterPoint> = vec![MeterPoint { tick: Tick::ZERO, numerator: 4, denominator: 4 }];
    for (tick, n, d) in meter_raw {
        if tick == 0 {
            meter[0] = MeterPoint { tick: Tick::ZERO, numerator: n, denominator: d };
            continue;
        }
        let last = *meter.last().unwrap();
        let bar = i64::from(last.numerator) * (4 * PPQ / i64::from(last.denominator));
        let bars = (tick - last.tick.0) / bar.max(1);
        let snapped = last.tick.0 + bars * bar;
        if snapped != tick {
            warnings.push(format!("time signature {n}/{d} at tick {tick} moved to the bar start at {snapped}"));
        }
        if snapped <= last.tick.0 {
            *meter.last_mut().unwrap() = MeterPoint { tick: last.tick, numerator: n, denominator: d };
        } else {
            meter.push(MeterPoint { tick: Tick(snapped), numerator: n, denominator: d });
        }
    }
    // type 0: split by MIDI channel; otherwise one project track per SMF track
    let mut split: Vec<Raw> = Vec::new();
    if smf.header.format == Format::SingleTrack {
        for raw in raws {
            let mut by_ch: Vec<Option<Raw>> = (0..16).map(|_| None).collect();
            for (tick, ch, msg) in raw.events {
                let slot = by_ch[ch as usize].get_or_insert_with(|| Raw { name: format!("{} ch {}", raw.name, ch + 1), events: Vec::new(), program: None });
                if let MidiMessage::ProgramChange { program } = msg
                    && slot.program.is_none()
                {
                    slot.program = Some(program.as_int());
                }
                slot.events.push((tick, ch, msg));
            }
            split.extend(by_ch.into_iter().flatten());
        }
    } else {
        split = raws;
    }
    let mut tracks = Vec::new();
    let mut end_tick = 0i64;
    for raw in split {
        let mut notes: Vec<Note> = Vec::new();
        let mut controllers = Vec::new();
        // first-on / first-off pairing per (channel, key)
        let mut open: Vec<VecDeque<(i64, f32)>> = (0..16 * 128).map(|_| VecDeque::new()).collect();
        let mut midi_channel = None;
        for (tick, ch, msg) in &raw.events {
            midi_channel.get_or_insert(*ch);
            match msg {
                MidiMessage::NoteOn { key, vel } if vel.as_int() > 0 => open[*ch as usize * 128 + key.as_int() as usize].push_back((*tick, f32::from(vel.as_int()) / 127.0)),
                MidiMessage::NoteOn { key, .. } | MidiMessage::NoteOff { key, .. } => {
                    if let Some((on, vel)) = open[*ch as usize * 128 + key.as_int() as usize].pop_front() {
                        let dur = (*tick - on).max(1);
                        notes.push(Note { id: NoteId::new(), tick: Tick(on), duration_ticks: Tick(dur), pitch: key.as_int(), velocity: vel.max(1.0 / 127.0), channel: *ch });
                        end_tick = end_tick.max(on + dur);
                    }
                }
                MidiMessage::Controller { controller, value } => {
                    controllers.push(ControllerEvent {
                        id: ControllerEventId::new(),
                        tick: Tick(*tick),
                        channel: *ch,
                        kind: ControllerKind::Cc { cc: controller.as_int(), value: f32::from(value.as_int()) / 127.0 },
                    });
                    end_tick = end_tick.max(*tick);
                }
                MidiMessage::PitchBend { bend } => {
                    let v = (f32::from(bend.0.as_int()) - 8192.0) / 8192.0;
                    controllers.push(ControllerEvent {
                        id: ControllerEventId::new(),
                        tick: Tick(*tick),
                        channel: *ch,
                        kind: ControllerKind::PitchBend { value: v.clamp(-1.0, 1.0) },
                    });
                }
                MidiMessage::ChannelAftertouch { vel } => {
                    controllers.push(ControllerEvent {
                        id: ControllerEventId::new(),
                        tick: Tick(*tick),
                        channel: *ch,
                        kind: ControllerKind::ChannelPressure { value: f32::from(vel.as_int()) / 127.0 },
                    });
                }
                _ => {}
            }
        }
        // notes still open at the end close at the last event
        for (slot, q) in open.iter_mut().enumerate() {
            while let Some((on, vel)) = q.pop_front() {
                let dur = (end_tick - on).max(PPQ / 4);
                notes.push(Note { id: NoteId::new(), tick: Tick(on), duration_ticks: Tick(dur), pitch: (slot % 128) as u8, velocity: vel, channel: (slot / 128) as u8 });
                warnings.push(format!("note {} without note-off closed at the end", slot % 128));
            }
        }
        if notes.is_empty() && controllers.is_empty() {
            continue;
        }
        notes.sort_by_key(|n| (n.tick.0, n.pitch));
        tracks.push(ImportedTrack { name: raw.name, midi_channel, program: raw.program, notes, controllers });
    }
    if tracks.iter().all(|t| t.notes.is_empty()) {
        return Err(SmfError::Empty);
    }
    let bar0 = i64::from(meter[0].numerator) * (4 * PPQ / i64::from(meter[0].denominator));
    let length = ((end_tick.max(1) + bar0 - 1) / bar0 * bar0).max(bar0);
    Ok(MidiImport { name: name.to_owned(), tempo, meter, tracks, length_ticks: Tick(length), warnings })
}

/// Notes of the song arrangement in absolute ticks per channel: pattern clips expanded
/// (loops unrolled, offsets applied, muted clips/tracks skipped).
pub fn song_notes(project: &Project) -> Vec<(rawdawg_types::ChannelId, Note)> {
    let mut out = Vec::new();
    for clip in &project.clips {
        if clip.muted || project.tracks.iter().find(|t| t.id == clip.track_id).is_some_and(|t| t.mute) {
            continue;
        }
        let ClipContent::Pattern { pattern_id, source_offset_ticks, looped } = &clip.content else { continue };
        let Some(pat) = project.pattern(*pattern_id) else { continue };
        let plen = pat.length_ticks.0.max(1);
        let reps = if *looped { ((clip.length_ticks.0 + source_offset_ticks.0 + plen - 1) / plen).max(1) } else { 1 };
        for lane in &pat.lanes {
            for rep in 0..reps {
                for n in &lane.notes {
                    let local_on = n.tick.0 + rep * plen - source_offset_ticks.0;
                    if local_on < 0 || local_on >= clip.length_ticks.0 {
                        continue;
                    }
                    let local_off = (n.end_tick().0 + rep * plen - source_offset_ticks.0).min(clip.length_ticks.0);
                    out.push((lane.channel_id, Note { tick: Tick(clip.start_tick.0 + local_on), duration_ticks: Tick((local_off - local_on).max(1)), ..*n }));
                }
            }
        }
    }
    out
}

/// One pattern as the arrangement: the pattern from tick 0 for its length, at the song's
/// starting tempo and meter (pattern mode plays it so).
fn pattern_view(project: &Project, pattern_id: rawdawg_types::PatternId) -> Option<Project> {
    let pat = project.pattern(pattern_id)?;
    let mut view = project.clone();
    view.tempo.truncate(1);
    view.meter.truncate(1);
    let track = crate::model::Track { id: rawdawg_types::TrackId::new(), name: pat.name.clone(), color: None, mute: false, solo: false, height: None, mixer_id: None };
    view.clips = vec![crate::model::Clip {
        id: rawdawg_types::ClipId::new(),
        track_id: track.id,
        start_tick: Tick::ZERO,
        length_ticks: pat.length_ticks,
        muted: false,
        content: ClipContent::Pattern { pattern_id, source_offset_ticks: Tick::ZERO, looped: false },
    }];
    view.tracks = vec![track];
    Some(view)
}

/// Notes the song export would write (muted clips and tracks are skipped).
pub fn song_note_count(project: &Project) -> usize {
    song_notes(project).len()
}

/// Notes a pattern export would write.
pub fn pattern_note_count(project: &Project, pattern_id: rawdawg_types::PatternId) -> usize {
    pattern_view(project, pattern_id).map_or(0, |v| song_notes(&v).len())
}

/// Export one pattern as SMF type 1, as pattern mode plays it.
pub fn export_pattern(project: &Project, pattern_id: rawdawg_types::PatternId) -> Result<Vec<u8>, SmfError> {
    let view = pattern_view(project, pattern_id).ok_or(SmfError::Empty)?;
    // the view lives only for this call: write from it, then hand back owned bytes
    export_song(&view)
}

/// Export the song as SMF type 1 (conductor track + one track per channel, PPQ 960).
pub fn export_song<'a>(project: &'a Project) -> Result<Vec<u8>, SmfError> {
    let mut smf = Smf::new(Header::new(Format::Parallel, Timing::Metrical(u15::new(PPQ as u16))));
    // conductor
    let mut cond: Vec<(i64, u8, TrackEventKind<'a>)> = Vec::new();
    cond.push((0, 0, TrackEventKind::Meta(MetaMessage::TrackName(project.name.as_bytes()))));
    for t in &project.tempo {
        cond.push((t.tick.0, 1, TrackEventKind::Meta(MetaMessage::Tempo(u24::new(t.us_per_quarter.0.min(0xFF_FFFF))))));
    }
    for m in &project.meter {
        let d_pow = m.denominator.max(1).trailing_zeros() as u8;
        cond.push((m.tick.0, 1, TrackEventKind::Meta(MetaMessage::TimeSignature(m.numerator, d_pow, 24, 8))));
    }
    smf.tracks.push(to_track(cond));
    let notes = song_notes(project);
    for ch in &project.channels {
        let mut evs: Vec<(i64, u8, TrackEventKind<'a>)> = vec![(0, 0, TrackEventKind::Meta(MetaMessage::TrackName(ch.name.as_bytes())))];
        for (cid, n) in notes.iter().filter(|(c, _)| *c == ch.id) {
            let _ = cid;
            let key = u7::new(n.pitch.min(127));
            let vel = u7::new(((n.velocity.clamp(0.0, 1.0) * 127.0).round() as u8).max(1));
            let channel = u4::new(n.channel.min(15));
            evs.push((n.tick.0, 3, TrackEventKind::Midi { channel, message: MidiMessage::NoteOn { key, vel } }));
            evs.push((n.end_tick().0, 2, TrackEventKind::Midi { channel, message: MidiMessage::NoteOff { key, vel: u7::new(0) } }));
        }
        // controllers from every clip of this channel's patterns (same expansion as notes)
        for clip in project.clips.iter().filter(|c| !c.muted) {
            let ClipContent::Pattern { pattern_id, source_offset_ticks, looped } = &clip.content else { continue };
            let Some(pat) = project.pattern(*pattern_id) else { continue };
            let plen = pat.length_ticks.0.max(1);
            let reps = if *looped { ((clip.length_ticks.0 + source_offset_ticks.0 + plen - 1) / plen).max(1) } else { 1 };
            for lane in pat.lanes.iter().filter(|l| l.channel_id == ch.id) {
                for rep in 0..reps {
                    for c in &lane.controllers {
                        let local = c.tick.0 + rep * plen - source_offset_ticks.0;
                        if local < 0 || local >= clip.length_ticks.0 {
                            continue;
                        }
                        let channel = u4::new(c.channel.min(15));
                        let message = match c.kind {
                            ControllerKind::Cc { cc, value } => {
                                MidiMessage::Controller { controller: u7::new(cc.min(127)), value: u7::new((value.clamp(0.0, 1.0) * 127.0).round() as u8) }
                            }
                            ControllerKind::PitchBend { value } => MidiMessage::PitchBend {
                                bend: midly::PitchBend(midly::num::u14::new(((value.clamp(-1.0, 1.0) * 8192.0).round() + 8192.0).clamp(0.0, 16383.0) as u16)),
                            },
                            ControllerKind::ChannelPressure { value } => MidiMessage::ChannelAftertouch { vel: u7::new((value.clamp(0.0, 1.0) * 127.0).round() as u8) },
                        };
                        evs.push((clip.start_tick.0 + local, 1, TrackEventKind::Midi { channel, message }));
                    }
                }
            }
        }
        smf.tracks.push(to_track(evs));
    }
    let mut out = Vec::new();
    smf.write_std(&mut out).map_err(|e| SmfError::Write(e.to_string()))?;
    Ok(out)
}

/// Sort by (tick, priority: meta 0/1, note-off 2, note-on 3) and delta-encode, ending with EndOfTrack.
fn to_track<'a>(mut evs: Vec<(i64, u8, TrackEventKind<'a>)>) -> midly::Track<'a> {
    evs.sort_by_key(|(t, pri, _)| (*t, *pri));
    let mut track = Vec::with_capacity(evs.len() + 1);
    let mut last = 0i64;
    for (tick, _, kind) in evs {
        let tick = tick.max(last);
        track.push(TrackEvent { delta: u28::new((tick - last).clamp(0, 0x0FFF_FFFF) as u32), kind });
        last = tick;
    }
    track.push(TrackEvent { delta: u28::new(0), kind: TrackEventKind::Meta(MetaMessage::EndOfTrack) });
    track
}
