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

/// The exported VST3 module laid out as a bundle (copied under a temp name, then renamed).
fn vst3_bundle() -> PathBuf {
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
