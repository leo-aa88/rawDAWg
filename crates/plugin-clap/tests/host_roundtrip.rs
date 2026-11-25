//! T-PLUGIN (M0 subset): load the exported CLAP entry as a host, instantiate,
//! activate, feed a note and verify real audio, param access and state round trip.

use clack_extensions::params::PluginParams;
use clack_extensions::state::PluginState;
use clack_host::events::event_types::*;
use clack_host::prelude::*;
use rawdawg_clap::{Entry, SYNTH_CLAP_ID};
use rawdawg_dsp::analysis::dominant_frequency;

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

fn exercise(entry: &PluginEntry) {
    let factory = entry.get_plugin_factory().expect("plugin factory");
    let desc = factory.plugin_descriptors().find(|d| d.id().unwrap().to_bytes() == SYNTH_CLAP_ID.as_bytes()).expect("rawdawg.synth descriptor");
    assert_eq!(desc.name().unwrap().to_str().unwrap(), "rawDAWg Synth");
    assert_eq!(desc.vendor().unwrap().to_str().unwrap(), "rawDAWg");
    let mut instance = PluginInstance::<Host>::new(|_| Shared, |_| (), entry, desc.id().unwrap(), &host_info()).expect("instantiate");

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

    // State save → load round trip through the ABI streams.
    let state: PluginState = instance.plugin_handle().get_extension().expect("state extension");
    let mut saved = Vec::new();
    state.save(&instance.plugin_handle(), &mut saved).expect("save");
    assert!(saved.len() > 100 && saved.starts_with(b"{"), "state is the JSON patch");
    let mut cursor = std::io::Cursor::new(saved.clone());
    state.load(&instance.plugin_handle(), &mut cursor).expect("load");
    let mut garbage = std::io::Cursor::new(b"not a patch".to_vec());
    assert!(state.load(&instance.plugin_handle(), &mut garbage).is_err(), "invalid state must be rejected");

    // Activate and render: note on A4 for 1 s at 48 kHz in 512-frame blocks.
    let config = PluginAudioConfiguration { sample_rate: 48_000.0, min_frames_count: 1, max_frames_count: 512 };
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
        let status = processor.process(&input_audio, &mut output_audio, &in_events, &mut out_events, None, None).expect("process");
        let _ = status;
        rendered.extend_from_slice(&out_l);
    }
    let processor = processor.stop_processing();
    instance.deactivate(processor);
    let sounding = &rendered[4096..30_000];
    assert!(rawdawg_dsp::math::rms(sounding) > 0.02, "plugin rendered audio");
    let f = dominant_frequency(sounding, 48_000.0);
    assert!((f - 440.0).abs() < 0.5, "{f}");
    // released: silent after note-off (block 60 = frame 30720) + 180 ms release
    assert_eq!(rawdawg_dsp::math::peak(&rendered[30_720 + 10_000..]), 0.0, "note released after note-off");
    assert!(rendered.iter().all(|s| s.is_finite()));
}

#[test]
fn in_process_entry_round_trip() {
    let entry = PluginEntry::load_from_clack::<Entry>(std::ffi::CString::new("rawdawg.clap").unwrap().as_c_str()).expect("load entry");
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
