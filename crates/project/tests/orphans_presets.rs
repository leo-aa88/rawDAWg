//! CORE-07/08 and DATA-02: removing an automated/mapped instance orphans its curves and
//! drops its mappings with an exact undo; effect and rack preset files round-trip.

use rawdawg_project::preset::{PresetManifest, read_preset, write_preset};
use rawdawg_project::{AutomationCurve, AutomationMode, AutomationPoint, AutomationTarget, Command, History, Instance, Interpolation, MidiMapping, Project};
use rawdawg_types::units::Tick;
use rawdawg_types::{CurveId, MappingId, PointId};

#[test]
fn removing_an_instance_orphans_its_curves_drops_its_mappings_and_undo_restores_both() {
    let mut p = Project::new_default("orphans", serde_json::json!({}));
    let mut h = History::new();
    let ch = p.channels[0].clone();
    let inst = ch.instrument.id;
    let curve = AutomationCurve {
        id: CurveId::new(),
        name: "cutoff".into(),
        target: AutomationTarget { instance_id: inst, param_id: 7 },
        points: vec![AutomationPoint { id: PointId::new(), tick: Tick(0), value: 0.3, interpolation: Interpolation::Linear }],
        mode: AutomationMode::Read,
        disabled: false,
    };
    h.apply(&mut p, Command::AddAutomationCurve { curve: curve.clone() }).unwrap();
    let mapping =
        MidiMapping { id: MappingId::new(), device: None, channel: None, cc: 1, target: AutomationTarget { instance_id: inst, param_id: 7 }, min: 0.0, max: 1.0, invert: false };
    h.apply(&mut p, Command::SetMidiMappings { mappings: vec![mapping.clone()] }).unwrap();
    // an insert with its own curve and mapping
    let master = p.mixer.master_id;
    let fx = Instance::native("native.fx.eq", serde_json::Value::Null);
    let fx_id = fx.id;
    h.apply(&mut p, Command::AddInsert { mixer_id: master, instance: fx, index: None }).unwrap();
    let fx_curve = AutomationCurve {
        id: CurveId::new(),
        name: "gain".into(),
        target: AutomationTarget { instance_id: fx_id, param_id: 1 },
        points: vec![],
        mode: AutomationMode::Read,
        disabled: false,
    };
    h.apply(&mut p, Command::AddAutomationCurve { curve: fx_curve.clone() }).unwrap();
    // remove the channel: the curve is kept but disabled, the mapping is gone, the project stays valid
    h.apply(&mut p, Command::RemoveChannel { id: ch.id }).unwrap();
    rawdawg_project::validate(&p).unwrap();
    let c = p.automation.iter().find(|c| c.id == curve.id).expect("orphan curve kept");
    assert!(c.disabled && c.points.len() == 1 && c.target.instance_id == inst, "orphan keeps its points and target");
    assert!(p.midi_mappings.is_empty(), "mappings of a removed instance are dropped");
    assert!(!p.automation.iter().find(|c| c.id == fx_curve.id).unwrap().disabled, "other curves untouched");
    h.undo(&mut p).unwrap();
    let c = p.automation.iter().find(|c| c.id == curve.id).unwrap();
    assert!(!c.disabled, "undo re-enables the curve");
    assert_eq!(p.midi_mappings, vec![mapping], "undo restores the mapping");
    assert!(p.channels.iter().any(|x| x.id == ch.id));
    // same for inserts
    h.apply(&mut p, Command::RemoveInsert { mixer_id: master, instance_id: fx_id }).unwrap();
    rawdawg_project::validate(&p).unwrap();
    assert!(p.automation.iter().find(|c| c.id == fx_curve.id).unwrap().disabled);
    h.undo(&mut p).unwrap();
    assert!(!p.automation.iter().find(|c| c.id == fx_curve.id).unwrap().disabled);
    assert_eq!(p.mixer_channel(master).unwrap().inserts.len(), 1);
    // repairing: retarget the orphan to another instance re-enables it
    h.apply(&mut p, Command::RemoveChannel { id: ch.id }).unwrap();
    let repaired =
        AutomationCurve { target: AutomationTarget { instance_id: fx_id, param_id: 7 }, disabled: false, ..p.automation.iter().find(|c| c.id == curve.id).unwrap().clone() };
    h.apply(&mut p, Command::UpdateAutomationCurve { curve: repaired }).unwrap();
    rawdawg_project::validate(&p).unwrap();
    assert!(!p.automation.iter().find(|c| c.id == curve.id).unwrap().disabled);
}

#[test]
fn effect_and_rack_presets_round_trip_through_files() {
    let dir = std::env::temp_dir().join(format!("rawdawg-presets-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let state = serde_json::json!({ "version": 1, "params": { "decay": 0.7, "mix": 0.4 } });
    let m = PresetManifest::new_effect("Big hall", "native.fx.reverb", state.clone());
    let path = dir.join("hall.rawdawgpreset");
    write_preset(&path, &m, &[]).unwrap();
    let back = read_preset(&path).unwrap();
    assert_eq!(back.manifest.kind, "effect");
    assert_eq!(back.manifest.type_id, "native.fx.reverb");
    assert_eq!(back.manifest.state, state);
    assert_eq!(back.manifest.name, "Big hall");
    // rack: two inserts with bypass/mix come back as fresh instances in order
    let mut a = Instance::native("native.fx.eq", serde_json::json!({ "version": 1, "params": { "output": 0.5 } }));
    a.display_name = "EQ".into();
    let mut b = Instance::native("native.fx.compressor", serde_json::Value::Null);
    b.bypass = true;
    b.mix = 0.25;
    let rack = PresetManifest::new_rack("Vocal chain", &[a.clone(), b.clone()]);
    let rpath = dir.join("vocal.rawdawgpreset");
    write_preset(&rpath, &rack, &[]).unwrap();
    let loaded = read_preset(&rpath).unwrap();
    assert_eq!(loaded.manifest.kind, "rack");
    let inserts = loaded.manifest.rack_inserts();
    assert_eq!(inserts.len(), 2);
    assert_eq!(inserts[0].type_id.as_str(), "native.fx.eq");
    assert_eq!(inserts[0].display_name, "EQ");
    assert_eq!(inserts[0].state, a.state);
    assert_ne!(inserts[0].id, a.id, "fresh ids");
    assert!(inserts[1].bypass && (inserts[1].mix - 0.25).abs() < 1e-6);
    let _ = std::fs::remove_dir_all(&dir);
}
