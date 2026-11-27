//! T-PLUGIN (host side, in-process bridge): host rawDAWg's own CLAP module through the
//! engine and compare against the native processors. Covers instantiation, descriptors,
//! parameters (inactive flush and realtime events), state envelope round trip, activation
//! hand-off to the plan, migration between plans without re-activation, return and
//! deactivation when the plan is dropped, and an instrument fed by the sequencer.

use rawdawg_audio_engine::commands::EngineCommand;
use rawdawg_audio_engine::engine::Engine;
use rawdawg_audio_engine::externals::{ExternalPlugins, PluginRole};
use rawdawg_audio_engine::plan::{compile, compile_with};
use rawdawg_dsp::BusLayout;
use rawdawg_dsp::analysis::dominant_frequency;
use rawdawg_dsp::math::{peak, rms};
use rawdawg_dsp::processor::{PrepareSpec, Processor};
use rawdawg_plugin_host::catalog::PluginDescription;
use rawdawg_plugin_host::clap::{ClapInstance, ModuleCache, STATE_KEY};
use rawdawg_project::{Instance, Note, PluginFormat, Project};
use rawdawg_types::NoteId;
use rawdawg_types::limits::EngineLimits;
use rawdawg_types::units::{SampleRate, Tick};
use std::collections::HashMap;
use std::path::PathBuf;

fn built_clap_module() -> PathBuf {
    let status = std::process::Command::new(env!("CARGO")).args(["build", "-q", "-p", "rawdawg-plugin-clap"]).status().expect("cargo build");
    assert!(status.success());
    let exe = std::env::current_exe().unwrap();
    let dir = exe.parent().unwrap().parent().unwrap();
    let name = if cfg!(target_os = "windows") { "rawdawg_clap.dll" } else { "librawdawg_clap.so" };
    let p = dir.join(name);
    assert!(p.is_file(), "{}", p.display());
    p
}

struct Fixture {
    cache: ModuleCache,
    library: rawdawg_plugin_host::catalog::ScannedLibrary,
}

impl Fixture {
    fn new() -> Self {
        let module = built_clap_module();
        // SAFETY: our own freshly built module.
        let library = unsafe { rawdawg_plugin_host::describe::describe_clap_library(&module) }.expect("describe");
        Self { cache: ModuleCache::default(), library }
    }
    fn description(&self, id: &str) -> &PluginDescription {
        self.library.plugins.iter().find(|p| p.plugin_id == id).unwrap_or_else(|| panic!("{id}"))
    }
    fn instance(&mut self, id: &str) -> ClapInstance {
        // SAFETY: module passed the (in-process) description above.
        let module = unsafe { self.cache.load(&self.library.path) }.expect("load module");
        let desc = self.description(id).clone();
        ClapInstance::new(module, &desc).expect("instantiate")
    }
}

/// Test provider: a map of live main-thread instances keyed by project instance id.
struct Host {
    instances: HashMap<rawdawg_types::InstanceId, ClapInstance>,
}

impl ExternalPlugins for Host {
    fn instantiate(&mut self, inst: &Instance, _role: PluginRole, spec: &PrepareSpec) -> Result<Option<Box<dyn Processor>>, String> {
        match self.instances.get_mut(&inst.id) {
            Some(live) => live.activate_for_rt(spec).map(|p| Some(Box::new(p) as Box<dyn Processor>)),
            None => Ok(None),
        }
    }
}

fn project_with_notes(notes: &[(i64, i64, u8)]) -> Project {
    let mut p = Project::new_default("t", serde_json::json!({}));
    let lane = &mut p.patterns[0].lanes[0];
    for (tick, dur, pitch) in notes {
        lane.notes.push(Note { id: NoteId::new(), tick: Tick(*tick), duration_ticks: Tick(*dur), pitch: *pitch, velocity: 0.9, channel: 0 });
    }
    p
}

const BLOCK: usize = 256;

fn render(engine: &mut Engine, blocks: usize) -> Vec<f32> {
    let mut l = vec![0.0f32; BLOCK];
    let mut r = vec![0.0f32; BLOCK];
    let mut out = Vec::with_capacity(blocks * BLOCK);
    for _ in 0..blocks {
        l.fill(0.0);
        r.fill(0.0);
        engine.render_block(&mut l, &mut r);
        out.extend_from_slice(&l);
    }
    out
}

fn max_diff(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| (x - y).abs()).fold(0.0, f32::max)
}

#[test]
fn instance_exposes_descriptors_params_and_state_envelope() {
    let mut fx = Fixture::new();
    let mut inst = fx.instance("rawdawg.fx.stereo");
    assert_eq!(inst.plugin_id(), "rawdawg.fx.stereo");
    assert_eq!(Processor::type_id(&inst), "clap:rawdawg.fx.stereo");
    assert!(!inst.is_instrument());
    assert_eq!(inst.layout().inputs, vec![2]);
    assert_eq!(inst.layout().outputs, vec![2]);
    let descs = inst.descriptors();
    let native = rawdawg_effects::make_effect("native.fx.stereo", None).unwrap();
    let visible_native = native.descriptors().iter().filter(|d| !d.flags.hidden).count();
    assert_eq!(descs.len(), visible_native + 1, "native visible params + wrapper Bypass (stereo has no mix)");
    let bypass = descs.iter().find(|d| d.flags.is_bypass).expect("bypass flagged");
    assert_eq!(inst.param(bypass.id), Some(0.0));
    let gain = native.descriptors().iter().find(|d| d.key == "gain").unwrap();
    let hosted_gain = descs.iter().find(|d| d.id == gain.id).expect("same parameter ids as the native effect");
    assert_eq!(hosted_gain.name, gain.name);
    let default = inst.param(gain.id).unwrap();
    assert!((default - gain.default_normalized).abs() < 1e-5, "{default} vs {}", gain.default_normalized);
    // inactive set reaches the plugin's main-thread value (read back through the ABI)
    assert!(inst.set_param(gain.id, 0.25));
    inst.refresh_values();
    assert!((inst.param(gain.id).unwrap() - 0.25).abs() < 1e-4, "{:?}", inst.param(gain.id));
    assert!(!inst.set_param(rawdawg_types::ParamId(0xDEAD_BEEF), 0.5), "unknown ids are refused");
    // state: JSON envelope with base64 opaque bytes; round trip restores the value
    let saved = inst.save_state();
    let env: serde_json::Value = serde_json::from_slice(&saved).expect("envelope JSON");
    assert_eq!(env["format"], "clap");
    assert_eq!(env["plugin_id"], "rawdawg.fx.stereo");
    let raw = rawdawg_plugin_host::b64::decode(env[STATE_KEY].as_str().unwrap()).unwrap();
    assert!(raw.starts_with(b"{"), "the plugin's own state is the CLAP wrapper envelope");
    assert!(inst.set_param(gain.id, 0.9));
    inst.refresh_values();
    assert!((inst.param(gain.id).unwrap() - 0.9).abs() < 1e-4);
    inst.load_state(&saved).expect("load");
    assert!((inst.param(gain.id).unwrap() - 0.25).abs() < 1e-4, "state restored the earlier value");
    let other = serde_json::json!({ "format": "clap", "plugin_id": "rawdawg.fx.delay", STATE_KEY: "AAAA" });
    assert!(inst.load_state(&serde_json::to_vec(&other).unwrap()).is_err(), "another plugin's state is refused");
    assert!(inst.load_state(b"garbage").is_err());
    assert!(inst.load_state(&serde_json::to_vec(&serde_json::json!({"format": "clap", STATE_KEY: "@@@@"})).unwrap()).is_err(), "bad base64");
    assert!(!inst.is_active());
}

#[test]
fn hosted_effect_matches_native_effect_in_the_engine_and_migrates_between_plans() {
    let mut fx = Fixture::new();
    let limits = EngineLimits::DEFAULT;
    // native reference: delay on the master inserts
    let mut native = project_with_notes(&[(0, 960, 69), (1920, 480, 72)]);
    let delay = Instance::native("native.fx.delay", serde_json::json!({}));
    native.mixer.channels[0].inserts.push(delay);
    let (mut engine_n, mut handle_n) = Engine::new(SampleRate(48_000), BLOCK, &limits);
    engine_n.install_plan(compile(&native, 1, BLOCK, &limits, &Default::default(), &rawdawg_audio_engine::NoAssets).unwrap().plan);
    handle_n.send(EngineCommand::Play).unwrap();
    let reference = render(&mut engine_n, 200);
    assert!(rms(&reference[20_000..]) > 0.01, "reference sounds");

    // hosted: same project, the insert is the CLAP export of the same effect
    let mut hosted = native.clone();
    let ext = Instance::external(PluginFormat::Clap, "rawdawg.fx.delay", "rawDAWg Delay", "rawDAWg", "0.1.0", None);
    let ext_id = ext.id;
    hosted.mixer.channels[0].inserts[0] = ext;
    let mut host = Host { instances: HashMap::new() };
    host.instances.insert(ext_id, fx.instance("rawdawg.fx.delay"));
    let (mut engine, mut handle) = Engine::new(SampleRate(48_000), BLOCK, &limits);
    let out = compile_with(&hosted, 1, BLOCK, &limits, &Default::default(), &rawdawg_audio_engine::NoAssets, &mut host).unwrap();
    assert!(out.missing_plugins.is_empty(), "{:?}", out.missing_plugins);
    assert!(host.instances[&ext_id].is_active(), "activated for the plan");
    let old = engine.install_plan(out.plan);
    drop(old);
    handle.send(EngineCommand::Play).unwrap();
    let first = render(&mut engine, 100);
    let d = max_diff(&first, &reference[..first.len()]);
    assert!(d < 1e-5, "hosted delay equals the native delay (max diff {d})");

    // recompile while the first plan runs: the shell carries the live audio half over.
    // (Native effects restart their buffers on a recompile, so the comparison here is the
    // CLAP delay's own tail: the echoes of the notes keep sounding after the swap, which a
    // fresh, un-migrated shell could not produce.)
    let out2 = compile_with(&hosted, 2, BLOCK, &limits, &out.fingerprints, &rawdawg_audio_engine::NoAssets, &mut host).unwrap();
    let old = engine.install_plan(out2.plan);
    handle.send(EngineCommand::Stop).unwrap(); // no new notes: only the delay tail remains
    let second = render(&mut engine, 100);
    drop(old); // retired plan dropped off RT: its shell has nothing to return
    host.instances.get_mut(&ext_id).unwrap().collect_returns();
    assert!(host.instances[&ext_id].is_active(), "still the same activation after migration");
    assert!(rms(&second[BLOCK * 20..BLOCK * 60]) > 1e-3, "the delay tail survived the plan swap: rms {}", rms(&second[BLOCK * 20..BLOCK * 60]));
    assert_eq!(host.instances[&ext_id].problem_count(), 0);
    // dropping the running plan returns the audio half; the main thread deactivates it
    let current = engine.install_plan(compile(&project_with_notes(&[]), 3, BLOCK, &limits, &Default::default(), &rawdawg_audio_engine::NoAssets).unwrap().plan);
    drop(current);
    let live = host.instances.get_mut(&ext_id).unwrap();
    live.collect_returns();
    assert!(!live.is_active(), "deactivated after its plan was dropped");
    // and it can be activated again
    let again = live.activate_for_rt(&PrepareSpec::new(48_000.0, BLOCK, BusLayout::Stereo)).expect("re-activate");
    assert!(live.is_active());
    drop(again);
    live.collect_returns();
    assert!(!live.is_active());
}

#[test]
fn hosted_instrument_plays_the_sequencer_notes() {
    let mut fx = Fixture::new();
    let limits = EngineLimits::DEFAULT;
    let mut p = project_with_notes(&[(0, 1920, 69)]);
    let ext = Instance::external(PluginFormat::Clap, "rawdawg.synth", "rawDAWg Synth", "rawDAWg", "0.1.0", None);
    let ext_id = ext.id;
    p.channels[0].instrument = ext;
    let mut host = Host { instances: HashMap::new() };
    let synth = fx.instance("rawdawg.synth");
    assert!(synth.is_instrument());
    assert_eq!(synth.layout().note_inputs, 1);
    assert!(synth.layout().accepts_clap_notes);
    host.instances.insert(ext_id, synth);
    let (mut engine, mut handle) = Engine::new(SampleRate(48_000), BLOCK, &limits);
    let out = compile_with(&p, 1, BLOCK, &limits, &Default::default(), &rawdawg_audio_engine::NoAssets, &mut host).unwrap();
    assert!(out.missing_plugins.is_empty());
    engine.install_plan(out.plan);
    handle.send(EngineCommand::Play).unwrap();
    let a = render(&mut engine, 300);
    let sounding = &a[4096..40_000];
    assert!(rms(sounding) > 0.02, "hosted synth sounds");
    let f = dominant_frequency(sounding, 48_000.0);
    assert!((f - 440.0).abs() < 0.5, "{f}");
    // note ends at tick 1920 = frame 48000; released well before the end of the render
    assert_eq!(peak(&a[48_000 + 12_000..]), 0.0, "note released");
    // without a provider the same project compiles to a silent stand-in and reports it
    let out = compile(&p, 2, BLOCK, &limits, &Default::default(), &rawdawg_audio_engine::NoAssets).unwrap();
    assert_eq!(out.missing_plugins.len(), 1, "{:?}", out.missing_plugins);
}

#[test]
fn realtime_param_events_reach_the_hosted_plugin_like_the_native_one() {
    let mut fx = Fixture::new();
    let limits = EngineLimits::DEFAULT;
    let mix = rawdawg_effects::make_effect("native.fx.delay", None).unwrap().descriptors().iter().find(|d| d.key == "mix").map(|d| d.id).expect("delay mix param");
    let feedback = rawdawg_effects::make_effect("native.fx.delay", None).unwrap().descriptors().iter().find(|d| d.key == "feedback").map(|d| d.id).expect("delay feedback param");
    let mut native = project_with_notes(&[(0, 960, 69), (1920, 480, 72)]);
    native.mixer.channels[0].inserts.push(Instance::native("native.fx.delay", serde_json::json!({})));
    let native_id = native.mixer.channels[0].inserts[0].id;
    let mut hosted = native.clone();
    let ext = Instance::external(PluginFormat::Clap, "rawdawg.fx.delay", "rawDAWg Delay", "rawDAWg", "0.1.0", None);
    let ext_id = ext.id;
    hosted.mixer.channels[0].inserts[0] = ext;
    let mut host = Host { instances: HashMap::new() };
    host.instances.insert(ext_id, fx.instance("rawdawg.fx.delay"));

    let (mut engine_n, mut handle_n) = Engine::new(SampleRate(48_000), BLOCK, &limits);
    engine_n.install_plan(compile(&native, 1, BLOCK, &limits, &Default::default(), &rawdawg_audio_engine::NoAssets).unwrap().plan);
    let (mut engine, mut handle) = Engine::new(SampleRate(48_000), BLOCK, &limits);
    engine.install_plan(compile_with(&hosted, 1, BLOCK, &limits, &Default::default(), &rawdawg_audio_engine::NoAssets, &mut host).unwrap().plan);
    for (h, id) in [(&mut handle_n, native_id), (&mut handle, ext_id)] {
        h.send(EngineCommand::Param { instance: id, id: mix, value: 0.8 }).unwrap();
        h.send(EngineCommand::Param { instance: id, id: feedback, value: 0.7 }).unwrap();
        h.send(EngineCommand::Play).unwrap();
    }
    let a = render(&mut engine, 200);
    let b = render(&mut engine_n, 200);
    assert!(rms(&a[30_000..]) > 0.01, "sounds");
    let d = max_diff(&a, &b);
    assert!(d < 1e-5, "parameter events behave like the native path (max diff {d})");
    // a parameter change mid-flight too
    handle.send(EngineCommand::Param { instance: ext_id, id: mix, value: 0.0 }).unwrap();
    handle_n.send(EngineCommand::Param { instance: native_id, id: mix, value: 0.0 }).unwrap();
    let a = render(&mut engine, 60);
    let b = render(&mut engine_n, 60);
    let d = max_diff(&a, &b);
    assert!(d < 1e-5, "mid-flight parameter change matches (max diff {d})");
}

/// Values at which every readout is compared (discrete parameters snap to their steps).
const TEXT_PROBES: [f32; 6] = [0.0, 0.13, 0.25, 0.5, 0.77, 1.0];

/// Discrete and stepped parameters take whole steps through the ABI.
fn snapped(d: &rawdawg_types::ParamDescriptor, n: f32) -> f32 {
    match d.mapping {
        rawdawg_types::ParamMapping::Discrete { .. } => d.to_normalized(d.to_physical(n)),
        rawdawg_types::ParamMapping::Linear { .. } if d.flags.stepped => d.to_normalized(d.to_physical(n).round()),
        _ => n,
    }
}

#[test]
fn hosted_readouts_are_the_plugins_own_text_and_discrete_parameters_keep_their_steps() {
    use rawdawg_types::ParamMapping;
    let mut fx = Fixture::new();
    let (mut readouts, mut discrete, mut stepped_ranges) = (0usize, 0usize, 0usize);
    for exported in rawdawg_plugin_api::exported_plugins() {
        let mut inst = fx.instance(&exported.clap_id);
        let mut native = exported.make().unwrap();
        // prepared like the plugin's own main-thread instance (the synth sizes its voice pool)
        native.prepare(&PrepareSpec::new(48_000.0, 512, BusLayout::Stereo)).unwrap();
        let native_descs = native.descriptors();
        let wrapper = rawdawg_plugin_api::wrap::wrapper_descriptors(exported.is_synth(), exported.has_mix);
        for nd in native_descs.iter().filter(|d| !d.flags.hidden).chain(wrapper.iter()) {
            let hd = inst.descriptors().iter().find(|d| d.id == nd.id).unwrap_or_else(|| panic!("{} {}", exported.clap_id, nd.key));
            match nd.mapping {
                ParamMapping::Discrete { steps } => {
                    // declared as integer steps 0..steps-1, so hosts offer every choice (CLAP
                    // cannot tell a two-way choice from a switch)
                    let expected = if steps == 2 { ParamMapping::Toggle } else { ParamMapping::Discrete { steps } };
                    assert_eq!(hd.mapping, expected, "{} {}", exported.clap_id, nd.key);
                    // steps above 1 reach the plugin (a stepped 0..1 range would truncate them),
                    // and read back as the native processor reads them back
                    for step in [steps - 1, 2.min(steps - 1)] {
                        let n = nd.to_normalized(f64::from(step));
                        assert!(inst.set_param(nd.id, n));
                        inst.refresh_values();
                        native.set_param(nd.id, n);
                        // synth layer slots the default patch does not use have no value
                        let Some(expected) = native.param(nd.id) else { continue };
                        assert!((inst.param(nd.id).unwrap() - expected).abs() < 1e-6, "{} {} step {step}: {:?} vs {expected}", exported.clap_id, nd.key, inst.param(nd.id));
                        discrete += 1;
                    }
                }
                ParamMapping::Toggle => assert_eq!(hd.mapping, ParamMapping::Toggle, "{} {}", exported.clap_id, nd.key),
                ParamMapping::Linear { min, max } if nd.flags.stepped && min != 0.0 => {
                    // stepped ranges keep their integer units (lookahead 1–10 ms, shimmer −12…+24)
                    assert_eq!(hd.mapping, ParamMapping::Linear { min, max }, "{} {}", exported.clap_id, nd.key);
                    assert!(hd.flags.stepped);
                    let n = nd.to_normalized(min + 1.0);
                    assert!(inst.set_param(nd.id, n));
                    inst.refresh_values();
                    assert!((inst.param(nd.id).unwrap() - n).abs() < 1e-6, "{} {} second step", exported.clap_id, nd.key);
                    stepped_ranges += 1;
                }
                _ => {}
            }
            for n in TEXT_PROBES.map(|n| snapped(nd, n)) {
                let text = inst.param_text(nd.id, n);
                assert_eq!(text.as_deref(), Some(nd.format(n).as_str()), "{} {} at {n}", exported.clap_id, nd.key);
                assert_eq!(rawdawg_dsp::processor::display_value(&inst, hd, n), nd.format(n));
                readouts += 1;
            }
        }
        assert_eq!(inst.param_text(rawdawg_types::ParamId(0xDEAD_BEEF), 0.5), None, "unknown ids have no text");
        if exported.is_synth() {
            // what another host shows for the synth's choices and counts (issue #28)
            use rawdawg_synthesizer::params as sp;
            let text = |id: u32, n: f32| inst.param_text(rawdawg_types::ParamId(id), n);
            assert_eq!(text(sp::FILTER_MODE, 0.0).as_deref(), Some("Low-pass"));
            assert_eq!(text(sp::VOICE_MODE, 1.0).as_deref(), Some("Legato"));
            assert_eq!(text(sp::TRANSPOSE, 0.5).as_deref(), Some("+0 st"));
            assert_eq!(text(sp::MAX_VOICES, 31.0 / 63.0).as_deref(), Some("32"));
            assert_eq!(text(sp::layer_param(0, sp::L_WAVE), 2.0 / 3.0).as_deref(), Some("Saw"));
        }
    }
    assert!(discrete >= 20, "discrete read-backs checked: {discrete}");
    assert_eq!(stepped_ranges, 2, "limiter lookahead and reverb shimmer pitch");
    assert!(readouts > 500, "readouts checked: {readouts}");
}
