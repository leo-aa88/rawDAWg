//! Channel rack editing (CORE-02): duplicate a channel (its sound and settings, optionally
//! with its notes) as one undoable step.

use rawdawg_project::{Command, Note, Project};
use rawdawg_types::{ChannelId, ControllerEventId, InstanceId, NoteId};

/// The name for a copy: "Lead" → "Lead 2", and a copy of "Lead 2" → "Lead 3", skipping names
/// in use. A trailing number counts as a copy number only when the name without it is another
/// channel's ("Kick 808" stays "Kick 808 2").
pub(crate) fn duplicate_name(project: &Project, name: &str) -> String {
    let base = match name.rsplit_once(' ') {
        Some((stem, n)) if n.parse::<u32>().is_ok() && project.channels.iter().any(|c| c.name == stem) => stem,
        _ => name,
    };
    (2u32..).map(|n| format!("{base} {n}")).find(|candidate| !project.channels.iter().any(|c| &c.name == candidate)).unwrap_or_else(|| format!("{base} copy"))
}

/// One command that inserts a copy of channel `id` right after it: the same instrument and
/// state, gain, pan, mute and mixer channel, with new ids. With `with_notes`, the copy also
/// gets the original's notes and controller events in every pattern. Returns the command and
/// the new channel's id.
pub(crate) fn duplicate_channel(project: &Project, id: ChannelId, with_notes: bool) -> Option<(Command, ChannelId)> {
    let index = project.channels.iter().position(|c| c.id == id)?;
    let mut copy = project.channels[index].clone();
    copy.id = ChannelId::new();
    copy.instrument.id = InstanceId::new();
    copy.name = duplicate_name(project, &copy.name);
    // a soloed original stays the only soloed one
    copy.solo = false;
    let new_id = copy.id;
    let mut commands = vec![Command::AddChannel { channel: copy, index: Some(index + 1) }];
    if with_notes {
        for pattern in &project.patterns {
            let Some(lane) = pattern.lanes.iter().find(|l| l.channel_id == id) else { continue };
            if !lane.notes.is_empty() {
                let notes: Vec<Note> = lane.notes.iter().map(|n| Note { id: NoteId::new(), ..*n }).collect();
                commands.push(Command::AddNotes { pattern_id: pattern.id, channel_id: new_id, notes });
            }
            if !lane.controllers.is_empty() {
                let events = lane.controllers.iter().map(|e| rawdawg_project::ControllerEvent { id: ControllerEventId::new(), ..*e }).collect();
                commands.push(Command::AddControllerEvents { pattern_id: pattern.id, channel_id: new_id, events });
            }
        }
    }
    let command = if commands.len() == 1 { commands.pop()? } else { Command::Batch { commands } };
    Some((command, new_id))
}

/// One command that adds a fresh rawDAWg Synth channel (the default sound) right after `after`
/// (or at the end), with its own mixer channel feeding the master, like the first synth of a
/// new project. Returns the command and the new channel's id.
pub(crate) fn new_synth_channel(project: &Project, after: Option<ChannelId>) -> (Command, ChannelId) {
    use rawdawg_types::units::{Db, Pan};
    let name = if project.channels.iter().any(|c| c.name == "Synth") { duplicate_name(project, "Synth") } else { "Synth".to_owned() };
    // the next free "Insert n" (strips can be deleted, so the count alone may repeat a name)
    let inserts = project.mixer.channels.iter().filter(|m| m.id != project.mixer.master_id).count();
    let n = (inserts + 1..).find(|n| !project.mixer.channels.iter().any(|m| m.name == format!("Insert {n}"))).unwrap_or(inserts + 1);
    let mixer = rawdawg_project::MixerChannel {
        id: rawdawg_types::MixerChannelId::new(),
        name: format!("Insert {n}"),
        color: None,
        gain_db: Db::UNITY,
        pan: Pan::CENTER,
        mute: false,
        solo: false,
        inserts: vec![],
        sends: vec![],
        output_id: Some(project.mixer.master_id),
    };
    let patch = serde_json::to_value(rawdawg_synthesizer::Patch::default()).unwrap_or(serde_json::Value::Null);
    let channel = rawdawg_project::Channel {
        id: ChannelId::new(),
        name,
        color: Some([0x87, 0xC8, 0xA2]),
        instrument: rawdawg_project::Instance::native(rawdawg_types::ids::native_types::SYNTH, patch),
        gain_db: Db::UNITY,
        pan: Pan::CENTER,
        mute: false,
        solo: false,
        mixer_id: mixer.id,
    };
    let id = channel.id;
    let index = after.and_then(|a| project.channels.iter().position(|c| c.id == a)).map(|i| i + 1);
    let command = Command::Batch { commands: vec![Command::AddMixerChannel { channel: mixer, index: None }, Command::AddChannel { channel, index }] };
    (command, id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rawdawg_project::History;
    use rawdawg_types::units::Tick;

    fn project_with_lead() -> Project {
        let mut p = Project::new_default("t", serde_json::json!({ "name": "Bright" }));
        p.channels[0].name = "Lead".into();
        p.channels[0].solo = true;
        let ch = p.channels[0].id;
        let lane = p.patterns[0].lanes.iter_mut().find(|l| l.channel_id == ch).unwrap();
        for (t, k) in [(0, 60), (960, 64)] {
            lane.notes.push(Note { id: NoteId::new(), tick: Tick(t), duration_ticks: Tick(480), pitch: k, velocity: 0.8, channel: 0 });
        }
        p
    }

    #[test]
    fn copies_are_named_after_the_original_with_the_first_free_number() {
        let mut p = project_with_lead();
        assert_eq!(duplicate_name(&p, "Lead"), "Lead 2");
        let mut second = p.channels[0].clone();
        second.name = "Lead 2".into();
        p.channels.push(second);
        assert_eq!(duplicate_name(&p, "Lead"), "Lead 3");
        assert_eq!(duplicate_name(&p, "Lead 2"), "Lead 3", "a copy of a copy counts on");
        assert_eq!(duplicate_name(&p, "Kick 808"), "Kick 808 2", "a number that is part of the name stays");
        assert_eq!(duplicate_name(&p, "Pad 2"), "Pad 2 2", "no channel named Pad: not a copy number");
        assert_eq!(duplicate_name(&p, "2"), "2 2");
    }

    #[test]
    fn duplicate_copies_sound_and_settings_after_the_original_in_one_undo_step() {
        let mut p = project_with_lead();
        let original = p.channels[0].clone();
        let mut h = History::new();
        let (cmd, new_id) = duplicate_channel(&p, original.id, false).unwrap();
        h.apply(&mut p, cmd).unwrap();
        assert_eq!(p.channels.len(), 2);
        let copy = &p.channels[1];
        assert_eq!(copy.id, new_id);
        assert_ne!(copy.instrument.id, original.instrument.id, "its own instrument instance");
        assert_eq!(copy.instrument.state, original.instrument.state, "the same sound");
        assert_eq!((copy.gain_db, copy.pan, copy.mute, copy.mixer_id), (original.gain_db, original.pan, original.mute, original.mixer_id));
        assert_eq!(copy.name, "Lead 2");
        assert!(!copy.solo && p.channels[0].solo, "solo stays on the original");
        let lane = p.patterns[0].lanes.iter().find(|l| l.channel_id == new_id).expect("a lane for the copy");
        assert!(lane.notes.is_empty(), "notes stay on the original");
        rawdawg_project::validate(&p).unwrap();
        h.undo(&mut p).unwrap();
        assert_eq!(p.channels.len(), 1);
        assert!(p.patterns[0].lanes.iter().all(|l| l.channel_id != new_id));
    }

    #[test]
    fn duplicate_with_notes_copies_every_pattern_with_new_ids_in_one_undo_step() {
        let mut p = project_with_lead();
        let original = p.channels[0].id;
        let mut h = History::new();
        let (cmd, new_id) = duplicate_channel(&p, original, true).unwrap();
        h.apply(&mut p, cmd).unwrap();
        let notes = |p: &Project, ch| p.patterns[0].lanes.iter().find(|l| l.channel_id == ch).map(|l| l.notes.clone()).unwrap_or_default();
        let (a, b) = (notes(&p, original), notes(&p, new_id));
        assert_eq!(b.len(), 2);
        assert_eq!(a.iter().map(|n| (n.tick, n.pitch)).collect::<Vec<_>>(), b.iter().map(|n| (n.tick, n.pitch)).collect::<Vec<_>>());
        assert!(a.iter().all(|n| b.iter().all(|m| m.id != n.id)), "copied notes have their own ids");
        rawdawg_project::validate(&p).unwrap();
        assert_eq!(h.undo_len(), 1, "one undo step");
        h.undo(&mut p).unwrap();
        assert_eq!(p.channels.len(), 1);
        assert_eq!(notes(&p, original).len(), 2, "the original is untouched");
        assert!(duplicate_channel(&p, ChannelId::new(), false).is_none(), "unknown channel");
    }

    #[test]
    fn a_new_synth_channel_gets_the_default_sound_its_own_mixer_strip_and_one_undo_step() {
        let mut p = project_with_lead();
        let first = p.channels[0].id;
        let mut h = History::new();
        let (cmd, id) = new_synth_channel(&p, Some(first));
        h.apply(&mut p, cmd).unwrap();
        rawdawg_project::validate(&p).unwrap();
        assert_eq!(p.channels.iter().map(|c| c.id).collect::<Vec<_>>(), vec![first, id], "right after the selected channel");
        let ch = p.channel(id).unwrap();
        assert_eq!(ch.name, "Synth");
        assert_eq!(ch.instrument.type_id.as_str(), rawdawg_types::ids::native_types::SYNTH);
        let patch: rawdawg_synthesizer::Patch = serde_json::from_value(ch.instrument.state.clone()).unwrap();
        assert_eq!(patch, rawdawg_synthesizer::Patch::default(), "the default sound");
        let strip = p.mixer_channel(ch.mixer_id).expect("its own mixer channel");
        assert_eq!(strip.output_id, Some(p.mixer.master_id));
        assert!(p.patterns[0].lanes.iter().any(|l| l.channel_id == id), "a lane in every pattern");
        // the next one is "Synth 2"; with no channel at all it is "Synth" again
        let (cmd, id2) = new_synth_channel(&p, None);
        h.apply(&mut p, cmd).unwrap();
        assert_eq!(p.channel(id2).unwrap().name, "Synth 2");
        assert_eq!(p.channels.last().unwrap().id, id2, "at the end without a selection");
        h.undo(&mut p).unwrap();
        h.undo(&mut p).unwrap();
        assert_eq!(p.channels.len(), 1, "one undo step each");
        assert_eq!(p.mixer.channels.len(), project_with_lead().mixer.channels.len(), "the mixer strips go too");
    }
}
