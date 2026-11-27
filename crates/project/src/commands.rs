//! Typed, validated, invertible commands and undo history (ARCHITECTURE.md §7).
//!
//! Every persistent edit is a [`Command`]. `apply` validates, mutates and
//! returns the inverse command. Gestures (`begin_gesture` … `commit_gesture`)
//! coalesce many preview applications into one undo entry.

use crate::model::*;
use rawdawg_types::ids::*;
use rawdawg_types::units::{Db, Pan, Tick, UsPerQuarter};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CommandError {
    #[error("unknown {kind} {id}")]
    Unknown { kind: &'static str, id: String },
    #[error("invalid value: {0}")]
    Invalid(String),
    #[error("would leave project invalid: {0}")]
    Validation(String),
    #[error("nothing to undo")]
    NothingToUndo,
    #[error("nothing to redo")]
    NothingToRedo,
    #[error("a gesture is already active")]
    GestureActive,
    #[error("no gesture is active")]
    NoGesture,
}

/// All persistent edits. Each variant is invertible.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Command {
    RenameProject {
        name: String,
    },
    SetTempo {
        index: usize,
        us_per_quarter: UsPerQuarter,
    },
    /// Replace the whole tempo map (validated: sorted, first point at tick 0, ranges).
    SetTempoMap {
        tempo: Vec<TempoPoint>,
    },
    /// Replace the whole meter map (validated).
    SetMeterMap {
        meter: Vec<MeterPoint>,
    },
    AddChannel {
        channel: Channel,
        index: Option<usize>,
    },
    RemoveChannel {
        id: ChannelId,
    },
    RenameChannel {
        id: ChannelId,
        name: String,
    },
    SetChannelGain {
        id: ChannelId,
        gain_db: Db,
    },
    SetChannelPan {
        id: ChannelId,
        pan: Pan,
    },
    SetChannelMute {
        id: ChannelId,
        mute: bool,
    },
    SetChannelSolo {
        id: ChannelId,
        solo: bool,
    },
    MoveChannel {
        id: ChannelId,
        to_index: usize,
    },
    SetChannelMixer {
        id: ChannelId,
        mixer_id: MixerChannelId,
    },
    /// Replace an instance's native/opaque state (synth edits, preset load, parameter commits).
    SetInstanceState {
        instance_id: InstanceId,
        state_version: u32,
        state: Value,
    },
    SetInstanceBypass {
        instance_id: InstanceId,
        bypass: bool,
    },
    SetInstanceMix {
        instance_id: InstanceId,
        mix: f32,
    },
    AddPattern {
        pattern: Pattern,
    },
    RemovePattern {
        id: PatternId,
    },
    RenamePattern {
        id: PatternId,
        name: String,
    },
    SetPatternLength {
        id: PatternId,
        length_ticks: Tick,
        steps: u16,
        step_ticks: Tick,
    },
    SelectPattern {
        id: Option<PatternId>,
    },
    AddNotes {
        pattern_id: PatternId,
        channel_id: ChannelId,
        notes: Vec<Note>,
    },
    RemoveNotes {
        pattern_id: PatternId,
        channel_id: ChannelId,
        note_ids: Vec<NoteId>,
    },
    AddControllerEvents {
        pattern_id: PatternId,
        channel_id: ChannelId,
        events: Vec<ControllerEvent>,
    },
    RemoveControllerEvents {
        pattern_id: PatternId,
        channel_id: ChannelId,
        ids: Vec<ControllerEventId>,
    },
    /// Replace notes by id (move/resize/velocity edits).
    UpdateNotes {
        pattern_id: PatternId,
        channel_id: ChannelId,
        notes: Vec<Note>,
    },
    AddTrack {
        track: Track,
        index: Option<usize>,
    },
    RemoveTrack {
        id: TrackId,
    },
    RenameTrack {
        id: TrackId,
        name: String,
    },
    SetTrackMute {
        id: TrackId,
        mute: bool,
    },
    SetTrackMixer {
        id: TrackId,
        mixer_id: Option<MixerChannelId>,
    },
    SetTrackSolo {
        id: TrackId,
        solo: bool,
    },
    AddClip {
        clip: Clip,
    },
    RemoveClip {
        id: ClipId,
    },
    UpdateClip {
        clip: Clip,
    },
    AddMixerChannel {
        channel: MixerChannel,
        index: Option<usize>,
    },
    RemoveMixerChannel {
        id: MixerChannelId,
    },
    SetMixerGain {
        id: MixerChannelId,
        gain_db: Db,
    },
    RenameMixerChannel {
        id: MixerChannelId,
        name: String,
    },
    SetMixerPan {
        id: MixerChannelId,
        pan: Pan,
    },
    SetMixerMute {
        id: MixerChannelId,
        mute: bool,
    },
    SetMixerSolo {
        id: MixerChannelId,
        solo: bool,
    },
    SetMixerOutput {
        id: MixerChannelId,
        output_id: Option<MixerChannelId>,
    },
    SetMixerSends {
        id: MixerChannelId,
        sends: Vec<Send>,
    },
    AddInsert {
        mixer_id: MixerChannelId,
        instance: Instance,
        index: Option<usize>,
    },
    /// Replace a channel's instrument (native or hosted); the old instance is kept in the
    /// inverse, references to it (automation, MIDI mappings) are orphaned with undo.
    SetInstrument {
        channel_id: ChannelId,
        instance: Instance,
    },
    RemoveInsert {
        mixer_id: MixerChannelId,
        instance_id: InstanceId,
    },
    MoveInsert {
        mixer_id: MixerChannelId,
        instance_id: InstanceId,
        to_index: usize,
    },
    SetMasterProtection {
        enabled: bool,
    },
    SetLoop {
        range: Option<LoopRange>,
    },
    AddAutomationCurve {
        curve: AutomationCurve,
    },
    RemoveAutomationCurve {
        id: CurveId,
    },
    UpdateAutomationCurve {
        curve: AutomationCurve,
    },
    AddAsset {
        asset: Asset,
    },
    /// Inverse of `AddAsset` (not meant to be issued directly).
    RemoveAssetInternal {
        id: AssetId,
        idx: usize,
    },
    SetMidiMappings {
        mappings: Vec<MidiMapping>,
    },
    /// Relink with different content: swap the asset entry and rewrite every reference
    /// (audio clips, instrument and insert states) from `old` to `new.id`.
    ReplaceAsset {
        old: AssetId,
        new: Asset,
    },
    /// Several commands applied atomically as one undo entry.
    Batch {
        commands: Vec<Command>,
    },
}

/// Rewrite every occurrence of an asset id inside a JSON state (sampler/SF2 `asset_id`,
/// convolution `ir_asset`, anything else that stores the hex id as a string).
fn rewrite_asset_refs(v: &mut Value, old: &str, new: &str) -> usize {
    match v {
        Value::String(s) if s == old => {
            *s = new.to_owned();
            1
        }
        Value::Array(items) => items.iter_mut().map(|i| rewrite_asset_refs(i, old, new)).sum(),
        Value::Object(map) => map.values_mut().map(|i| rewrite_asset_refs(i, old, new)).sum(),
        _ => 0,
    }
}

/// After an instance disappears: its automation curves become disabled orphans (kept, with
/// their points) and its MIDI mappings are dropped. Returns the commands that restore both
/// (appended to the removal's inverse).
fn orphan_references(p: &mut Project, instance_id: InstanceId) -> Vec<Command> {
    let mut restores = Vec::new();
    for c in p.automation.iter_mut().filter(|c| c.target.instance_id == instance_id && !c.disabled) {
        restores.push(Command::UpdateAutomationCurve { curve: c.clone() });
        c.disabled = true;
    }
    if p.midi_mappings.iter().any(|m| m.target.instance_id == instance_id) {
        let old = p.midi_mappings.clone();
        p.midi_mappings.retain(|m| m.target.instance_id != instance_id);
        restores.push(Command::SetMidiMappings { mappings: old });
    }
    restores
}

fn unknown<T: std::fmt::Display>(kind: &'static str, id: T) -> CommandError {
    CommandError::Unknown { kind, id: id.to_string() }
}

impl Command {
    /// Apply to the project, returning the inverse. On error the project is unchanged
    /// (the implementation validates before mutating; multi-step batches roll back).
    pub fn apply(self, p: &mut Project) -> Result<Command, CommandError> {
        match self {
            Command::RenameProject { name } => {
                let old = std::mem::replace(&mut p.name, name);
                Ok(Command::RenameProject { name: old })
            }
            Command::SetTempo { index, us_per_quarter } => {
                if !us_per_quarter.is_valid() {
                    return Err(CommandError::Invalid("tempo out of range".into()));
                }
                let t = p.tempo.get_mut(index).ok_or_else(|| unknown("tempo point", index))?;
                let old = std::mem::replace(&mut t.us_per_quarter, us_per_quarter);
                Ok(Command::SetTempo { index, us_per_quarter: old })
            }
            Command::SetTempoMap { tempo } => {
                if tempo.is_empty() || tempo.iter().any(|t| !t.us_per_quarter.is_valid() || t.tick.0 < 0) {
                    return Err(CommandError::Invalid("tempo map".into()));
                }
                let old = std::mem::replace(&mut p.tempo, tempo);
                if let Err(e) = p.tempo_map() {
                    p.tempo = old;
                    return Err(CommandError::Invalid(format!("tempo map: {e}")));
                }
                Ok(Command::SetTempoMap { tempo: old })
            }
            Command::SetMeterMap { meter } => {
                if meter.is_empty() || meter.iter().any(|m| !m.meter().is_valid() || m.tick.0 < 0) {
                    return Err(CommandError::Invalid("meter map".into()));
                }
                let old = std::mem::replace(&mut p.meter, meter);
                if let Err(e) = p.meter_map() {
                    p.meter = old;
                    return Err(CommandError::Invalid(format!("meter map: {e}")));
                }
                Ok(Command::SetMeterMap { meter: old })
            }
            Command::AddChannel { channel, index } => {
                if p.channel(channel.id).is_some() {
                    return Err(CommandError::Invalid("channel id already exists".into()));
                }
                if p.mixer_channel(channel.mixer_id).is_none() {
                    return Err(unknown("mixer channel", channel.mixer_id));
                }
                let id = channel.id;
                let idx = index.unwrap_or(p.channels.len()).min(p.channels.len());
                p.channels.insert(idx, channel);
                // every pattern gets a lane for the channel
                for pat in &mut p.patterns {
                    if !pat.lanes.iter().any(|l| l.channel_id == id) {
                        pat.lanes.push(Lane { channel_id: id, notes: vec![], controllers: vec![] });
                    }
                }
                Ok(Command::RemoveChannel { id })
            }
            Command::RemoveChannel { id } => {
                let idx = p.channels.iter().position(|c| c.id == id).ok_or_else(|| unknown("channel", id))?;
                let channel = p.channels.remove(idx);
                let instance_id = channel.instrument.id;
                let mut orphan_restores = orphan_references(p, instance_id);
                // Lanes with data are preserved in the inverse through a batch.
                let mut restores = Vec::new();
                for pat in &mut p.patterns {
                    if let Some(li) = pat.lanes.iter().position(|l| l.channel_id == id) {
                        let lane = pat.lanes.remove(li);
                        if !lane.notes.is_empty() {
                            restores.push(Command::AddNotes { pattern_id: pat.id, channel_id: id, notes: lane.notes });
                        }
                    }
                }
                let mut cmds = vec![Command::AddChannel { channel, index: Some(idx) }];
                cmds.extend(restores);
                cmds.append(&mut orphan_restores);
                Ok(Command::Batch { commands: cmds })
            }
            Command::RenameChannel { id, name } => {
                let c = p.channel_mut(id).ok_or_else(|| unknown("channel", id))?;
                let old = std::mem::replace(&mut c.name, name);
                Ok(Command::RenameChannel { id, name: old })
            }
            Command::SetChannelGain { id, gain_db } => {
                if !gain_db.is_valid() {
                    return Err(CommandError::Invalid("gain".into()));
                }
                let c = p.channel_mut(id).ok_or_else(|| unknown("channel", id))?;
                let old = std::mem::replace(&mut c.gain_db, gain_db);
                Ok(Command::SetChannelGain { id, gain_db: old })
            }
            Command::SetChannelPan { id, pan } => {
                if !pan.is_valid() {
                    return Err(CommandError::Invalid("pan".into()));
                }
                let c = p.channel_mut(id).ok_or_else(|| unknown("channel", id))?;
                let old = std::mem::replace(&mut c.pan, pan);
                Ok(Command::SetChannelPan { id, pan: old })
            }
            Command::SetChannelMute { id, mute } => {
                let c = p.channel_mut(id).ok_or_else(|| unknown("channel", id))?;
                let old = std::mem::replace(&mut c.mute, mute);
                Ok(Command::SetChannelMute { id, mute: old })
            }
            Command::SetChannelSolo { id, solo } => {
                let c = p.channel_mut(id).ok_or_else(|| unknown("channel", id))?;
                let old = std::mem::replace(&mut c.solo, solo);
                Ok(Command::SetChannelSolo { id, solo: old })
            }
            Command::MoveChannel { id, to_index } => {
                let from = p.channels.iter().position(|c| c.id == id).ok_or_else(|| unknown("channel", id))?;
                let to = to_index.min(p.channels.len() - 1);
                let c = p.channels.remove(from);
                p.channels.insert(to, c);
                Ok(Command::MoveChannel { id, to_index: from })
            }
            Command::SetChannelMixer { id, mixer_id } => {
                if p.mixer_channel(mixer_id).is_none() {
                    return Err(unknown("mixer channel", mixer_id));
                }
                let c = p.channel_mut(id).ok_or_else(|| unknown("channel", id))?;
                let old = std::mem::replace(&mut c.mixer_id, mixer_id);
                Ok(Command::SetChannelMixer { id, mixer_id: old })
            }
            Command::SetInstanceState { instance_id, state_version, state } => {
                if state_version == 0 {
                    return Err(CommandError::Invalid("state_version".into()));
                }
                let i = p.instance_mut(instance_id).ok_or_else(|| unknown("instance", instance_id))?;
                let old_v = std::mem::replace(&mut i.state_version, state_version);
                let old = std::mem::replace(&mut i.state, state);
                Ok(Command::SetInstanceState { instance_id, state_version: old_v, state: old })
            }
            Command::SetInstanceBypass { instance_id, bypass } => {
                let i = p.instance_mut(instance_id).ok_or_else(|| unknown("instance", instance_id))?;
                let old = std::mem::replace(&mut i.bypass, bypass);
                Ok(Command::SetInstanceBypass { instance_id, bypass: old })
            }
            Command::SetInstanceMix { instance_id, mix } => {
                if !mix.is_finite() || !(0.0..=1.0).contains(&mix) {
                    return Err(CommandError::Invalid("mix".into()));
                }
                let i = p.instance_mut(instance_id).ok_or_else(|| unknown("instance", instance_id))?;
                let old = std::mem::replace(&mut i.mix, mix);
                Ok(Command::SetInstanceMix { instance_id, mix: old })
            }
            Command::AddPattern { mut pattern } => {
                if p.pattern(pattern.id).is_some() {
                    return Err(CommandError::Invalid("pattern id already exists".into()));
                }
                if pattern.length_ticks.0 <= 0 || pattern.steps == 0 || pattern.step_ticks.0 <= 0 {
                    return Err(CommandError::Invalid("pattern length".into()));
                }
                for c in &p.channels {
                    if !pattern.lanes.iter().any(|l| l.channel_id == c.id) {
                        pattern.lanes.push(Lane { channel_id: c.id, notes: vec![], controllers: vec![] });
                    }
                }
                let id = pattern.id;
                p.patterns.push(pattern);
                Ok(Command::RemovePattern { id })
            }
            Command::RemovePattern { id } => {
                let idx = p.patterns.iter().position(|x| x.id == id).ok_or_else(|| unknown("pattern", id))?;
                if p.clips.iter().any(|c| matches!(&c.content, ClipContent::Pattern { pattern_id, .. } if *pattern_id == id)) {
                    return Err(CommandError::Invalid("pattern is used by clips; remove them first".into()));
                }
                let pattern = p.patterns.remove(idx);
                let mut cmds = vec![Command::AddPattern { pattern }];
                // its pattern automation goes with it (and comes back on undo)
                let (gone, kept): (Vec<_>, Vec<_>) = std::mem::take(&mut p.automation).into_iter().partition(|c| c.pattern_id == Some(id));
                p.automation = kept;
                cmds.extend(gone.into_iter().map(|curve| Command::AddAutomationCurve { curve }));
                if p.selected_pattern == Some(id) {
                    let new_sel = p.patterns.first().map(|x| x.id);
                    p.selected_pattern = new_sel;
                    cmds.push(Command::SelectPattern { id: Some(id) });
                }
                Ok(Command::Batch { commands: cmds })
            }
            Command::RenamePattern { id, name } => {
                let x = p.pattern_mut(id).ok_or_else(|| unknown("pattern", id))?;
                let old = std::mem::replace(&mut x.name, name);
                Ok(Command::RenamePattern { id, name: old })
            }
            Command::SetPatternLength { id, length_ticks, steps, step_ticks } => {
                if length_ticks.0 <= 0 || steps == 0 || step_ticks.0 <= 0 {
                    return Err(CommandError::Invalid("pattern length".into()));
                }
                let x = p.pattern_mut(id).ok_or_else(|| unknown("pattern", id))?;
                if x.lanes.iter().any(|l| l.notes.iter().any(|n| n.end_tick() > length_ticks)) {
                    return Err(CommandError::Invalid("notes extend past the new length; delete or move them first".into()));
                }
                let old = (x.length_ticks, x.steps, x.step_ticks);
                x.length_ticks = length_ticks;
                x.steps = steps;
                x.step_ticks = step_ticks;
                Ok(Command::SetPatternLength { id, length_ticks: old.0, steps: old.1, step_ticks: old.2 })
            }
            Command::SelectPattern { id } => {
                if let Some(i) = id
                    && p.pattern(i).is_none()
                {
                    return Err(unknown("pattern", i));
                }
                let old = std::mem::replace(&mut p.selected_pattern, id);
                Ok(Command::SelectPattern { id: old })
            }
            Command::AddControllerEvents { pattern_id, channel_id, events } => {
                let length = p.pattern(pattern_id).ok_or_else(|| unknown("pattern", pattern_id))?.length_ticks;
                if p.channel(channel_id).is_none() {
                    return Err(unknown("channel", channel_id));
                }
                for e in &events {
                    let v = match e.kind {
                        ControllerKind::Cc { cc, value } => {
                            if cc > 127 {
                                return Err(CommandError::Invalid("controller number".into()));
                            }
                            value
                        }
                        ControllerKind::PitchBend { value } => value.abs(),
                        ControllerKind::ChannelPressure { value } => value,
                    };
                    if e.tick.0 < 0 || e.tick > length || e.channel > 15 || !v.is_finite() || !(0.0..=1.0).contains(&v) {
                        return Err(CommandError::Invalid(format!("controller event {}", e.id)));
                    }
                }
                let pat = p.pattern_mut(pattern_id).expect("checked");
                let lane = match pat.lanes.iter().position(|l| l.channel_id == channel_id) {
                    Some(i) => &mut pat.lanes[i],
                    None => {
                        pat.lanes.push(Lane { channel_id, notes: vec![], controllers: vec![] });
                        pat.lanes.last_mut().expect("just pushed")
                    }
                };
                let ids: Vec<ControllerEventId> = events.iter().map(|e| e.id).collect();
                if events.iter().any(|e| lane.controllers.iter().any(|x| x.id == e.id)) {
                    return Err(CommandError::Invalid("controller event id already exists".into()));
                }
                lane.controllers.extend(events);
                lane.controllers.sort_by_key(|e| e.tick.0);
                Ok(Command::RemoveControllerEvents { pattern_id, channel_id, ids })
            }
            Command::RemoveControllerEvents { pattern_id, channel_id, ids } => {
                let pat = p.pattern_mut(pattern_id).ok_or_else(|| unknown("pattern", pattern_id))?;
                let lane = pat.lanes.iter_mut().find(|l| l.channel_id == channel_id).ok_or_else(|| unknown("lane", channel_id))?;
                let mut removed = Vec::with_capacity(ids.len());
                lane.controllers.retain(|e| {
                    if ids.contains(&e.id) {
                        removed.push(*e);
                        false
                    } else {
                        true
                    }
                });
                Ok(Command::AddControllerEvents { pattern_id, channel_id, events: removed })
            }
            Command::AddNotes { pattern_id, channel_id, notes } => {
                let length = p.pattern(pattern_id).ok_or_else(|| unknown("pattern", pattern_id))?.length_ticks;
                if p.channel(channel_id).is_none() {
                    return Err(unknown("channel", channel_id));
                }
                for n in &notes {
                    if n.tick.0 < 0 || n.duration_ticks.0 <= 0 || n.pitch > 127 || !n.velocity.is_finite() || n.velocity <= 0.0 || n.velocity > 1.0 || n.channel > 15 {
                        return Err(CommandError::Invalid(format!("note {}", n.id)));
                    }
                    if n.end_tick() > length {
                        return Err(CommandError::Invalid(format!("note {} extends past pattern end", n.id)));
                    }
                }
                let pat = p.pattern_mut(pattern_id).expect("checked");
                let lane = match pat.lanes.iter().position(|l| l.channel_id == channel_id) {
                    Some(i) => &mut pat.lanes[i],
                    None => {
                        pat.lanes.push(Lane { channel_id, notes: vec![], controllers: vec![] });
                        pat.lanes.last_mut().expect("just pushed")
                    }
                };
                let ids: Vec<NoteId> = notes.iter().map(|n| n.id).collect();
                for n in &notes {
                    if lane.notes.iter().any(|e| e.id == n.id) {
                        return Err(CommandError::Invalid(format!("note id {} already exists", n.id)));
                    }
                }
                lane.notes.extend(notes);
                lane.notes.sort_by_key(|n| (n.tick, n.pitch, n.id));
                Ok(Command::RemoveNotes { pattern_id, channel_id, note_ids: ids })
            }
            Command::RemoveNotes { pattern_id, channel_id, note_ids } => {
                let pat = p.pattern_mut(pattern_id).ok_or_else(|| unknown("pattern", pattern_id))?;
                let lane = pat.lanes.iter_mut().find(|l| l.channel_id == channel_id).ok_or_else(|| unknown("lane", channel_id))?;
                let mut removed = Vec::with_capacity(note_ids.len());
                for id in &note_ids {
                    let i = lane.notes.iter().position(|n| n.id == *id).ok_or_else(|| unknown("note", id))?;
                    removed.push(lane.notes.remove(i));
                }
                Ok(Command::AddNotes { pattern_id, channel_id, notes: removed })
            }
            Command::UpdateNotes { pattern_id, channel_id, notes } => {
                let length = p.pattern(pattern_id).ok_or_else(|| unknown("pattern", pattern_id))?.length_ticks;
                for n in &notes {
                    if n.tick.0 < 0 || n.duration_ticks.0 <= 0 || n.pitch > 127 || !n.velocity.is_finite() || n.velocity <= 0.0 || n.velocity > 1.0 || n.end_tick() > length {
                        return Err(CommandError::Invalid(format!("note {}", n.id)));
                    }
                }
                let pat = p.pattern_mut(pattern_id).expect("checked");
                let lane = pat.lanes.iter_mut().find(|l| l.channel_id == channel_id).ok_or_else(|| unknown("lane", channel_id))?;
                let mut old = Vec::with_capacity(notes.len());
                for n in &notes {
                    let slot = lane.notes.iter_mut().find(|e| e.id == n.id).ok_or_else(|| unknown("note", n.id))?;
                    old.push(std::mem::replace(slot, *n));
                }
                lane.notes.sort_by_key(|n| (n.tick, n.pitch, n.id));
                Ok(Command::UpdateNotes { pattern_id, channel_id, notes: old })
            }
            Command::AddTrack { track, index } => {
                if p.tracks.iter().any(|t| t.id == track.id) {
                    return Err(CommandError::Invalid("track id already exists".into()));
                }
                let id = track.id;
                let idx = index.unwrap_or(p.tracks.len()).min(p.tracks.len());
                p.tracks.insert(idx, track);
                Ok(Command::RemoveTrack { id })
            }
            Command::RemoveTrack { id } => {
                let idx = p.tracks.iter().position(|t| t.id == id).ok_or_else(|| unknown("track", id))?;
                let clips: Vec<Clip> = p.clips.iter().filter(|c| c.track_id == id).cloned().collect();
                p.clips.retain(|c| c.track_id != id);
                let track = p.tracks.remove(idx);
                let mut cmds = vec![Command::AddTrack { track, index: Some(idx) }];
                cmds.extend(clips.into_iter().map(|clip| Command::AddClip { clip }));
                Ok(Command::Batch { commands: cmds })
            }
            Command::RenameTrack { id, name } => {
                let t = p.tracks.iter_mut().find(|t| t.id == id).ok_or_else(|| unknown("track", id))?;
                let old = std::mem::replace(&mut t.name, name);
                Ok(Command::RenameTrack { id, name: old })
            }
            Command::SetTrackMute { id, mute } => {
                let t = p.tracks.iter_mut().find(|t| t.id == id).ok_or_else(|| unknown("track", id))?;
                let old = std::mem::replace(&mut t.mute, mute);
                Ok(Command::SetTrackMute { id, mute: old })
            }
            Command::SetTrackMixer { id, mixer_id } => {
                if let Some(m) = mixer_id
                    && !p.mixer.channels.iter().any(|c| c.id == m)
                {
                    return Err(unknown("mixer channel", m));
                }
                let t = p.tracks.iter_mut().find(|t| t.id == id).ok_or_else(|| unknown("track", id))?;
                let old = std::mem::replace(&mut t.mixer_id, mixer_id);
                Ok(Command::SetTrackMixer { id, mixer_id: old })
            }
            Command::SetTrackSolo { id, solo } => {
                let t = p.tracks.iter_mut().find(|t| t.id == id).ok_or_else(|| unknown("track", id))?;
                let old = std::mem::replace(&mut t.solo, solo);
                Ok(Command::SetTrackSolo { id, solo: old })
            }
            Command::AddClip { clip } => {
                check_clip(p, &clip)?;
                if p.clips.iter().any(|c| c.id == clip.id) {
                    return Err(CommandError::Invalid("clip id already exists".into()));
                }
                let id = clip.id;
                p.clips.push(clip);
                Ok(Command::RemoveClip { id })
            }
            Command::RemoveClip { id } => {
                let idx = p.clips.iter().position(|c| c.id == id).ok_or_else(|| unknown("clip", id))?;
                let clip = p.clips.remove(idx);
                Ok(Command::AddClip { clip })
            }
            Command::UpdateClip { clip } => {
                check_clip(p, &clip)?;
                let slot = p.clips.iter_mut().find(|c| c.id == clip.id).ok_or_else(|| unknown("clip", clip.id))?;
                let old = std::mem::replace(slot, clip);
                Ok(Command::UpdateClip { clip: old })
            }
            Command::RenameMixerChannel { id, name } => {
                if name.trim().is_empty() {
                    return Err(CommandError::Invalid("empty name".into()));
                }
                let m = p.mixer_channel_mut(id).ok_or_else(|| unknown("mixer channel", id))?;
                let old = std::mem::replace(&mut m.name, name);
                Ok(Command::RenameMixerChannel { id, name: old })
            }
            Command::AddMixerChannel { channel, index } => {
                if p.mixer_channel(channel.id).is_some() {
                    return Err(CommandError::Invalid("mixer channel id already exists".into()));
                }
                let id = channel.id;
                let idx = index.unwrap_or(p.mixer.channels.len()).min(p.mixer.channels.len());
                p.mixer.channels.insert(idx, channel);
                if let Err(e) = crate::validate::validate(p) {
                    p.mixer.channels.remove(idx);
                    return Err(CommandError::Validation(e.to_string()));
                }
                Ok(Command::RemoveMixerChannel { id })
            }
            Command::RemoveMixerChannel { id } => {
                if id == p.mixer.master_id {
                    return Err(CommandError::Invalid("the master channel cannot be removed".into()));
                }
                let idx = p.mixer.channels.iter().position(|c| c.id == id).ok_or_else(|| unknown("mixer channel", id))?;
                if p.channels.iter().any(|c| c.mixer_id == id)
                    || p.tracks.iter().any(|t| t.mixer_id == Some(id))
                    || p.mixer.channels.iter().any(|m| m.output_id == Some(id) || m.sends.iter().any(|s| s.target_id == id))
                {
                    return Err(CommandError::Invalid("mixer channel is still a routing target; reroute first".into()));
                }
                let channel = p.mixer.channels.remove(idx);
                let inserts: Vec<InstanceId> = channel.inserts.iter().map(|i| i.id).collect();
                // each insert's restores snapshot what was left after the previous insert went:
                // apply the latest first, so the first (complete) MIDI mapping list wins
                let mut restores = Vec::new();
                for i in inserts {
                    restores.push(orphan_references(p, i));
                }
                let mut cmds = vec![Command::AddMixerChannel { channel, index: Some(idx) }];
                for mut r in restores.into_iter().rev() {
                    cmds.append(&mut r);
                }
                if cmds.len() == 1 { Ok(cmds.pop().unwrap()) } else { Ok(Command::Batch { commands: cmds }) }
            }
            Command::SetMixerGain { id, gain_db } => {
                if !gain_db.is_valid() {
                    return Err(CommandError::Invalid("gain".into()));
                }
                let m = p.mixer_channel_mut(id).ok_or_else(|| unknown("mixer channel", id))?;
                let old = std::mem::replace(&mut m.gain_db, gain_db);
                Ok(Command::SetMixerGain { id, gain_db: old })
            }
            Command::SetMixerPan { id, pan } => {
                if !pan.is_valid() {
                    return Err(CommandError::Invalid("pan".into()));
                }
                let m = p.mixer_channel_mut(id).ok_or_else(|| unknown("mixer channel", id))?;
                let old = std::mem::replace(&mut m.pan, pan);
                Ok(Command::SetMixerPan { id, pan: old })
            }
            Command::SetMixerMute { id, mute } => {
                let m = p.mixer_channel_mut(id).ok_or_else(|| unknown("mixer channel", id))?;
                let old = std::mem::replace(&mut m.mute, mute);
                Ok(Command::SetMixerMute { id, mute: old })
            }
            Command::SetMixerSolo { id, solo } => {
                let m = p.mixer_channel_mut(id).ok_or_else(|| unknown("mixer channel", id))?;
                let old = std::mem::replace(&mut m.solo, solo);
                Ok(Command::SetMixerSolo { id, solo: old })
            }
            Command::SetMixerOutput { id, output_id } => {
                let m = p.mixer_channel_mut(id).ok_or_else(|| unknown("mixer channel", id))?;
                let old = std::mem::replace(&mut m.output_id, output_id);
                if let Err(e) = crate::validate::validate(p) {
                    p.mixer_channel_mut(id).expect("exists").output_id = old;
                    return Err(CommandError::Validation(e.to_string()));
                }
                Ok(Command::SetMixerOutput { id, output_id: old })
            }
            Command::SetMixerSends { id, sends } => {
                let m = p.mixer_channel_mut(id).ok_or_else(|| unknown("mixer channel", id))?;
                let old = std::mem::replace(&mut m.sends, sends);
                if let Err(e) = crate::validate::validate(p) {
                    p.mixer_channel_mut(id).expect("exists").sends = old;
                    return Err(CommandError::Validation(e.to_string()));
                }
                Ok(Command::SetMixerSends { id, sends: old })
            }
            Command::AddInsert { mixer_id, instance, index } => {
                if p.instance(instance.id).is_some() {
                    return Err(CommandError::Invalid("instance id already exists".into()));
                }
                let m = p.mixer_channel_mut(mixer_id).ok_or_else(|| unknown("mixer channel", mixer_id))?;
                let id = instance.id;
                let idx = index.unwrap_or(m.inserts.len()).min(m.inserts.len());
                m.inserts.insert(idx, instance);
                Ok(Command::RemoveInsert { mixer_id, instance_id: id })
            }
            Command::SetInstrument { channel_id, instance } => {
                if instance.id != p.channel(channel_id).map(|c| c.instrument.id).unwrap_or(instance.id) && p.instance(instance.id).is_some() {
                    return Err(CommandError::Invalid("instance id already exists".into()));
                }
                let c = p.channels.iter_mut().find(|c| c.id == channel_id).ok_or_else(|| unknown("channel", channel_id))?;
                let old = std::mem::replace(&mut c.instrument, instance);
                let old_id = old.id;
                let mut cmds = vec![Command::SetInstrument { channel_id, instance: old }];
                if old_id != p.channel(channel_id).map(|c| c.instrument.id).unwrap_or(old_id) {
                    cmds.append(&mut orphan_references(p, old_id));
                }
                if cmds.len() == 1 { Ok(cmds.pop().unwrap()) } else { Ok(Command::Batch { commands: cmds }) }
            }
            Command::RemoveInsert { mixer_id, instance_id } => {
                let m = p.mixer_channel_mut(mixer_id).ok_or_else(|| unknown("mixer channel", mixer_id))?;
                let idx = m.inserts.iter().position(|i| i.id == instance_id).ok_or_else(|| unknown("insert", instance_id))?;
                let instance = m.inserts.remove(idx);
                let mut cmds = vec![Command::AddInsert { mixer_id, instance, index: Some(idx) }];
                cmds.append(&mut orphan_references(p, instance_id));
                if cmds.len() == 1 { Ok(cmds.pop().unwrap()) } else { Ok(Command::Batch { commands: cmds }) }
            }
            Command::MoveInsert { mixer_id, instance_id, to_index } => {
                let m = p.mixer_channel_mut(mixer_id).ok_or_else(|| unknown("mixer channel", mixer_id))?;
                let from = m.inserts.iter().position(|i| i.id == instance_id).ok_or_else(|| unknown("insert", instance_id))?;
                let to = to_index.min(m.inserts.len() - 1);
                let ins = m.inserts.remove(from);
                m.inserts.insert(to, ins);
                Ok(Command::MoveInsert { mixer_id, instance_id, to_index: from })
            }
            Command::SetMasterProtection { enabled } => {
                let old = std::mem::replace(&mut p.mixer.master_protection, enabled);
                Ok(Command::SetMasterProtection { enabled: old })
            }
            Command::SetLoop { range } => {
                if let Some(r) = &range
                    && (r.start_tick.0 < 0 || r.end_tick <= r.start_tick)
                {
                    return Err(CommandError::Invalid("loop range".into()));
                }
                let old = std::mem::replace(&mut p.r#loop, range);
                Ok(Command::SetLoop { range: old })
            }
            Command::AddAutomationCurve { curve } => {
                if p.automation.iter().any(|c| c.id == curve.id) {
                    return Err(CommandError::Invalid("curve id already exists".into()));
                }
                check_curve(&curve)?;
                let id = curve.id;
                p.automation.push(curve);
                Ok(Command::RemoveAutomationCurve { id })
            }
            Command::RemoveAutomationCurve { id } => {
                if p.clips.iter().any(|c| matches!(&c.content, ClipContent::Automation { curve_id, .. } if *curve_id == id)) {
                    return Err(CommandError::Invalid("curve is used by clips; remove them first".into()));
                }
                let idx = p.automation.iter().position(|c| c.id == id).ok_or_else(|| unknown("automation curve", id))?;
                let curve = p.automation.remove(idx);
                Ok(Command::AddAutomationCurve { curve })
            }
            Command::UpdateAutomationCurve { curve } => {
                check_curve(&curve)?;
                let slot = p.automation.iter_mut().find(|c| c.id == curve.id).ok_or_else(|| unknown("automation curve", curve.id))?;
                let old = std::mem::replace(slot, curve);
                Ok(Command::UpdateAutomationCurve { curve: old })
            }
            Command::AddAsset { asset } => {
                if p.assets.iter().any(|a| a.id == asset.id) {
                    // Content-addressed: adding the same asset twice is a no-op with no-op inverse.
                    return Ok(Command::Batch { commands: vec![] });
                }
                if !asset.id.is_valid() || !crate::validate::is_safe_relative_path(&asset.path) {
                    return Err(CommandError::Invalid("asset".into()));
                }
                let id = asset.id.clone();
                p.assets.push(asset);
                let idx = p.assets.len() - 1;
                // inverse: remove by position (assets are append-only otherwise)
                Ok(Command::Batch { commands: vec![Command::RemoveAssetInternal { id, idx }] })
            }
            Command::RemoveAssetInternal { id, idx } => {
                if p.assets.get(idx).is_some_and(|a| a.id == id) {
                    let asset = p.assets.remove(idx);
                    Ok(Command::AddAsset { asset })
                } else {
                    Err(unknown("asset", id))
                }
            }
            Command::SetMidiMappings { mappings } => {
                let old = std::mem::replace(&mut p.midi_mappings, mappings);
                Ok(Command::SetMidiMappings { mappings: old })
            }
            Command::ReplaceAsset { old, new } => {
                if !new.id.is_valid() || !crate::validate::is_safe_relative_path(&new.path) {
                    return Err(CommandError::Invalid("asset".into()));
                }
                let idx = p.assets.iter().position(|a| a.id == old).ok_or_else(|| unknown("asset", &old))?;
                let previous = std::mem::replace(&mut p.assets[idx], new.clone());
                let (o, n) = (old.to_string(), new.id.to_string());
                for c in p.clips.iter_mut() {
                    if let ClipContent::Audio { asset_id, .. } = &mut c.content
                        && *asset_id == old
                    {
                        *asset_id = new.id.clone();
                    }
                }
                for ch in p.channels.iter_mut() {
                    rewrite_asset_refs(&mut ch.instrument.state, &o, &n);
                }
                for m in p.mixer.channels.iter_mut() {
                    for ins in m.inserts.iter_mut() {
                        rewrite_asset_refs(&mut ins.state, &o, &n);
                    }
                }
                Ok(Command::ReplaceAsset { old: new.id.clone(), new: previous })
            }
            Command::Batch { commands } => {
                let mut inverses = Vec::with_capacity(commands.len());
                for c in commands {
                    match c.apply(p) {
                        Ok(inv) => inverses.push(inv),
                        Err(e) => {
                            // roll back what was applied
                            for inv in inverses.into_iter().rev() {
                                let _ = inv.apply(p);
                            }
                            return Err(e);
                        }
                    }
                }
                inverses.reverse();
                Ok(Command::Batch { commands: inverses })
            }
        }
    }

    pub fn is_noop(&self) -> bool {
        matches!(self, Command::Batch { commands } if commands.is_empty())
    }

    /// Short label for the Edit menu.
    pub fn label(&self) -> &'static str {
        match self {
            Command::RenameProject { .. } => "Rename project",
            Command::SetTempo { .. } => "Change tempo",
            Command::SetTempoMap { .. } => "Edit tempo map",
            Command::AddControllerEvents { .. } => "Record controllers",
            Command::RemoveControllerEvents { .. } => "Remove controllers",
            Command::SetMeterMap { .. } => "Edit time signatures",
            Command::AddChannel { .. } => "Add channel",
            Command::RemoveChannel { .. } => "Delete channel",
            Command::RenameChannel { .. } => "Rename channel",
            Command::SetChannelGain { .. } => "Channel gain",
            Command::SetChannelPan { .. } => "Channel pan",
            Command::SetChannelMute { .. } => "Mute channel",
            Command::SetChannelSolo { .. } => "Solo channel",
            Command::MoveChannel { .. } => "Move channel",
            Command::SetChannelMixer { .. } => "Route channel",
            Command::SetInstanceState { .. } => "Edit sound",
            Command::SetInstanceBypass { .. } => "Bypass",
            Command::RenameMixerChannel { .. } => "Rename mixer channel",
            Command::SetInstanceMix { .. } => "Mix",
            Command::AddPattern { .. } => "Add pattern",
            Command::RemovePattern { .. } => "Delete pattern",
            Command::RenamePattern { .. } => "Rename pattern",
            Command::SetPatternLength { .. } => "Pattern length",
            Command::SelectPattern { .. } => "Select pattern",
            Command::AddNotes { .. } => "Add notes",
            Command::RemoveNotes { .. } => "Delete notes",
            Command::UpdateNotes { .. } => "Edit notes",
            Command::AddTrack { .. } => "Add track",
            Command::RemoveTrack { .. } => "Delete track",
            Command::RenameTrack { .. } => "Rename track",
            Command::SetTrackMixer { .. } => "Route track",
            Command::SetTrackMute { .. } => "Mute track",
            Command::SetTrackSolo { .. } => "Solo track",
            Command::AddClip { .. } => "Add clip",
            Command::RemoveClip { .. } => "Delete clip",
            Command::UpdateClip { .. } => "Edit clip",
            Command::AddMixerChannel { .. } => "Add mixer channel",
            Command::RemoveMixerChannel { .. } => "Delete mixer channel",
            Command::SetMixerGain { .. } => "Mixer gain",
            Command::SetMixerPan { .. } => "Mixer pan",
            Command::SetMixerMute { .. } => "Mixer mute",
            Command::SetMixerSolo { .. } => "Mixer solo",
            Command::SetMixerOutput { .. } => "Mixer routing",
            Command::SetMixerSends { .. } => "Sends",
            Command::AddInsert { .. } => "Add effect",
            Command::SetInstrument { .. } => "Change instrument",
            Command::RemoveInsert { .. } => "Delete effect",
            Command::MoveInsert { .. } => "Move effect",
            Command::SetMasterProtection { .. } => "Master protection",
            Command::SetLoop { .. } => "Loop range",
            Command::AddAutomationCurve { .. } => "Add automation",
            Command::RemoveAutomationCurve { .. } => "Delete automation",
            Command::UpdateAutomationCurve { .. } => "Edit automation",
            Command::AddAsset { .. } => "Import asset",
            Command::RemoveAssetInternal { .. } => "Remove asset",
            Command::SetMidiMappings { .. } => "MIDI mappings",
            Command::ReplaceAsset { .. } => "Relink asset",
            // a batch is named by what it does to the arrangement's structure, if anything
            Command::Batch { commands } => {
                let has = |f: fn(&Command) -> bool| commands.iter().any(f);
                if has(|c| matches!(c, Command::RemoveChannel { .. })) {
                    "Delete channel"
                } else if has(|c| matches!(c, Command::AddChannel { .. })) {
                    "Add channel"
                } else if has(|c| matches!(c, Command::RemoveMixerChannel { .. })) {
                    "Delete mixer channel"
                } else if has(|c| matches!(c, Command::AddMixerChannel { .. })) {
                    "Add mixer channel"
                } else {
                    "Edit"
                }
            }
        }
    }
}

fn check_clip(p: &Project, clip: &Clip) -> Result<(), CommandError> {
    if !p.tracks.iter().any(|t| t.id == clip.track_id) {
        return Err(unknown("track", clip.track_id));
    }
    if clip.start_tick.0 < 0 || clip.length_ticks.0 <= 0 {
        return Err(CommandError::Invalid("clip position".into()));
    }
    match &clip.content {
        ClipContent::Pattern { pattern_id, .. } => {
            if p.pattern(*pattern_id).is_none() {
                return Err(unknown("pattern", pattern_id));
            }
        }
        ClipContent::Automation { curve_id, .. } => {
            if !p.automation.iter().any(|c| c.id == *curve_id) {
                return Err(unknown("automation curve", curve_id));
            }
        }
        ClipContent::Audio { source_length_frames, playback_rate, .. } => {
            if *source_length_frames == 0 || !playback_rate.is_finite() || *playback_rate <= 0.0 {
                return Err(CommandError::Invalid("audio clip".into()));
            }
        }
    }
    Ok(())
}

fn check_curve(c: &AutomationCurve) -> Result<(), CommandError> {
    let mut last = i64::MIN;
    for pt in &c.points {
        if pt.tick.0 < 0 || pt.tick.0 < last || !pt.value.is_finite() || !(0.0..=1.0).contains(&pt.value) {
            return Err(CommandError::Invalid("automation point".into()));
        }
        last = pt.tick.0;
    }
    Ok(())
}

/// The highest revision a history starts from; billions of edits above it still fit in a u64.
pub const MAX_REVISION: u64 = u64::MAX / 2;

/// Undo/redo history with gesture coalescing and revision tracking.
#[derive(Debug, Default)]
pub struct History {
    undo: Vec<(Command, &'static str)>,
    redo: Vec<(Command, &'static str)>,
    /// Inverse that restores the pre-gesture state while a gesture is active.
    gesture: Option<(Command, &'static str)>,
    revision: u64,
    saved_revision: u64,
    max_depth: usize,
}

impl History {
    pub fn new() -> Self {
        Self { max_depth: 1000, ..Default::default() }
    }

    /// A history for a project opened at `revision`: clean, and every edit numbered above it,
    /// so an autosave of this session is newer than the saved manifest (revisions used to restart
    /// at 0 on open, and a crash in a reopened song was never offered for recovery).
    /// A damaged or hand-edited manifest's huge revision is clamped (to `MAX_REVISION`), so
    /// numbering edits can never overflow.
    pub fn continuing_from(revision: u64) -> Self {
        let revision = revision.min(MAX_REVISION);
        Self { revision, saved_revision: revision, max_depth: 1000, ..Default::default() }
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }
    pub fn is_dirty(&self) -> bool {
        self.revision != self.saved_revision
    }
    /// Treat the current state as unsaved (e.g. after loading an autosave into memory).
    pub fn mark_dirty(&mut self) {
        self.saved_revision = u64::MAX;
    }
    pub fn mark_saved(&mut self) {
        self.saved_revision = self.revision;
    }
    /// Record that a save completed for `revision` (clears dirty only if still current).
    pub fn mark_saved_at(&mut self, revision: u64) {
        self.saved_revision = revision;
    }
    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty() && self.gesture.is_none()
    }
    /// Number of undo entries (tests: "one gesture = one entry").
    pub fn undo_len(&self) -> usize {
        self.undo.len()
    }
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty() && self.gesture.is_none()
    }
    pub fn undo_label(&self) -> Option<&'static str> {
        self.undo.last().map(|(_, l)| *l)
    }
    pub fn redo_label(&self) -> Option<&'static str> {
        self.redo.last().map(|(_, l)| *l)
    }
    pub fn gesture_active(&self) -> bool {
        self.gesture.is_some()
    }

    /// Apply a command as one undo entry.
    pub fn apply(&mut self, p: &mut Project, cmd: Command) -> Result<(), CommandError> {
        if self.gesture.is_some() {
            return Err(CommandError::GestureActive);
        }
        let label = cmd.label();
        let inverse = cmd.apply(p)?;
        if inverse.is_noop() {
            return Ok(());
        }
        self.push_undo(inverse, label);
        self.redo.clear();
        self.revision += 1;
        Ok(())
    }

    fn push_undo(&mut self, inverse: Command, label: &'static str) {
        self.undo.push((inverse, label));
        if self.undo.len() > self.max_depth {
            self.undo.remove(0);
        }
    }

    /// Start a gesture: subsequent `preview` calls mutate without creating entries.
    pub fn begin_gesture(&mut self) -> Result<(), CommandError> {
        if self.gesture.is_some() {
            return Err(CommandError::GestureActive);
        }
        self.gesture = Some((Command::Batch { commands: vec![] }, "Edit"));
        Ok(())
    }

    /// Apply a preview step inside a gesture (inverse accumulated).
    pub fn preview(&mut self, p: &mut Project, cmd: Command) -> Result<(), CommandError> {
        let Some((acc, label)) = self.gesture.as_mut() else { return Err(CommandError::NoGesture) };
        *label = cmd.label();
        let inv = cmd.apply(p)?;
        if let Command::Batch { commands } = acc {
            // inverses must run in reverse order of application
            commands.insert(0, inv);
        }
        Ok(())
    }

    /// Commit the gesture as one undo entry (no entry if nothing changed).
    pub fn commit_gesture(&mut self) -> Result<(), CommandError> {
        let Some((inverse, label)) = self.gesture.take() else { return Err(CommandError::NoGesture) };
        if inverse.is_noop() {
            return Ok(());
        }
        self.push_undo(inverse, label);
        self.redo.clear();
        self.revision += 1;
        Ok(())
    }

    /// Cancel the gesture, restoring the pre-gesture state exactly.
    pub fn cancel_gesture(&mut self, p: &mut Project) -> Result<(), CommandError> {
        let Some((inverse, _)) = self.gesture.take() else { return Err(CommandError::NoGesture) };
        inverse.apply(p)?;
        Ok(())
    }

    pub fn undo(&mut self, p: &mut Project) -> Result<&'static str, CommandError> {
        if self.gesture.is_some() {
            return Err(CommandError::GestureActive);
        }
        let (inverse, label) = self.undo.pop().ok_or(CommandError::NothingToUndo)?;
        let redo = inverse.apply(p)?;
        self.redo.push((redo, label));
        self.revision += 1;
        Ok(label)
    }

    pub fn redo(&mut self, p: &mut Project) -> Result<&'static str, CommandError> {
        if self.gesture.is_some() {
            return Err(CommandError::GestureActive);
        }
        let (cmd, label) = self.redo.pop().ok_or(CommandError::NothingToRedo)?;
        let inverse = cmd.apply(p)?;
        self.undo.push((inverse, label));
        self.revision += 1;
        Ok(label)
    }

    pub fn clear(&mut self) {
        self.undo.clear();
        self.redo.clear();
        self.gesture = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proj() -> Project {
        Project::new_default("t", serde_json::json!({"x": 1}))
    }

    fn note(tick: i64, pitch: u8) -> Note {
        Note { id: NoteId::new(), tick: Tick(tick), duration_ticks: Tick(240), pitch, velocity: 0.8, channel: 0 }
    }

    #[test]
    fn add_remove_notes_round_trip_exactly() {
        let mut p = proj();
        let original = p.clone();
        let mut h = History::new();
        let pid = p.patterns[0].id;
        let cid = p.channels[0].id;
        let notes = vec![note(0, 60), note(960, 64), note(1920, 67)];
        h.apply(&mut p, Command::AddNotes { pattern_id: pid, channel_id: cid, notes: notes.clone() }).unwrap();
        assert_eq!(p.patterns[0].lanes[0].notes.len(), 3);
        assert!(h.is_dirty());
        assert_eq!(h.undo_label(), Some("Add notes"));
        h.undo(&mut p).unwrap();
        assert_eq!(p, original);
        h.redo(&mut p).unwrap();
        assert_eq!(p.patterns[0].lanes[0].notes.len(), 3);
        h.apply(&mut p, Command::RemoveNotes { pattern_id: pid, channel_id: cid, note_ids: vec![notes[1].id] }).unwrap();
        assert_eq!(p.patterns[0].lanes[0].notes.len(), 2);
        h.undo(&mut p).unwrap();
        assert_eq!(p.patterns[0].lanes[0].notes.len(), 3);
        assert!(p.patterns[0].lanes[0].notes.iter().any(|n| n.id == notes[1].id));
    }

    #[test]
    fn invalid_commands_leave_project_untouched() {
        let mut p = proj();
        let original = p.clone();
        let mut h = History::new();
        let pid = p.patterns[0].id;
        let cid = p.channels[0].id;
        let bad = Command::AddNotes { pattern_id: pid, channel_id: cid, notes: vec![note(0, 60), note(3800, 60)] };
        assert!(h.apply(&mut p, bad).is_err());
        assert_eq!(p, original);
        assert!(!h.is_dirty());
        assert!(h.apply(&mut p, Command::SetChannelPan { id: cid, pan: Pan(2.0) }).is_err());
        assert!(h.apply(&mut p, Command::RemoveChannel { id: ChannelId::new() }).is_err());
        assert_eq!(p, original);
    }

    #[test]
    fn gesture_produces_single_undo_entry_and_cancel_restores() {
        let mut p = proj();
        let original = p.clone();
        let mut h = History::new();
        let cid = p.channels[0].id;
        h.begin_gesture().unwrap();
        for i in 0..50 {
            h.preview(&mut p, Command::SetChannelGain { id: cid, gain_db: Db(-f64::from(i)) }).unwrap();
        }
        assert!(h.gesture_active());
        assert!(!h.can_undo());
        h.commit_gesture().unwrap();
        assert_eq!(p.channels[0].gain_db, Db(-49.0));
        assert_eq!(h.revision(), 1);
        h.undo(&mut p).unwrap();
        assert_eq!(p, original);
        assert!(!h.can_undo());
        h.begin_gesture().unwrap();
        h.preview(&mut p, Command::SetChannelGain { id: cid, gain_db: Db(-20.0) }).unwrap();
        h.preview(&mut p, Command::RenameChannel { id: cid, name: "X".into() }).unwrap();
        h.cancel_gesture(&mut p).unwrap();
        assert_eq!(p, original);
    }

    #[test]
    fn dirty_flag_tracks_saved_revision() {
        let mut p = proj();
        let mut h = History::new();
        assert!(!h.is_dirty());
        h.apply(&mut p, Command::RenameProject { name: "A".into() }).unwrap();
        assert!(h.is_dirty());
        let rev = h.revision();
        h.apply(&mut p, Command::RenameProject { name: "B".into() }).unwrap();
        h.mark_saved_at(rev);
        assert!(h.is_dirty(), "save of an older revision must not clear dirty");
        h.mark_saved();
        assert!(!h.is_dirty());
        h.undo(&mut p).unwrap();
        assert!(h.is_dirty());
    }

    #[test]
    fn a_huge_saved_revision_is_clamped_so_edits_never_overflow() {
        let mut p = proj();
        let mut h = History::continuing_from(u64::MAX);
        assert_eq!(h.revision(), MAX_REVISION);
        assert!(!h.is_dirty());
        h.apply(&mut p, Command::RenameProject { name: "A".into() }).unwrap();
        assert_eq!(h.revision(), MAX_REVISION + 1);
        assert!(h.is_dirty());
        h.mark_dirty();
        h.undo(&mut p).unwrap();
        assert!(h.is_dirty(), "marked dirty stays dirty even back at the opened revision");
    }

    #[test]
    fn remove_channel_restores_lane_data_on_undo() {
        let mut p = proj();
        let mut h = History::new();
        let pid = p.patterns[0].id;
        let cid = p.channels[0].id;
        h.apply(&mut p, Command::AddNotes { pattern_id: pid, channel_id: cid, notes: vec![note(0, 60)] }).unwrap();
        let before = p.clone();
        h.apply(&mut p, Command::RemoveChannel { id: cid }).unwrap();
        assert!(p.channels.is_empty());
        assert!(p.patterns[0].lanes.is_empty());
        h.undo(&mut p).unwrap();
        assert_eq!(p, before);
    }

    #[test]
    fn batch_rolls_back_on_failure() {
        let mut p = proj();
        let original = p.clone();
        let mut h = History::new();
        let cid = p.channels[0].id;
        let r = h.apply(&mut p, Command::Batch { commands: vec![Command::RenameChannel { id: cid, name: "ok".into() }, Command::SetChannelPan { id: cid, pan: Pan(9.0) }] });
        assert!(r.is_err());
        assert_eq!(p, original);
    }

    #[test]
    fn mixer_routing_commands_validate_cycles() {
        let mut p = proj();
        let mut h = History::new();
        let master = p.mixer.master_id;
        let insert = p.mixer.channels[1].id;
        assert!(h.apply(&mut p, Command::SetMixerOutput { id: master, output_id: Some(insert) }).is_err());
        assert_eq!(p.mixer_channel(master).unwrap().output_id, None);
        assert!(h.apply(&mut p, Command::RemoveMixerChannel { id: master }).is_err());
        assert!(h.apply(&mut p, Command::RemoveMixerChannel { id: insert }).is_err(), "still routed from channel");
    }

    #[test]
    fn commands_serialize() {
        let c = Command::SetChannelMute { id: ChannelId::from_u128(1), mute: true };
        let j = serde_json::to_string(&c).unwrap();
        assert!(j.contains("set_channel_mute"));
        let back: Command = serde_json::from_str(&j).unwrap();
        assert_eq!(back, c);
    }
}
