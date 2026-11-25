//! T-TIME / T-RT subset for the engine and offline renderer.

use rawdawg_audio_engine::commands::{EngineCommand, TransportMode};
use rawdawg_audio_engine::engine::Engine;
use rawdawg_audio_engine::offline::{OfflineOptions, WavFormat, render_project, render_project_to_wav};
use rawdawg_audio_engine::plan::compile;
use rawdawg_dsp::analysis::dominant_frequency;
use rawdawg_dsp::math::{peak, rms};
use rawdawg_project::{Note, Project};
use rawdawg_rt::alloc_guard::{GuardAllocator, measure_rt};
use rawdawg_types::NoteId;
use rawdawg_types::events::{EventSource, TimedEvent};
use rawdawg_types::limits::EngineLimits;
use rawdawg_types::units::{SampleRate, Tick};

#[global_allocator]
static GLOBAL: GuardAllocator = GuardAllocator;

fn project_with_notes(notes: &[(i64, i64, u8)]) -> Project {
    let mut p = Project::new_default("t", serde_json::json!({}));
    let lane = &mut p.patterns[0].lanes[0];
    for (tick, dur, pitch) in notes {
        lane.notes.push(Note { id: NoteId::new(), tick: Tick(*tick), duration_ticks: Tick(*dur), pitch: *pitch, velocity: 0.9, channel: 0 });
    }
    p
}

#[test]
fn offline_render_of_a4_quarter_note_has_correct_timing_and_pitch() {
    // note at beat 2 (tick 960) for one beat: frames 24000..48000 at 48k/120 BPM
    let p = project_with_notes(&[(960, 960, 69)]);
    let opts = OfflineOptions { tail_seconds: 0.5, ..Default::default() };
    let (l, r, stats) = render_project(&p, &opts).unwrap();
    assert_eq!(stats.sample_rate, 48_000);
    assert_eq!(stats.frames, 96_000 + 24_000);
    assert_eq!(l.len(), r.len());
    assert_eq!(peak(&l[..23_900]), 0.0, "silence before the note");
    assert!(rms(&l[30_000..47_000]) > 0.02, "note sounding");
    let f = dominant_frequency(&l[28_000..47_000], 48_000.0);
    assert!((f - 440.0).abs() < 0.5, "{f}");
    // note-off at frame 48000; release 180 ms → silent by 0.3 s later (and limiter adds 48 frames)
    assert_eq!(peak(&l[48_000 + 14_400..96_000]), 0.0, "note released");
    assert_eq!(stats.nonfinite_blocks, 0);
    assert_eq!(stats.dropped_events, 0);
    assert_eq!(stats.clipped_samples, 0);
}

#[test]
fn offline_render_is_deterministic_and_block_size_invariant() {
    let p = project_with_notes(&[(0, 480, 60), (480, 480, 64), (960, 1920, 67), (1500, 300, 72)]);
    let a = render_project(&p, &OfflineOptions { block_frames: 256, tail_seconds: 0.2, ..Default::default() }).unwrap();
    let b = render_project(&p, &OfflineOptions { block_frames: 256, tail_seconds: 0.2, ..Default::default() }).unwrap();
    assert_eq!(a.0, b.0);
    for block in [32usize, 127, 511, 1024, 2048] {
        let c = render_project(&p, &OfflineOptions { block_frames: block, tail_seconds: 0.2, ..Default::default() }).unwrap();
        assert_eq!(c.0.len(), a.0.len());
        let err = a.0.iter().zip(&c.0).map(|(x, y)| (x - y).abs()).fold(0.0f32, f32::max);
        assert!(err <= 1e-5, "block {block}: {err}");
    }
}

#[test]
fn render_at_all_rates_keeps_musical_timing() {
    let p = project_with_notes(&[(960, 960, 69)]);
    for sr in SampleRate::SUPPORTED {
        let rate = SampleRate(sr);
        let (l, _, stats) = render_project(&p, &OfflineOptions { sample_rate: Some(rate), tail_seconds: 0.0, ..Default::default() }).unwrap();
        assert_eq!(stats.frames as u32, sr * 2, "one bar at 120 BPM = 2 s");
        let beat = (sr / 2) as usize;
        assert_eq!(peak(&l[..beat - 10]), 0.0, "{sr}: silence before beat 2");
        assert!(l[beat..beat + beat / 4].iter().any(|s| s.abs() > 0.01), "{sr}: note starts at beat 2");
        let f = dominant_frequency(&l[beat + beat / 10..2 * beat - 100], sr as f64);
        assert!((f - 440.0).abs() < 0.5, "{sr}: {f}");
    }
}

#[test]
fn pattern_loop_retriggers_and_truncates_at_boundary() {
    // two-beat pattern (1920 ticks = 48000 frames): note A on beat 1, note B on beat 2
    // ending exactly at the loop end (truncated by the wrap, re-triggered next pass).
    let mut p = project_with_notes(&[(0, 960, 60), (960, 960, 72)]);
    p.patterns[0].length_ticks = Tick(1920);
    let limits = EngineLimits::DEFAULT;
    let plan = compile(&p, 1, 256, &limits, &Default::default(), &rawdawg_audio_engine::NoAssets).unwrap().plan;
    let (mut engine, mut handle) = Engine::new(SampleRate(48_000), 256, &limits);
    engine.install_plan(plan);
    handle.send(EngineCommand::Play).unwrap();
    let mut out = Vec::new();
    let mut l = vec![0.0f32; 256];
    let mut r = vec![0.0f32; 256];
    let voices = |e: &Engine| e.status().active_voices.load(std::sync::atomic::Ordering::Relaxed);
    let mut render = |engine: &mut Engine, out: &mut Vec<f32>, blocks: usize| {
        for _ in 0..blocks {
            l.fill(0.0);
            r.fill(0.0);
            engine.render_block(&mut l, &mut r);
            out.extend_from_slice(&l);
        }
    };
    // just past the first loop boundary (48000 frames = 187.5 blocks)
    render(&mut engine, &mut out, 188);
    assert!(engine.position_frame() < 48_000, "playhead wrapped: {}", engine.position_frame());
    assert_eq!(voices(&engine), 2, "B releasing (truncated) while A re-attacks");
    // mid-way through A's second pass (frame ~60000): B's release has finished
    render(&mut engine, &mut out, 47);
    assert_eq!(voices(&engine), 1, "B's release finished; only A sounds");
    render(&mut engine, &mut out, 50);
    let f_a = dominant_frequency(&out[4000..23_000], 48_000.0);
    let f_b = dominant_frequency(&out[28_000..47_000], 48_000.0);
    let f_a2 = dominant_frequency(&out[48_000 + 4000..48_000 + 23_000], 48_000.0);
    assert!((f_a - 261.63).abs() < 1.0, "{f_a}");
    assert!((f_b - 523.25).abs() < 1.0, "{f_b}");
    assert!((f_a2 - 261.63).abs() < 1.0, "retriggered after the wrap: {f_a2}");
    assert_eq!(engine.status().dropped_events.get(), 0);
}

#[test]
fn stop_releases_notes_and_seek_has_no_retroactive_attack() {
    let p = project_with_notes(&[(0, 3840, 60)]);
    let limits = EngineLimits::DEFAULT;
    let (mut engine, mut handle) = Engine::new(SampleRate(48_000), 512, &limits);
    engine.install_plan(compile(&p, 1, 512, &limits, &Default::default(), &rawdawg_audio_engine::NoAssets).unwrap().plan);
    handle.send(EngineCommand::Play).unwrap();
    let mut l = vec![0.0f32; 512];
    let mut r = vec![0.0f32; 512];
    let mut render = |engine: &mut Engine, blocks: usize| {
        let mut out = Vec::new();
        for _ in 0..blocks {
            l.fill(0.0);
            r.fill(0.0);
            engine.render_block(&mut l, &mut r);
            out.extend_from_slice(&l);
        }
        out
    };
    let a = render(&mut engine, 20);
    assert!(rms(&a[5000..]) > 0.02);
    handle.send(EngineCommand::Stop).unwrap();
    let b = render(&mut engine, 60);
    assert_eq!(peak(&b[20_000..]), 0.0, "stopped: note released to silence");
    assert_eq!(engine.position_frame(), 0);
    // seek into the middle of the held note and play: no retroactive attack
    handle.send(EngineCommand::SeekTick(1920)).unwrap();
    handle.send(EngineCommand::Play).unwrap();
    let c = render(&mut engine, 20);
    assert_eq!(peak(&c), 0.0, "note that started before the seek must not sound");
    assert_eq!(engine.position_frame(), 48_000 + 20 * 512);
}

#[test]
fn live_events_reach_the_instrument_and_panic_releases() {
    let p = project_with_notes(&[]);
    let channel = p.channels[0].id;
    let limits = EngineLimits::DEFAULT;
    let (mut engine, mut handle) = Engine::new(SampleRate(48_000), 256, &limits);
    engine.install_plan(compile(&p, 1, 256, &limits, &Default::default(), &rawdawg_audio_engine::NoAssets).unwrap().plan);
    handle.send(EngineCommand::Live { channel, event: TimedEvent::note_on(0, EventSource::TypingKeyboard, 0, 1, 69, 0.8) }).unwrap();
    let mut l = vec![0.0f32; 256];
    let mut r = vec![0.0f32; 256];
    let mut out = Vec::new();
    for _ in 0..100 {
        l.fill(0.0);
        r.fill(0.0);
        engine.render_block(&mut l, &mut r);
        out.extend_from_slice(&l);
    }
    assert!(rms(&out[5000..]) > 0.02, "live note sounds without transport running");
    assert!((dominant_frequency(&out[4096..], 48_000.0) - 440.0).abs() < 0.5);
    assert_eq!(engine.status().active_voices.load(std::sync::atomic::Ordering::Relaxed), 1);
    handle.send(EngineCommand::Panic { source: None }).unwrap();
    out.clear();
    for _ in 0..100 {
        l.fill(0.0);
        r.fill(0.0);
        engine.render_block(&mut l, &mut r);
        out.extend_from_slice(&l);
    }
    assert_eq!(peak(&out[15_000..]), 0.0);
    // unknown channel → counted, not crashing
    handle.send(EngineCommand::Live { channel: rawdawg_types::ChannelId::new(), event: TimedEvent::note_on(0, EventSource::Mouse, 0, 2, 60, 0.5) }).unwrap();
    engine.render_block(&mut l, &mut r);
    assert_eq!(engine.status().dropped_events.get(), 1);
}

#[test]
fn plan_swap_at_block_boundary_keeps_audio_running_and_retires_off_rt() {
    let p = project_with_notes(&[(0, 3840, 60)]);
    let limits = EngineLimits::DEFAULT;
    let (mut engine, mut handle) = Engine::new(SampleRate(48_000), 256, &limits);
    engine.install_plan(compile(&p, 1, 256, &limits, &Default::default(), &rawdawg_audio_engine::NoAssets).unwrap().plan);
    handle.send(EngineCommand::Play).unwrap();
    let mut l = vec![0.0f32; 256];
    let mut r = vec![0.0f32; 256];
    for _ in 0..40 {
        engine.render_block(&mut l, &mut r);
    }
    let mut p2 = p.clone();
    p2.channels[0].gain_db = rawdawg_types::units::Db(-20.0);
    assert!(handle.publish_plan(compile(&p2, 2, 256, &limits, &Default::default(), &rawdawg_audio_engine::NoAssets).unwrap().plan).is_ok());
    engine.render_block(&mut l, &mut r);
    assert_eq!(engine.status().plan_revision.load(std::sync::atomic::Ordering::Relaxed), 2);
    assert_eq!(handle.collect_retired(), 1);
    assert!(engine.is_playing());
}

#[test]
fn render_block_is_allocation_free_under_traffic() {
    let p = project_with_notes(&[(0, 240, 60), (240, 240, 64), (480, 240, 67), (720, 240, 72), (960, 2880, 48)]);
    let channel = p.channels[0].id;
    let limits = EngineLimits::DEFAULT;
    let (mut engine, mut handle) = Engine::new(SampleRate(48_000), 2048, &limits);
    engine.install_plan(compile(&p, 1, 2048, &limits, &Default::default(), &rawdawg_audio_engine::NoAssets).unwrap().plan);
    handle.send(EngineCommand::Play).unwrap();
    handle.send(EngineCommand::SetMetronome(true)).unwrap();
    let mut l = vec![0.0f32; 2048];
    let mut r = vec![0.0f32; 2048];
    // warm-up
    for _ in 0..8 {
        engine.render_block(&mut l[..256], &mut r[..256]);
    }
    let sizes = [32usize, 64, 127, 128, 256, 511, 1024, 2048];
    let (_, allocs) = measure_rt(|| {
        for i in 0..400 {
            let n = sizes[i % sizes.len()];
            if i % 7 == 0 {
                handle.send(EngineCommand::Live { channel, event: TimedEvent::note_on(0, EventSource::TypingKeyboard, 0, 1000 + i as u32, 40 + (i % 40) as u8, 0.7) }).unwrap();
            }
            if i % 11 == 0 {
                handle
                    .send(EngineCommand::Live {
                        channel,
                        event: TimedEvent::note_off(0, EventSource::TypingKeyboard, 0, 1000 + (i as u32).saturating_sub(7), 40 + ((i + 7) % 40) as u8),
                    })
                    .unwrap();
            }
            if i == 200 {
                handle.send(EngineCommand::SeekTick(0)).unwrap();
            }
            if i == 300 {
                handle.send(EngineCommand::Panic { source: Some(EventSource::TypingKeyboard) }).unwrap();
            }
            engine.render_block(&mut l[..n], &mut r[..n]);
        }
    });
    assert_eq!(allocs, 0, "allocations on the render path");
    assert_eq!(engine.status().nonfinite_blocks.get(), 0);
}

#[test]
fn wav_export_round_trips_sample_count_and_content() {
    let p = project_with_notes(&[(0, 960, 69)]);
    let dir = std::env::temp_dir().join(format!("rawdawg-engine-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let opts = OfflineOptions { tail_seconds: 0.0, ..Default::default() };
    for (fmt, name) in [(WavFormat::Float32, "f32.wav"), (WavFormat::Pcm24, "p24.wav"), (WavFormat::Pcm16, "p16.wav")] {
        let path = dir.join(name);
        let stats = render_project_to_wav(&p, &opts, fmt, &path).unwrap();
        let mut reader = hound::WavReader::open(&path).unwrap();
        let spec = reader.spec();
        assert_eq!(spec.channels, 2);
        assert_eq!(spec.sample_rate, 48_000);
        assert_eq!(reader.duration() as usize, stats.frames);
        let (l, _, _) = render_project(&p, &opts).unwrap();
        match fmt {
            WavFormat::Float32 => {
                let samples: Vec<f32> = reader.samples::<f32>().map(|s| s.unwrap()).collect();
                for (i, s) in l.iter().enumerate() {
                    assert_eq!(samples[i * 2], *s);
                }
            }
            WavFormat::Pcm24 => {
                let samples: Vec<i32> = reader.samples::<i32>().map(|s| s.unwrap()).collect();
                for (i, s) in l.iter().enumerate().step_by(97) {
                    assert!((samples[i * 2] as f64 / 8_388_607.0 - f64::from(*s)).abs() < 2.0 / 8_388_607.0);
                }
            }
            WavFormat::Pcm16 => {
                let samples: Vec<i16> = reader.samples::<i16>().map(|s| s.unwrap()).collect();
                for (i, s) in l.iter().enumerate().step_by(97) {
                    assert!((f64::from(samples[i * 2]) / 32767.0 - f64::from(*s)).abs() < 3.0 / 32767.0);
                }
            }
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn song_mode_renders_clips_with_tempo_map() {
    let mut p = project_with_notes(&[(0, 960, 69)]);
    let pid = p.patterns[0].id;
    let track = rawdawg_project::Track { id: rawdawg_types::TrackId::new(), name: "T".into(), color: None, mute: false, solo: false, height: None };
    let tid = track.id;
    p.tracks.push(track);
    // clip at bar 2, one bar long, looped (pattern is one bar)
    p.clips.push(rawdawg_project::Clip {
        id: rawdawg_types::ClipId::new(),
        track_id: tid,
        start_tick: Tick(3840),
        length_ticks: Tick(3840),
        muted: false,
        content: rawdawg_project::ClipContent::Pattern { pattern_id: pid, source_offset_ticks: Tick(0), looped: true },
    });
    // 60 BPM from bar 2
    p.tempo.push(rawdawg_project::TempoPoint { tick: Tick(3840), us_per_quarter: rawdawg_types::units::UsPerQuarter(1_000_000) });
    let (l, _, stats) = render_project(&p, &OfflineOptions { mode: TransportMode::Song, tail_seconds: 0.0, ..Default::default() }).unwrap();
    // bar 1 at 120 BPM = 96000 frames; bar 2 at 60 BPM = 192000 frames
    assert_eq!(stats.frames, 96_000 + 192_000);
    assert_eq!(peak(&l[..95_900]), 0.0);
    assert!(rms(&l[100_000..140_000]) > 0.02, "note at bar 2 sounds for a whole 60 BPM beat");
    assert!(rms(&l[150_000..180_000]) < 0.01, "note released after one beat");
}

#[test]
fn plan_swap_carries_live_voices_when_state_fingerprint_matches() {
    use std::collections::HashMap;
    let p = project_with_notes(&[(0, 3840, 60)]);
    let limits = EngineLimits::DEFAULT;
    let (mut engine, mut handle) = Engine::new(SampleRate(48_000), 256, &limits);
    let first = compile(&p, 1, 256, &limits, &HashMap::new(), &rawdawg_audio_engine::NoAssets).unwrap();
    let live = first.fingerprints.clone();
    engine.install_plan(first.plan);
    handle.send(EngineCommand::Play).unwrap();
    let mut l = vec![0.0f32; 256];
    let mut r = vec![0.0f32; 256];
    let mut out = Vec::new();
    for _ in 0..40 {
        l.fill(0.0);
        r.fill(0.0);
        engine.render_block(&mut l, &mut r);
        out.extend_from_slice(&l);
    }
    assert_eq!(engine.status().active_voices.load(std::sync::atomic::Ordering::Relaxed), 1);
    // same instrument state, different mixer gain → carry over, no retrigger
    let mut p2 = p.clone();
    p2.mixer.channels[0].gain_db = rawdawg_types::units::Db(-3.0);
    let second = compile(&p2, 2, 256, &limits, &live, &rawdawg_audio_engine::NoAssets).unwrap();
    assert!(second.plan.channels[0].carry_over);
    assert!(second.table_senders.is_empty(), "carried-over instance keeps its live table channel");
    assert!(handle.publish_plan(second.plan).is_ok());
    for _ in 0..40 {
        l.fill(0.0);
        r.fill(0.0);
        engine.render_block(&mut l, &mut r);
        out.extend_from_slice(&l);
    }
    assert_eq!(engine.status().plan_revision.load(std::sync::atomic::Ordering::Relaxed), 2);
    assert_eq!(engine.status().active_voices.load(std::sync::atomic::Ordering::Relaxed), 1, "voice survived the swap");
    // continuity across the swap boundary (block 40): no gap, no re-attack dip
    let boundary = 40 * 256;
    assert!(rms(&out[boundary - 2048..boundary]) > 0.02 && rms(&out[boundary..boundary + 2048]) > 0.02);
    let max_jump = out[boundary - 256..boundary + 256].windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0f32, f32::max);
    assert!(max_jump < 0.2, "discontinuity at swap: {max_jump}");
    assert_eq!(handle.collect_retired(), 1);
    // changed instrument state → not carried over: fresh processor, old voice gone
    let mut p3 = p2.clone();
    p3.channels[0].instrument.state = serde_json::to_value(rawdawg_synthesizer::Patch { name: "other".into(), ..rawdawg_synthesizer::Patch::default() }).unwrap();
    let third = compile(&p3, 3, 256, &limits, &second.fingerprints, &rawdawg_audio_engine::NoAssets).unwrap();
    assert!(!third.plan.channels[0].carry_over);
    assert_eq!(third.table_senders.len(), 1);
    assert!(handle.publish_plan(third.plan).is_ok());
    engine.render_block(&mut l, &mut r);
    assert_eq!(engine.status().active_voices.load(std::sync::atomic::Ordering::Relaxed), 0);
}

#[test]
fn sampler_channel_renders_imported_asset_from_project_folder() {
    use rawdawg_audio_engine::offline::{OfflineOptions, render_project};
    let root = std::env::temp_dir().join(format!("rawdawg-engine-sampler-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    // 0.5 s 440 Hz tone, 48 kHz mono 16-bit
    let wav = root.join("tone.wav");
    let spec = hound::WavSpec { channels: 1, sample_rate: 48_000, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
    let mut w = hound::WavWriter::create(&wav, spec).unwrap();
    for i in 0..24_000 {
        w.write_sample(((std::f64::consts::TAU * 440.0 * i as f64 / 48_000.0).sin() * 16000.0) as i16).unwrap();
    }
    w.finalize().unwrap();
    let dir = root.join("S.rawdawg");
    let mut p = project_with_notes(&[(0, 960, 69)]);
    rawdawg_project::save_project_dir(&dir, &p).unwrap();
    let asset = rawdawg_project::assets::import_asset(&dir, &wav).unwrap();
    p.assets.push(asset.clone());
    let patch = rawdawg_sampler::SamplerPatch {
        asset_id: Some(asset.id.clone()),
        root_pitch: 69,
        env: rawdawg_dsp::envelope::AdsrParams { attack_s: 0.0, decay_s: 0.0, sustain: 1.0, release_s: 0.01 },
        ..rawdawg_sampler::SamplerPatch::default()
    };
    p.channels[0].instrument = rawdawg_project::Instance::native(rawdawg_types::ids::native_types::SAMPLER, serde_json::to_value(&patch).unwrap());
    rawdawg_project::save_project_dir(&dir, &p).unwrap();
    let reloaded = rawdawg_project::load_project_dir(&dir).unwrap();
    let opts = OfflineOptions { tail_seconds: 0.0, project_dir: Some(dir.clone()), ..Default::default() };
    let (l, _, _) = render_project(&reloaded, &opts).unwrap();
    assert!(rms(&l[2000..20_000]) > 0.1, "sampler channel sounds");
    assert!((dominant_frequency(&l[2000..20_000], 48_000.0) - 440.0).abs() < 1.0);
    // without the project folder the asset cannot be resolved: silent, not an error
    let (l2, _, _) = render_project(&reloaded, &OfflineOptions { tail_seconds: 0.0, project_dir: None, ..Default::default() }).unwrap();
    assert_eq!(peak(&l2), 0.0);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn editing_notes_while_playing_keeps_note_offs_and_picks_up_new_notes() {
    use std::collections::HashMap;
    // one long note sounding; the edit removes it and adds a later note
    let p = project_with_notes(&[(0, 1920, 60)]);
    let limits = EngineLimits::DEFAULT;
    let (mut engine, mut handle) = Engine::new(SampleRate(48_000), 256, &limits);
    let first = compile(&p, 1, 256, &limits, &HashMap::new(), &rawdawg_audio_engine::NoAssets).unwrap();
    let live = first.fingerprints.clone();
    engine.install_plan(first.plan);
    handle.send(EngineCommand::Play).unwrap();
    let mut l = vec![0.0f32; 256];
    let mut r = vec![0.0f32; 256];
    for _ in 0..20 {
        engine.render_block(&mut l, &mut r);
    }
    assert_eq!(engine.status().active_voices.load(std::sync::atomic::Ordering::Relaxed), 1);
    // edit: the sounding note is deleted, a new note starts at beat 3 (tick 1920 = frame 48000)
    let mut p2 = p.clone();
    p2.patterns[0].lanes[0].notes.clear();
    p2.patterns[0].lanes[0].notes.push(Note { id: NoteId::new(), tick: Tick(1920), duration_ticks: Tick(480), pitch: 72, velocity: 0.9, channel: 0 });
    let second = compile(&p2, 2, 256, &limits, &live, &rawdawg_audio_engine::NoAssets).unwrap();
    assert!(second.plan.channels[0].carry_over, "instrument untouched: voices carry over");
    assert!(handle.publish_plan(second.plan).is_ok());
    // render up to frame 48000 (block 187): the deleted note's note-off at frame 48000 must still arrive
    let mut out = Vec::new();
    while engine.position_frame() < 48_000 + 6_000 {
        l.fill(0.0);
        r.fill(0.0);
        engine.render_block(&mut l, &mut r);
        out.extend_from_slice(&l);
    }
    // the deleted note was released at its original end (its release tail is fading) and the new note (C5) dominates
    let f = dominant_frequency(&out[out.len() - 4_000..], 48_000.0);
    assert!((f - 523.25).abs() < 2.0, "new note plays after the edit: {f} Hz");
    // once the release tail is over only the new voice remains: no hanging note from the removed one
    while engine.position_frame() < 48_000 + 11_000 {
        engine.render_block(&mut l, &mut r);
    }
    assert_eq!(engine.status().active_voices.load(std::sync::atomic::Ordering::Relaxed), 1, "old voice released (no hanging note), new voice active");
    assert_eq!(engine.status().dropped_events.get(), 0);
}

#[test]
fn count_in_clicks_then_starts_and_recording_echoes_live_events_with_frames() {
    use std::collections::HashMap;
    let p = project_with_notes(&[(0, 960, 69)]);
    let limits = EngineLimits::DEFAULT;
    let (mut engine, mut handle) = Engine::new(SampleRate(48_000), 256, &limits);
    let first = compile(&p, 1, 256, &limits, &HashMap::new(), &rawdawg_audio_engine::NoAssets).unwrap();
    engine.install_plan(first.plan);
    handle.send(EngineCommand::SetRecording(true)).unwrap();
    handle.send(EngineCommand::PlayWithCountIn { beats: 4 }).unwrap();
    let mut l = vec![0.0f32; 256];
    let mut r = vec![0.0f32; 256];
    let mut count_in = Vec::new();
    // four beats at 120 BPM = 96 000 frames = 375 blocks of count-in (playback starts at its last frame)
    for _ in 0..374 {
        l.fill(0.0);
        r.fill(0.0);
        engine.render_block(&mut l, &mut r);
        count_in.extend_from_slice(&l);
    }
    assert!(!engine.status().playing.load(std::sync::atomic::Ordering::Relaxed), "still counting in");
    assert_eq!(engine.position_frame(), 0, "transport did not advance during the count-in");
    l.fill(0.0);
    r.fill(0.0);
    engine.render_block(&mut l, &mut r);
    count_in.extend_from_slice(&l);
    assert!(engine.status().playing.load(std::sync::atomic::Ordering::Relaxed), "count-in finished");
    assert_eq!(engine.position_frame(), 0);
    // clicks at each beat (frames 0, 24000, 48000, 72000), silence between them
    for beat in 0..4 {
        let at = beat * 24_000;
        assert!(peak(&count_in[at..at + 500]) > 0.1, "click at beat {beat}");
        assert_eq!(peak(&count_in[at + 6_000..at + 23_000]), 0.0, "no transport notes during the count-in");
    }
    // next block: playing, the scheduled A4 starts at frame 0
    l.fill(0.0);
    engine.render_block(&mut l, &mut r);
    assert!(engine.status().playing.load(std::sync::atomic::Ordering::Relaxed));
    assert_eq!(engine.position_frame(), 256);
    assert_eq!(engine.status().active_voices.load(std::sync::atomic::Ordering::Relaxed), 1);
    // recording: a live note sent now lands at the start of the next block (frame 256) and is echoed with that frame
    let ch = p.channels[0].id;
    let ev = TimedEvent::note_on(0, EventSource::MidiDevice(0), 0, 77, 64, 0.7);
    handle.send(EngineCommand::Live { channel: ch, event: ev }).unwrap();
    engine.render_block(&mut l, &mut r);
    let rec = handle.poll_recorded().expect("recorded event");
    assert_eq!(rec.frame, 256);
    assert_eq!(rec.event.kind, ev.kind);
    assert_eq!(rec.event.source, EventSource::MidiDevice(0));
    assert!(handle.poll_recorded().is_none());
    // not recording → nothing echoed
    handle.send(EngineCommand::SetRecording(false)).unwrap();
    handle.send(EngineCommand::Live { channel: ch, event: TimedEvent::note_off(0, EventSource::MidiDevice(0), 0, 77, 64) }).unwrap();
    engine.render_block(&mut l, &mut r);
    assert!(handle.poll_recorded().is_none());
    assert_eq!(engine.status().record_overflows.get(), 0);
}
