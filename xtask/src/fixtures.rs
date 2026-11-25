//! Deterministic fixtures: demo project, sine/impulse/sweep/noise WAVs, chord project.

use anyhow::Context;
use rawdawg_project::{ControllerEvent, ControllerKind, Lane, Note, Pattern, Project};
use rawdawg_types::units::Tick;
use rawdawg_types::{ChannelId, NoteId, PatternId};
use std::path::Path;

fn fixed_note(seed: u128, tick: i64, dur: i64, pitch: u8, vel: f32) -> Note {
    Note { id: NoteId::from_u128(seed), tick: Tick(tick), duration_ticks: Tick(dur), pitch, velocity: vel, channel: 0 }
}

/// The demo project: one bar, a simple motif on the default synth. Stable IDs
/// so the fixture is byte-identical across runs.
pub fn demo_project() -> Project {
    let synth = rawdawg_synthesizer::Patch::default();
    let mut p = Project::new_default("rawDAWg Demo", serde_json::to_value(&synth).expect("patch serializes"));
    p.project_id = rawdawg_types::ProjectId::from_u128(0xDA00_0001);
    let ch = ChannelId::from_u128(0xC4A0_0001);
    let mix = rawdawg_types::MixerChannelId::from_u128(0x3100_0002);
    let master = rawdawg_types::MixerChannelId::from_u128(0x3100_0001);
    let pat = PatternId::from_u128(0x9A77_0001);
    p.channels[0].id = ch;
    p.channels[0].instrument.id = rawdawg_types::InstanceId::from_u128(0x1457_0001);
    p.channels[0].mixer_id = mix;
    p.mixer.master_id = master;
    p.mixer.channels[0].id = master;
    p.mixer.channels[1].id = mix;
    p.mixer.channels[1].output_id = Some(master);
    p.patterns[0] = Pattern {
        id: pat,
        name: "Motif".into(),
        color: None,
        length_ticks: Tick(3840),
        steps: 16,
        step_ticks: Tick(240),
        lanes: vec![Lane {
            channel_id: ch,
            notes: vec![
                fixed_note(1, 0, 480, 60, 0.9),
                fixed_note(2, 480, 480, 64, 0.8),
                fixed_note(3, 960, 480, 67, 0.8),
                fixed_note(4, 1440, 480, 72, 0.9),
                fixed_note(5, 1920, 960, 67, 0.7),
                fixed_note(6, 1920, 960, 60, 0.7),
                fixed_note(7, 2880, 960, 64, 0.8),
            ],
            // a captured expression gesture: pitch bend dip, mod wheel rise, sustain on/off
            controllers: (0..=8)
                .map(|i| ControllerEvent {
                    id: rawdawg_types::ControllerEventId::from_u128(0xCE00_0100 + i),
                    tick: Tick(1920 + i as i64 * 60),
                    channel: 0,
                    kind: ControllerKind::PitchBend { value: -(i as f32 / 8.0) * 0.5 },
                })
                .chain((0..=4).map(|i| ControllerEvent {
                    id: rawdawg_types::ControllerEventId::from_u128(0xCE00_0200 + i),
                    tick: Tick(i as i64 * 480),
                    channel: 0,
                    kind: ControllerKind::Cc { cc: 1, value: i as f32 / 4.0 },
                }))
                .chain([
                    ControllerEvent { id: rawdawg_types::ControllerEventId::from_u128(0xCE00_0301), tick: Tick(2880), channel: 0, kind: ControllerKind::Cc { cc: 64, value: 1.0 } },
                    ControllerEvent { id: rawdawg_types::ControllerEventId::from_u128(0xCE00_0302), tick: Tick(3800), channel: 0, kind: ControllerKind::Cc { cc: 64, value: 0.0 } },
                ])
                .collect(),
        }],
    };
    p.selected_pattern = Some(pat);
    p
}

/// The demo motif arranged on two tracks (song mode): clip, looped clip, second pattern.
pub fn demo_song_project() -> Project {
    use rawdawg_project::{Clip, ClipContent, Track};
    let mut p = demo_project();
    p.name = "rawDAWg Song Demo".into();
    p.project_id = rawdawg_types::ProjectId::from_u128(0xDA00_0004);
    let ch = p.channels[0].id;
    let motif = p.patterns[0].id;
    let bass = PatternId::from_u128(0x9A77_0002);
    p.patterns.push(Pattern {
        id: bass,
        name: "Bass".into(),
        color: None,
        length_ticks: Tick(3840),
        steps: 16,
        step_ticks: Tick(240),
        lanes: vec![Lane {
            channel_id: ch,
            notes: vec![fixed_note(11, 0, 720, 36, 0.9), fixed_note(12, 960, 720, 36, 0.8), fixed_note(13, 1920, 720, 43, 0.8), fixed_note(14, 2880, 720, 41, 0.8)],
            controllers: vec![],
        }],
    });
    let t1 = rawdawg_types::TrackId::from_u128(0x7A00_0001);
    let t2 = rawdawg_types::TrackId::from_u128(0x7A00_0002);
    p.tracks.push(Track { id: t1, name: "Lead".into(), color: None, mute: false, solo: false, height: None });
    p.tracks.push(Track { id: t2, name: "Bass".into(), color: None, mute: false, solo: false, height: None });
    p.clips.push(Clip {
        id: rawdawg_types::ClipId::from_u128(0xC11_0001),
        track_id: t1,
        start_tick: Tick(0),
        length_ticks: Tick(3840),
        muted: false,
        content: ClipContent::Pattern { pattern_id: motif, source_offset_ticks: Tick::ZERO, looped: false },
    });
    p.clips.push(Clip {
        id: rawdawg_types::ClipId::from_u128(0xC11_0002),
        track_id: t1,
        start_tick: Tick(7680),
        length_ticks: Tick(7680),
        muted: false,
        content: ClipContent::Pattern { pattern_id: motif, source_offset_ticks: Tick::ZERO, looped: true },
    });
    p.clips.push(Clip {
        id: rawdawg_types::ClipId::from_u128(0xC11_0003),
        track_id: t2,
        start_tick: Tick(3840),
        length_ticks: Tick(11520),
        muted: false,
        content: ClipContent::Pattern { pattern_id: bass, source_offset_ticks: Tick::ZERO, looped: true },
    });
    p.tempo.push(rawdawg_project::TempoPoint { tick: Tick(7680), us_per_quarter: rawdawg_types::units::UsPerQuarter(461_538) });
    // automation: the synth's filter cutoff sweeps up over bars 3–4 (Read mode, Song playback)
    use rawdawg_project::{AutomationCurve, AutomationMode, AutomationPoint, AutomationTarget, Interpolation};
    let curve_id = rawdawg_types::CurveId::from_u128(0xA070_0001);
    p.automation.push(AutomationCurve {
        id: curve_id,
        name: "Graph synth · Cutoff".into(),
        target: AutomationTarget { instance_id: p.channels[0].instrument.id, param_id: rawdawg_synthesizer::params::FILTER_CUTOFF },
        points: vec![
            AutomationPoint { id: rawdawg_types::PointId::from_u128(0xA0A0_0001), tick: Tick(0), value: 0.25, interpolation: Interpolation::Linear },
            AutomationPoint { id: rawdawg_types::PointId::from_u128(0xA0A0_0002), tick: Tick(7680), value: 0.95, interpolation: Interpolation::Linear },
        ],
        mode: AutomationMode::Read,
        disabled: false,
    });
    let t3 = rawdawg_types::TrackId::from_u128(0x7A00_0003);
    p.tracks.push(Track { id: t3, name: "Automation".into(), color: Some([0xE6, 0xC9, 0x6E]), mute: false, solo: false, height: None });
    p.clips.push(Clip {
        id: rawdawg_types::ClipId::from_u128(0xC11_0004),
        track_id: t3,
        start_tick: Tick(7680),
        length_ticks: Tick(7680),
        muted: false,
        content: ClipContent::Automation { curve_id, source_offset_ticks: Tick::ZERO, looped: false },
    });
    // mixer: the synth goes through a "Lead" bus with an EQ, and sends to a "Wide" bus with a stereo widener
    use rawdawg_project::{Instance, MixerChannel, Send, SendTap};
    let master = p.mixer.master_id;
    let lead = rawdawg_types::MixerChannelId::from_u128(0x3100_0010);
    let wide = rawdawg_types::MixerChannelId::from_u128(0x3100_0011);
    let eq_state: serde_json::Value =
        serde_json::from_slice(&rawdawg_effects::preset_state(rawdawg_effects::eq::TYPE_ID, &rawdawg_effects::eq::PRESETS[1]).expect("eq preset")).expect("json");
    let mut eq = Instance { id: rawdawg_types::InstanceId::from_u128(0xF000_0001), ..Instance::native(rawdawg_effects::eq::TYPE_ID, eq_state) };
    eq.display_name = "Parametric EQ".into();
    let wide_state: serde_json::Value =
        serde_json::from_slice(&rawdawg_effects::preset_state(rawdawg_effects::stereo::TYPE_ID, &rawdawg_effects::stereo::PRESETS[2]).expect("stereo preset")).expect("json");
    let mut widener = Instance { id: rawdawg_types::InstanceId::from_u128(0xF000_0002), ..Instance::native(rawdawg_effects::stereo::TYPE_ID, wide_state) };
    widener.display_name = "Stereo".into();
    p.mixer.channels.push(MixerChannel {
        id: lead,
        name: "Lead".into(),
        color: None,
        gain_db: rawdawg_types::units::Db::UNITY,
        pan: rawdawg_types::units::Pan::CENTER,
        mute: false,
        solo: false,
        inserts: vec![eq],
        sends: vec![Send { target_id: wide, level_db: rawdawg_types::units::Db(-9.0), tap: SendTap::PostFader, sidechain: false }],
        output_id: Some(master),
    });
    p.mixer.channels.push(MixerChannel {
        id: wide,
        name: "Wide".into(),
        color: None,
        gain_db: rawdawg_types::units::Db::UNITY,
        pan: rawdawg_types::units::Pan::CENTER,
        mute: false,
        solo: false,
        inserts: vec![widener],
        sends: vec![],
        output_id: Some(master),
    });
    p.channels[0].mixer_id = lead;
    // the synth filter must be on for the sweep to be audible
    let mut patch: rawdawg_synthesizer::Patch = serde_json::from_value(p.channels[0].instrument.state.clone()).expect("patch");
    patch.filter.enabled = true;
    patch.filter.cutoff_hz = 800.0;
    p.channels[0].instrument.state = serde_json::to_value(&patch).expect("patch serializes");
    p
}

/// Demo project whose synth uses the modular graph (SYN-07): a voice graph with a
/// sub-oscillator, an envelope-driven expression waveshaper, an LFO-swept filter and
/// an explicit delayed feedback loop, plus a global graph with a panned echo.
pub fn graph_demo_project() -> Project {
    use rawdawg_synthesizer::graph::{Edge, GraphNodeId as N, NodeKind, Scope, SubGraph};
    let mut synth = rawdawg_synthesizer::Patch { name: "Graph demo".into(), ..Default::default() };
    // four motion frames on layer 1 so the frame editor has something to show
    if let Some(l) = synth.layers.first_mut() {
        use rawdawg_dsp::wavetable::cycles;
        l.generator = rawdawg_synthesizer::patch::Generator::Wavetable {
            cycle: cycles::soft_saw(),
            remove_dc: true,
            frames: vec![cycles::sine(), cycles::soft_saw(), cycles::square(), cycles::triangle()],
            position: 0.35,
        };
    }
    let mut v = SubGraph::passthrough(Scope::Voice);
    v.node_mut(N(1)).unwrap().x = 20.0;
    v.node_mut(N(1)).unwrap().y = 20.0;
    v.node_mut(N(2)).unwrap().x = 880.0;
    v.node_mut(N(2)).unwrap().y = 100.0;
    let osc = v.add_node(NodeKind::Osc, 20.0, 270.0);
    let mix = v.add_node(NodeKind::Mix, 220.0, 20.0);
    let env = v.add_node(NodeKind::Env, 220.0, 250.0);
    let shaper = v.add_node(NodeKind::Math, 430.0, 20.0);
    let lfo = v.add_node(NodeKind::Lfo, 430.0, 350.0);
    let filter = v.add_node(NodeKind::Filter, 650.0, 20.0);
    let delay = v.add_node(NodeKind::Delay, 650.0, 250.0);
    {
        let o = v.node_mut(osc).unwrap();
        o.params[0] = 0.0; // sine
        o.params[1] = -12.0;
        let m = v.node_mut(shaper).unwrap();
        m.expr = "tanh(x * (1 + ctrl1 * 6)) * 0.7".into();
        let f = v.node_mut(filter).unwrap();
        f.params[1] = 1200.0;
        f.params[2] = 2.0;
        f.params[3] = 2.0;
        let l = v.node_mut(lfo).unwrap();
        l.params[1] = 0.4;
        let d = v.node_mut(delay).unwrap();
        d.params[0] = 375.0;
        d.params[1] = 0.45;
        let mx = v.node_mut(mix).unwrap();
        mx.params[1] = 0.6;
        mx.params[2] = 0.5;
    }
    v.edges.clear();
    let e = |from: N, fp: u8, to: N, tp: u8| Edge { from, from_port: fp, to, to_port: tp };
    v.edges.extend([
        e(N(1), 1, mix, 0),
        e(osc, 0, mix, 1),
        e(mix, 0, shaper, 0),
        e(env, 0, shaper, 4),
        e(shaper, 0, filter, 0),
        e(lfo, 0, filter, 1),
        e(filter, 0, N(2), 0),
        e(filter, 0, delay, 0),
        e(delay, 0, mix, 2),
    ]);
    let mut g = SubGraph::passthrough(Scope::Global);
    g.node_mut(N(1)).unwrap().x = 20.0;
    g.node_mut(N(1)).unwrap().y = 20.0;
    g.node_mut(N(2)).unwrap().x = 700.0;
    g.node_mut(N(2)).unwrap().y = 60.0;
    let echo = g.add_node(NodeKind::Delay, 240.0, 200.0);
    let pan = g.add_node(NodeKind::ToStereo, 460.0, 200.0);
    let smix = g.add_node(NodeKind::StereoMix, 460.0, 20.0);
    {
        let d = g.node_mut(echo).unwrap();
        d.params[0] = 500.0;
        d.params[1] = 0.3;
        g.node_mut(pan).unwrap().params[0] = 0.6;
        g.node_mut(smix).unwrap().params[1] = 0.5;
    }
    g.edges.clear();
    g.edges.extend([e(N(1), 0, smix, 0), e(N(1), 1, echo, 0), e(echo, 0, pan, 0), e(pan, 0, smix, 1), e(smix, 0, N(2), 0)]);
    synth.graph.voice = v;
    synth.graph.global = g;
    synth.validate().expect("demo graph compiles");
    let mut p = demo_project();
    p.name = "rawDAWg Graph Demo".into();
    p.project_id = rawdawg_types::ProjectId::from_u128(0xDA00_0003);
    p.channels[0].name = "Graph synth".into();
    p.channels[0].instrument.state = serde_json::to_value(&synth).expect("patch serializes");
    p
}

fn write_wav(path: &Path, samples: impl Iterator<Item = f32>, rate: u32) -> anyhow::Result<()> {
    let spec = hound::WavSpec { channels: 1, sample_rate: rate, bits_per_sample: 32, sample_format: hound::SampleFormat::Float };
    let mut w = hound::WavWriter::create(path, spec)?;
    for s in samples {
        w.write_sample(s)?;
    }
    w.finalize()?;
    Ok(())
}

/// Minimal SoundFont fixture (two zones, looped sines) written next to the demo.
pub fn fixture_sf2() -> Vec<u8> {
    use rawdawg_sampler::sf2::{FixtureSample, FixtureZone, build_fixture};
    let sine = |n: usize, period: usize| -> Vec<i16> { (0..n).map(|i| ((i as f64 / period as f64 * std::f64::consts::TAU).sin() * 16000.0) as i16).collect() };
    build_fixture(
        "rawDAWg Fixture Font",
        0,
        0,
        &[
            FixtureSample { name: "a4", data: sine(44_100, 100), rate: 44_100, root: 69, loop_start: 100, loop_end: 44_000 },
            FixtureSample { name: "a5", data: sine(22_050, 50), rate: 44_100, root: 81, loop_start: 50, loop_end: 22_000 },
        ],
        &[
            FixtureZone { sample: 0, key_lo: 0, key_hi: 75, vel_lo: 0, vel_hi: 127, loop_mode: 1, extra: vec![] },
            FixtureZone { sample: 1, key_lo: 76, key_hi: 127, vel_lo: 0, vel_hi: 127, loop_mode: 1, extra: vec![] },
        ],
    )
}

/// Second demo: a sampler channel (440 Hz sine asset, root A4) and a SoundFont channel.
pub fn sampler_demo_project(dir: &Path) -> anyhow::Result<Project> {
    let mut p = demo_project();
    p.name = "rawDAWg Sampler Demo".into();
    p.project_id = rawdawg_types::ProjectId::from_u128(0xDA00_0002);
    let wav_asset = rawdawg_project::assets::import_asset(dir, &dir.parent().unwrap_or(dir).join("sine_440_48k.wav")).context("importing sine asset")?;
    let sf2_path = dir.parent().unwrap_or(dir).join("fixture.sf2");
    std::fs::write(&sf2_path, fixture_sf2())?;
    let sf2_asset = rawdawg_project::assets::import_asset(dir, &sf2_path).context("importing sf2 asset")?;
    let sampler = rawdawg_sampler::SamplerPatch { asset_id: Some(wav_asset.id.clone()), name: "Sine".into(), root_pitch: 69, ..Default::default() };
    p.channels[0].name = "Sine sample".into();
    p.channels[0].instrument = rawdawg_project::Instance {
        id: rawdawg_types::InstanceId::from_u128(0x1457_0002),
        ..rawdawg_project::Instance::native(rawdawg_types::ids::native_types::SAMPLER, serde_json::to_value(&sampler)?)
    };
    let master = p.mixer.master_id;
    let mix_id = rawdawg_types::MixerChannelId::from_u128(0x3100_0003);
    p.mixer.channels.push(rawdawg_project::MixerChannel {
        id: mix_id,
        name: "SoundFont".into(),
        color: None,
        gain_db: rawdawg_types::units::Db::UNITY,
        pan: rawdawg_types::units::Pan::CENTER,
        mute: false,
        solo: false,
        inserts: vec![],
        sends: vec![],
        output_id: Some(master),
    });
    let sf2 = rawdawg_sampler::Sf2Patch { asset_id: Some(sf2_asset.id.clone()), ..Default::default() };
    let ch_id = ChannelId::from_u128(0xC4A0_0002);
    p.channels.push(rawdawg_project::Channel {
        id: ch_id,
        name: "SoundFont".into(),
        color: None,
        instrument: rawdawg_project::Instance {
            id: rawdawg_types::InstanceId::from_u128(0x1457_0003),
            ..rawdawg_project::Instance::native(rawdawg_types::ids::native_types::SF2, serde_json::to_value(&sf2)?)
        },
        gain_db: rawdawg_types::units::Db::UNITY,
        pan: rawdawg_types::units::Pan::CENTER,
        mute: false,
        solo: false,
        mixer_id: mix_id,
    });
    p.patterns[0].lanes.push(Lane { channel_id: ch_id, notes: vec![fixed_note(10, 0, 1920, 69, 0.8), fixed_note(11, 1920, 1920, 84, 0.8)], controllers: vec![] });
    p.assets = vec![wav_asset, sf2_asset];
    Ok(p)
}

pub fn generate(dir: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(dir)?;
    let demo = dir.join("demo.rawdawg");
    rawdawg_project::save_project_dir(&demo, &demo_project()).context("writing demo project")?;
    let rate = 48_000u32;
    let n = rate as usize * 2;
    write_wav(&dir.join("sine_440_48k.wav"), (0..n).map(|i| (std::f64::consts::TAU * 440.0 * i as f64 / rate as f64).sin() as f32 * 0.5), rate)?;
    write_wav(&dir.join("impulse_48k.wav"), (0..rate as usize).map(|i| if i == 0 { 1.0 } else { 0.0 }), rate)?;
    write_wav(
        &dir.join("sweep_20_20k_48k.wav"),
        (0..n).map(|i| {
            let t = i as f64 / rate as f64;
            let f0 = 20.0f64;
            let f1 = 20_000.0f64;
            let k = (f1 / f0).ln() / 2.0;
            (std::f64::consts::TAU * f0 * ((k * t).exp() - 1.0) / k).sin() as f32 * 0.5
        }),
        rate,
    )?;
    let mut rng = rawdawg_dsp::rng::DeterministicRng::new(0xF1D7, 1);
    write_wav(&dir.join("noise_48k.wav"), (0..n).map(|_| rng.bipolar_f32() * 0.5), rate)?;
    let sampler_dir = dir.join("demo-sampler.rawdawg");
    std::fs::create_dir_all(&sampler_dir)?;
    let sp = sampler_demo_project(&sampler_dir)?;
    rawdawg_project::save_project_dir(&sampler_dir, &sp).context("writing sampler demo project")?;
    rawdawg_project::save_project_dir(&dir.join("demo-graph.rawdawg"), &graph_demo_project()).context("writing graph demo project")?;
    rawdawg_project::save_project_dir(&dir.join("demo-song.rawdawg"), &demo_song_project()).context("writing song demo project")?;
    std::fs::write(
        dir.join("README.md"),
        "# Fixtures\n\nGenerated by `cargo xtask fixtures`; deterministic and license-free (synthesized).\n\n- `demo.rawdawg/` — demo project (one bar motif on the default synth)\n- `demo-sampler.rawdawg/` — sampler + SoundFont channels with imported assets\n- `demo-graph.rawdawg/` — the demo motif on a synth with voice/global modular graphs (delayed feedback, expression node)\n- `demo-song.rawdawg/` — the motif and a bass pattern arranged on two tracks with a looped clip and a tempo change\n- `fixture.sf2` — minimal generated SoundFont (two looped sine zones)\n- `sine_440_48k.wav`, `impulse_48k.wav`, `sweep_20_20k_48k.wav`, `noise_48k.wav` — mono 32-bit float test signals\n",
    )?;
    println!("fixtures written to {}", dir.display());
    Ok(())
}
