//! T-PLUGIN (CLAP export): load the exported CLAP entry as a host, enumerate
//! every exported plugin, instantiate, activate, process real audio and round
//! trip state through the ABI streams. Covers the synth and every effect family.

use clack_extensions::audio_ports::{AudioPortInfoBuffer, PluginAudioPorts};
use clack_extensions::latency::PluginLatency;
use clack_extensions::params::PluginParams;
use clack_extensions::state::PluginState;
use clack_extensions::tail::{PluginTail, TailLength};
use clack_host::events::event_types::*;
use clack_host::prelude::*;
use rawdawg_clap::{RawdawgEntry, SYNTH_CLAP_ID, WRAP_BYPASS, WRAP_MIX, exported_plugins};
use rawdawg_dsp::analysis::dominant_frequency;
use rawdawg_dsp::math::{peak, rms};

struct Shared;
impl<'a> SharedHandler<'a> for Shared {
    fn request_restart(&self) {}
    fn request_process(&self) {}
    fn request_callback(&self) {}
}
struct Host;
impl HostHandlers for Host {
    type Shared<'a> = Shared;
    type MainThread<'a> = ();
    type AudioProcessor<'a> = ();
}

fn host_info() -> HostInfo {
    HostInfo::new("rawDAWg test host", "rawDAWg", "https://github.com/leonardo-aa/rawDAWg", "0.1.0").unwrap()
}

fn built_cdylib_path() -> std::path::PathBuf {
    // target/<profile>/deps/<test> → target/<profile>/
    let exe = std::env::current_exe().unwrap();
    let dir = exe.parent().unwrap().parent().unwrap();
    let name = if cfg!(target_os = "windows") { "rawdawg_clap.dll" } else { "librawdawg_clap.so" };
    dir.join(name)
}

fn instantiate(entry: &PluginEntry, id: &str) -> PluginInstance<Host> {
    let factory = entry.get_plugin_factory().expect("plugin factory");
    let desc = factory.plugin_descriptors().find(|d| d.id().unwrap().to_bytes() == id.as_bytes()).unwrap_or_else(|| panic!("descriptor {id}"));
    PluginInstance::<Host>::new(|_| Shared, |_| (), entry, desc.id().unwrap(), &host_info()).expect("instantiate")
}

const SR: f64 = 48_000.0;
const BLOCK: usize = 512;

/// Run `blocks` stereo blocks of `input` through an effect instance, returning the output (L channel) and tail info.
fn run_effect(instance: &mut PluginInstance<Host>, input: &[f32], sidechain: Option<&[f32]>, param_events: &[(u32, f64)], blocks: usize) -> (Vec<f32>, Vec<f32>, TailLength) {
    let config = PluginAudioConfiguration { sample_rate: SR, min_frames_count: 1, max_frames_count: BLOCK as u32 };
    let processor = instance.activate(|_, _| (), config).expect("activate");
    let mut processor = processor.start_processing().expect("start");
    let mut in_l = vec![0.0f32; BLOCK];
    let mut in_r = vec![0.0f32; BLOCK];
    let mut sc_l = vec![0.0f32; BLOCK];
    let mut sc_r = vec![0.0f32; BLOCK];
    let mut out_l = vec![0.0f32; BLOCK];
    let mut out_r = vec![0.0f32; BLOCK];
    let mut ol = Vec::new();
    let mut or = Vec::new();
    let in_count = if sidechain.is_some() { 2 } else { 1 };
    let mut in_ports = AudioPorts::with_capacity(2, in_count);
    let mut out_ports = AudioPorts::with_capacity(2, 1);
    let mut out_events_buf = EventBuffer::new();
    let mut tail = TailLength::Finite(0);
    for b in 0..blocks {
        for i in 0..BLOCK {
            let x = input.get(b * BLOCK + i).copied().unwrap_or(0.0);
            in_l[i] = x;
            in_r[i] = x;
            let s = sidechain.and_then(|s| s.get(b * BLOCK + i).copied()).unwrap_or(0.0);
            sc_l[i] = s;
            sc_r[i] = s;
        }
        let mut in_events_buf = EventBuffer::new();
        if b == 0 {
            for &(id, v) in param_events {
                in_events_buf.push(&ParamValueEvent::new(0, ClapId::new(id), Pckn::match_all(), v));
            }
        }
        let in_events = InputEvents::from_buffer(&in_events_buf);
        let mut out_events = OutputEvents::from_buffer(&mut out_events_buf);
        let main_in =
            AudioPortBuffer { latency: 0, channels: AudioPortBufferType::f32_input_only([InputChannel::variable(&mut in_l), InputChannel::variable(&mut in_r)].into_iter()) };
        let input_audio = if sidechain.is_some() {
            let sc_in =
                AudioPortBuffer { latency: 0, channels: AudioPortBufferType::f32_input_only([InputChannel::variable(&mut sc_l), InputChannel::variable(&mut sc_r)].into_iter()) };
            in_ports.with_input_buffers([main_in, sc_in])
        } else {
            in_ports.with_input_buffers([main_in])
        };
        let mut output_audio = out_ports
            .with_output_buffers([AudioPortBuffer { latency: 0, channels: AudioPortBufferType::f32_output_only([out_l.as_mut_slice(), out_r.as_mut_slice()].into_iter()) }]);
        processor.process(&input_audio, &mut output_audio, &in_events, &mut out_events, None, None).expect("process");
        ol.extend_from_slice(&out_l);
        or.extend_from_slice(&out_r);
        if b == 0 {
            let mut handle = processor.plugin_handle();
            if let Some(t) = handle.get_extension::<PluginTail>() {
                tail = t.get(&handle);
            }
            let _ = &mut handle;
        }
    }
    let processor = processor.stop_processing();
    instance.deactivate(processor);
    (ol, or, tail)
}

fn sine(freq: f32, frames: usize, amp: f32) -> Vec<f32> {
    (0..frames).map(|i| amp * (2.0 * std::f32::consts::PI * freq * i as f32 / SR as f32).sin()).collect()
}

fn exercise_synth(entry: &PluginEntry) {
    let mut instance = instantiate(entry, SYNTH_CLAP_ID);
    let factory = entry.get_plugin_factory().unwrap();
    let desc = factory.plugin_descriptors().find(|d| d.id().unwrap().to_bytes() == SYNTH_CLAP_ID.as_bytes()).unwrap();
    assert_eq!(desc.name().unwrap().to_str().unwrap(), "rawDAWg Synth");
    assert_eq!(desc.vendor().unwrap().to_str().unwrap(), "rawDAWg");

    // Parameters are visible and round-trip text.
    let params: PluginParams = instance.plugin_handle().get_extension().expect("params extension");
    let count = params.count(&instance.plugin_handle());
    assert!(count > 20, "param count {count}");
    let cutoff = ClapId::new(rawdawg_synthesizer::params::FILTER_CUTOFF);
    let v = params.get_value(&instance.plugin_handle(), cutoff).expect("cutoff value");
    assert!((0.0..=1.0).contains(&v));
    let mut buf = [0u8; 64];
    let text = params.value_to_text(&instance.plugin_handle(), cutoff, v, &mut buf).expect("text");
    let text = std::str::from_utf8(text).unwrap();
    assert!(text.contains("Hz"), "{text:?}");
    // the synth exposes no wrapper params
    assert!(params.get_value(&instance.plugin_handle(), ClapId::new(WRAP_BYPASS)).is_none());

    // State save → load round trip through the ABI streams.
    let state: PluginState = instance.plugin_handle().get_extension().expect("state extension");
    let mut saved = Vec::new();
    state.save(&instance.plugin_handle(), &mut saved).expect("save");
    assert!(saved.len() > 100 && saved.starts_with(b"{"), "state is the JSON patch");
    let mut cursor = std::io::Cursor::new(saved.clone());
    state.load(&instance.plugin_handle(), &mut cursor).expect("load");
    let mut garbage = std::io::Cursor::new(b"not a patch".to_vec());
    assert!(state.load(&instance.plugin_handle(), &mut garbage).is_err(), "invalid state must be rejected");

    // Ports: notes in, stereo out, no audio input.
    let ports: PluginAudioPorts = instance.plugin_handle().get_extension().expect("audio ports");
    assert_eq!(ports.count(&instance.plugin_handle(), true), 0);
    assert_eq!(ports.count(&instance.plugin_handle(), false), 1);

    // Activate and render: note on A4 for 1 s at 48 kHz in 512-frame blocks.
    let config = PluginAudioConfiguration { sample_rate: SR, min_frames_count: 1, max_frames_count: 512 };
    let processor = instance.activate(|_, _| (), config).expect("activate");
    let mut processor = processor.start_processing().expect("start");
    let mut out_l = vec![0.0f32; 512];
    let mut out_r = vec![0.0f32; 512];
    let mut rendered = Vec::new();
    let mut ports = AudioPorts::with_capacity(2, 1);
    let mut in_ports = AudioPorts::with_capacity(0, 0);
    let mut out_events_buf = EventBuffer::new();
    for block in 0..94 {
        let note_on = NoteOnEvent::new(0, Pckn::new(0u16, 0u16, 69u16, 7u32), 0.9);
        let note_off = NoteOffEvent::new(0, Pckn::new(0u16, 0u16, 69u16, 7u32), 0.0);
        let mut in_events_buf = EventBuffer::new();
        if block == 0 {
            in_events_buf.push(&note_on);
        }
        if block == 60 {
            in_events_buf.push(&note_off);
        }
        let in_events = InputEvents::from_buffer(&in_events_buf);
        let mut out_events = OutputEvents::from_buffer(&mut out_events_buf);
        let input_audio = in_ports.with_input_buffers::<[AudioPortBuffer<std::iter::Empty<InputChannel<f32>>, std::iter::Empty<InputChannel<f64>>>; 0], _, _, _>([]);
        let mut output_audio =
            ports.with_output_buffers([AudioPortBuffer { latency: 0, channels: AudioPortBufferType::f32_output_only([out_l.as_mut_slice(), out_r.as_mut_slice()].into_iter()) }]);
        processor.process(&input_audio, &mut output_audio, &in_events, &mut out_events, None, None).expect("process");
        rendered.extend_from_slice(&out_l);
    }
    let processor = processor.stop_processing();
    instance.deactivate(processor);
    let sounding = &rendered[4096..30_000];
    assert!(rms(sounding) > 0.02, "plugin rendered audio");
    let f = dominant_frequency(sounding, SR);
    assert!((f - 440.0).abs() < 0.5, "{f}");
    // released: silent after note-off (block 60 = frame 30720) + 180 ms release
    assert_eq!(peak(&rendered[30_720 + 10_000..]), 0.0, "note released after note-off");
    assert!(rendered.iter().all(|s| s.is_finite()));
}

/// Every effect family: descriptor, ports, params, state round trip and a real render that
/// differs from (or, for the stereo tool at default, equals) the input and stays finite.
fn exercise_every_effect(entry: &PluginEntry) {
    let factory = entry.get_plugin_factory().expect("plugin factory");
    let ids: Vec<String> = factory.plugin_descriptors().map(|d| d.id().unwrap().to_str().unwrap().to_owned()).collect();
    let expected: Vec<String> = exported_plugins().iter().map(|e| e.clap_id.clone()).collect();
    assert_eq!(ids, expected, "factory exposes every exported plugin in order");
    assert_eq!(ids.len(), 17, "synth + 16 listed effect families");
    assert!(ids.iter().all(|i| i == SYNTH_CLAP_ID || i.starts_with("rawdawg.fx.")));
    assert!(!ids.iter().any(|i| i.contains("test")), "test effects are not exported");

    let input = sine(220.0, BLOCK * 40, 0.4);
    for exported in exported_plugins().iter().filter(|e| !e.is_synth()) {
        let id = exported.clap_id.as_str();
        let desc = factory.plugin_descriptors().find(|d| d.id().unwrap().to_bytes() == id.as_bytes()).unwrap();
        assert_eq!(desc.name().unwrap().to_str().unwrap(), exported.display_name(), "{id}");
        assert!(desc.features().any(|f| f.to_bytes() == b"audio-effect"), "{id} advertises audio-effect");
        let mut instance = instantiate(entry, id);
        let handle = instance.plugin_handle();
        let ports: PluginAudioPorts = handle.get_extension().expect("audio ports");
        assert_eq!(ports.count(&handle, true), if exported.has_sidechain { 2 } else { 1 }, "{id} inputs");
        assert_eq!(ports.count(&handle, false), 1, "{id} outputs");
        let mut info_buf = AudioPortInfoBuffer::new();
        let main_in = ports.get(&handle, 0, true, &mut info_buf).expect("main input info");
        assert_eq!(main_in.channel_count, 2);
        let params: PluginParams = handle.get_extension().expect("params");
        let count = params.count(&handle);
        assert!(count >= 2, "{id} params {count}");
        let bypass = params.get_value(&handle, ClapId::new(WRAP_BYPASS)).unwrap_or_else(|| panic!("{id} exposes bypass"));
        assert_eq!(bypass, 0.0);
        let mix = params.get_value(&handle, ClapId::new(WRAP_MIX));
        assert_eq!(mix.is_some(), exported.has_mix, "{id} mix exposure follows the catalogue");
        // every exposed param formats and re-parses
        let mut seen_bypass_flag = false;
        for i in 0..count {
            let mut ibuf = clack_extensions::params::ParamInfoBuffer::new();
            let info = params.get_info(&handle, i, &mut ibuf).unwrap_or_else(|| panic!("{id} param {i}"));
            let pid = info.id;
            seen_bypass_flag |= info.flags.contains(clack_extensions::params::ParamInfoFlags::IS_BYPASS);
            let v = params.get_value(&handle, pid).unwrap_or_else(|| panic!("{id} value of {pid:?}"));
            assert!((0.0..=1.0).contains(&v), "{id} {pid:?} = {v}");
            let mut tbuf = [0u8; 96];
            let text = params.value_to_text(&handle, pid, v, &mut tbuf).unwrap_or_else(|_| panic!("{id} text of {pid:?}"));
            assert!(!text.is_empty(), "{id} {pid:?}");
        }
        assert!(seen_bypass_flag, "{id} flags its bypass parameter");
        // state round trip: envelope with wrapper params, and app-style bare processor state
        let state: PluginState = handle.get_extension().expect("state");
        let mut saved = Vec::new();
        state.save(&handle, &mut saved).expect("save");
        let json: serde_json::Value = serde_json::from_slice(&saved).expect("JSON envelope");
        assert_eq!(json["rawdawg_clap"], 1, "{id}");
        assert_eq!(json["type_id"], exported.native_type_id, "{id}");
        assert!(json["processor"].is_object(), "{id} carries the processor state");
        state.load(&handle, &mut std::io::Cursor::new(saved.clone())).expect("load envelope");
        let bare = serde_json::to_vec(&json["processor"]).unwrap();
        state.load(&handle, &mut std::io::Cursor::new(bare)).expect("load bare processor state (app preset)");
        assert!(state.load(&handle, &mut std::io::Cursor::new(b"\x00garbage".to_vec())).is_err(), "{id} rejects garbage");
        let mut wrong = json.clone();
        wrong["type_id"] = serde_json::Value::String("native.fx.somethingelse".into());
        assert!(state.load(&handle, &mut std::io::Cursor::new(serde_json::to_vec(&wrong).unwrap())).is_err(), "{id} rejects another plugin's state");

        // latency before activation is the default-prepared value
        let latency: PluginLatency = instance.plugin_handle().get_extension().expect("latency");
        let lat0 = latency.get(&instance.plugin_handle());
        let (out_l, out_r, tail) = run_effect(&mut instance, &input, None, &[], 40);
        assert!(out_l.iter().chain(out_r.iter()).all(|s| s.is_finite()), "{id} finite");
        assert!(rms(&out_l[BLOCK * 8..]) > 0.001, "{id} produces audio from a steady sine");
        let lat = latency.get(&instance.plugin_handle());
        assert_eq!(lat, lat0, "{id} latency is stable across activation at 48 kHz");
        if lat > 0 {
            assert!(matches!(tail, TailLength::Finite(n) if n >= lat) || matches!(tail, TailLength::Infinite), "{id} tail covers its latency: {tail:?}");
        }
        // bypass: output equals input delayed by the latency
        let (b_l, _, b_tail) = run_effect(&mut instance, &input, None, &[(WRAP_BYPASS, 1.0)], 40);
        assert_eq!(b_tail, TailLength::Finite(0), "{id} bypassed → no tail");
        let l = lat as usize;
        let n = BLOCK * 30;
        let err: f32 = (0..n).map(|i| (b_l[i + l] - input[i]).abs()).fold(0.0, f32::max);
        assert!(err < 1e-5, "{id} bypass is latency-aligned dry (max err {err})");
        // the test state above restored bypass=0 on the main thread; the renders used event-driven bypass only
        if exported.has_mix {
            let (m_l, _, _) = run_effect(&mut instance, &input, None, &[(WRAP_MIX, 0.0)], 40);
            // the mix smoother (10 ms) has settled well before frame 10 blocks in
            let err: f32 = (BLOCK * 10..n).map(|i| (m_l[i + l] - input[i]).abs()).fold(0.0, f32::max);
            assert!(err < 1e-3, "{id} mix 0 is latency-aligned dry (max err {err})");
        }
    }
}

/// Behaviour spot checks: gain, sidechain, latency reporting.
fn exercise_effect_behaviour(entry: &PluginEntry) {
    let input = sine(220.0, BLOCK * 40, 0.4);
    // Stereo tool: output gain -inf → silence, default → identity.
    {
        let mut instance = instantiate(entry, "rawdawg.fx.stereo");
        let (out_l, _, _) = run_effect(&mut instance, &input, None, &[], 40);
        let err: f32 = (0..BLOCK * 30).map(|i| (out_l[i] - input[i]).abs()).fold(0.0, f32::max);
        assert!(err < 1e-5, "stereo tool at default is identity ({err})");
        let gain_id = rawdawg_effects::make_effect("native.fx.stereo", None).unwrap().descriptors().iter().find(|d| d.key == "gain").unwrap().id.0;
        let (quiet, _, _) = run_effect(&mut instance, &input, None, &[(gain_id, 0.0)], 40);
        assert!(rms(&quiet[BLOCK * 4..]) < rms(&input[BLOCK * 4..]) * 0.5, "gain at minimum attenuates");
    }
    // Compressor with external sidechain: a loud sidechain burst ducks a quiet main signal.
    {
        let mut instance = instantiate(entry, "rawdawg.fx.compressor");
        let descs = rawdawg_effects::make_effect("native.fx.compressor", None).unwrap().descriptors();
        let id_of = |key: &str| descs.iter().find(|d| d.key == key).unwrap().id.0;
        let main = sine(220.0, BLOCK * 40, 0.05);
        let mut sc = vec![0.0f32; BLOCK * 40];
        for (i, s) in sc.iter_mut().enumerate().skip(BLOCK * 20) {
            *s = 0.9 * (2.0 * std::f32::consts::PI * 100.0 * i as f32 / SR as f32).sin();
        }
        let thr = descs.iter().find(|d| d.key == "threshold").unwrap();
        let events = [(id_of("sidechain"), 1.0), (id_of("threshold"), f64::from(thr.parse("-40").expect("threshold parses dB"))), (id_of("ratio"), 1.0)];
        let (out_l, _, _) = run_effect(&mut instance, &main, Some(&sc), &events, 40);
        let before = rms(&out_l[BLOCK * 10..BLOCK * 19]);
        let after = rms(&out_l[BLOCK * 25..BLOCK * 39]);
        assert!(after < before * 0.7, "sidechain ducks the main signal: before {before} after {after}");
    }
    // Pitch shifter reports its STFT latency.
    {
        let mut instance = instantiate(entry, "rawdawg.fx.pitch");
        let latency: PluginLatency = instance.plugin_handle().get_extension().expect("latency");
        assert_eq!(latency.get(&instance.plugin_handle()), 2048, "phase vocoder frame latency");
    }
    // Reverb has a tail after the input stops.
    {
        let mut instance = instantiate(entry, "rawdawg.fx.reverb");
        let mut burst = sine(220.0, BLOCK * 4, 0.5);
        burst.resize(BLOCK * 40, 0.0);
        let (out_l, _, tail) = run_effect(&mut instance, &burst, None, &[], 40);
        assert!(rms(&out_l[BLOCK * 8..BLOCK * 12]) > 1e-4, "reverb rings after the burst");
        assert!(!matches!(tail, TailLength::Finite(0)), "reverb reports a tail: {tail:?}");
    }
}

fn exercise(entry: &PluginEntry) {
    exercise_synth(entry);
    exercise_every_effect(entry);
    exercise_effect_behaviour(entry);
}

#[test]
fn in_process_entry_round_trip() {
    let entry = PluginEntry::load_from_clack::<RawdawgEntry>(std::ffi::CString::new("rawdawg.clap").unwrap().as_c_str()).expect("load entry");
    exercise(&entry);
}

#[test]
fn built_shared_library_exports_clap_entry() {
    let path = built_cdylib_path();
    assert!(path.is_file(), "expected built CLAP library at {}", path.display());
    // SAFETY: loading our own freshly built library from the cargo target directory.
    let entry = unsafe { PluginEntry::load(&path) }.expect("dlopen + clap_entry");
    exercise(&entry);
}

#[test]
fn id_mapping_is_bijective() {
    for e in exported_plugins() {
        assert_eq!(rawdawg_clap::clap_id_for_native(e.native_type_id).as_deref(), Some(e.clap_id.as_str()));
        assert_eq!(rawdawg_clap::native_type_id_for_clap(&e.clap_id).as_deref(), Some(e.native_type_id));
    }
    assert!(rawdawg_clap::clap_id_for_native("native.fx.test-latency").is_some(), "mapping is mechanical; export filtering happens in the catalogue");
    assert!(rawdawg_clap::native_type_id_for_clap("org.other.plugin").is_none());
}
