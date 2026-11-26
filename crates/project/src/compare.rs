//! What changed between two versions of a project, in words a musician reads (the recovery
//! offer's Compare view: the saved project against an autosave).

use crate::model::{Clip, Project};
use std::collections::{HashMap, HashSet};

/// The differences from `from` to `to`, one line each; empty when they sound and look the same
/// (selection, revision and other bookkeeping are ignored).
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
                if old.length_ticks != p.length_ticks {
                    out.push(format!("{}: length changed", p.name));
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
                if old.gain_db != m.gain_db || old.pan != m.pan || old.mute != m.mute || old.solo != m.solo || old.sends != m.sends {
                    out.push(format!("{}: fader, pan, sends or routing changed", m.name));
                }
            }
        }
    }
    for m in &from.mixer.channels {
        if !to.mixer.channels.iter().any(|x| x.id == m.id) {
            out.push(format!("Mixer channel removed: {}", m.name));
        }
    }
    if from.automation != to.automation {
        out.push(format!("Automation: {} → {} curve(s){}", from.automation.len(), to.automation.len(), if from.automation.len() == to.automation.len() { ", edited" } else { "" }));
    }
    if from.assets.len() != to.assets.len() {
        out.push(format!("Samples and files: {} → {}", from.assets.len(), to.assets.len()));
    }
    if from.midi_mappings != to.midi_mappings {
        out.push("MIDI mappings changed".into());
    }
    if from.r#loop != to.r#loop {
        out.push("Loop range changed".into());
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
        Project::new_default("Song", serde_json::json!({ "name": "Bright" }))
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
}
