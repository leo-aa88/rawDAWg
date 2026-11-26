//! SYN-10 through the plugin ABIs: rawDAWg hosting its own exported synth (CLAP and VST3)
//! forwards per-note expression and polyphonic pressure, and the exported synth applies them
//! exactly like the native one.

use rawdawg_dsp::processor::{PrepareSpec, ProcessContext, Processor, TransportView};
use rawdawg_dsp::{AudioBlock, BusLayout};
use rawdawg_plugin_host::clap::{ClapInstance, ModuleCache};
use rawdawg_plugin_host::vst3::{Vst3Instance, Vst3ModuleCache, describe_vst3_module};
use rawdawg_types::events::{EventKind, EventSource, NoteExpression, TimedEvent};
use std::path::PathBuf;

const SR: f64 = 48_000.0;
const BLOCK: usize = 256;
const BLOCKS: usize = 160;

fn built_module(package: &str, stem: &str) -> PathBuf {
    let status = std::process::Command::new(env!("CARGO")).args(["build", "-q", "-p", package]).status().expect("cargo build");
    assert!(status.success());
    let exe = std::env::current_exe().unwrap();
    let dir = exe.parent().unwrap().parent().unwrap();
    let name = if cfg!(target_os = "windows") { format!("{stem}.dll") } else { format!("lib{stem}.so") };
    let p = dir.join(name);
    assert!(p.is_file(), "{}", p.display());
    p
}

/// The exported VST3 module laid out as a bundle, built once per process (tests run in
/// parallel and must never load a file another thread is still copying).
fn vst3_bundle() -> PathBuf {
    static BUNDLE: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    BUNDLE
        .get_or_init(|| {
            let module = built_module("rawdawg-plugin-vst3", "rawdawg_vst3");
            let base = std::env::temp_dir().join(format!("rawdawg-noteexpr-{}", std::process::id()));
            let arch = if cfg!(target_os = "windows") { "x86_64-win" } else { "x86_64-linux" };
            let dir = base.join("rawDAWg.vst3").join("Contents").join(arch);
            std::fs::create_dir_all(&dir).unwrap();
            let target = dir.join(if cfg!(target_os = "windows") { "rawDAWg.vst3" } else { "rawDAWg.so" });
            let tmp = dir.join("partial.tmp");
            std::fs::copy(&module, &tmp).unwrap();
            std::fs::rename(&tmp, &target).unwrap();
            base.join("rawDAWg.vst3")
        })
        .clone()
}

fn ev(block: usize, kind: EventKind) -> (usize, TimedEvent) {
    (block, TimedEvent::new(7, EventSource::Transport, 0, kind))
}

fn x(block: usize, note_id: u32, expression: NoteExpression, value: f32) -> (usize, TimedEvent) {
    ev(block, EventKind::NoteExpression { note_id, expression, value })
}

/// Two notes shaped by every expression the synth understands, then released.
fn scenario(with_expression: bool) -> Vec<(usize, TimedEvent)> {
    let mut v = vec![
        ev(0, EventKind::NoteOn { note_id: 1, pitch: 57, velocity: 0.8 }),
        ev(0, EventKind::NoteOn { note_id: 2, pitch: 64, velocity: 0.7 }),
        ev(130, EventKind::NoteOff { note_id: 1, pitch: 57 }),
        ev(130, EventKind::NoteOff { note_id: 2, pitch: 64 }),
    ];
    if with_expression {
        v.extend([
            x(20, 1, NoteExpression::Tuning, 12.0),
            x(20, 2, NoteExpression::Volume, 0.5),
            x(35, 2, NoteExpression::Pan, 0.2),
            x(35, 1, NoteExpression::Brightness, 0.2),
            x(50, 2, NoteExpression::Vibrato, 0.8),
            x(50, 1, NoteExpression::Expression, 0.6),
            ev(65, EventKind::NotePressure { note_id: 1, value: 0.6 }),
            x(80, 2, NoteExpression::Tuning, -3.5),
        ]);
    }
    v
}

fn spec() -> PrepareSpec {
    PrepareSpec::new(SR, BLOCK, BusLayout::Stereo)
}

/// Notes without ids (as from a MIDI stream or a host that sends none): started, pressed,
/// shaped (when the format can address such notes) and released by pitch.
fn pitch_scenario(with_expressions: bool) -> Vec<(usize, TimedEvent)> {
    use rawdawg_types::events::pitch_note_id;
    let (a, b) = (pitch_note_id(57), pitch_note_id(64));
    let mut v = Vec::new();
    v.extend([
        ev(0, EventKind::NoteOn { note_id: a, pitch: 57, velocity: 0.8 }),
        ev(0, EventKind::NoteOn { note_id: b, pitch: 64, velocity: 0.7 }),
        ev(30, EventKind::NotePressure { note_id: a, value: 0.7 }),
        ev(100, EventKind::NoteOff { note_id: a, pitch: 57 }),
        ev(100, EventKind::NoteOff { note_id: b, pitch: 64 }),
    ]);
    // a note with a real id that equals the other note's pitch (64): an expression for it must not
    // reach the id-less note on key 64 (sending a key as an id would make the two collide)
    v.extend([ev(0, EventKind::NoteOn { note_id: 64, pitch: 52, velocity: 0.6 }), x(50, 64, NoteExpression::Volume, 0.0), ev(100, EventKind::NoteOff { note_id: 64, pitch: 52 })]);
    if with_expressions {
        v.extend([x(20, a, NoteExpression::Tuning, 7.0), x(40, b, NoteExpression::Volume, 0.4), x(60, a, NoteExpression::Pan, 0.1)]);
    }
    v
}

/// A patch whose aftertouch closes the amp, so the scenario's pressure is heard (route
/// parameters are not exposed to hosts, so it travels as the plugin's state).
fn pressure_patch() -> Vec<u8> {
    use rawdawg_synthesizer::patch::{ModRoute, ModSource, ModTarget, Patch};
    let mut p = Patch::default();
    p.mod_routes.push(ModRoute { source: ModSource::Aftertouch, target: ModTarget::Amp, depth: -0.8, enabled: true });
    p.to_json()
}

/// A hosted instance's saved state with the plugin's own bytes (`key`) replaced by `patch`
/// (the exported synth's state is its patch).
fn with_patch(saved: &[u8], key: &str, patch: &[u8]) -> Vec<u8> {
    let mut env: serde_json::Value = serde_json::from_slice(saved).expect("envelope");
    env[key] = serde_json::Value::String(rawdawg_plugin_host::b64::encode(patch));
    serde_json::to_vec(&env).unwrap()
}

fn native_with_pressure() -> Box<dyn Processor> {
    let mut p = native();
    p.load_state(&pressure_patch()).unwrap();
    p.prepare(&spec()).unwrap();
    p
}

/// Render the scenario through a prepared processor; returns left then right channel.
fn render(p: &mut dyn Processor, events: &[(usize, TimedEvent)]) -> Vec<f32> {
    let transport = TransportView::stopped(SR);
    let (mut l, mut r) = (vec![0.0; BLOCK], vec![0.0; BLOCK]);
    let (mut out_l, mut out_r) = (Vec::new(), Vec::new());
    for b in 0..BLOCKS {
        let evs: Vec<TimedEvent> = events.iter().filter(|(at, _)| *at == b).map(|(_, e)| *e).collect();
        l.fill(0.0);
        r.fill(0.0);
        let mut outs: [&mut [f32]; 2] = [&mut l, &mut r];
        let mut ctx = ProcessContext { audio: AudioBlock::output_only(BLOCK, &mut outs), events: &evs, transport: &transport };
        p.process(&mut ctx);
        out_l.extend_from_slice(&l);
        out_r.extend_from_slice(&r);
    }
    out_l.extend(out_r);
    out_l
}

fn max_diff(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| (x - y).abs()).fold(0.0, f32::max)
}

fn native() -> Box<dyn Processor> {
    let mut p = rawdawg_plugin_api::exported_plugins().into_iter().find(|p| p.clap_id == "rawdawg.synth").expect("exported synth").make().unwrap();
    p.prepare(&spec()).unwrap();
    p
}

fn check(format: &str, hosted: &mut dyn Processor) {
    let reference = render(native().as_mut(), &scenario(true));
    let plain = render(native().as_mut(), &scenario(false));
    assert!(max_diff(&reference, &plain) > 0.02, "the expressions change the sound");
    let got = render(hosted, &scenario(true));
    let d = max_diff(&got, &reference);
    assert!(d < 1e-5, "{format}: hosted synth follows the note expressions like the native one (max diff {d})");
}

#[test]
fn clap_note_expressions_reach_the_exported_synth() {
    let module = built_module("rawdawg-plugin-clap", "rawdawg_clap");
    // SAFETY: our own freshly built module.
    let library = unsafe { rawdawg_plugin_host::describe::describe_clap_library(&module) }.expect("describe");
    let desc = library.plugins.iter().find(|p| p.plugin_id == "rawdawg.synth").expect("synth").clone();
    let mut cache = ModuleCache::default();
    // SAFETY: the module passed the description above.
    let loaded = unsafe { cache.load(&library.path) }.expect("load");
    let mut synth = ClapInstance::new(loaded, &desc).expect("instantiate");
    // the audio half, as the engine uses it (the main-thread instance never renders)
    let mut rt = synth.activate_for_rt(&spec()).expect("activate");
    check("CLAP", &mut rt);
}

#[test]
fn vst3_note_expressions_reach_the_exported_synth() {
    let bundle = vst3_bundle();
    // SAFETY: our own freshly built module.
    let library = unsafe { describe_vst3_module(&bundle) }.expect("describe");
    let desc = library.plugins.iter().find(|p| p.name == "rawDAWg Synth").expect("synth").clone();
    let mut cache = Vst3ModuleCache::default();
    // SAFETY: the module was described above.
    let loaded = unsafe { cache.load(&library.path) }.expect("load");
    let mut synth = Vst3Instance::new(loaded, &desc).expect("instantiate");
    let mut rt = synth.activate_for_rt(&spec()).expect("activate");
    check("VST3", &mut rt);
}

fn check_pitch(format: &str, hosted: &mut dyn Processor, with_expressions: bool) {
    let reference = render(native_with_pressure().as_mut(), &pitch_scenario(with_expressions));
    let no_pressure: Vec<(usize, TimedEvent)> = pitch_scenario(false).into_iter().filter(|(_, e)| !matches!(e.kind, EventKind::NotePressure { .. })).collect();
    assert!(max_diff(&reference, &render(native_with_pressure().as_mut(), &no_pressure)) > 0.02, "pressure changes the sound");
    let got = render(hosted, &pitch_scenario(with_expressions));
    let d = max_diff(&got, &reference);
    assert!(d < 1e-5, "{format}: notes without ids play, are pressed and released like native (max diff {d})");
    let n = got.len() / 2;
    assert!(got[n - 4096..n].iter().all(|s| s.abs() < 1e-6), "{format}: released, nothing hangs");
}

#[test]
fn clap_notes_without_ids_are_addressed_by_key() {
    let module = built_module("rawdawg-plugin-clap", "rawdawg_clap");
    // SAFETY: our own freshly built module.
    let library = unsafe { rawdawg_plugin_host::describe::describe_clap_library(&module) }.expect("describe");
    let desc = library.plugins.iter().find(|p| p.plugin_id == "rawdawg.synth").expect("synth").clone();
    let mut cache = ModuleCache::default();
    // SAFETY: the module passed the description above.
    let loaded = unsafe { cache.load(&library.path) }.expect("load");
    let mut synth = ClapInstance::new(loaded, &desc).expect("instantiate");
    let state = with_patch(&synth.save_state(), rawdawg_plugin_host::clap::STATE_KEY, &pressure_patch());
    synth.load_state(&state).expect("state");
    let mut rt = synth.activate_for_rt(&spec()).expect("activate");
    // CLAP addresses expressions of id-less notes by key: all of them reach the synth
    check_pitch("CLAP", &mut rt, true);
}

#[test]
fn vst3_notes_without_ids_are_addressed_by_pitch() {
    let bundle = vst3_bundle();
    // SAFETY: our own freshly built module.
    let library = unsafe { describe_vst3_module(&bundle) }.expect("describe");
    let desc = library.plugins.iter().find(|p| p.name == "rawDAWg Synth").expect("synth").clone();
    let mut cache = Vst3ModuleCache::default();
    // SAFETY: the module was described above.
    let loaded = unsafe { cache.load(&library.path) }.expect("load");
    let mut synth = Vst3Instance::new(loaded, &desc).expect("instantiate");
    let state = with_patch(&synth.save_state(), rawdawg_plugin_host::vst3::COMPONENT_STATE_KEY, &pressure_patch());
    synth.load_state(&state).expect("state");
    let mut rt = synth.activate_for_rt(&spec()).expect("activate");
    // VST3 addresses note expressions by id only, so id-less notes get pressure, not expressions
    check_pitch("VST3", &mut rt, false);
}
