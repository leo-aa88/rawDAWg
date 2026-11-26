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
    assert!(handle.send(EngineCommand::Play).is_ok());
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
    assert!(handle.send(EngineCommand::Play).is_ok());
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
    assert!(handle.send(EngineCommand::Play).is_ok());
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
    assert!(handle.send(EngineCommand::Play).is_ok());
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
    assert!(handle.send(EngineCommand::Play).is_ok());
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
            if i % 5 == 0 {
                // timed (MIDI) input: some due now, some held for later blocks, some late
                let now = handle.status.block_start_frame.load(std::sync::atomic::Ordering::Relaxed);
                for (k, at) in [now + 10, now + 5000, now.saturating_sub(100)].into_iter().enumerate() {
                    let event = TimedEvent::note_on(0, EventSource::MidiDevice(0), 0, 5000 + i as u32 * 3 + k as u32, 50 + (i % 30) as u8, 0.6);
                    handle.send(EngineCommand::LiveAt { channel, event, stream_frame: at }).unwrap();
                }
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
    let track = rawdawg_project::Track { id: rawdawg_types::TrackId::new(), name: "T".into(), color: None, mute: false, solo: false, height: None, mixer_id: None };
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
    assert!(handle.send(EngineCommand::Play).is_ok());
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
    assert!(handle.send(EngineCommand::Play).is_ok());
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

fn automation_song(points: Vec<(i64, f32, rawdawg_project::Interpolation)>, mode: rawdawg_project::AutomationMode) -> (Project, rawdawg_types::CurveId) {
    use rawdawg_project::{AutomationCurve, AutomationPoint, AutomationTarget, Clip, ClipContent, Track};
    use rawdawg_types::{ClipId, CurveId, PointId, TrackId};
    // one held note for two bars in song mode, master gain automated
    let mut p = project_with_notes(&[(0, 3840, 60)]);
    let tid = TrackId::new();
    p.tracks.push(Track { id: tid, name: "T".into(), color: None, mute: false, solo: false, height: None, mixer_id: None });
    p.clips.push(Clip {
        id: ClipId::new(),
        track_id: tid,
        start_tick: Tick(0),
        length_ticks: Tick(7680),
        muted: false,
        content: ClipContent::Pattern { pattern_id: p.patterns[0].id, source_offset_ticks: Tick::ZERO, looped: true },
    });
    let curve_id = CurveId::new();
    let curve = AutomationCurve {
        id: curve_id,
        name: "gain".into(),
        target: AutomationTarget { instance_id: p.channels[0].instrument.id, param_id: rawdawg_synthesizer::params::MASTER_GAIN },
        points: points.into_iter().map(|(t, v, i)| AutomationPoint { id: PointId::new(), tick: Tick(t), value: v, interpolation: i }).collect(),
        mode,
        disabled: false,
        pattern_id: None,
    };
    p.automation.push(curve);
    p.clips.push(Clip {
        id: ClipId::new(),
        track_id: tid,
        start_tick: Tick(0),
        length_ticks: Tick(7680),
        muted: false,
        content: ClipContent::Automation { curve_id, source_offset_ticks: Tick::ZERO, looped: false },
    });
    (p, curve_id)
}

#[test]
fn automation_curve_schedules_sampled_param_events_and_renders_a_ramp() {
    use rawdawg_audio_engine::schedule::Schedule;
    use rawdawg_project::Interpolation;
    // master gain: 0 at bar 1, linear to 1 at bar 2 (tick 3840), step down to 0.25 at bar 3 start
    let (p, _) =
        automation_song(vec![(0, 0.0, Interpolation::Linear), (3840, 1.0, Interpolation::Linear), (3841, 0.25, Interpolation::Step)], rawdawg_project::AutomationMode::Read);
    let s = Schedule::for_song(&p, &p.tempo_map().unwrap(), SampleRate(48_000));
    assert!(!s.params.is_empty());
    assert_eq!(s.params[0].frame, 0);
    assert_eq!(s.params[0].value, 0.0);
    assert!(s.params.windows(2).all(|w| w[0].frame <= w[1].frame), "sorted by frame");
    let at_bar2 = s.params.iter().filter(|q| q.frame == 96_000).map(|q| q.value).next_back().unwrap();
    assert!((at_bar2 - 1.0).abs() < 1e-6);
    let spacing: Vec<i64> = s.params.windows(2).take(10).map(|w| w[1].frame - w[0].frame).collect();
    assert!(spacing.iter().all(|d| *d == 64), "linear segments sampled every 64 frames: {spacing:?}");
    let step_pt = s.params.iter().find(|q| q.value == 0.25 && q.frame >= 96_000).expect("step point");
    assert_eq!(step_pt.frame, 96_025, "step point exact at its tick (tick 3841 = frame 96025)");
    // offline song render: level rises through bar 1 and drops after the step
    let (l, _, _) = render_project(&p, &OfflineOptions { mode: TransportMode::Song, tail_seconds: 0.0, ..Default::default() }).unwrap();
    let early = rms(&l[4_000..12_000]);
    let mid = rms(&l[44_000..52_000]);
    let late = rms(&l[88_000..95_000]);
    let after = rms(&l[110_000..130_000]);
    assert!(early < mid && mid < late, "ramp: {early} {mid} {late}");
    assert!(late > 0.1 && after < late * 0.5, "step down to 0.25 after bar 2: late {late} after {after}");
    // Off mode produces no events
    let (p_off, _) = automation_song(vec![(0, 0.0, Interpolation::Linear), (3840, 1.0, Interpolation::Linear)], rawdawg_project::AutomationMode::Off);
    assert!(Schedule::for_song(&p_off, &p_off.tempo_map().unwrap(), SampleRate(48_000)).params.is_empty());
}

#[test]
fn seeking_into_automation_chases_the_current_value() {
    use rawdawg_project::Interpolation;
    use std::collections::HashMap;
    let (p, _) = automation_song(vec![(0, 0.0, Interpolation::Linear), (3840, 1.0, Interpolation::Linear)], rawdawg_project::AutomationMode::Read);
    let limits = EngineLimits::DEFAULT;
    let (mut engine, mut handle) = Engine::new(SampleRate(48_000), 256, &limits);
    let plan = compile(&p, 1, 256, &limits, &HashMap::new(), &rawdawg_audio_engine::NoAssets).unwrap().plan;
    engine.install_plan(plan);
    assert!(handle.send(EngineCommand::SetMode(TransportMode::Song)).is_ok());
    // seek to bar 2 (gain = 1.0 by the curve) and play: no note-on happened at bar 2 start in the pattern
    // (the looped clip retriggers at tick 3840), so the note is heard at full gain right away
    assert!(handle.send(EngineCommand::SeekTick(3840)).is_ok());
    assert!(handle.send(EngineCommand::Play).is_ok());
    let mut l = vec![0.0f32; 256];
    let mut r = vec![0.0f32; 256];
    let mut out = Vec::new();
    for _ in 0..40 {
        l.fill(0.0);
        r.fill(0.0);
        engine.render_block(&mut l, &mut r);
        out.extend_from_slice(&l);
    }
    let chased = rms(&out[2_000..10_000]);
    // compare with starting from 0 where the gain curve starts at 0
    let (mut engine2, mut handle2) = Engine::new(SampleRate(48_000), 256, &limits);
    let plan2 = compile(&p, 1, 256, &limits, &HashMap::new(), &rawdawg_audio_engine::NoAssets).unwrap().plan;
    engine2.install_plan(plan2);
    handle2.send(EngineCommand::SetMode(TransportMode::Song)).unwrap();
    handle2.send(EngineCommand::Play).unwrap();
    let mut out2 = Vec::new();
    for _ in 0..40 {
        l.fill(0.0);
        r.fill(0.0);
        engine2.render_block(&mut l, &mut r);
        out2.extend_from_slice(&l);
    }
    let from_start = rms(&out2[2_000..10_000]);
    assert!(chased > 0.1, "chased gain is audible: {chased}");
    assert!(from_start < chased * 0.3, "at the start the curve is near 0: {from_start} vs {chased}");
}

/// Project with one synth channel feeding a bus with the given insert chain, plus an
/// optional parallel path (second bus) so summing alignment can be measured.
fn mixer_project(inserts: Vec<rawdawg_project::Instance>) -> (Project, rawdawg_types::MixerChannelId) {
    use rawdawg_project::MixerChannel;
    let mut p = project_with_notes(&[(0, 240, 69)]);
    let master = p.mixer.master_id;
    let bus = rawdawg_types::MixerChannelId::new();
    p.mixer.channels.push(MixerChannel {
        id: bus,
        name: "Bus".into(),
        color: None,
        gain_db: rawdawg_types::units::Db::UNITY,
        pan: rawdawg_types::units::Pan::CENTER,
        mute: false,
        solo: false,
        inserts,
        sends: vec![],
        output_id: Some(master),
    });
    (p, bus)
}

fn latency_insert(frames: u32) -> rawdawg_project::Instance {
    let state = serde_json::json!({ "version": 1, "params": { "latency": frames as f64 / 4096.0 } });
    rawdawg_project::Instance::native(rawdawg_effects::testfx::TYPE_ID, state)
}

fn render_song_or_pattern(p: &Project) -> Vec<f32> {
    let (l, _, _) = render_project(p, &OfflineOptions { tail_seconds: 0.5, ..Default::default() }).unwrap();
    l
}

fn first_onset(buf: &[f32]) -> usize {
    buf.iter().position(|v| v.abs() > 1e-4).unwrap_or(usize::MAX)
}

#[test]
fn insert_latency_is_compensated_at_summing_points_within_one_frame() {
    use rawdawg_project::{Send, SendTap};
    // A: channel → bus(latency 1000) → master ; B: same channel also sent pre-fader to a dry bus → master.
    // Both paths must arrive at the master aligned: the dry bus gets a 1000-frame compensation.
    let (mut p, bus) = mixer_project(vec![latency_insert(1000)]);
    let master = p.mixer.master_id;
    let dry = rawdawg_types::MixerChannelId::new();
    p.mixer.channels.push(rawdawg_project::MixerChannel {
        id: dry,
        name: "Dry".into(),
        color: None,
        gain_db: rawdawg_types::units::Db::UNITY,
        pan: rawdawg_types::units::Pan::CENTER,
        mute: false,
        solo: false,
        inserts: vec![],
        sends: vec![],
        output_id: Some(master),
    });
    p.channels[0].mixer_id = bus;
    p.mixer_channel_mut(bus).unwrap().sends.push(Send { target_id: dry, level_db: rawdawg_types::units::Db::UNITY, tap: SendTap::PreFader, sidechain: false });
    // reference: the channel straight to master (no latency anywhere)
    let mut reference = p.clone();
    reference.channels[0].mixer_id = master;
    reference.mixer_channel_mut(bus).unwrap().sends.clear();
    reference.mixer_channel_mut(bus).unwrap().inserts.clear();
    let r = render_song_or_pattern(&reference);
    let out = render_song_or_pattern(&p);
    let (on_ref, on_out) = (first_onset(&r), first_onset(&out));
    assert_eq!(on_out, on_ref + 1000, "master output is delayed by the longest path (1000 frames) once, not twice");
    // the summed signal is exactly twice the reference (both paths aligned): compare shapes
    let n = 20_000;
    let err = (0..n).map(|i| (out[i + 1000] - 2.0 * r[i]).abs()).fold(0.0f32, f32::max);
    assert!(err < 1e-4, "misaligned parallel paths: max error {err}");
    // bypassing the latency insert keeps the alignment (true bypass retains latency)
    let mut bypassed = p.clone();
    bypassed.mixer_channel_mut(bus).unwrap().inserts[0].bypass = true;
    let outb = render_song_or_pattern(&bypassed);
    assert_eq!(first_onset(&outb), on_ref + 1000);
    let errb = (0..n).map(|i| (outb[i + 1000] - 2.0 * r[i]).abs()).fold(0.0f32, f32::max);
    assert!(errb < 1e-4, "bypass broke the alignment: {errb}");
    // compile reports the output latency
    let limits = EngineLimits::DEFAULT;
    let out = compile(&p, 1, 256, &limits, &Default::default(), &rawdawg_audio_engine::NoAssets).unwrap();
    assert_eq!(out.output_latency, 1000);
    assert!(out.slots.contains_key(&p.mixer_channel(bus).unwrap().inserts[0].id));
}

#[test]
fn sends_pre_post_and_sidechain_route_where_declared() {
    use rawdawg_project::{Send, SendTap};
    let (mut p, bus) = mixer_project(vec![]);
    let master = p.mixer.master_id;
    p.channels[0].mixer_id = bus;
    // the bus is muted at the fader: a post-fader send carries nothing, a pre-fader send still does
    let aux = rawdawg_types::MixerChannelId::new();
    p.mixer.channels.push(rawdawg_project::MixerChannel {
        id: aux,
        name: "Aux".into(),
        color: None,
        gain_db: rawdawg_types::units::Db::UNITY,
        pan: rawdawg_types::units::Pan::CENTER,
        mute: false,
        solo: false,
        inserts: vec![],
        sends: vec![],
        output_id: Some(master),
    });
    p.mixer_channel_mut(bus).unwrap().gain_db = rawdawg_types::units::Db(-60.0);
    p.mixer_channel_mut(bus).unwrap().sends.push(Send { target_id: aux, level_db: rawdawg_types::units::Db(-6.0), tap: SendTap::PostFader, sidechain: false });
    let post = render_song_or_pattern(&p);
    assert!(peak(&post) < 0.01, "post-fader send follows the (−60 dB) fader: {}", peak(&post));
    p.mixer_channel_mut(bus).unwrap().sends[0].tap = SendTap::PreFader;
    let pre = render_song_or_pattern(&p);
    assert!(peak(&pre) > 0.05, "pre-fader send ignores the fader");
    // the send level (−6 dB) scales the aux contribution
    let mut unity = p.clone();
    unity.mixer_channel_mut(bus).unwrap().sends[0].level_db = rawdawg_types::units::Db::UNITY;
    let u = render_song_or_pattern(&unity);
    let ratio = rms(&pre[..10_000]) / rms(&u[..10_000]);
    assert!((ratio - 0.501).abs() < 0.02, "{ratio}");
    // sidechain sends do not reach the target's main input (no insert listens: silence at master)
    p.mixer_channel_mut(bus).unwrap().sends[0].sidechain = true;
    let sc = render_song_or_pattern(&p);
    // only the bus's own −60 dB main output remains
    assert!(peak(&sc) < 0.002 && peak(&sc) < peak(&pre) * 0.01, "sidechain sends never enter the audio path: {}", peak(&sc));
}

#[test]
fn insert_effect_processes_audio_receives_params_and_carries_over() {
    use std::collections::HashMap;
    // EQ insert on the bus with a −24 dB bell at 440 Hz and Q 1 (note A4)
    let eq_state =
        serde_json::json!({ "version": 1, "params": { "band0.on": 0.0, "band1.on": 1.0, "band1.kind": 0.0, "band1.freq": 0.4475, "band1.gain": 0.0, "band1.q": 0.4675 } });
    let (mut p, bus) = mixer_project(vec![rawdawg_project::Instance::native(rawdawg_effects::eq::TYPE_ID, eq_state)]);
    p.channels[0].mixer_id = bus;
    p.patterns[0].lanes[0].notes[0].duration_ticks = Tick(3840);
    // a pure sine so the bell on the fundamental is the whole story
    let sine =
        rawdawg_synthesizer::Patch { layers: vec![rawdawg_synthesizer::patch::Layer::basic(rawdawg_dsp::oscillator::BasicWave::Sine)], ..rawdawg_synthesizer::Patch::default() };
    p.channels[0].instrument.state = serde_json::to_value(&sine).unwrap();
    let cut = render_song_or_pattern(&p);
    let mut flat = p.clone();
    flat.mixer_channel_mut(bus).unwrap().inserts[0].bypass = true;
    let dry = render_song_or_pattern(&flat);
    let ratio = rms(&cut[20_000..40_000]) / rms(&dry[20_000..40_000]);
    assert!(ratio < 0.2, "−24 dB bell on the fundamental: ratio {ratio}");
    // live parameter change through the engine command reaches the insert (gain back to 0 dB)
    let limits = EngineLimits::DEFAULT;
    let (mut engine, mut handle) = Engine::new(SampleRate(48_000), 256, &limits);
    let out = compile(&p, 1, 256, &limits, &HashMap::new(), &rawdawg_audio_engine::NoAssets).unwrap();
    let fx_id = p.mixer_channel(bus).unwrap().inserts[0].id;
    let live = out.fingerprints.clone();
    engine.install_plan(out.plan);
    assert!(handle.send(EngineCommand::Play).is_ok());
    let mut l = vec![0.0f32; 256];
    let mut r = vec![0.0f32; 256];
    let mut quiet = Vec::new();
    for _ in 0..100 {
        l.fill(0.0);
        engine.render_block(&mut l, &mut r);
        quiet.extend_from_slice(&l);
    }
    handle.send(EngineCommand::Param { instance: fx_id, id: rawdawg_types::ParamId(rawdawg_effects::eq::band_param(1, rawdawg_effects::eq::B_GAIN)), value: 0.5 }).unwrap();
    let mut loud = Vec::new();
    for _ in 0..100 {
        l.fill(0.0);
        engine.render_block(&mut l, &mut r);
        loud.extend_from_slice(&l);
    }
    assert!(rms(&loud[12_000..]) > rms(&quiet[12_000..]) * 4.0, "param event restored the level: {} vs {}", rms(&loud[12_000..]), rms(&quiet[12_000..]));
    assert_eq!(engine.status().dropped_events.get(), 0);
    // a plan swap with the same insert state carries the insert over (no reset glitch) and keeps playing
    let mut p2 = p.clone();
    p2.mixer_channel_mut(bus).unwrap().gain_db = rawdawg_types::units::Db(-1.0);
    let second = compile(&p2, 2, 256, &limits, &live, &rawdawg_audio_engine::NoAssets).unwrap();
    assert!(second.plan.mixer.iter().find(|m| m.id == bus).unwrap().inserts[0].carry_over);
    assert!(handle.publish_plan(second.plan).is_ok());
    for _ in 0..10 {
        engine.render_block(&mut l, &mut r);
    }
    assert_eq!(engine.status().plan_revision.load(std::sync::atomic::Ordering::Relaxed), 2);
    assert!(engine.status().active_voices.load(std::sync::atomic::Ordering::Relaxed) >= 1);
}

/// A project folder with a 0.5 s 440 Hz tone imported as an asset, an "Audio" track and
/// one audio clip of it at bar 2 (song mode). Returns (project dir, project, track id, clip id).
fn audio_clip_project(tag: &str) -> (std::path::PathBuf, Project, rawdawg_types::TrackId, rawdawg_types::ClipId) {
    use rawdawg_project::{Clip, ClipContent, Track};
    let root = std::env::temp_dir().join(format!("rawdawg-engine-clip-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let wav = root.join("tone.wav");
    let spec = hound::WavSpec { channels: 1, sample_rate: 48_000, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
    let mut w = hound::WavWriter::create(&wav, spec).unwrap();
    for i in 0..24_000 {
        w.write_sample(((std::f64::consts::TAU * 440.0 * i as f64 / 48_000.0).sin() * 16000.0) as i16).unwrap();
    }
    w.finalize().unwrap();
    let dir = root.join("A.rawdawg");
    let mut p = Project::new_default("clips", serde_json::json!({}));
    rawdawg_project::save_project_dir(&dir, &p).unwrap();
    let asset = rawdawg_project::assets::import_asset(&dir, &wav).unwrap();
    p.assets.push(asset.clone());
    let tid = rawdawg_types::TrackId::new();
    p.tracks.push(Track { id: tid, name: "Audio".into(), color: None, mute: false, solo: false, height: None, mixer_id: None });
    let cid = rawdawg_types::ClipId::new();
    p.clips.push(Clip {
        id: cid,
        track_id: tid,
        start_tick: Tick(3840),
        length_ticks: Tick(960),
        muted: false,
        content: ClipContent::Audio {
            asset_id: asset.id.clone(),
            source_offset_frames: 0,
            source_length_frames: 24_000,
            gain_db: rawdawg_types::units::Db(0.0),
            fade_in_frames: 0,
            fade_out_frames: 0,
            playback_rate: 1.0,
        },
    });
    // an extra bar of silence keeps the song end past the clip
    rawdawg_project::save_project_dir(&dir, &p).unwrap();
    (dir, p, tid, cid)
}

fn song_opts(dir: &std::path::Path) -> OfflineOptions {
    // unprotected: the master limiter's 1 ms lookahead would shift every onset by 48 frames
    OfflineOptions { mode: TransportMode::Song, tail_seconds: 0.0, end_tick: Some(Tick(3840 * 2)), project_dir: Some(dir.to_path_buf()), unprotected: true, ..Default::default() }
}

#[test]
fn audio_clip_plays_at_its_bar_in_song_mode_with_gain_rate_and_fades() {
    use rawdawg_project::ClipContent;
    let (dir, mut p, _tid, _cid) = audio_clip_project("basic");
    let (l, r, stats) = render_project(&p, &song_opts(&dir)).unwrap();
    assert_eq!(stats.nonfinite_blocks, 0);
    // bar 2 at 120 BPM = 2.0 s = frame 96 000; 0.5 s long
    let onset = first_onset(&l);
    assert!((onset as i64 - 96_000).abs() <= 2, "onset at frame {onset}");
    assert!(peak(&l[..95_990]) == 0.0, "silence before the clip");
    assert!((dominant_frequency(&l[96_500..110_000], 48_000.0) - 440.0).abs() < 1.0);
    assert!(peak(&l[96_000..120_000]) > 0.45 && peak(&l[96_000..120_000]) < 0.5, "16000/32768 ≈ 0.488: {}", peak(&l[96_000..120_000]));
    let last = l.iter().rposition(|v| v.abs() > 1e-4).unwrap();
    assert!((last as i64 - 120_000).abs() <= 4, "clip ends after 0.5 s: {last}");
    assert!(l.iter().zip(&r).skip(96_000).take(20_000).all(|(a, b)| (a - b).abs() < 1e-6), "mono asset feeds both channels");
    // gain −6 dB, rate 2 (an octave up, half as long), 100 ms fades
    if let ClipContent::Audio { gain_db, playback_rate, fade_in_frames, fade_out_frames, .. } = &mut p.clips[0].content {
        *gain_db = rawdawg_types::units::Db(-6.0);
        *playback_rate = 2.0;
        *fade_in_frames = 4800;
        *fade_out_frames = 4800;
    }
    let (l2, _, _) = render_project(&p, &song_opts(&dir)).unwrap();
    assert!((dominant_frequency(&l2[97_000..106_000], 48_000.0) - 880.0).abs() < 2.0, "rate 2 doubles the pitch");
    let last2 = l2.iter().rposition(|v| v.abs() > 1e-4).unwrap();
    assert!((last2 as i64 - 108_000).abs() <= 4, "rate 2 halves the length: {last2}");
    let pk = peak(&l2[100_000..104_000]);
    assert!((pk / 0.488 - 0.501).abs() < 0.03, "−6 dB: {pk}");
    // the fades ramp in and out (2 400 output frames each at rate 2)
    assert!(rms(&l2[96_000..96_400]) < rms(&l2[98_400..98_800]) * 0.35, "fade in");
    assert!(rms(&l2[107_500..107_900]) < rms(&l2[104_000..104_400]) * 0.35, "fade out");
    let _ = std::fs::remove_dir_all(dir.parent().unwrap());
}

#[test]
fn audio_clips_follow_track_routing_mute_and_pattern_mode() {
    use rawdawg_project::{MixerChannel, Send, SendTap};
    let (dir, mut p, tid, _cid) = audio_clip_project("routing");
    // pattern mode never plays clips
    let opts_pattern = OfflineOptions { tail_seconds: 0.0, end_tick: Some(Tick(3840 * 2)), project_dir: Some(dir.clone()), ..Default::default() };
    let (lp, _, _) = render_project(&p, &opts_pattern).unwrap();
    assert_eq!(peak(&lp), 0.0, "pattern mode: no audio clips");
    // muted track: silent
    p.tracks[0].mute = true;
    let (lm, _, _) = render_project(&p, &song_opts(&dir)).unwrap();
    assert_eq!(peak(&lm), 0.0, "muted track");
    p.tracks[0].mute = false;
    // routed to a bus: the bus's fader applies; a muted bus silences it
    let master = p.mixer.master_id;
    let bus = rawdawg_types::MixerChannelId::new();
    p.mixer.channels.push(MixerChannel {
        id: bus,
        name: "Bus".into(),
        color: None,
        gain_db: rawdawg_types::units::Db(-12.0),
        pan: rawdawg_types::units::Pan::CENTER,
        mute: false,
        solo: false,
        inserts: vec![],
        sends: vec![Send { target_id: master, level_db: rawdawg_types::units::Db(0.0), tap: SendTap::PostFader, sidechain: false }],
        output_id: None,
    });
    let bus_idx = p.mixer.channels.len() - 1;
    p.tracks.iter_mut().find(|t| t.id == tid).unwrap().mixer_id = Some(bus);
    rawdawg_project::validate(&p).unwrap();
    let (lb, _, _) = render_project(&p, &song_opts(&dir)).unwrap();
    let pk = peak(&lb[96_000..120_000]);
    assert!((pk / 0.488 - 0.251).abs() < 0.03, "bus fader −12 dB applies: {pk}");
    p.mixer.channels[bus_idx].mute = true;
    let (lmb, _, _) = render_project(&p, &song_opts(&dir)).unwrap();
    assert_eq!(peak(&lmb), 0.0, "muted bus");
    // a missing asset is silence, not an error
    p.mixer.channels[bus_idx].mute = false;
    let (lo, _, _) = render_project(&p, &OfflineOptions { project_dir: None, ..song_opts(&dir) }).unwrap();
    assert_eq!(peak(&lo), 0.0);
    let _ = std::fs::remove_dir_all(dir.parent().unwrap());
}

#[test]
fn audio_clips_restart_at_the_loop_wrap_and_render_allocation_free() {
    use rawdawg_project::LoopRange;
    let (dir, mut p, _tid, _cid) = audio_clip_project("loop");
    // loop bars 2–3 (frames 96 000..192 000): the clip at bar 2 restarts at every wrap
    p.r#loop = Some(LoopRange { start_tick: Tick(3840), end_tick: Tick(7680), enabled: true });
    let mut cache = rawdawg_audio_engine::ProjectAssetCache::new(Some(dir.clone()));
    cache.sync(Some(dir.clone()), &p);
    let limits = EngineLimits::DEFAULT;
    let plan = compile(&p, 1, 256, &limits, &Default::default(), &cache).unwrap().plan;
    let (mut engine, mut handle) = Engine::new(SampleRate(48_000), 256, &limits);
    engine.install_plan(plan);
    assert!(handle.send(EngineCommand::SetMode(TransportMode::Song)).is_ok());
    assert!(handle.send(EngineCommand::SeekTick(3840)).is_ok());
    assert!(handle.send(EngineCommand::Play).is_ok());
    let mut out = Vec::new();
    let mut l = vec![0.0f32; 256];
    let mut r = vec![0.0f32; 256];
    for _ in 0..(144_000 / 256) {
        engine.render_block(&mut l, &mut r);
        out.extend_from_slice(&l);
    }
    // first pass: 0.5 s of tone, then silence until the wrap at 2 s (96 000 frames played)
    assert!(rms(&out[1000..20_000]) > 0.2);
    assert!(peak(&out[30_000..95_000]) == 0.0, "silence after the clip before the wrap");
    assert!(rms(&out[97_000..110_000]) > 0.2, "the clip restarts after the loop wrap");
    assert!((dominant_frequency(&out[97_000..110_000], 48_000.0) - 440.0).abs() < 1.0);
    // steady state is allocation-free
    let (_, allocs) = measure_rt(|| {
        for _ in 0..16 {
            engine.render_block(&mut l, &mut r);
        }
    });
    assert_eq!(allocs, 0, "audio clip playback allocates");
    let _ = std::fs::remove_dir_all(dir.parent().unwrap());
}

#[test]
fn flac_export_decodes_back_exactly_and_stems_share_the_render_length() {
    use rawdawg_audio_engine::offline::{ExportFormat, export_project, export_stems, render_project_with};
    let root = std::env::temp_dir().join(format!("rawdawg-export-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let p = project_with_notes(&[(0, 960, 69), (1920, 960, 64)]);
    let opts = OfflineOptions { tail_seconds: 0.25, ..Default::default() };
    let (l, r, stats) = render_project(&p, &opts).unwrap();
    for (format, tol) in [(ExportFormat::Flac24, 2.0 / 8_388_607.0), (ExportFormat::Flac16, 3.0 / 32767.0), (ExportFormat::Wav(WavFormat::Float32), 0.0)] {
        let path = root.join(format!("out-{}.{}", format.label().replace(' ', "_"), format.extension()));
        let mut calls = 0;
        let mut last = 0;
        let st = export_project(&p, &opts, format, &path, &mut |done, total| {
            calls += 1;
            assert!(done >= last && done <= total, "progress monotonic");
            last = done;
            true
        })
        .unwrap();
        assert_eq!(st.frames, stats.frames);
        assert_eq!(last, stats.frames, "progress reaches the end");
        assert!(calls > 10);
        let decoded = rawdawg_sampler::decode::decode_file(&path).unwrap();
        assert_eq!(decoded.sample_rate, 48_000);
        assert_eq!(decoded.channels.len(), 2, "{}", format.label());
        assert_eq!(decoded.frames(), stats.frames, "{}: exact sample count", format.label());
        let max_err = decoded.channels[0].iter().zip(&l).chain(decoded.channels[1].iter().zip(&r)).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
        assert!(max_err <= tol as f32 + 1e-7, "{}: max error {max_err} > {tol}", format.label());
    }
    // cancel: the callback refuses after a few blocks
    let mut n = 0;
    let err = render_project_with(
        &p,
        &opts,
        &mut |_, _| {
            n += 1;
            n < 5
        },
        None,
    )
    .unwrap_err();
    assert!(matches!(err, rawdawg_audio_engine::offline::RenderError::Cancelled));
    assert_eq!(n, 5);
    // stems: one file per mixer channel (channel + master), same length, unique sanitized names
    let mut p2 = p.clone();
    p2.mixer.channels[1].name = "Lead / Synth?".into();
    let master = p2.mixer.master_id;
    p2.mixer.channels.push(rawdawg_project::MixerChannel {
        id: rawdawg_types::MixerChannelId::new(),
        name: "Lead / Synth?".into(),
        color: None,
        gain_db: rawdawg_types::units::Db(0.0),
        pan: rawdawg_types::units::Pan::CENTER,
        mute: false,
        solo: false,
        inserts: vec![],
        sends: vec![],
        output_id: Some(master),
    });
    let dir = root.join("stems");
    let (files, st) = export_stems(&p2, &opts, ExportFormat::Wav(WavFormat::Float32), &dir, &mut |_, _| true).unwrap();
    assert_eq!(files.len(), 3);
    let names: Vec<&str> = files.iter().map(|f| f.name.as_str()).collect();
    assert!(names.iter().all(|n| !n.contains('/') && !n.contains('?')), "{names:?}");
    assert_eq!(names.iter().collect::<std::collections::HashSet<_>>().len(), 3, "unique: {names:?}");
    let mut lens = Vec::new();
    for f in &files {
        let d = rawdawg_sampler::decode::decode_file(&f.path).unwrap();
        lens.push(d.frames());
        assert_eq!(d.frames(), st.frames, "{}", f.name);
    }
    // the instrument channel's stem carries the notes; the master stem is the mix (limiter aside)
    let ch_stem = rawdawg_sampler::decode::decode_file(&files.iter().find(|f| f.mixer_id == p2.mixer.channels[1].id).unwrap().path).unwrap();
    assert!(rms(&ch_stem.channels[0][..20_000]) > 0.05);
    let master_stem = rawdawg_sampler::decode::decode_file(&files.iter().find(|f| f.mixer_id == master).unwrap().path).unwrap();
    assert!((rms(&master_stem.channels[0][..20_000]) - rms(&l[..20_000])).abs() < 1e-3);
    let empty = rawdawg_sampler::decode::decode_file(&files[2].path).unwrap();
    assert_eq!(peak(&empty.channels[0]), 0.0, "an unused bus exports a silent stem of full length");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn browser_preview_plays_an_asset_once_at_its_gain_and_can_be_stopped() {
    use std::sync::atomic::Ordering;
    let limits = EngineLimits::DEFAULT;
    let p = project_with_notes(&[]);
    let (mut engine, mut handle) = Engine::new(SampleRate(48_000), 256, &limits);
    engine.install_plan(compile(&p, 1, 256, &limits, &Default::default(), &rawdawg_audio_engine::NoAssets).unwrap().plan);
    // a 0.25 s 1 kHz tone at 96 kHz: played at 48 kHz it must still be 1 kHz (rate converted)
    let tone: Vec<f32> = (0..24_000).map(|i| (std::f64::consts::TAU * 1000.0 * i as f64 / 96_000.0).sin() as f32 * 0.5).collect();
    let asset = rawdawg_sampler::SampleAsset::mono("tone", 96_000, tone);
    let held = asset.clone();
    assert!(handle.send(EngineCommand::PreviewSample { asset, gain: 0.25 }).is_ok());
    let mut out = Vec::new();
    let mut l = vec![0.0f32; 256];
    let mut r = vec![0.0f32; 256];
    for _ in 0..80 {
        engine.render_block(&mut l, &mut r);
        out.extend_from_slice(&l);
    }
    assert!(!handle.status.preview_active.load(Ordering::Relaxed), "preview ended by itself (0.25 s < 80 blocks)");
    assert!((dominant_frequency(&out[..8192], 48_000.0) - 1000.0).abs() < 2.0, "rate converted");
    let pk = peak(&out[..12_000]);
    assert!((pk - 0.125).abs() < 0.01, "0.5 × gain 0.25: {pk}");
    assert_eq!(peak(&out[13_000..]), 0.0, "silence after the asset ends (12 000 output frames)");
    // stop early
    assert!(handle.send(EngineCommand::PreviewSample { asset: held.clone(), gain: 0.25 }).is_ok());
    engine.render_block(&mut l, &mut r);
    assert!(handle.status.preview_active.load(Ordering::Relaxed));
    assert!(peak(&l) > 0.1);
    assert!(handle.send(EngineCommand::StopPreview).is_ok());
    engine.render_block(&mut l, &mut r);
    assert!(!handle.status.preview_active.load(Ordering::Relaxed));
    // the master limiter's 1 ms lookahead still carries the previous block's tail: the next block is silent
    engine.render_block(&mut l, &mut r);
    assert_eq!(peak(&l), 0.0);
    // steady state with a preview running is allocation-free
    assert!(handle.send(EngineCommand::PreviewSample { asset: held.clone(), gain: 0.25 }).is_ok());
    let (_, allocs) = measure_rt(|| {
        for _ in 0..8 {
            engine.render_block(&mut l, &mut r);
        }
    });
    assert_eq!(allocs, 0);
    drop(held);
}

/// A 150 s mono file (440 Hz for 75 s, then 880 Hz): longer than the resident limit, so
/// it streams. Returns (project dir, project, asset id).
fn long_clip_project(tag: &str) -> (std::path::PathBuf, Project, rawdawg_types::AssetId) {
    use rawdawg_project::{Clip, ClipContent, Track};
    let root = std::env::temp_dir().join(format!("rawdawg-stream-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let wav = root.join("long.wav");
    let spec = hound::WavSpec { channels: 1, sample_rate: 48_000, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
    let mut w = hound::WavWriter::create(&wav, spec).unwrap();
    let total = 150 * 48_000usize;
    for i in 0..total {
        let f = if i < total / 2 { 440.0 } else { 880.0 };
        w.write_sample(((std::f64::consts::TAU * f * i as f64 / 48_000.0).sin() * 16000.0) as i16).unwrap();
    }
    w.finalize().unwrap();
    let dir = root.join("L.rawdawg");
    let mut p = Project::new_default("stream", serde_json::json!({}));
    rawdawg_project::save_project_dir(&dir, &p).unwrap();
    let asset = rawdawg_project::assets::import_asset(&dir, &wav).unwrap();
    p.assets.push(asset.clone());
    let tid = rawdawg_types::TrackId::new();
    p.tracks.push(Track { id: tid, name: "Audio".into(), color: None, mute: false, solo: false, height: None, mixer_id: None });
    // bar 1: the first 2 s of the file; bar 2: 2 s from 100 s into the file (880 Hz region)
    for (bar, offset) in [(0i64, 0u64), (1, 100 * 48_000)] {
        p.clips.push(Clip {
            id: rawdawg_types::ClipId::new(),
            track_id: tid,
            start_tick: Tick(bar * 3840),
            length_ticks: Tick(3840),
            muted: false,
            content: ClipContent::Audio {
                asset_id: asset.id.clone(),
                source_offset_frames: offset,
                source_length_frames: 96_000,
                gain_db: rawdawg_types::units::Db(0.0),
                fade_in_frames: 0,
                fade_out_frames: 0,
                playback_rate: 1.0,
            },
        });
    }
    rawdawg_project::save_project_dir(&dir, &p).unwrap();
    (dir, p, asset.id)
}

#[test]
fn long_audio_clips_stream_from_disk_with_accurate_offsets_and_no_dropouts_offline() {
    use rawdawg_audio_engine::AssetResolver;
    let (dir, p, id) = long_clip_project("offline");
    let mut cache = rawdawg_audio_engine::ProjectAssetCache::new(Some(dir.clone()));
    cache.sync(Some(dir.clone()), &p);
    assert!(cache.sample(&id).is_none(), "150 s is above the resident limit");
    let info = cache.stream_info(&id).expect("probed");
    assert_eq!((info.sample_rate, info.channels, info.frames), (48_000, 1, 150 * 48_000));
    assert!(cache.problems().iter().any(|m| m.contains("streamed")), "{:?}", cache.problems());
    let opts = OfflineOptions { mode: TransportMode::Song, tail_seconds: 0.0, end_tick: Some(Tick(7680)), project_dir: Some(dir.clone()), unprotected: true, ..Default::default() };
    let (l, r, stats) = render_project(&p, &opts).unwrap();
    assert_eq!(stats.stream_dropouts, 0, "offline waits for the stream");
    assert_eq!(stats.nonfinite_blocks, 0);
    assert!((dominant_frequency(&l[2000..40_000], 48_000.0) - 440.0).abs() < 1.0, "bar 1 streams the file start");
    assert!((dominant_frequency(&l[100_000..140_000], 48_000.0) - 880.0).abs() < 1.0, "bar 2 streams from 100 s (accurate seek)");
    let pk = peak(&l[2000..90_000]);
    assert!((pk - 16000.0 / 32768.0).abs() < 0.01, "level {pk}");
    assert!(l.iter().zip(&r).all(|(a, b)| (a - b).abs() < 1e-6), "mono feeds both channels");
    // deterministic: a second render is identical
    let (l2, _, s2) = render_project(&p, &opts).unwrap();
    assert_eq!(s2.stream_dropouts, 0);
    assert_eq!(l, l2);
    let _ = std::fs::remove_dir_all(dir.parent().unwrap());
}

#[test]
fn streamed_clips_play_live_through_loop_wraps_without_a_gap_and_render_allocation_free() {
    use rawdawg_project::LoopRange;
    let (dir, mut p, _id) = long_clip_project("live");
    p.r#loop = Some(LoopRange { start_tick: Tick(0), end_tick: Tick(7680), enabled: true });
    let mut cache = rawdawg_audio_engine::ProjectAssetCache::new(Some(dir.clone()));
    cache.sync(Some(dir.clone()), &p);
    let limits = EngineLimits::DEFAULT;
    let out = compile(&p, 1, 256, &limits, &Default::default(), &cache).unwrap();
    let _worker = out.stream_worker.expect("a stream worker for the long clip");
    let (mut engine, mut handle) = Engine::new(SampleRate(48_000), 256, &limits);
    engine.install_plan(out.plan);
    assert!(handle.send(EngineCommand::SetMode(TransportMode::Song)).is_ok());
    assert!(handle.send(EngineCommand::SetMasterProtection(false)).is_ok());
    // give the worker a moment to pre-fill, then play 2.5 loops (10 s)
    std::thread::sleep(std::time::Duration::from_millis(200));
    assert!(handle.send(EngineCommand::Play).is_ok());
    let mut out_l = Vec::new();
    let mut l = vec![0.0f32; 256];
    let mut r = vec![0.0f32; 256];
    let t0 = std::time::Instant::now();
    let total_blocks = 10 * 48_000 / 256;
    for b in 0..total_blocks {
        // pace roughly like a device so the worker can keep up (2× faster than real time)
        let due = std::time::Duration::from_micros((b as u64 * 256 * 1_000_000 / 48_000) / 2);
        if let Some(wait) = due.checked_sub(t0.elapsed()) {
            std::thread::sleep(wait);
        }
        engine.render_block(&mut l, &mut r);
        out_l.extend_from_slice(&l);
    }
    // first pass: 440 Hz in bar 1, 880 Hz in bar 2
    assert!((dominant_frequency(&out_l[20_000..40_000], 48_000.0) - 440.0).abs() < 1.0);
    assert!((dominant_frequency(&out_l[120_000..160_000], 48_000.0) - 880.0).abs() < 1.0);
    // after each wrap (frames 192 000 and 384 000) bar 1 sounds again at once, played from the
    // clip's loop-entry head while the stream refills behind it, and bar 2 re-enters its clip
    // the same way
    for wrap in [192_000usize, 384_000] {
        // every block right after the wrap sounds (a refill gap would be silent blocks)
        for (k, block) in out_l[wrap..wrap + 9600].chunks(256).enumerate() {
            assert!(rms(block) > 0.2, "no gap after the wrap at {wrap}: block {k} rms {}", rms(block));
        }
        assert!((dominant_frequency(&out_l[wrap + 2000..wrap + 40_000], 48_000.0) - 440.0).abs() < 1.0);
    }
    assert!((dominant_frequency(&out_l[192_000 + 100_000..192_000 + 140_000], 48_000.0) - 880.0).abs() < 1.0);
    assert!(rms(&out_l[192_000 + 96_000..192_000 + 100_800]) > 0.2, "bar 2 re-enters without a gap");
    let dropouts = handle.status.stream_dropouts.get();
    assert_eq!(dropouts, 0, "no streamed frame was missing through 2.5 loop passes");
    // steady state is allocation-free on the audio side
    let (_, allocs) = measure_rt(|| {
        for _ in 0..8 {
            engine.render_block(&mut l, &mut r);
        }
    });
    assert_eq!(allocs, 0, "streamed clip playback allocates on the audio thread");
    drop(engine);
    let _ = std::fs::remove_dir_all(dir.parent().unwrap());
}

#[test]
fn third_party_instances_compile_as_stand_ins_and_their_state_is_preserved() {
    use rawdawg_project::{Instance, PluginFormat};
    let root = std::env::temp_dir().join(format!("rawdawg-missing-plugin-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let dir = root.join("P.rawdawg");
    let mut p = project_with_notes(&[(0, 960, 69)]);
    rawdawg_project::save_project_dir(&dir, &p).unwrap();
    // a CLAP insert on the master with an opaque state file, and a VST3 instrument channel
    let blob = vec![7u8, 1, 9, 3, 0, 255, 42];
    let blob_name = format!("plugin-state/{}.bin", rawdawg_project::preset::sha256_hex(&blob));
    std::fs::write(dir.join(&blob_name), &blob).unwrap();
    let mut clap = Instance::native("com.example.fx", serde_json::json!({ "note": "opaque" }));
    clap.format = PluginFormat::Clap;
    clap.display_name = "Example FX".into();
    clap.opaque_state_path = Some(blob_name.clone());
    let clap_id = clap.id;
    let master = p.mixer.master_id;
    p.mixer.channels.iter_mut().find(|m| m.id == master).unwrap().inserts.push(clap);
    let mut vst = Instance::native("com.example.synth", serde_json::json!({ "preset": 3 }));
    vst.format = PluginFormat::Vst3;
    let mix_id = p.channels[0].mixer_id;
    p.channels.push(rawdawg_project::Channel {
        id: rawdawg_types::ChannelId::new(),
        name: "Ext".into(),
        color: None,
        instrument: vst,
        gain_db: rawdawg_types::units::Db(0.0),
        pan: rawdawg_types::units::Pan::CENTER,
        mute: false,
        solo: false,
        mixer_id: mix_id,
    });
    rawdawg_project::validate(&p).unwrap();
    let limits = EngineLimits::DEFAULT;
    let out = compile(&p, 1, 256, &limits, &Default::default(), &rawdawg_audio_engine::NoAssets).unwrap();
    assert_eq!(out.missing_plugins.len(), 2, "{:?}", out.missing_plugins);
    assert!(out.missing_plugins.iter().any(|m| m.contains("Example FX") && m.contains("Clap")));
    // the native synth still sounds through the pass-through insert; the external instrument is silent
    let (l, _, stats) = render_project(&p, &OfflineOptions { tail_seconds: 0.0, ..Default::default() }).unwrap();
    assert_eq!(stats.nonfinite_blocks, 0);
    assert!(rms(&l[2000..20_000]) > 0.05, "stand-in insert passes audio through");
    assert!((dominant_frequency(&l[2000..20_000], 48_000.0) - 440.0).abs() < 1.0);
    // state round trips through save/load and Save As, bytes untouched
    rawdawg_project::save_project_dir(&dir, &p).unwrap();
    let back = rawdawg_project::load_project_dir(&dir).unwrap();
    let ins = back.mixer_channel(master).unwrap().inserts.iter().find(|i| i.id == clap_id).unwrap();
    assert_eq!(ins.opaque_state_path.as_deref(), Some(blob_name.as_str()));
    assert_eq!(ins.state, serde_json::json!({ "note": "opaque" }));
    assert_eq!(ins.format, PluginFormat::Clap);
    let copy = root.join("Copy.rawdawg");
    rawdawg_project::save_as(Some(&dir), &copy, &back).unwrap();
    assert_eq!(std::fs::read(copy.join(&blob_name)).unwrap(), blob, "opaque state copied by Save As");
    let _ = std::fs::remove_dir_all(&root);
}

/// Two playing synths: one through latency- and tail-bearing inserts (delay, chorus, a
/// spectral effect at 50 % wet/dry so its latency-aligned dry line carries audio, a lookahead
/// limiter), one with no inserts, so its paths into the master and the reverb bus need
/// latency compensation; both send to a reverb bus; master protection on.
fn swap_test_project() -> Project {
    use rawdawg_project::{Instance, Lane, MixerChannel, Send, SendTap};
    let mut p = project_with_notes(&[(0, 960, 60), (960, 960, 64), (1920, 1920, 67)]);
    p.mixer.master_protection = true;
    let master = p.mixer.master_id;
    let bus = rawdawg_types::MixerChannelId::new();
    let plain = rawdawg_types::MixerChannelId::new();
    let strip = |id, name: &str, inserts: Vec<Instance>, sends: Vec<Send>| MixerChannel {
        id,
        name: name.into(),
        color: None,
        gain_db: rawdawg_types::units::Db(0.0),
        pan: rawdawg_types::units::Pan::CENTER,
        mute: false,
        solo: false,
        inserts,
        sends,
        output_id: Some(master),
    };
    let to_bus = || Send { target_id: bus, level_db: rawdawg_types::units::Db(-6.0), tap: SendTap::PostFader, sidechain: false };
    p.mixer.channels.push(strip(bus, "Verb bus", vec![Instance::native("native.fx.reverb", serde_json::Value::Null)], vec![]));
    p.mixer.channels.push(strip(plain, "Plain", vec![], vec![to_bus()]));
    let ch_mixer = p.channels[0].mixer_id;
    let ch = p.mixer.channels.iter_mut().find(|m| m.id == ch_mixer).unwrap();
    let mut spectral = Instance::native("native.fx.spectral", serde_json::Value::Null);
    spectral.mix = 0.5;
    ch.inserts = vec![
        Instance::native("native.fx.delay", serde_json::Value::Null),
        Instance::native("native.fx.chorus", serde_json::Value::Null),
        spectral,
        Instance::native("native.fx.limiter", serde_json::Value::Null),
    ];
    ch.sends.push(to_bus());
    // a second synth on the plain strip, playing its own notes
    let mut second = p.channels[0].clone();
    second.id = rawdawg_types::ChannelId::new();
    second.instrument.id = rawdawg_types::InstanceId::new();
    second.name = "Second".into();
    second.mixer_id = plain;
    let notes = [(480, 1440, 48), (2400, 1200, 55)].map(|(t, d, k)| Note { id: NoteId::new(), tick: Tick(t), duration_ticks: Tick(d), pitch: k, velocity: 0.8, channel: 0 });
    p.patterns[0].lanes.push(Lane { channel_id: second.id, notes: notes.to_vec(), controllers: vec![] });
    p.channels.push(second);
    rawdawg_project::validate(&p).unwrap();
    p
}

/// Render `blocks` blocks, publishing `swap` (compiled from `project`) before block `at`.
fn render_with_swap(project: &Project, swap: Option<(&Project, usize)>, blocks: usize) -> Vec<f32> {
    let limits = EngineLimits::DEFAULT;
    let (mut engine, mut handle) = Engine::new(SampleRate(48_000), 256, &limits);
    let first = compile(project, 1, 256, &limits, &Default::default(), &rawdawg_audio_engine::NoAssets).unwrap();
    let live = first.fingerprints.clone();
    engine.install_plan(first.plan);
    assert!(handle.send(EngineCommand::Play).is_ok());
    let (mut l, mut r) = (vec![0.0f32; 256], vec![0.0f32; 256]);
    let mut out = Vec::with_capacity(blocks * 512);
    for b in 0..blocks {
        if let Some((next, at)) = swap
            && b == at
        {
            let second = compile(next, 2, 256, &limits, &live, &rawdawg_audio_engine::NoAssets).unwrap();
            assert!(handle.publish_plan(second.plan).is_ok());
        }
        l.fill(0.0);
        r.fill(0.0);
        engine.render_block(&mut l, &mut r);
        out.extend_from_slice(&l);
        out.extend_from_slice(&r);
    }
    out
}

#[test]
fn recompiling_unchanged_processors_is_sample_exact_through_tails_latency_and_the_limiter() {
    let p = swap_test_project();
    let plan = compile(&p, 1, 256, &EngineLimits::DEFAULT, &Default::default(), &rawdawg_audio_engine::NoAssets).unwrap().plan;
    assert!(plan.mixer.iter().flat_map(|m| m.inserts.iter()).any(|i| i.has_mix && i.latency > 0), "a latency-bearing insert mixes with its dry line");
    assert!(plan.channels.iter().any(|c| c.comp_frames > 0) || plan.mixer.iter().any(|m| m.comp_frames > 0), "a path into the master is latency-compensated");
    assert!(plan.mixer.iter().flat_map(|m| m.sends.iter()).any(|s| s.comp_frames > 0), "a send into the bus is latency-compensated");
    let reference = render_with_swap(&p, None, 400);
    assert!(rms(&reference) > 0.01, "the test project is audible");
    // the same project recompiled mid-note and mid-tail
    let swapped = render_with_swap(&p, Some((&p, 150)), 400);
    let diff = reference.iter().zip(&swapped).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
    assert_eq!(diff, 0.0, "a recompile with unchanged state changed the output (max diff {diff})");
    // structural edits elsewhere (a new empty bus inserted first, so every mixer index
    // shifts; a new silent channel; a renamed bus) keep every unchanged processor, delay line,
    // send and the limiter running
    let mut edited = p.clone();
    let master = edited.mixer.master_id;
    edited.mixer.channels.insert(
        0,
        rawdawg_project::MixerChannel {
            id: rawdawg_types::MixerChannelId::new(),
            name: "Empty bus".into(),
            color: None,
            gain_db: rawdawg_types::units::Db(0.0),
            pan: rawdawg_types::units::Pan::CENTER,
            mute: false,
            solo: false,
            inserts: vec![rawdawg_project::Instance::native("native.fx.delay", serde_json::Value::Null)],
            sends: vec![],
            output_id: Some(master),
        },
    );
    let mut silent = edited.channels[0].clone();
    silent.id = rawdawg_types::ChannelId::new();
    silent.instrument.id = rawdawg_types::InstanceId::new();
    silent.name = "Silent".into();
    edited.channels.push(silent);
    edited.mixer.channels.iter_mut().find(|m| m.name == "Verb bus").unwrap().name = "Reverb".into();
    rawdawg_project::validate(&edited).unwrap();
    let swapped = render_with_swap(&p, Some((&edited, 150)), 400);
    let diff = reference.iter().zip(&swapped).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
    assert_eq!(diff, 0.0, "an unrelated edit changed the output (max diff {diff})");
}

/// A one-bar pattern holding a note for the whole bar, with a pattern curve on the synth's
/// master gain: quiet for the first half, loud for the second.
fn pattern_automation_project() -> (Project, rawdawg_types::CurveId) {
    use rawdawg_project::{AutomationCurve, AutomationPoint, AutomationTarget, Interpolation};
    use rawdawg_types::{CurveId, PointId};
    let mut p = project_with_notes(&[(0, 3840, 60)]);
    p.patterns[0].length_ticks = Tick(3840);
    let id = CurveId::new();
    let point = |t: i64, v: f32| AutomationPoint { id: PointId::new(), tick: Tick(t), value: v, interpolation: Interpolation::Step };
    p.automation.push(AutomationCurve {
        id,
        name: "gain".into(),
        target: AutomationTarget { instance_id: p.channels[0].instrument.id, param_id: rawdawg_synthesizer::params::MASTER_GAIN },
        points: vec![point(0, 0.2), point(1920, 1.0)],
        mode: rawdawg_project::AutomationMode::Read,
        disabled: false,
        pattern_id: Some(p.patterns[0].id),
    });
    rawdawg_project::validate(&p).unwrap();
    (p, id)
}

#[test]
fn pattern_automation_loops_with_the_pattern_and_follows_its_clips_in_song_mode() {
    use rawdawg_audio_engine::schedule::Schedule;
    use rawdawg_project::{Clip, ClipContent, Track};
    use rawdawg_types::{ClipId, TrackId};
    let (mut p, _) = pattern_automation_project();
    let rate = SampleRate(48_000);
    let tempo = p.tempo_map().unwrap();
    // pattern mode: the curve starts each pass at its start value
    let s = Schedule::for_pattern(&p, p.patterns[0].id, &tempo, rate);
    let events: Vec<(i64, f32)> = s.params.iter().map(|q| (q.frame, q.value)).collect();
    assert_eq!(events, vec![(0, 0.2), (48_000, 1.0)], "value at the pattern start, then the step at beat 3");
    // the real-time engine loops the pattern (offline export plays one pass)
    let limits = EngineLimits::DEFAULT;
    let plan = compile(&p, 1, 256, &limits, &Default::default(), &rawdawg_audio_engine::NoAssets).unwrap().plan;
    let (mut engine, mut handle) = Engine::new(rate, 256, &limits);
    engine.install_plan(plan);
    handle.send(EngineCommand::Play).unwrap();
    let mut l = Vec::new();
    let (mut bl, mut br) = (vec![0.0f32; 256], vec![0.0f32; 256]);
    for _ in 0..(2 * 96_000 / 256) {
        bl.fill(0.0);
        br.fill(0.0);
        engine.render_block(&mut bl, &mut br);
        l.extend_from_slice(&bl);
    }
    // two passes: each one quiet then loud (the loop restarts the curve)
    for pass in 0..2 {
        let base = pass * 96_000;
        let (quiet, loud) = (rms(&l[base + 10_000..base + 44_000]), rms(&l[base + 58_000..base + 92_000]));
        assert!(loud > quiet * 2.0, "pass {pass}: quiet {quiet} then loud {loud}");
    }
    // song mode: no automation clip needed; the curve plays inside a clip of the pattern,
    // entering at the clip's offset and starting again at every loop of the pattern
    let tid = TrackId::new();
    p.tracks.push(Track { id: tid, name: "T".into(), color: None, mute: false, solo: false, height: None, mixer_id: None });
    let pid = p.patterns[0].id;
    p.clips.push(Clip {
        id: ClipId::new(),
        track_id: tid,
        start_tick: Tick(7680),
        length_ticks: Tick(7680),
        muted: false,
        content: ClipContent::Pattern { pattern_id: pid, source_offset_ticks: Tick(1920), looped: true },
    });
    let s = Schedule::for_song(&p, &tempo, rate);
    let events: Vec<(i64, f32)> = s.params.iter().map(|q| (q.frame, q.value)).collect();
    // clip at bar 3 (frame 192000), entering half way: loud; the pattern restarts at frame
    // 240000 (quiet), its step comes at 288000 (loud), it restarts at 336000 (quiet)
    assert_eq!(events, vec![(192_000, 1.0), (240_000, 0.2), (288_000, 1.0), (336_000, 0.2)]);
    // an Off pattern curve plays nowhere
    p.automation[0].mode = rawdawg_project::AutomationMode::Off;
    assert!(Schedule::for_song(&p, &tempo, rate).params.is_empty());
    assert!(Schedule::for_pattern(&p, pid, &tempo, rate).params.is_empty());
}

#[test]
fn removing_a_pattern_takes_its_automation_along_and_undo_brings_it_back() {
    let (mut p, curve) = pattern_automation_project();
    let other = rawdawg_project::Pattern { id: rawdawg_types::PatternId::new(), name: "Pattern 2".into(), lanes: Vec::new(), ..p.patterns[0].clone() };
    let other_id = other.id;
    let mut h = rawdawg_project::History::new();
    h.apply(&mut p, rawdawg_project::Command::AddPattern { pattern: other }).unwrap();
    let pid = p.patterns[0].id;
    h.apply(&mut p, rawdawg_project::Command::RemovePattern { id: pid }).unwrap();
    assert!(p.automation.iter().all(|c| c.id != curve), "the pattern's curve went with it");
    rawdawg_project::validate(&p).unwrap();
    h.undo(&mut p).unwrap();
    assert!(p.automation.iter().any(|c| c.id == curve && c.pattern_id == Some(pid)));
    rawdawg_project::validate(&p).unwrap();
    // a curve pointing at a missing pattern is refused
    let mut broken = p.clone();
    broken.automation[0].pattern_id = Some(rawdawg_types::PatternId::new());
    assert!(rawdawg_project::validate(&broken).is_err());
    assert!(p.patterns.iter().any(|x| x.id == other_id));
}

#[test]
fn timed_live_events_land_on_their_stream_frame_or_are_counted_late() {
    use rawdawg_types::events::{EventSource, TimedEvent};
    let p = project_with_notes(&[]);
    let ch = p.channels[0].id;
    let limits = EngineLimits::DEFAULT;
    // render `blocks` blocks of 256; `send(b)` gives the commands sent before block b
    let run = |send: &dyn Fn(usize) -> Vec<EngineCommand>, blocks: usize| -> (Vec<f32>, u64) {
        let plan = compile(&p, 1, 256, &limits, &Default::default(), &rawdawg_audio_engine::NoAssets).unwrap().plan;
        let (mut engine, mut handle) = Engine::new(SampleRate(48_000), 256, &limits);
        engine.install_plan(plan);
        let (mut l, mut r) = (vec![0.0f32; 256], vec![0.0f32; 256]);
        let mut out = Vec::new();
        for b in 0..blocks {
            for c in send(b) {
                handle.send(c).unwrap();
            }
            engine.render_block(&mut l, &mut r);
            out.extend_from_slice(&l);
        }
        (out, handle.status.late_events.get())
    };
    let note = |at: u64| EngineCommand::LiveAt { channel: ch, event: TimedEvent::note_on(0, EventSource::MidiDevice(0), 0, 1, 69, 0.9), stream_frame: at };
    // reference: the note at the start of block 4 (stream frame 1024)
    let (reference, late) = run(&|b| if b == 4 { vec![note(1024)] } else { vec![] }, 40);
    assert_eq!(late, 0);
    let onset = reference.iter().position(|s| s.abs() > 1e-6).expect("the note sounds");
    assert!((1024..1280).contains(&onset), "the note starts in block 4 (the attack rises from silence): {onset}");
    // the same note for frame 1124 (100 frames into block 4) is the reference shifted by 100
    let (inside, late) = run(&|b| if b == 4 { vec![note(1124)] } else { vec![] }, 40);
    assert_eq!(late, 0);
    assert!(inside[..1124].iter().all(|s| *s == 0.0));
    let d = (0..8000).map(|k| (inside[1124 + k] - reference[1024 + k]).abs()).fold(0.0f32, f32::max);
    assert!(d < 1e-6, "sample-accurate inside the block (max diff {d})");
    // sent two blocks early, for frame 1124 + 512: held, then exactly on its frame
    let (held, late) = run(&|b| if b == 4 { vec![note(1124 + 512)] } else { vec![] }, 40);
    assert_eq!(late, 0);
    let d = (0..8000).map(|k| (held[1636 + k] - reference[1024 + k]).abs()).fold(0.0f32, f32::max);
    assert!(held[..1636].iter().all(|s| *s == 0.0) && d < 1e-6, "held until its block (max diff {d})");
    // its frame has passed: played at the block start, counted late
    let (late_out, late) = run(&|b| if b == 4 { vec![note(500)] } else { vec![] }, 40);
    assert_eq!(late, 1);
    let d = (0..8000).map(|k| (late_out[1024 + k] - reference[1024 + k]).abs()).fold(0.0f32, f32::max);
    assert!(d < 1e-6, "late = as soon as possible (max diff {d})");
    // the hold is bounded: beyond its capacity events play at once and count as late
    let many: Vec<EngineCommand> = (0..rawdawg_audio_engine::engine::MAX_PENDING_LIVE as u64 + 10)
        .map(|k| EngineCommand::LiveAt { channel: ch, event: TimedEvent::note_off(0, EventSource::MidiDevice(0), 0, 1, 69), stream_frame: 100_000 + k })
        .collect();
    let many = std::cell::RefCell::new(Some(many));
    let (_, late) = run(&|b| if b == 1 { many.borrow_mut().take().unwrap() } else { vec![] }, 3);
    assert_eq!(late, 10);
}

#[test]
fn timed_events_placed_from_the_engines_own_stamps_are_never_late_with_large_blocks() {
    use rawdawg_audio_engine::timing::{JitterBuffer, place_event};
    use rawdawg_types::events::{EventSource, TimedEvent};
    let p = project_with_notes(&[]);
    let ch = p.channels[0].id;
    let limits = EngineLimits::DEFAULT;
    let plan = compile(&p, 1, 1024, &limits, &Default::default(), &rawdawg_audio_engine::NoAssets).unwrap().plan;
    let (mut engine, mut handle) = Engine::new(SampleRate(48_000), 1024, &limits);
    engine.install_plan(plan);
    let (mut l, mut r) = (vec![0.0f32; 1024], vec![0.0f32; 1024]);
    let mut jb = JitterBuffer::default();
    for b in 0..40u32 {
        engine.render_block(&mut l, &mut r);
        let st = &handle.status;
        let (frame, wall, block) = (
            st.block_start_frame.load(std::sync::atomic::Ordering::Acquire),
            st.block_wall_us.load(std::sync::atomic::Ordering::Relaxed),
            st.block_frames.load(std::sync::atomic::Ordering::Relaxed),
        );
        assert_eq!(block, 1024);
        // a message sent just now, and one sent 15 ms ago (a UI frame of polling delay)
        let now = rawdawg_rt::clock::now_us();
        for (k, sent) in [now, now.saturating_sub(15_000)].into_iter().enumerate() {
            let at = place_event(&mut jb, frame, wall, block, 48_000, sent, now);
            let event = TimedEvent::note_on(0, EventSource::MidiDevice(0), 0, b * 2 + k as u32, 60, 0.8);
            handle.send(EngineCommand::LiveAt { channel: ch, event, stream_frame: at }).unwrap();
        }
    }
    engine.render_block(&mut l, &mut r);
    assert_eq!(handle.status.late_events.get(), 0, "placed from the engine's stamps, nothing is late");
}

#[test]
fn panic_and_a_devices_all_notes_off_cancel_its_held_notes() {
    use rawdawg_types::events::{EventKind, EventSource, TimedEvent};
    let p = project_with_notes(&[]);
    let ch = p.channels[0].id;
    let limits = EngineLimits::DEFAULT;
    let run = |release: Option<EngineCommand>| -> f32 {
        let plan = compile(&p, 1, 256, &limits, &Default::default(), &rawdawg_audio_engine::NoAssets).unwrap().plan;
        let (mut engine, mut handle) = Engine::new(SampleRate(48_000), 256, &limits);
        engine.install_plan(plan);
        let (mut l, mut r) = (vec![0.0f32; 256], vec![0.0f32; 256]);
        engine.render_block(&mut l, &mut r);
        // a note due two blocks from now from the device, and a typing note due as well
        let note = |src, id| EngineCommand::LiveAt { channel: ch, event: TimedEvent::note_on(0, src, 0, id, 60, 0.8), stream_frame: 256 + 600 };
        handle.send(note(EventSource::MidiDevice(0), 1)).unwrap();
        if let Some(c) = release {
            handle.send(c).unwrap();
        }
        let mut peak = 0.0f32;
        for _ in 0..20 {
            engine.render_block(&mut l, &mut r);
            peak = l.iter().fold(peak, |m, s| m.max(s.abs()));
        }
        peak
    };
    assert!(run(None) > 0.01, "the held note plays on its frame");
    assert_eq!(run(Some(EngineCommand::Panic { source: None })), 0.0, "Panic cancels it");
    assert_eq!(run(Some(EngineCommand::Panic { source: Some(EventSource::MidiDevice(0)) })), 0.0, "a panic of its source cancels it");
    assert!(run(Some(EngineCommand::Panic { source: Some(EventSource::TypingKeyboard) })) > 0.01, "another source's panic does not");
    // the release a disconnected device produces (an all-notes-off sent at once) cancels it too
    let off = EngineCommand::Live { channel: ch, event: TimedEvent::new(0, EventSource::MidiDevice(0), 0, EventKind::AllNotesOff { source: Some(EventSource::MidiDevice(0)) }) };
    assert_eq!(run(Some(off)), 0.0, "no note starts after the device's release");
}

#[test]
fn loop_heads_are_decoded_once_and_reused_by_every_recompile() {
    use rawdawg_audio_engine::AssetResolver;
    let (dir, p, id) = long_clip_project("heads");
    let mut cache = rawdawg_audio_engine::ProjectAssetCache::new(Some(dir.clone()));
    cache.sync(Some(dir.clone()), &p);
    let path = cache.stream_info(&id).expect("streamed").path;
    let limits = EngineLimits::DEFAULT;
    // no loop range: the Loop button can still loop the song at any time, so heads exist
    assert!(p.r#loop.is_none());
    for revision in 1..=5 {
        let out = compile(&p, revision, 256, &limits, &Default::default(), &cache).unwrap();
        drop(out);
    }
    // two clips (bar 1 at the song start, bar 2 inside the loop): one decode each, ever
    assert_eq!(rawdawg_audio_engine::stream::heads_decoded(&path), 2, "five compiles, two heads decoded");
    let _ = std::fs::remove_dir_all(dir.parent().unwrap());
}

#[test]
fn a_morph_rack_keeps_its_notes_and_automated_position_across_a_recompile() {
    use rawdawg_synthesizer::morph_rack::{MorphRackState, PARAM_X};
    use rawdawg_synthesizer::patch::{Layer, Patch};
    // a held note; corner A a sine, corner B the same an octave up
    let mut p = project_with_notes(&[(0, 3840, 57)]);
    let sine = Patch { layers: vec![Layer::basic(rawdawg_dsp::oscillator::BasicWave::Sine)], ..Patch::default() };
    let mut up = sine.clone();
    up.layers[0].coarse_semitones = 12;
    let state = MorphRackState::new([sine.clone(), up.clone(), sine, up], 0.0, 0.0);
    p.channels[0].instrument = rawdawg_project::Instance::native(rawdawg_types::ids::native_types::MORPH, serde_json::to_value(&state).unwrap());
    let rack = p.channels[0].instrument.id;
    let limits = EngineLimits::DEFAULT;
    let first = compile(&p, 1, 256, &limits, &Default::default(), &rawdawg_audio_engine::NoAssets).unwrap();
    let live = first.fingerprints.clone();
    let (mut engine, mut handle) = Engine::new(SampleRate(48_000), 256, &limits);
    engine.install_plan(first.plan);
    handle.send(EngineCommand::Play).unwrap();
    // automation (a parameter event) moves X to corner B while the note plays
    handle.send(EngineCommand::Param { instance: rack, id: rawdawg_types::ParamId(PARAM_X), value: 1.0 }).unwrap();
    let (mut l, mut r) = (vec![0.0f32; 256], vec![0.0f32; 256]);
    let mut out = Vec::new();
    for _ in 0..80 {
        engine.render_block(&mut l, &mut r);
        out.extend_from_slice(&l);
    }
    // an unrelated edit recompiles: the rack's state is unchanged, so the running rack continues
    p.channels[0].name = "Renamed".into();
    let second = compile(&p, 2, 256, &limits, &live, &rawdawg_audio_engine::NoAssets).unwrap();
    assert!(handle.publish_plan(second.plan).is_ok());
    let swap_at = out.len();
    for _ in 0..80 {
        engine.render_block(&mut l, &mut r);
        out.extend_from_slice(&l);
    }
    assert!((dominant_frequency(&out[swap_at - 8192..swap_at], 48_000.0) - 440.0).abs() < 1.0, "at B before the edit");
    assert!((dominant_frequency(&out[swap_at + 1024..swap_at + 9216], 48_000.0) - 440.0).abs() < 1.0, "still at B after it (not back at the saved X)");
    for (k, block) in out[swap_at - 256..swap_at + 2048].chunks(256).enumerate() {
        assert!(rms(block) > 0.01, "the note keeps sounding through the swap (block {k})");
    }
}
