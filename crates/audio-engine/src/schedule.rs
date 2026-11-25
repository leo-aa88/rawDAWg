//! Pattern/song note schedule compiled off-thread into frame positions.

use rawdawg_project::{ClipContent, Project};
use rawdawg_transport::TempoMap;
use rawdawg_types::units::{SampleRate, Tick};

/// A scheduled note with absolute project frame positions.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScheduledNote {
    pub on_frame: i64,
    pub off_frame: i64,
    pub channel_index: u16,
    pub pitch: u8,
    pub velocity: f32,
    pub midi_channel: u8,
    /// Deterministic unique id within the schedule (index).
    pub id: u32,
}

/// Immutable compiled schedule, sorted by `on_frame` then deterministic order.
#[derive(Clone, Debug, Default)]
pub struct Schedule {
    pub notes: Vec<ScheduledNote>,
    /// Loop region in frames (pattern length in pattern mode; loop range or song end in song mode).
    pub loop_start_frame: i64,
    pub loop_end_frame: i64,
    pub loop_enabled: bool,
}

impl Schedule {
    /// Pattern mode: the selected pattern repeats with the tempo at tick 0.
    pub fn for_pattern(project: &Project, pattern_id: rawdawg_types::PatternId, tempo: &TempoMap, rate: SampleRate) -> Self {
        let Some(pat) = project.pattern(pattern_id) else { return Self::default() };
        // pattern mode uses the tempo at the playhead start (tick 0 of the pattern)
        let constant = TempoMap::constant(tempo.tempo_at(Tick::ZERO));
        let mut notes = Vec::new();
        for lane in &pat.lanes {
            let Some(ci) = project.channels.iter().position(|c| c.id == lane.channel_id) else { continue };
            for n in &lane.notes {
                let on = constant.tick_to_frame(n.tick, rate);
                let off = constant.tick_to_frame(n.end_tick(), rate).max(on + 1);
                notes.push(ScheduledNote { on_frame: on, off_frame: off, channel_index: ci as u16, pitch: n.pitch, velocity: n.velocity, midi_channel: n.channel, id: 0 });
            }
        }
        finish(&mut notes);
        Self { notes, loop_start_frame: 0, loop_end_frame: constant.tick_to_frame(pat.length_ticks, rate).max(1), loop_enabled: true }
    }

    /// Song mode: pattern clips placed on tracks, using the full tempo map.
    pub fn for_song(project: &Project, tempo: &TempoMap, rate: SampleRate) -> Self {
        let mut notes = Vec::new();
        let mut end_tick = Tick::ZERO;
        for clip in &project.clips {
            if clip.muted {
                continue;
            }
            let track_muted = project.tracks.iter().find(|t| t.id == clip.track_id).is_some_and(|t| t.mute);
            if track_muted {
                continue;
            }
            let clip_end = clip.start_tick + clip.length_ticks;
            if clip_end > end_tick {
                end_tick = clip_end;
            }
            let ClipContent::Pattern { pattern_id, source_offset_ticks, looped } = &clip.content else { continue };
            let Some(pat) = project.pattern(*pattern_id) else { continue };
            let plen = pat.length_ticks.0.max(1);
            // repetitions covering the clip when looped; a single pass otherwise
            let reps = if *looped { ((clip.length_ticks.0 + source_offset_ticks.0 + plen - 1) / plen).max(1) } else { 1 };
            for lane in &pat.lanes {
                let Some(ci) = project.channels.iter().position(|c| c.id == lane.channel_id) else { continue };
                for rep in 0..reps {
                    for n in &lane.notes {
                        let local_on = n.tick.0 + rep * plen - source_offset_ticks.0;
                        if local_on < 0 || local_on >= clip.length_ticks.0 {
                            continue;
                        }
                        let local_off = (n.end_tick().0 + rep * plen - source_offset_ticks.0).min(clip.length_ticks.0);
                        let on_tick = Tick(clip.start_tick.0 + local_on);
                        let off_tick = Tick(clip.start_tick.0 + local_off);
                        let on = tempo.tick_to_frame(on_tick, rate);
                        let off = tempo.tick_to_frame(off_tick, rate).max(on + 1);
                        notes.push(ScheduledNote { on_frame: on, off_frame: off, channel_index: ci as u16, pitch: n.pitch, velocity: n.velocity, midi_channel: n.channel, id: 0 });
                    }
                }
            }
        }
        finish(&mut notes);
        let (ls, le, en) = match project.r#loop {
            Some(l) if l.enabled => (tempo.tick_to_frame(l.start_tick, rate), tempo.tick_to_frame(l.end_tick, rate), true),
            _ => (0, tempo.tick_to_frame(end_tick, rate).max(1), false),
        };
        Self { notes, loop_start_frame: ls, loop_end_frame: le.max(ls + 1), loop_enabled: en }
    }
}

fn finish(notes: &mut [ScheduledNote]) {
    notes.sort_by_key(|n| (n.on_frame, n.channel_index, n.pitch, n.midi_channel));
    for (i, n) in notes.iter_mut().enumerate() {
        n.id = i as u32;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rawdawg_project::Note;
    use rawdawg_types::NoteId;

    #[test]
    fn pattern_schedule_frames_at_120bpm() {
        let mut p = Project::new_default("t", serde_json::json!({}));
        let pid = p.patterns[0].id;
        let lane = &mut p.patterns[0].lanes[0];
        lane.notes.push(Note { id: NoteId::new(), tick: Tick(960), duration_ticks: Tick(480), pitch: 60, velocity: 0.5, channel: 0 });
        lane.notes.push(Note { id: NoteId::new(), tick: Tick(0), duration_ticks: Tick(240), pitch: 64, velocity: 0.5, channel: 0 });
        let s = Schedule::for_pattern(&p, pid, &p.tempo_map().unwrap(), SampleRate(48_000));
        assert_eq!(s.notes.len(), 2);
        assert_eq!(s.notes[0].on_frame, 0);
        assert_eq!(s.notes[0].off_frame, 6000);
        assert_eq!(s.notes[1].on_frame, 24_000);
        assert_eq!(s.notes[1].off_frame, 36_000);
        assert_eq!(s.loop_end_frame, 96_000);
        assert!(s.loop_enabled);
    }
}
