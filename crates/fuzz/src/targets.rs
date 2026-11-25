//! Fuzz targets: one per untrusted input surface. Each `run` must not panic on any input
//! and asserts the properties the format promises (finite audio, round trips, bounds).

use crate::{Mode, Target};
use rawdawg_dsp::processor::{PrepareSpec, ProcessContext, Processor, TransportView};
use rawdawg_dsp::{AudioBlock, BusLayout};
use rawdawg_types::events::{EventSource, TimedEvent};
use std::path::{Path, PathBuf};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures")
}

fn read(p: &Path) -> Option<Vec<u8>> {
    std::fs::read(p).ok()
}

fn project_seeds() -> Vec<Vec<u8>> {
    let mut v: Vec<Vec<u8>> =
        ["demo.rawdawg", "demo-song.rawdawg", "demo-sampler.rawdawg", "demo-graph.rawdawg"].iter().filter_map(|d| read(&fixtures().join(d).join("project.json"))).collect();
    let p = rawdawg_project::Project::new_default("seed", serde_json::to_value(rawdawg_synthesizer::Patch::default()).unwrap_or_default());
    v.push(rawdawg_project::persist::project_to_json(&p));
    v
}

fn assert_finite(buf: &[f32], what: &str) {
    if let Some(i) = buf.iter().position(|v| !v.is_finite()) {
        panic!("{what}: non-finite output at sample {i}");
    }
}

/// Run a processor for `blocks` 256-frame blocks of a test signal (plus optional note).
fn exercise(p: &mut dyn Processor, blocks: usize, note: bool, what: &str) {
    let block = 256usize;
    if p.prepare(&PrepareSpec::new(48_000.0, block, BusLayout::Stereo)).is_err() {
        return;
    }
    let input: Vec<f32> = (0..block).map(|i| 0.5 * (i as f32 * 0.07).sin()).collect();
    let mut ol = vec![0.0f32; block];
    let mut or = vec![0.0f32; block];
    let transport = TransportView { playing: true, ..TransportView::stopped(48_000.0) };
    let on = [TimedEvent::note_on(0, EventSource::Host, 0, 1, 60, 0.9), TimedEvent::note_on(5, EventSource::Host, 0, 2, 67, 0.5)];
    for b in 0..blocks {
        let events: &[TimedEvent] = if note && b == 0 { &on } else { &[] };
        let inputs: [&[f32]; 2] = [&input, &input];
        let mut outs: [&mut [f32]; 2] = [&mut ol, &mut or];
        let mut ctx = ProcessContext { audio: AudioBlock { frames: block, inputs: &inputs, outputs: &mut outs, sidechain: &[] }, events, transport: &transport };
        p.process(&mut ctx);
        assert_finite(&ol, what);
        assert_finite(&or, what);
    }
}

fn run_project(bytes: &[u8]) {
    let Ok(p) = rawdawg_project::persist::parse_project(bytes) else { return };
    crate::mark_accepted();
    // a valid project serializes and reparses to the same model
    let again =
        rawdawg_project::persist::parse_project(&rawdawg_project::persist::project_to_json(&p)).unwrap_or_else(|e| panic!("valid project fails to reparse after save: {e}"));
    assert!(again == p, "project changes across save/load");
    // compile and render a few blocks in both modes: no panic, finite output
    let limits = rawdawg_types::limits::EngineLimits::DEFAULT;
    let Ok(out) = rawdawg_audio_engine::compile(&p, 1, 256, &limits, &Default::default(), &rawdawg_audio_engine::NoAssets) else { return };
    let (mut engine, mut handle) = rawdawg_audio_engine::Engine::new(p.sample_rate_hz, 256, &limits);
    let _worker = out.stream_worker;
    engine.install_plan(out.plan);
    let mut l = vec![0.0f32; 256];
    let mut r = vec![0.0f32; 256];
    for mode in [rawdawg_audio_engine::TransportMode::Pattern, rawdawg_audio_engine::TransportMode::Song] {
        let _ = handle.send(rawdawg_audio_engine::EngineCommand::SetMode(mode));
        let _ = handle.send(rawdawg_audio_engine::EngineCommand::Play);
        for _ in 0..6 {
            l.fill(0.0);
            r.fill(0.0);
            engine.render_block(&mut l, &mut r);
            assert_finite(&l, "engine");
            assert_finite(&r, "engine");
        }
        let _ = handle.send(rawdawg_audio_engine::EngineCommand::Stop);
    }
}

fn preset_seeds() -> Vec<Vec<u8>> {
    // unique per call: tests in one process build seeds concurrently
    static CALLS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("rawdawg-fuzz-presets-{}-{n}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let mut out = Vec::new();
    let patch = rawdawg_synthesizer::presets::factory_presets()[0].patch.clone();
    let synth = rawdawg_project::preset::PresetManifest::new_synth("Seed", &patch, &["fuzz".to_owned()]);
    let reverb = rawdawg_effects::preset_state(rawdawg_effects::reverb::TYPE_ID, &rawdawg_effects::presets(rawdawg_effects::reverb::TYPE_ID)[0])
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    let effect = rawdawg_project::preset::PresetManifest::new_effect("Seed FX", rawdawg_effects::reverb::TYPE_ID, reverb);
    let inserts = vec![
        rawdawg_project::Instance::native(rawdawg_effects::eq::TYPE_ID, serde_json::Value::Null),
        rawdawg_project::Instance::native(rawdawg_effects::delay::TYPE_ID, serde_json::Value::Null),
    ];
    let rack = rawdawg_project::preset::PresetManifest::new_rack("Seed rack", &inserts);
    let wav = read(&fixtures().join("impulse_48k.wav")).unwrap_or_default();
    for (i, (m, assets)) in [(synth, vec![]), (effect, vec![]), (rack, vec![(format!("{}.wav", rawdawg_project::preset::sha256_hex(&wav)), wav)])].into_iter().enumerate() {
        let path = dir.join(format!("seed{i}.rawdawgpreset"));
        let mut m = m;
        if !assets.is_empty() {
            m.assets = assets
                .iter()
                .map(|(n, b)| rawdawg_project::preset::PresetAssetRef { name: n.clone(), sha256: rawdawg_project::preset::sha256_hex(b), bytes: b.len() as u64 })
                .collect();
        }
        if rawdawg_project::preset::write_preset(&path, &m, &assets).is_ok()
            && let Some(b) = read(&path)
        {
            out.push(b);
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    out
}

fn run_preset(bytes: &[u8]) {
    let Ok(f) = rawdawg_project::preset::read_preset_from(std::io::Cursor::new(bytes)) else { return };
    crate::mark_accepted();
    let m = &f.manifest;
    assert!(f.assets.len() <= rawdawg_project::preset::MAX_ENTRIES, "asset count above the limit");
    let total: u64 = f.assets.iter().map(|(_, b)| b.len() as u64).sum();
    assert!(total <= rawdawg_project::preset::MAX_ASSET_TOTAL_BYTES, "assets above the size limit");
    match m.kind.as_str() {
        "synth" => {
            if let Ok(p) = rawdawg_synthesizer::Patch::from_json(&serde_json::to_vec(&m.state).unwrap_or_default()) {
                let mut s = rawdawg_synthesizer::Synth::default();
                if s.load_state(&serde_json::to_vec(&p).unwrap_or_default()).is_ok() {
                    exercise(&mut s, 2, true, "synth from preset");
                }
            }
        }
        "effect" => {
            if let Ok(mut fx) = rawdawg_effects::make_effect(&m.type_id, Some(&serde_json::to_vec(&m.state).unwrap_or_default())) {
                exercise(fx.as_mut(), 2, false, "effect from preset");
            }
        }
        "rack" => {
            for inst in m.rack_inserts() {
                if let Ok(mut fx) = rawdawg_effects::make_effect(inst.type_id.as_str(), Some(&serde_json::to_vec(&inst.state).unwrap_or_default())) {
                    exercise(fx.as_mut(), 1, false, "rack insert");
                }
            }
        }
        _ => {}
    }
}

/// Preset manifests as JSON: the target packs each mutated manifest into a valid archive
/// together with the seed asset, so mutations reach manifest validation and loading
/// instead of dying at the zip CRC check.
fn manifest_seeds() -> Vec<Vec<u8>> {
    preset_seeds().iter().filter_map(|b| rawdawg_project::preset::read_preset_from(std::io::Cursor::new(b)).ok()).filter_map(|f| serde_json::to_vec(&f.manifest).ok()).collect()
}

fn run_preset_manifest(bytes: &[u8]) {
    use std::io::Write;
    // the impulse asset every seed may reference (name = its hash)
    let wav = read(&fixtures().join("impulse_48k.wav")).unwrap_or_default();
    let mut buf = std::io::Cursor::new(Vec::new());
    {
        let mut zip = zip::ZipWriter::new(&mut buf);
        let opts = zip::write::SimpleFileOptions::default();
        if zip.start_file("manifest.json", opts).is_err() || zip.write_all(bytes).is_err() {
            return;
        }
        if zip.start_file(format!("assets/{}.wav", rawdawg_project::preset::sha256_hex(&wav)), opts).is_ok() {
            let _ = zip.write_all(&wav);
        }
        if zip.finish().is_err() {
            return;
        }
    }
    run_preset(buf.get_ref());
}

fn smf_seeds() -> Vec<Vec<u8>> {
    let mut v: Vec<Vec<u8>> = read(&fixtures().join("motif.mid")).into_iter().collect();
    if let Ok(p) = rawdawg_project::load_project_dir(&fixtures().join("demo-song.rawdawg"))
        && let Ok(b) = rawdawg_project::smf::export_song(&p)
    {
        v.push(b);
    }
    v
}

fn run_smf(bytes: &[u8]) {
    let Ok(imp) = rawdawg_project::smf::import(bytes, "fuzz") else { return };
    crate::mark_accepted();
    assert!(imp.length_ticks.0 >= 0, "negative import length");
    for t in &imp.tracks {
        for n in &t.notes {
            assert!(n.tick.0 >= 0 && n.duration_ticks.0 > 0 && n.pitch <= 127, "imported note out of range: {n:?}");
        }
    }
    assert!(imp.tempo.iter().all(|t| t.us_per_quarter.0 > 0), "imported tempo not positive");
}

fn run_sf2(bytes: &[u8]) {
    let Ok(font) = rawdawg_sampler::sf2::parse(bytes) else { return };
    crate::mark_accepted();
    let font = std::sync::Arc::new(font);
    let presets: Vec<(u16, u16)> = font.presets.iter().take(4).map(|p| (p.bank, p.preset)).collect();
    for (bank, preset) in presets {
        let patch = rawdawg_sampler::Sf2Patch { bank, preset, ..Default::default() };
        let mut player = rawdawg_sampler::Sf2Player::new(patch, Some(font.clone()));
        exercise(&mut player, 3, true, "sf2 player");
    }
}

fn audio_seeds() -> Vec<Vec<u8>> {
    ["sine_440_48k.wav", "impulse_48k.wav", "sine_440_48k.flac"]
        .iter()
        .filter_map(|f| read(&fixtures().join(f)))
        .map(|mut b| {
            b.truncate(64 * 1024); // short seeds keep each decode fast; truncation is itself a mutation
            b
        })
        .collect()
}

fn run_audio(bytes: &[u8]) {
    let ext = if bytes.starts_with(b"fLaC") {
        Some("flac")
    } else if bytes.starts_with(b"RIFF") {
        Some("wav")
    } else {
        None
    };
    let Ok(asset) = rawdawg_sampler::decode_bytes(bytes.to_vec(), ext, "fuzz") else { return };
    crate::mark_accepted();
    assert!(asset.sample_rate > 0, "decoded rate 0");
    assert!((1..=2).contains(&asset.channels.len()), "decoded {} channels", asset.channels.len());
    let frames = asset.channels[0].len();
    for c in &asset.channels {
        assert_eq!(c.len(), frames, "channel lengths differ");
        assert_finite(c, "decoded audio");
    }
}

fn expr_seeds() -> Vec<Vec<u8>> {
    [
        "x",
        "sin(phase * 6.2831853) * ctrl1",
        "tanh(in1 * 4) + in2 * 0.5",
        "x / ctrl1 + log(ctrl1) + exp(x * 1000)",
        "min(max(in1, -1), 1) * sr / 48000",
        "(x + in1) * (in2 - in3) / (in4 + 0.001)",
        "abs(sin(t * 440)) ^ 2",
        "floor(x * 8) / 8",
        "sqrt(abs(in1)) * sign(in1)",
    ]
    .iter()
    .map(|s| s.as_bytes().to_vec())
    .collect()
}

fn run_expr(bytes: &[u8]) {
    let src = String::from_utf8_lossy(bytes);
    let Ok(prog) = rawdawg_synthesizer::expr::compile(&src) else { return };
    crate::mark_accepted();
    let mut ev = rawdawg_synthesizer::expr::Evaluator::new();
    let n = rawdawg_synthesizer::expr::VARIABLES.len();
    for v in [0.0f32, 1.0, -1.0, f32::NAN, f32::INFINITY, f32::NEG_INFINITY, f32::MAX, f32::MIN_POSITIVE, 1e-30] {
        let mut vars = [v; 12];
        vars[n - 1] = 48_000.0;
        let y = ev.eval(&prog, &vars);
        assert!(y.is_finite() && y.abs() <= rawdawg_synthesizer::expr::OUTPUT_LIMIT, "expression output {y} for inputs {v}: {src}");
    }
}

fn patch_seeds() -> Vec<Vec<u8>> {
    let mut v: Vec<Vec<u8>> = rawdawg_synthesizer::presets::factory_presets().iter().take(6).filter_map(|p| serde_json::to_vec(&p.patch).ok()).collect();
    // instrument states from the fixtures (incl. the modular graph demo)
    for d in ["demo-graph.rawdawg", "demo.rawdawg"] {
        if let Ok(p) = rawdawg_project::load_project_dir(&fixtures().join(d)) {
            for c in &p.channels {
                if c.instrument.type_id.as_str() == rawdawg_types::ids::native_types::SYNTH
                    && let Ok(b) = serde_json::to_vec(&c.instrument.state)
                {
                    v.push(b);
                }
            }
        }
    }
    v
}

fn run_patch(bytes: &[u8]) {
    let parsed = rawdawg_synthesizer::Patch::from_json(bytes);
    let mut s = rawdawg_synthesizer::Synth::default();
    let loaded = s.load_state(bytes);
    if parsed.is_ok() && loaded.is_err() {
        panic!("patch validates but the synth refuses it: {:?}", loaded.err());
    }
    if loaded.is_ok() {
        crate::mark_accepted();
        exercise(&mut s, 3, true, "synth");
    }
}

fn effect_seeds() -> Vec<Vec<u8>> {
    let mut v = Vec::new();
    for info in rawdawg_effects::CATALOG.iter().filter(|i| i.listed) {
        let Ok(p) = rawdawg_effects::make_effect(info.type_id, None) else { continue };
        let state: serde_json::Value = serde_json::from_slice(&p.save_state()).unwrap_or_default();
        v.push(serde_json::to_vec(&serde_json::json!({ "effect": info.type_id, "state": state })).unwrap_or_default());
        for preset in rawdawg_effects::presets(info.type_id).iter().take(2) {
            if let Some(b) = rawdawg_effects::preset_state(info.type_id, preset) {
                let state: serde_json::Value = serde_json::from_slice(&b).unwrap_or_default();
                v.push(serde_json::to_vec(&serde_json::json!({ "effect": info.type_id, "state": state })).unwrap_or_default());
            }
        }
    }
    v
}

fn run_effect_state(bytes: &[u8]) {
    let Ok(serde_json::Value::Object(o)) = serde_json::from_slice::<serde_json::Value>(bytes) else { return };
    let Some(type_id) = o.get("effect").and_then(|v| v.as_str()) else { return };
    let state = serde_json::to_vec(o.get("state").unwrap_or(&serde_json::Value::Null)).unwrap_or_default();
    let Ok(mut fx) = rawdawg_effects::make_effect(type_id, Some(&state)) else { return };
    crate::mark_accepted();
    exercise(fx.as_mut(), 4, false, type_id);
    // what it saves loads again
    let saved = fx.save_state();
    if let Err(e) = rawdawg_effects::make_effect(type_id, Some(&saved)) {
        panic!("{type_id}: saved state does not load: {e}");
    }
}

fn envelope_seeds() -> Vec<Vec<u8>> {
    rawdawg_plugin_api::exported_plugins()
        .iter()
        .filter(|p| !p.is_synth())
        .filter_map(|p| {
            let bytes = p.make().ok()?.save_state();
            rawdawg_plugin_api::wrap::save_envelope(p.native_type_id, &bytes, false, 0.7).ok()
        })
        .collect()
}

fn run_envelope(bytes: &[u8]) {
    // the envelope names its effect; parse against that and against a fixed other type
    let named = serde_json::from_slice::<serde_json::Value>(bytes).ok().and_then(|v| v.get("type_id").and_then(|t| t.as_str()).map(str::to_owned));
    let mut parsed_any = false;
    for type_id in named.iter().map(String::as_str).chain([rawdawg_effects::reverb::TYPE_ID]) {
        if let Ok(parsed) = rawdawg_plugin_api::wrap::parse_state(type_id, bytes) {
            parsed_any = true;
            if let Some(m) = parsed.mix {
                assert!(m.is_finite(), "non-finite mix accepted");
            }
            if let Ok(mut fx) = rawdawg_effects::make_effect(type_id, None)
                && fx.load_state(&parsed.processor).is_ok()
            {
                exercise(fx.as_mut(), 2, false, "effect from envelope");
            }
        }
    }
    if parsed_any {
        crate::mark_accepted();
    }
}

fn run_midi_messages(bytes: &[u8]) {
    crate::mark_accepted();
    for c in bytes.chunks(3) {
        let (s, a, b) = (c[0], c.get(1).copied().unwrap_or(0), c.get(2).copied().unwrap_or(0));
        if let Some(k) = rawdawg_types::events::midi_message_to_kind(s, a, b, 1) {
            match k {
                rawdawg_types::events::EventKind::NoteOn { pitch, velocity, .. } => assert!(pitch <= 127 && velocity > 0.0 && velocity <= 1.0, "{k:?}"),
                rawdawg_types::events::EventKind::Control { cc, value } => assert!(cc <= 127 && (0.0..=1.0).contains(&value), "{k:?}"),
                rawdawg_types::events::EventKind::PitchBend { value } => assert!((-1.0..=1.0).contains(&value), "{k:?}"),
                _ => {}
            }
        }
    }
}

/// Every target with its seeds.
pub fn all() -> Vec<Target> {
    vec![
        Target { name: "project", mode: Mode::Json, max_len: 2 * 1024 * 1024, seeds: project_seeds(), run: run_project },
        Target { name: "preset", mode: Mode::Bytes, max_len: 512 * 1024, seeds: preset_seeds(), run: run_preset },
        Target { name: "preset-manifest", mode: Mode::Json, max_len: 512 * 1024, seeds: manifest_seeds(), run: run_preset_manifest },
        Target { name: "smf", mode: Mode::Bytes, max_len: 256 * 1024, seeds: smf_seeds(), run: run_smf },
        Target { name: "sf2", mode: Mode::Bytes, max_len: 512 * 1024, seeds: read(&fixtures().join("fixture.sf2")).into_iter().collect(), run: run_sf2 },
        Target { name: "audio", mode: Mode::Bytes, max_len: 128 * 1024, seeds: audio_seeds(), run: run_audio },
        Target { name: "expr", mode: Mode::Bytes, max_len: 4096, seeds: expr_seeds(), run: run_expr },
        Target { name: "patch", mode: Mode::Json, max_len: 1024 * 1024, seeds: patch_seeds(), run: run_patch },
        Target { name: "effect-state", mode: Mode::Json, max_len: 256 * 1024, seeds: effect_seeds(), run: run_effect_state },
        Target { name: "plugin-envelope", mode: Mode::Json, max_len: 256 * 1024, seeds: envelope_seeds(), run: run_envelope },
        Target { name: "midi-messages", mode: Mode::Bytes, max_len: 3 * 64, seeds: vec![vec![0x90, 60, 100, 0x80, 60, 0, 0xB0, 7, 127, 0xE0, 0, 64]], run: run_midi_messages },
    ]
}

pub fn by_name(name: &str) -> Option<Target> {
    all().into_iter().find(|t| t.name == name)
}
