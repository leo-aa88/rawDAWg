//! T-PLUGIN (host side, VST3 bridge): host rawDAWg's own VST3 module through the engine
//! and compare against the native processors — the VST3 twin of `clap_host.rs`.

use rawdawg_audio_engine::commands::EngineCommand;
use rawdawg_audio_engine::engine::Engine;
use rawdawg_audio_engine::externals::{ExternalPlugins, PluginRole};
use rawdawg_audio_engine::plan::{compile, compile_with};
use rawdawg_dsp::analysis::dominant_frequency;
use rawdawg_dsp::math::{peak, rms};
use rawdawg_dsp::processor::{PrepareSpec, Processor};
use rawdawg_plugin_host::catalog::PluginDescription;
use rawdawg_plugin_host::vst3::{COMPONENT_STATE_KEY, Vst3Instance, Vst3ModuleCache, cid_hex, describe_vst3_module, module_binary};
use rawdawg_plugin_host::{Format, HostedInstance};
use rawdawg_project::{Instance, Note, PluginFormat, Project};
use rawdawg_types::NoteId;
use rawdawg_types::limits::EngineLimits;
use rawdawg_types::units::{SampleRate, Tick};
use std::collections::HashMap;
use std::path::PathBuf;

fn built_vst3_module() -> PathBuf {
    let status = std::process::Command::new(env!("CARGO")).args(["build", "-q", "-p", "rawdawg-plugin-vst3"]).status().expect("cargo build");
    assert!(status.success());
    let exe = std::env::current_exe().unwrap();
    let dir = exe.parent().unwrap().parent().unwrap();
    let name = if cfg!(target_os = "windows") { "rawdawg_vst3.dll" } else { "librawdawg_vst3.so" };
    let p = dir.join(name);
    assert!(p.is_file(), "{}", p.display());
    p
}

/// A standard `.vst3` bundle built from the module (what users install). Built once per
/// process and written through a temp name + rename: tests run in parallel threads and must
/// never `dlopen` a file another thread is still copying.
fn bundle_dir() -> PathBuf {
    static BUNDLE: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    BUNDLE
        .get_or_init(|| {
            let module = built_vst3_module();
            let base = std::env::temp_dir().join(format!("rawdawg-vst3host-{}", std::process::id()));
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

struct Fixture {
    cache: Vst3ModuleCache,
    library: rawdawg_plugin_host::catalog::ScannedLibrary,
}

impl Fixture {
    fn new() -> Self {
        let bundle = bundle_dir();
        // SAFETY: our own freshly built module.
        let library = unsafe { describe_vst3_module(&bundle) }.expect("describe");
        Self { cache: Vst3ModuleCache::default(), library }
    }
    fn description(&self, name: &str) -> &PluginDescription {
        self.library.plugins.iter().find(|p| p.name == name).unwrap_or_else(|| panic!("{name}"))
    }
    fn instance(&mut self, name: &str) -> Vst3Instance {
        // SAFETY: module described above.
        let module = unsafe { self.cache.load(&self.library.path) }.expect("load module");
        let desc = self.description(name).clone();
        Vst3Instance::new(module, &desc).expect("instantiate")
    }
}

struct Host {
    instances: HashMap<rawdawg_types::InstanceId, Vst3Instance>,
}

impl ExternalPlugins for Host {
    fn instantiate(&mut self, inst: &Instance, _role: PluginRole, spec: &PrepareSpec) -> Result<Option<Box<dyn Processor>>, String> {
        match self.instances.get_mut(&inst.id) {
            Some(live) => HostedInstance::activate_for_rt(live, spec).map(Some),
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
fn bundle_is_described_with_buses_params_and_stable_class_ids() {
    let fx = Fixture::new();
    assert_eq!(fx.library.plugins.len(), 17, "one description per audio module class");
    assert!(module_binary(&fx.library.path).is_some());
    let synth = fx.description("rawDAWg Synth");
    assert!(synth.is_instrument && !synth.is_effect);
    assert_eq!((synth.audio_inputs, synth.audio_outputs, synth.note_inputs), (0, 1, 1));
    assert!(synth.param_count > 20);
    assert_eq!(synth.vendor, "rawDAWg");
    assert!(synth.features.iter().any(|f| f == "Instrument"));
    let comp = fx.description("rawDAWg Compressor");
    assert!(comp.is_effect && !comp.is_instrument);
    assert_eq!((comp.audio_inputs, comp.audio_outputs), (2, 1), "main + aux sidechain");
    assert_eq!(comp.main_input_channels, 2);
    assert!(comp.has_gui, "the exported plugins have an editor (IPlugView)");
    let expected = rawdawg_plugin_api::exported_plugins();
    for p in &expected {
        let (pcid, _) = p.vst3_class_ids();
        let hex = cid_hex(&vst3::uid(pcid.0, pcid.1, pcid.2, pcid.3));
        assert!(fx.library.plugins.iter().any(|d| d.plugin_id == hex && d.name == p.name), "{} listed under its recorded class id", p.name);
    }
    assert!(fx.library.plugins.iter().all(|p| p.notes.is_empty()));
}

#[test]
fn instance_exposes_descriptors_params_and_state_envelope() {
    let mut fx = Fixture::new();
    let mut inst = fx.instance("rawDAWg Stereo");
    assert!(Processor::type_id(&inst).starts_with("vst3:"));
    assert_eq!(inst.layout().inputs, vec![2]);
    assert_eq!(inst.layout().outputs, vec![2]);
    assert!(!inst.layout().event_input);
    let descs = inst.descriptors();
    let native = rawdawg_effects::make_effect("native.fx.stereo", None).unwrap();
    let visible = native.descriptors().iter().filter(|d| !d.flags.hidden).count();
    assert_eq!(descs.len(), visible + 1, "native visible params + wrapper Bypass");
    let bypass = descs.iter().find(|d| d.flags.is_bypass).expect("bypass flagged");
    assert_eq!(inst.param(bypass.id), Some(0.0));
    let gain = native.descriptors().iter().find(|d| d.key == "gain").unwrap();
    let hosted = descs.iter().find(|d| d.id == gain.id).expect("same parameter ids");
    assert_eq!(hosted.name, gain.name);
    assert!((inst.param(gain.id).unwrap() - gain.default_normalized).abs() < 1e-5);
    assert!(inst.set_param(gain.id, 0.25));
    inst.refresh_values();
    assert!((inst.param(gain.id).unwrap() - 0.25).abs() < 1e-4, "{:?}", inst.param(gain.id));
    let saved = inst.save_state();
    let env: serde_json::Value = serde_json::from_slice(&saved).unwrap();
    assert_eq!(env["format"], "vst3");
    let raw = rawdawg_plugin_host::b64::decode(env[COMPONENT_STATE_KEY].as_str().unwrap()).unwrap();
    let inner: serde_json::Value = serde_json::from_slice(&raw).expect("component state is the shared envelope");
    assert_eq!(inner["rawdawg_state"], 1);
    assert!(inst.set_param(gain.id, 0.9));
    inst.load_state(&saved).expect("load");
    assert!((inst.param(gain.id).unwrap() - 0.25).abs() < 1e-4, "state restored");
    let other = serde_json::json!({ "format": "vst3", "plugin_id": "00000000000000000000000000000000", COMPONENT_STATE_KEY: "AAAA" });
    assert!(inst.load_state(&serde_json::to_vec(&other).unwrap()).is_err());
    assert!(inst.load_state(b"garbage").is_err());
    assert!(!inst.is_active());
}

#[test]
fn hosted_effect_matches_native_effect_and_migrates_between_plans() {
    let mut fx = Fixture::new();
    let limits = EngineLimits::DEFAULT;
    let mut native = project_with_notes(&[(0, 960, 69), (1920, 480, 72)]);
    native.mixer.channels[0].inserts.push(Instance::native("native.fx.delay", serde_json::json!({})));
    let (mut engine_n, mut handle_n) = Engine::new(SampleRate(48_000), BLOCK, &limits);
    engine_n.install_plan(compile(&native, 1, BLOCK, &limits, &Default::default(), &rawdawg_audio_engine::NoAssets).unwrap().plan);
    handle_n.send(EngineCommand::Play).unwrap();
    let reference = render(&mut engine_n, 200);
    assert!(rms(&reference[20_000..]) > 0.01);

    let mut hosted = native.clone();
    let desc = fx.description("rawDAWg Delay").clone();
    let ext = Instance::external(PluginFormat::Vst3, &desc.plugin_id, &desc.name, &desc.vendor, &desc.version, None);
    let ext_id = ext.id;
    hosted.mixer.channels[0].inserts[0] = ext;
    let mut host = Host { instances: HashMap::new() };
    host.instances.insert(ext_id, fx.instance("rawDAWg Delay"));
    let (mut engine, mut handle) = Engine::new(SampleRate(48_000), BLOCK, &limits);
    let out = compile_with(&hosted, 1, BLOCK, &limits, &Default::default(), &rawdawg_audio_engine::NoAssets, &mut host).unwrap();
    assert!(out.missing_plugins.is_empty(), "{:?}", out.missing_plugins);
    assert!(host.instances[&ext_id].is_active());
    drop(engine.install_plan(out.plan));
    handle.send(EngineCommand::Play).unwrap();
    let first = render(&mut engine, 100);
    let d = max_diff(&first, &reference[..first.len()]);
    assert!(d < 1e-5, "hosted VST3 delay equals the native delay (max diff {d})");

    let out2 = compile_with(&hosted, 2, BLOCK, &limits, &out.fingerprints, &rawdawg_audio_engine::NoAssets, &mut host).unwrap();
    let old = engine.install_plan(out2.plan);
    handle.send(EngineCommand::Stop).unwrap();
    let second = render(&mut engine, 100);
    drop(old);
    host.instances.get_mut(&ext_id).unwrap().collect_returns();
    assert!(host.instances[&ext_id].is_active(), "same activation after migration");
    assert!(rms(&second[BLOCK * 20..BLOCK * 60]) > 1e-3, "delay tail survived the swap");
    assert_eq!(host.instances[&ext_id].problem_count(), 0);

    let current = engine.install_plan(compile(&project_with_notes(&[]), 3, BLOCK, &limits, &Default::default(), &rawdawg_audio_engine::NoAssets).unwrap().plan);
    drop(current);
    let live = host.instances.get_mut(&ext_id).unwrap();
    live.collect_returns();
    assert!(!live.is_active(), "deactivated after its plan was dropped");
    let again = Vst3Instance::activate_for_rt(live, &PrepareSpec::new(48_000.0, BLOCK, rawdawg_dsp::BusLayout::Stereo)).expect("re-activate");
    assert!(live.is_active());
    drop(again);
    live.collect_returns();
    assert!(!live.is_active());
}

#[test]
fn realtime_param_events_and_inactive_sets_reach_the_hosted_plugin() {
    let mut fx = Fixture::new();
    let limits = EngineLimits::DEFAULT;
    let descs = rawdawg_effects::make_effect("native.fx.delay", None).unwrap().descriptors();
    let mix = descs.iter().find(|d| d.key == "mix").map(|d| d.id).unwrap();
    let feedback = descs.iter().find(|d| d.key == "feedback").map(|d| d.id).unwrap();
    let mut native = project_with_notes(&[(0, 960, 69), (1920, 480, 72)]);
    native.mixer.channels[0].inserts.push(Instance::native("native.fx.delay", serde_json::json!({})));
    let native_id = native.mixer.channels[0].inserts[0].id;
    let mut hosted = native.clone();
    let desc = fx.description("rawDAWg Delay").clone();
    let ext = Instance::external(PluginFormat::Vst3, &desc.plugin_id, &desc.name, &desc.vendor, &desc.version, None);
    let ext_id = ext.id;
    hosted.mixer.channels[0].inserts[0] = ext;
    let mut host = Host { instances: HashMap::new() };
    let mut live = fx.instance("rawDAWg Delay");
    // a set while inactive travels with the first block
    assert!(live.set_param(feedback, 0.7));
    host.instances.insert(ext_id, live);

    let (mut engine_n, mut handle_n) = Engine::new(SampleRate(48_000), BLOCK, &limits);
    engine_n.install_plan(compile(&native, 1, BLOCK, &limits, &Default::default(), &rawdawg_audio_engine::NoAssets).unwrap().plan);
    let (mut engine, mut handle) = Engine::new(SampleRate(48_000), BLOCK, &limits);
    engine.install_plan(compile_with(&hosted, 1, BLOCK, &limits, &Default::default(), &rawdawg_audio_engine::NoAssets, &mut host).unwrap().plan);
    handle_n.send(EngineCommand::Param { instance: native_id, id: feedback, value: 0.7 }).unwrap();
    for (h, id) in [(&mut handle_n, native_id), (&mut handle, ext_id)] {
        h.send(EngineCommand::Param { instance: id, id: mix, value: 0.8 }).unwrap();
        h.send(EngineCommand::Play).unwrap();
    }
    let a = render(&mut engine, 200);
    let b = render(&mut engine_n, 200);
    assert!(rms(&a[30_000..]) > 0.01);
    let d = max_diff(&a, &b);
    assert!(d < 1e-5, "parameter events behave like the native path (max diff {d})");
    handle.send(EngineCommand::Param { instance: ext_id, id: mix, value: 0.0 }).unwrap();
    handle_n.send(EngineCommand::Param { instance: native_id, id: mix, value: 0.0 }).unwrap();
    let a = render(&mut engine, 60);
    let b = render(&mut engine_n, 60);
    let d = max_diff(&a, &b);
    assert!(d < 1e-5, "mid-flight parameter change matches (max diff {d})");
}

#[test]
fn hosted_instrument_plays_the_sequencer_notes() {
    let mut fx = Fixture::new();
    let limits = EngineLimits::DEFAULT;
    let mut p = project_with_notes(&[(0, 1920, 69)]);
    let desc = fx.description("rawDAWg Synth").clone();
    let ext = Instance::external(PluginFormat::Vst3, &desc.plugin_id, &desc.name, &desc.vendor, &desc.version, None);
    let ext_id = ext.id;
    p.channels[0].instrument = ext;
    let synth = fx.instance("rawDAWg Synth");
    assert!(synth.is_instrument() && synth.layout().event_input);
    let mut host = Host { instances: HashMap::new() };
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
    assert_eq!(peak(&a[48_000 + 12_000..]), 0.0, "note released");
    assert_eq!(host.instances[&ext_id].problem_count(), 0);
    let _ = Format::Vst3;
}

/// Values at which every readout is compared (discrete and stepped parameters snap).
const TEXT_PROBES: [f32; 6] = [0.0, 0.13, 0.25, 0.5, 0.77, 1.0];

fn snapped(d: &rawdawg_types::ParamDescriptor, n: f32) -> f32 {
    match d.mapping {
        rawdawg_types::ParamMapping::Discrete { .. } => d.to_normalized(d.to_physical(n)),
        rawdawg_types::ParamMapping::Linear { .. } if d.flags.stepped => d.to_normalized(d.to_physical(n).round()),
        _ => n,
    }
}

#[test]
fn hosted_readouts_are_the_plugins_own_text_and_steps_match_the_native_parameters() {
    use rawdawg_types::ParamMapping;
    let mut fx = Fixture::new();
    let (mut readouts, mut discrete, mut with_units) = (0usize, 0usize, 0usize);
    for exported in rawdawg_plugin_api::exported_plugins() {
        let mut inst = fx.instance(&exported.name);
        let native = exported.make().unwrap();
        let wrapper = rawdawg_plugin_api::wrap::wrapper_descriptors(exported.is_synth(), exported.has_mix);
        for nd in native.descriptors().iter().filter(|d| !d.flags.hidden).chain(wrapper.iter()) {
            let hd = inst.descriptors().iter().find(|d| d.id == nd.id).unwrap_or_else(|| panic!("{} {}", exported.name, nd.key));
            let expected = match nd.mapping {
                ParamMapping::Discrete { steps: 2 } | ParamMapping::Toggle => Some(ParamMapping::Toggle),
                ParamMapping::Discrete { steps } => Some(ParamMapping::Discrete { steps }),
                ParamMapping::Linear { min, max } if nd.flags.stepped => Some(ParamMapping::Discrete { steps: (max - min) as u32 + 1 }),
                _ => None,
            };
            if let Some(expected) = expected {
                assert_eq!(hd.mapping, expected, "{} {}", exported.name, nd.key);
                discrete += 1;
            }
            for n in TEXT_PROBES.map(|n| snapped(nd, n)) {
                let native_text = nd.format(n);
                let text = inst.param_text(nd.id, n).unwrap_or_else(|| panic!("{} {} at {n}: no text", exported.name, nd.key));
                if text != native_text {
                    // VST3 reports the unit apart from the value string; a bare number gets it
                    let unit = text.strip_prefix(native_text.as_str()).and_then(|r| r.strip_prefix(' '));
                    assert!(
                        unit.is_some_and(|u| !u.is_empty() && !u.contains(' ')) && native_text.ends_with(|c: char| c.is_ascii_digit()),
                        "{} {} at {n}: {text:?} vs {native_text:?}",
                        exported.name,
                        nd.key
                    );
                    with_units += 1;
                }
                assert_eq!(rawdawg_dsp::processor::display_value(&inst, hd, n), text);
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
        // a state load forgets cached readouts and still answers afterwards
        let saved = inst.save_state();
        inst.load_state(&saved).expect("reload own state");
        let first = native.descriptors().iter().find(|d| !d.flags.hidden).unwrap();
        assert_eq!(inst.param_text(first.id, 0.5).as_deref(), Some(first.format(0.5).as_str()));
    }
    assert!(discrete >= 30, "step mappings checked: {discrete}");
    assert!(readouts > 500, "readouts checked: {readouts}");
    // the unit is added only to bare numbers (checked per readout above); rawDAWg's own plugins
    // show their stepped counts with units now (Transpose "+0 st", not "48"), so few or none need it
    assert!(with_units < readouts / 4, "unit labels appended to bare numbers: {with_units}");
}
