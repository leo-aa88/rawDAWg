//! Removing a mixer strip or a rack channel together with what depends on it, as one batch
//! (one undo step): nothing is left pointing at a strip that is gone, and a strip that only
//! existed for a deleted channel goes with it.

use crate::commands::{Command, CommandError};
use crate::model::*;
use rawdawg_types::ids::*;

/// What [`remove_mixer_channel`] rerouted, by name, for the message that tells the user.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Rerouted {
    /// The strip everything now plays into (the removed strip's own output, or the master).
    pub to: String,
    /// Rack channels, audio tracks and strips that played into the removed strip.
    pub sources: Vec<String>,
    /// Strips whose send to the removed strip was dropped.
    pub sends_dropped: Vec<String>,
}

impl Rerouted {
    pub fn is_empty(&self) -> bool {
        self.sources.is_empty() && self.sends_dropped.is_empty()
    }
}

/// Delete mixer strip `id`. Everything that plays into it is rerouted to where it played (its
/// own output, the master for a strip without one): rack channels and audio tracks switch to
/// that strip, strips that output to it output there, and sends to it are dropped. Its inserts
/// leave with it (their automation becomes disabled orphans, their MIDI mappings are dropped,
/// both restored by undo). The master cannot be removed.
pub fn remove_mixer_channel(p: &Project, id: MixerChannelId) -> Result<(Command, Rerouted), CommandError> {
    let master = p.mixer.master_id;
    if id == master {
        return Err(CommandError::Invalid("the master channel cannot be removed".into()));
    }
    let strip = p.mixer_channel(id).ok_or_else(|| CommandError::Unknown { kind: "mixer channel", id: id.to_string() })?;
    let to = strip.output_id.filter(|o| *o != id && p.mixer_channel(*o).is_some()).unwrap_or(master);
    let mut rerouted = Rerouted { to: p.mixer_channel(to).map(|m| m.name.clone()).unwrap_or_default(), ..Rerouted::default() };
    let mut cmds = Vec::new();
    // sends first: a strip that both sends and outputs to it may then output to `to`
    for m in p.mixer.channels.iter().filter(|m| m.id != id && m.sends.iter().any(|s| s.target_id == id)) {
        cmds.push(Command::SetMixerSends { id: m.id, sends: m.sends.iter().filter(|s| s.target_id != id).cloned().collect() });
        rerouted.sends_dropped.push(m.name.clone());
    }
    for c in p.channels.iter().filter(|c| c.mixer_id == id) {
        cmds.push(Command::SetChannelMixer { id: c.id, mixer_id: to });
        rerouted.sources.push(c.name.clone());
    }
    for t in p.tracks.iter().filter(|t| t.mixer_id == Some(id)) {
        cmds.push(Command::SetTrackMixer { id: t.id, mixer_id: (to != master).then_some(to) });
        rerouted.sources.push(t.name.clone());
    }
    for m in p.mixer.channels.iter().filter(|m| m.id != id && m.output_id == Some(id)) {
        cmds.push(Command::SetMixerOutput { id: m.id, output_id: Some(to) });
        rerouted.sources.push(m.name.clone());
    }
    cmds.push(Command::RemoveMixerChannel { id });
    Ok((Command::Batch { commands: cmds }, rerouted))
}

/// The strip that only exists for rack channel `id`: no other channel or audio track plays
/// into it, no strip outputs or sends to it, and it holds no effects. `None` for the master or
/// a strip anything else uses.
pub fn dedicated_strip(p: &Project, id: ChannelId) -> Option<MixerChannelId> {
    let mixer_id = p.channel(id)?.mixer_id;
    let strip = p.mixer_channel(mixer_id)?;
    let shared = mixer_id == p.mixer.master_id
        || !strip.inserts.is_empty()
        || p.channels.iter().any(|c| c.id != id && c.mixer_id == mixer_id)
        || p.tracks.iter().any(|t| t.mixer_id == Some(mixer_id))
        || p.mixer.channels.iter().any(|m| m.output_id == Some(mixer_id) || m.sends.iter().any(|s| s.target_id == mixer_id));
    (!shared).then_some(mixer_id)
}

/// Delete rack channel `id`, and its mixer strip when the strip only existed for it
/// ([`dedicated_strip`]). Returns the command and whether the strip goes too.
pub fn remove_channel(p: &Project, id: ChannelId) -> (Command, Option<MixerChannelId>) {
    match dedicated_strip(p, id) {
        Some(strip) => (Command::Batch { commands: vec![Command::RemoveChannel { id }, Command::RemoveMixerChannel { id: strip }] }, Some(strip)),
        None => (Command::RemoveChannel { id }, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::History;
    use rawdawg_types::units::{Db, Pan};

    fn strip(name: &str, output_id: Option<MixerChannelId>) -> MixerChannel {
        MixerChannel {
            id: MixerChannelId::new(),
            name: name.into(),
            color: None,
            gain_db: Db::UNITY,
            pan: Pan::CENTER,
            mute: false,
            solo: false,
            inserts: vec![],
            sends: vec![],
            output_id,
        }
    }

    #[test]
    fn a_bus_is_removed_with_its_sources_rerouted_to_where_it_played_and_undo_restores_all() {
        let mut p = Project::new_default("t", serde_json::json!({}));
        let mut h = History::new();
        let master = p.mixer.master_id;
        // group <- bus <- (synth channel, a track, a strip, and a send from another strip)
        let group = strip("Group", Some(master));
        let bus = strip("Bus", Some(group.id));
        let feeder = strip("Feeder", Some(bus.id));
        let mut sender = strip("Sender", Some(master));
        sender.sends.push(Send { target_id: bus.id, level_db: Db(-6.0), tap: SendTap::PostFader, sidechain: false });
        sender.sends.push(Send { target_id: group.id, level_db: Db(-3.0), tap: SendTap::PostFader, sidechain: false });
        for m in [group.clone(), bus.clone(), feeder.clone(), sender.clone()] {
            h.apply(&mut p, Command::AddMixerChannel { channel: m, index: None }).unwrap();
        }
        let cid = p.channels[0].id;
        h.apply(&mut p, Command::SetChannelMixer { id: cid, mixer_id: bus.id }).unwrap();
        let track = TrackId::new();
        h.apply(
            &mut p,
            Command::AddTrack { track: Track { id: track, name: "Track 1".into(), color: None, mute: false, solo: false, height: None, mixer_id: Some(bus.id) }, index: None },
        )
        .unwrap();
        let before = p.clone();
        // the plain command refuses (a track routes there too)
        assert!(Command::RemoveMixerChannel { id: bus.id }.apply(&mut p.clone()).is_err());

        let (cmd, rerouted) = remove_mixer_channel(&p, bus.id).unwrap();
        h.apply(&mut p, cmd).unwrap();
        assert!(p.mixer_channel(bus.id).is_none());
        assert_eq!(p.channel(cid).unwrap().mixer_id, group.id, "the channel plays where the bus played");
        assert_eq!(p.tracks[0].mixer_id, Some(group.id));
        assert_eq!(p.mixer_channel(feeder.id).unwrap().output_id, Some(group.id));
        let sends = &p.mixer_channel(sender.id).unwrap().sends;
        assert_eq!(sends.len(), 1, "the send to the bus is dropped, the other kept");
        assert_eq!(sends[0].target_id, group.id);
        assert_eq!(rerouted.to, "Group");
        assert_eq!(rerouted.sources.len(), 3);
        assert_eq!(rerouted.sends_dropped, vec!["Sender".to_string()]);
        crate::validate::validate(&p).unwrap();
        // one undo step brings everything back
        h.undo(&mut p).unwrap();
        assert_eq!(p, before);
        assert!(remove_mixer_channel(&p, master).is_err(), "never the master");
    }

    #[test]
    fn a_strip_with_effects_takes_their_automation_along_and_undo_brings_it_back() {
        let mut p = Project::new_default("t", serde_json::json!({}));
        let mut h = History::new();
        let mut bus = strip("Bus", Some(p.mixer.master_id));
        let fx = Instance::native("native.fx.gain", serde_json::Value::Null);
        let fx_id = fx.id;
        bus.inserts.push(fx);
        h.apply(&mut p, Command::AddMixerChannel { channel: bus.clone(), index: None }).unwrap();
        let curve = AutomationCurve {
            id: CurveId::new(),
            name: "Gain".into(),
            target: AutomationTarget { instance_id: fx_id, param_id: 0 },
            points: vec![],
            mode: AutomationMode::Read,
            disabled: false,
            pattern_id: None,
        };
        h.apply(&mut p, Command::AddAutomationCurve { curve: curve.clone() }).unwrap();
        let before = p.clone();
        let (cmd, rerouted) = remove_mixer_channel(&p, bus.id).unwrap();
        assert!(rerouted.is_empty(), "nothing played into it");
        h.apply(&mut p, cmd).unwrap();
        assert!(p.automation.iter().find(|c| c.id == curve.id).unwrap().disabled, "its insert's lane is an orphan now");
        h.undo(&mut p).unwrap();
        assert_eq!(p, before);
    }

    #[test]
    fn undo_brings_back_the_midi_mappings_of_every_effect_on_a_deleted_strip() {
        let mut p = Project::new_default("t", serde_json::json!({}));
        let mut h = History::new();
        let mut bus = strip("Bus", Some(p.mixer.master_id));
        let fx: Vec<Instance> = (0..3).map(|_| Instance::native("native.fx.gain", serde_json::Value::Null)).collect();
        bus.inserts.extend(fx.iter().cloned());
        h.apply(&mut p, Command::AddMixerChannel { channel: bus.clone(), index: None }).unwrap();
        // one mapping per effect, and one elsewhere that must survive anyway
        let other = p.channels[0].instrument.id;
        let mapping = |instance_id, cc| MidiMapping {
            id: MappingId::new(),
            device: None,
            channel: None,
            cc,
            target: AutomationTarget { instance_id, param_id: 0 },
            min: 0.0,
            max: 1.0,
            invert: false,
        };
        let mut mappings: Vec<MidiMapping> = fx.iter().enumerate().map(|(i, f)| mapping(f.id, 20 + i as u8)).collect();
        mappings.push(mapping(other, 7));
        h.apply(&mut p, Command::SetMidiMappings { mappings: mappings.clone() }).unwrap();
        let before = p.clone();
        let (cmd, _) = remove_mixer_channel(&p, bus.id).unwrap();
        h.apply(&mut p, cmd).unwrap();
        assert_eq!(p.midi_mappings.len(), 1, "only the mapping of something else stays");
        h.undo(&mut p).unwrap();
        assert_eq!(p.midi_mappings, mappings, "all three effects' mappings are back");
        assert_eq!(p, before);
    }

    #[test]
    fn deleting_a_channel_takes_its_own_strip_but_never_a_shared_one() {
        let mut p = Project::new_default("t", serde_json::json!({}));
        let mut h = History::new();
        let own = strip("Own", Some(p.mixer.master_id));
        h.apply(&mut p, Command::AddMixerChannel { channel: own.clone(), index: None }).unwrap();
        let cid = p.channels[0].id;
        h.apply(&mut p, Command::SetChannelMixer { id: cid, mixer_id: own.id }).unwrap();
        let before = p.clone();
        let (cmd, gone) = remove_channel(&p, cid);
        assert_eq!(gone, Some(own.id));
        h.apply(&mut p, cmd).unwrap();
        assert!(p.channel(cid).is_none() && p.mixer_channel(own.id).is_none());
        h.undo(&mut p).unwrap();
        assert_eq!(p, before, "one undo step restores both");
        // shared: another channel plays there / it has effects / it is the master
        let mut other = p.channels[0].clone();
        other.id = ChannelId::new();
        other.instrument.id = InstanceId::new();
        h.apply(&mut p, Command::AddChannel { channel: other.clone(), index: None }).unwrap();
        assert_eq!(dedicated_strip(&p, cid), None);
        h.apply(&mut p, Command::RemoveChannel { id: other.id }).unwrap();
        assert_eq!(dedicated_strip(&p, cid), Some(own.id));
        h.apply(&mut p, Command::AddInsert { mixer_id: own.id, instance: Instance::native("native.fx.gain", serde_json::Value::Null), index: None }).unwrap();
        assert_eq!(dedicated_strip(&p, cid), None, "a strip with effects stays");
        let master = p.mixer.master_id;
        h.apply(&mut p, Command::SetChannelMixer { id: cid, mixer_id: master }).unwrap();
        assert_eq!(dedicated_strip(&p, cid), None, "never the master");
        assert!(matches!(remove_channel(&p, cid).0, Command::RemoveChannel { .. }));
    }
}
