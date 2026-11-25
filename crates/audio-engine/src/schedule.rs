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

/// A scheduled parameter value (automation), absolute project frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScheduledParam {
    pub frame: i64,
    /// Processor slot (see `plan::slot_layout`).
    pub slot: u16,
    pub id: rawdawg_types::ParamId,
    pub value: f32,
}

/// Linear automation segments are sampled every this many frames (the instrument
/// smooths in between); step points are exact.
pub const AUTOMATION_STEP_FRAMES: i64 = 64;
/// Upper bound of generated automation events per schedule.
pub const MAX_AUTOMATION_EVENTS: usize = 1 << 20;

/// Immutable compiled schedule, sorted by `on_frame` then deterministic order.
#[derive(Clone, Debug, Default)]
pub struct Schedule {
    pub notes: Vec<ScheduledNote>,
    /// Automation events sorted by frame (song mode only).
    pub params: Vec<ScheduledParam>,
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
        Self { notes, params: Vec::new(), loop_start_frame: 0, loop_end_frame: constant.tick_to_frame(pat.length_ticks, rate).max(1), loop_enabled: true }
    }

    /// Value of a curve at `tick` (step/linear between points; held outside).
    pub fn curve_value(curve: &rawdawg_project::AutomationCurve, tick: i64) -> Option<f32> {
        let pts = &curve.points;
        if pts.is_empty() {
            return None;
        }
        if tick <= pts[0].tick.0 {
            return Some(pts[0].value);
        }
        let i = pts.partition_point(|p| p.tick.0 <= tick);
        if i >= pts.len() {
            return Some(pts[pts.len() - 1].value);
        }
        let (a, b) = (&pts[i - 1], &pts[i]);
        Some(match b.interpolation {
            rawdawg_project::Interpolation::Step => a.value,
            rawdawg_project::Interpolation::Linear => {
                let span = (b.tick.0 - a.tick.0).max(1) as f32;
                a.value + (b.value - a.value) * ((tick - a.tick.0) as f32 / span)
            }
        })
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
        // automation clips → parameter events (curves in Off mode are skipped)
        let mut params: Vec<ScheduledParam> = Vec::new();
        for clip in &project.clips {
            if clip.muted || project.tracks.iter().find(|t| t.id == clip.track_id).is_some_and(|t| t.mute) {
                continue;
            }
            let ClipContent::Automation { curve_id, source_offset_ticks, looped } = &clip.content else { continue };
            let Some(curve) = project.automation.iter().find(|c| c.id == *curve_id) else { continue };
            if curve.disabled || curve.mode == rawdawg_project::AutomationMode::Off || curve.points.is_empty() {
                continue;
            }
            let Some(ci) = crate::plan::slot_layout(project).iter().position(|i| *i == curve.target.instance_id) else { continue };
            let id = rawdawg_types::ParamId(curve.target.param_id);
            let clip_end = clip.start_tick.0 + clip.length_ticks.0;
            let plen = curve.points.last().map(|p| p.tick.0).unwrap_or(0).max(1);
            let reps = if *looped { ((clip.length_ticks.0 + source_offset_ticks.0 + plen - 1) / plen).max(1) } else { 1 };
            let push = |frame: i64, value: f32, params: &mut Vec<ScheduledParam>| {
                if params.len() < MAX_AUTOMATION_EVENTS {
                    params.push(ScheduledParam { frame, slot: ci as u16, id, value: value.clamp(0.0, 1.0) });
                }
            };
            let frame_of = |tick: i64| tempo.tick_to_frame(Tick(tick), rate);
            // value at the clip start (chase into the curve)
            if let Some(v) = Self::curve_value(curve, source_offset_ticks.0) {
                push(frame_of(clip.start_tick.0), v, &mut params);
            }
            for rep in 0..reps {
                for w in curve.points.windows(2) {
                    let (a, b) = (&w[0], &w[1]);
                    let ta = clip.start_tick.0 + a.tick.0 + rep * plen - source_offset_ticks.0;
                    let tb = clip.start_tick.0 + b.tick.0 + rep * plen - source_offset_ticks.0;
                    if tb <= clip.start_tick.0 || ta >= clip_end {
                        continue;
                    }
                    match b.interpolation {
                        rawdawg_project::Interpolation::Step => {
                            if tb < clip_end && tb > clip.start_tick.0 {
                                push(frame_of(tb), b.value, &mut params);
                            }
                        }
                        rawdawg_project::Interpolation::Linear => {
                            let (fa, fb) = (frame_of(ta), frame_of(tb));
                            let (f_lo, f_hi) = (frame_of(clip.start_tick.0), frame_of(clip_end));
                            let span_frames = (fb - fa).max(1);
                            let step = AUTOMATION_STEP_FRAMES.max(span_frames / 4096);
                            let mut f = fa + step;
                            while f < fb {
                                if f > f_lo && f < f_hi {
                                    let v = a.value + (b.value - a.value) * ((f - fa) as f32 / span_frames as f32);
                                    push(f, v, &mut params);
                                }
                                f += step;
                            }
                            if fb > f_lo && fb < f_hi {
                                push(fb, b.value, &mut params);
                            }
                        }
                    }
                }
            }
        }
        params.sort_by_key(|p| (p.frame, p.slot, p.id.0));
        let (ls, le, en) = match project.r#loop {
            Some(l) if l.enabled => (tempo.tick_to_frame(l.start_tick, rate), tempo.tick_to_frame(l.end_tick, rate), true),
            _ => (0, tempo.tick_to_frame(end_tick, rate).max(1), false),
        };
        Self { notes, params, loop_start_frame: ls, loop_end_frame: le.max(ls + 1), loop_enabled: en }
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
