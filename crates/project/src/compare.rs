//! What changed between two versions of a project, in words a musician reads (the recovery
//! offer's Compare view: the saved project against an autosave).

use crate::model::{Clip, Project};
use std::collections::{HashMap, HashSet};

/// The differences from `from` to `to`, one line each; empty exactly when they hold the same
/// work (`persist::same_work`: the selection and the revision are bookkeeping).
pub fn describe_changes(from: &Project, to: &Project) -> Vec<String> {
    let mut out = Vec::new();
    if from.name != to.name {
        out.push(format!("Renamed “{}” → “{}”", from.name, to.name));
    }
    if from.tempo != to.tempo {
        let bpm = |p: &Project| p.tempo.first().map(|t| t.us_per_quarter.bpm().0).unwrap_or(120.0);
        let (a, b) = (bpm(from), bpm(to));
        if (a - b).abs() > 1e-6 {
            out.push(format!("Tempo {a:.2} → {b:.2} BPM"));
        }
        if from.tempo.len() != to.tempo.len() {
            out.push(format!("Tempo changes: {} → {}", from.tempo.len(), to.tempo.len()));
        } else if (a - b).abs() <= 1e-6 {
            out.push("Tempo changes edited".into());
        }
    }
    if from.meter != to.meter {
        out.push("Time signature changed".into());
    }
    // channels
    let names = |p: &Project| p.channels.iter().map(|c| (c.id, c.name.clone())).collect::<HashMap<_, _>>();
    let (fc, tc) = (names(from), names(to));
    for c in &to.channels {
        match fc.get(&c.id) {
            None => out.push(format!("Channel added: {}", c.name)),
            Some(old) if *old != c.name => out.push(format!("Channel renamed: {old} → {}", c.name)),
            _ => {}
        }
    }
    for c in &from.channels {
        if !tc.contains_key(&c.id) {
            out.push(format!("Channel removed: {}", c.name));
        }
    }
    for c in &to.channels {
        let Some(old) = from.channels.iter().find(|x| x.id == c.id) else { continue };
        if old.instrument.type_id != c.instrument.type_id || old.instrument.format != c.instrument.format {
            out.push(format!("{}: different instrument", c.name));
        } else if old.instrument.state != c.instrument.state {
            out.push(format!("{}: sound edited", c.name));
        }
        if old.gain_db != c.gain_db || old.pan != c.pan || old.mute != c.mute || old.solo != c.solo {
            out.push(format!("{}: level, pan, mute or solo changed", c.name));
        }
        if old.mixer_id != c.mixer_id {
            out.push(format!("{}: plays into another mixer channel", c.name));
        }
        if old.color != c.color {
            out.push(format!("{}: color changed", c.name));
        }
    }
    let order = |p: &Project| p.channels.iter().map(|c| c.id).filter(|id| fc.contains_key(id) && tc.contains_key(id)).collect::<Vec<_>>();
    if order(from) != order(to) {
        out.push("Channel order changed".into());
    }
    // notes per pattern
    for p in &to.patterns {
        let count = |pat: &crate::model::Pattern| pat.lanes.iter().map(|l| l.notes.len()).sum::<usize>();
        match from.patterns.iter().find(|x| x.id == p.id) {
            None => out.push(format!("Pattern added: {} ({} notes)", p.name, count(p))),
            Some(old) => {
                let (a, b) = (count(old), count(p));
                let ids = |pat: &crate::model::Pattern| pat.lanes.iter().flat_map(|l| l.notes.iter().map(|n| (n.id, n.tick, n.duration_ticks, n.pitch))).collect::<HashSet<_>>();
                if a != b {
                    out.push(format!("{}: {a} → {b} notes", p.name));
                } else if ids(old) != ids(p) || old.lanes != p.lanes {
                    out.push(format!("{}: notes edited", p.name));
                }
                if old.name != p.name {
                    out.push(format!("Pattern renamed: {} → {}", old.name, p.name));
                }
                if old.length_ticks != p.length_ticks {
                    out.push(format!("{}: length changed", p.name));
                }
                if old.steps != p.steps || old.step_ticks != p.step_ticks {
                    out.push(format!("{}: steps {} → {}", p.name, old.steps, p.steps));
                }
                if old.color != p.color {
                    out.push(format!("{}: color changed", p.name));
                }
            }
        }
    }
    for p in &from.patterns {
        if !to.patterns.iter().any(|x| x.id == p.id) {
            out.push(format!("Pattern removed: {}", p.name));
        }
    }
    // arrangement
    let clips = |p: &Project| p.clips.iter().map(|c| (c.id, c.clone())).collect::<HashMap<_, Clip>>();
    let (a, b) = (clips(from), clips(to));
    let added = b.keys().filter(|k| !a.contains_key(k)).count();
    let removed = a.keys().filter(|k| !b.contains_key(k)).count();
    let changed = b.iter().filter(|(k, c)| a.get(k).is_some_and(|o| o != *c)).count();
    if added + removed + changed > 0 {
        out.push(format!("Playlist: {added} clip(s) added, {removed} removed, {changed} moved or edited"));
    }
    if from.tracks.len() != to.tracks.len() {
        out.push(format!("Tracks: {} → {}", from.tracks.len(), to.tracks.len()));
    }
    for t in &to.tracks {
        let Some(old) = from.tracks.iter().find(|x| x.id == t.id) else { continue };
        if old.name != t.name {
            out.push(format!("Track renamed: {} → {}", old.name, t.name));
        }
        if old.mute != t.mute || old.solo != t.solo {
            out.push(format!("{}: mute or solo changed", t.name));
        }
        if old.mixer_id != t.mixer_id {
            out.push(format!("{}: audio plays into another mixer channel", t.name));
        }
        if old.color != t.color || old.height != t.height {
            out.push(format!("{}: color or height changed", t.name));
        }
    }
    let track_order = |p: &Project, other: &Project| p.tracks.iter().map(|t| t.id).filter(|id| other.tracks.iter().any(|x| x.id == *id)).collect::<Vec<_>>();
    if track_order(from, to) != track_order(to, from) {
        out.push("Track order changed".into());
    }
    // mixer
    for m in &to.mixer.channels {
        match from.mixer.channels.iter().find(|x| x.id == m.id) {
            None => out.push(format!("Mixer channel added: {}", m.name)),
            Some(old) => {
                if old.inserts.len() != m.inserts.len() {
                    out.push(format!("{}: {} → {} effects", m.name, old.inserts.len(), m.inserts.len()));
                } else if old.inserts != m.inserts {
                    out.push(format!("{}: effects edited", m.name));
                }
                if old.name != m.name {
                    out.push(format!("Mixer channel renamed: {} → {}", old.name, m.name));
                }
                if old.gain_db != m.gain_db || old.pan != m.pan || old.mute != m.mute || old.solo != m.solo || old.sends != m.sends || old.output_id != m.output_id {
                    out.push(format!("{}: fader, pan, sends or routing changed", m.name));
                }
                if old.color != m.color {
                    out.push(format!("{}: color changed", m.name));
                }
            }
        }
    }
    for m in &from.mixer.channels {
        if !to.mixer.channels.iter().any(|x| x.id == m.id) {
            out.push(format!("Mixer channel removed: {}", m.name));
        }
    }
    if from.mixer.master_protection != to.mixer.master_protection {
        out.push(format!("Master protection {}", if to.mixer.master_protection { "on" } else { "off" }));
    }
    if from.automation != to.automation {
        out.push(format!("Automation: {} → {} curve(s){}", from.automation.len(), to.automation.len(), if from.automation.len() == to.automation.len() { ", edited" } else { "" }));
    }
    if from.assets.len() != to.assets.len() {
        out.push(format!("Samples and files: {} → {}", from.assets.len(), to.assets.len()));
    } else if from.assets != to.assets {
        out.push("Samples and files replaced or relinked".into());
    }
    if from.midi_mappings != to.midi_mappings {
        out.push("MIDI mappings changed".into());
    }
    if from.r#loop != to.r#loop {
        out.push("Loop range changed".into());
    }
    // the same rule the recovery offer uses: anything but bookkeeping is a change worth naming
    if out.is_empty() && !crate::persist::same_work(from, to) {
        out.push("Other project settings changed".into());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Note;
    use rawdawg_types::NoteId;
    use rawdawg_types::units::Tick;

    fn base() -> Project {
        let mut p = Project::new_default("Song", serde_json::json!({ "name": "Bright" }));
        p.tracks.push(crate::model::Track { id: rawdawg_types::TrackId::new(), name: "Track 1".into(), color: None, mute: false, solo: false, height: None, mixer_id: None });
        p
    }

    #[test]
    fn the_same_project_has_no_changes_and_bookkeeping_is_ignored() {
        let a = base();
        let mut b = a.clone();
        b.revision += 7;
        b.selected_pattern = None;
        assert!(describe_changes(&a, &b).is_empty(), "{:?}", describe_changes(&a, &b));
    }

    #[test]
    fn musical_edits_are_described() {
        let a = base();
        let mut b = a.clone();
        b.name = "Song 2".into();
        b.tempo[0].us_per_quarter = rawdawg_types::units::UsPerQuarter(600_000);
        b.channels[0].name = "Lead".into();
        b.channels[0].instrument.state = serde_json::json!({ "name": "Dark" });
        let ch = b.channels[0].id;
        let lane = b.patterns[0].lanes.iter_mut().find(|l| l.channel_id == ch).unwrap();
        for t in [0, 480, 960] {
            lane.notes.push(Note { id: NoteId::new(), tick: Tick(t), duration_ticks: Tick(240), pitch: 60, velocity: 0.8, channel: 0 });
        }
        let master = b.mixer.master_id;
        b.mixer.channels.iter_mut().find(|m| m.id == master).unwrap().gain_db = rawdawg_types::units::Db(-6.0);
        let d = describe_changes(&a, &b);
        let has = |s: &str| d.iter().any(|l| l.contains(s));
        assert!(has("Renamed “Song” → “Song 2”"), "{d:?}");
        assert!(has("Tempo 120.00 → 100.00 BPM"), "{d:?}");
        assert!(has("Channel renamed:") && has("Lead: sound edited"), "{d:?}");
        assert!(has("0 → 3 notes"), "{d:?}");
        assert!(has("fader, pan, sends or routing changed"), "{d:?}");
        // and the other way round
        let back = describe_changes(&b, &a);
        assert!(back.iter().any(|l| l.contains("3 → 0 notes")), "{back:?}");
    }

    #[test]
    fn routing_names_mute_steps_and_protection_are_changes_too() {
        let a = base();
        // each edit alone must be named (the recovery offer depends on it)
        type Edit = Box<dyn Fn(&mut Project)>;
        let edits: Vec<(&str, Edit)> = vec![
            ("Track renamed", Box::new(|p| p.tracks[0].name = "Drums".into())),
            ("mute or solo changed", Box::new(|p| p.tracks[0].mute = true)),
            ("audio plays into another mixer channel", Box::new(|p| p.tracks[0].mixer_id = Some(p.mixer.master_id))),
            ("Mixer channel renamed", Box::new(|p| p.mixer.channels[0].name = "Bus".into())),
            ("routing changed", Box::new(|p| p.mixer.channels[1].output_id = None)),
            ("plays into another mixer channel", Box::new(|p| p.channels[0].mixer_id = p.mixer.master_id)),
            ("Pattern renamed", Box::new(|p| p.patterns[0].name = "Verse".into())),
            ("steps 16 → 32", Box::new(|p| p.patterns[0].steps = 32)),
            ("Master protection off", Box::new(|p| p.mixer.master_protection = false)),
            ("Other project settings changed", Box::new(|p| p.ppq = 480)),
        ];
        for (expect, edit) in edits {
            let mut b = a.clone();
            edit(&mut b);
            assert!(!crate::persist::same_work(&a, &b), "{expect}: the edit changes the work");
            let d = describe_changes(&a, &b);
            assert!(d.iter().any(|l| l.contains(expect)), "{expect}: {d:?}");
        }
    }

    #[test]
    fn channel_and_track_order_are_changes() {
        let mut a = base();
        let mut extra = a.channels[0].clone();
        extra.id = rawdawg_types::ChannelId::new();
        extra.name = "Second".into();
        a.channels.push(extra);
        let mut b = a.clone();
        b.channels.reverse();
        assert!(describe_changes(&a, &b).iter().any(|l| l == "Channel order changed"));
        let mut t = a.tracks[0].clone();
        t.id = rawdawg_types::TrackId::new();
        a.tracks.push(t);
        let mut c = a.clone();
        c.tracks.reverse();
        assert!(describe_changes(&a, &c).iter().any(|l| l == "Track order changed"), "{:?}", describe_changes(&a, &c));
    }
}
