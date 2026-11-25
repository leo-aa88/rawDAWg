//! Headless stress soak (TESTING.md "Performance": 30 minutes of mixed edits, record/play/
//! seek, graph swaps and sample import; resident memory after warm-up; no panic, hang or
//! stuck notes). The engine renders on its own thread at real-time pace; the main thread
//! plays the UI's role: it edits the project through the undo history, recompiles and
//! publishes plans, moves the transport, plays live notes and toggles recording.
//! Run: `cargo run --release -p xtask -- soak --minutes 30`.

use anyhow::{Context, bail};
use rawdawg_audio_engine::commands::{EngineCommand, TransportMode};
use rawdawg_audio_engine::engine::Engine;
use rawdawg_dsp::processor::Processor;
use rawdawg_project::{Command, History, Instance, Note};
use rawdawg_types::events::{EventSource, TimedEvent};
use rawdawg_types::limits::EngineLimits;
use rawdawg_types::units::{Db, PPQ, Tick, UsPerQuarter};
use rawdawg_types::{ChannelId, InstanceId, MixerChannelId, NoteId};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

const BLOCK: usize = 256;
const SR: u32 = 48_000;

struct Rng(u64);
impl Rng {
    fn next_u64(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        if n == 0 { 0 } else { (self.next_u64() % n as u64) as usize }
    }
    fn unit(&mut self) -> f32 {
        (self.next_u64() >> 40) as f32 / (1u64 << 24) as f32
    }
}

fn rss_bytes() -> Option<u64> {
    let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
    let pages: u64 = statm.split_whitespace().nth(1)?.parse().ok()?;
    Some(pages * 4096)
}

/// Audio-thread statistics shared with the main thread.
#[derive(Default)]
struct AudioStats {
    blocks: AtomicU64,
    nonfinite: AtomicU64,
    late_blocks: AtomicU64,
    max_render_ns: AtomicU64,
}

#[derive(serde::Serialize)]
struct StuckCheck {
    at_seconds: f64,
    voices_after_stop: u64,
    waited_ms: u64,
    /// Longest synth amp release set in the project at the time (seconds).
    longest_release_s: f64,
}

/// Longest amp release among the project's synths: a stopped voice may legitimately ring
/// this long, so the stuck-note check waits for it before calling a voice stuck.
fn longest_release(project: &rawdawg_project::Project) -> f64 {
    let mut longest = 0.0f64;
    for c in &project.channels {
        if c.instrument.type_id.as_str() != rawdawg_types::ids::native_types::SYNTH {
            continue;
        }
        let mut s = rawdawg_synthesizer::Synth::default();
        if serde_json::to_vec(&c.instrument.state).ok().is_some_and(|b| s.load_state(&b).is_ok())
            && let Some(d) = s.descriptors().iter().find(|d| d.key == "amp.release")
            && let Some(v) = s.param(d.id)
        {
            longest = longest.max(d.to_physical(v));
        }
    }
    longest
}

pub fn run(root: &Path, minutes: f64, seed: u64) -> anyhow::Result<()> {
    if cfg!(debug_assertions) {
        eprintln!("warning: debug build — run `cargo run --release -p xtask -- soak` for evidence");
    }
    let out = root.join("target/verification");
    std::fs::create_dir_all(&out)?;
    let dir = out.join("soak.rawdawg");
    let mut project = crate::bench::reference_project(root, &dir, 64)?;
    let mut history = History::new();
    let limits = EngineLimits::DEFAULT;
    let mut cache = rawdawg_audio_engine::ProjectAssetCache::new(Some(dir.clone()));
    cache.sync(Some(dir.clone()), &project);
    let first = rawdawg_audio_engine::compile(&project, 1, BLOCK, &limits, &Default::default(), &cache)?;
    let mut live = first.fingerprints.clone();
    let mut _worker = first.stream_worker;
    let (mut engine, mut handle) = Engine::new(project.sample_rate_hz, BLOCK, &limits);
    engine.install_plan(first.plan);
    let status = handle.status.clone();

    // ---- audio thread: real-time paced renders
    let stop = Arc::new(AtomicBool::new(false));
    let stats = Arc::new(AudioStats::default());
    let audio = {
        let (stop, stats) = (stop.clone(), stats.clone());
        std::thread::Builder::new().name("soak-audio".into()).spawn(move || {
            let mut l = vec![0.0f32; BLOCK];
            let mut r = vec![0.0f32; BLOCK];
            let period = Duration::from_nanos(BLOCK as u64 * 1_000_000_000 / u64::from(SR));
            let mut next = Instant::now();
            while !stop.load(Ordering::Relaxed) {
                l.fill(0.0);
                r.fill(0.0);
                let t = Instant::now();
                engine.render_block(&mut l, &mut r);
                let ns = t.elapsed().as_nanos() as u64;
                stats.max_render_ns.fetch_max(ns, Ordering::Relaxed);
                if l.iter().chain(r.iter()).any(|v| !v.is_finite()) {
                    stats.nonfinite.fetch_add(1, Ordering::Relaxed);
                }
                stats.blocks.fetch_add(1, Ordering::Relaxed);
                next += period;
                let now = Instant::now();
                if now < next {
                    std::thread::sleep(next - now);
                } else {
                    stats.late_blocks.fetch_add(1, Ordering::Relaxed);
                    if now - next > period * 50 {
                        next = now; // fell far behind (debugger, suspend): resynchronise
                    }
                }
            }
            engine
        })?
    };

    // ---- main thread: the editor
    let mut rng = Rng(seed | 1);
    let pattern = project.patterns[0].id;
    let synth_channels: Vec<(ChannelId, InstanceId)> =
        project.channels.iter().filter(|c| c.instrument.type_id.as_str() == rawdawg_types::ids::native_types::SYNTH).map(|c| (c.id, c.instrument.id)).collect();
    let track_mixers: Vec<MixerChannelId> = project.channels.iter().map(|c| c.mixer_id).collect();
    let fixtures = ["sine_440_48k.wav", "noise_48k.wav", "sweep_20_20k_48k.wav", "impulse_48k.wav", "sine_440_48k.flac"];
    // what the soak adds is recognised by not being part of the starting project; caps count
    // the project itself, so undo of a removal can never push the totals past them
    let original_inserts: std::collections::HashSet<InstanceId> = project.mixer.channels.iter().flat_map(|m| m.inserts.iter().map(|i| i.id)).collect();
    let original_channels: std::collections::HashSet<ChannelId> = project.channels.iter().map(|c| c.id).collect();
    let mut held: Vec<(ChannelId, u32, u8, Instant)> = Vec::new();
    let mut counts: HashMap<&'static str, u64> = HashMap::new();
    let mut revision = 1u64;
    let mut refused = 0u64;
    let mut compile_errors: Vec<String> = Vec::new();
    let mut stuck: Vec<StuckCheck> = Vec::new();
    let mut rss: Vec<(f64, u64)> = Vec::new();
    let mut samples: Vec<serde_json::Value> = Vec::new();
    let mut recording = false;
    let mut recorded_events = 0u64;
    let mut next_note_id = 1u32;
    let started = Instant::now();
    let total = Duration::from_secs_f64(minutes * 60.0);
    let mut next_rss = Instant::now();
    let mut next_stuck_check = Instant::now() + Duration::from_secs(60);
    let _ = handle.send(EngineCommand::SetMode(TransportMode::Song));
    let _ = handle.send(EngineCommand::Play);

    while started.elapsed() < total {
        if !handle.is_engine_alive() || audio.is_finished() {
            bail!("the audio thread stopped after {:.1} s", started.elapsed().as_secs_f64());
        }
        if Instant::now() >= next_rss {
            if let Some(b) = rss_bytes() {
                rss.push((started.elapsed().as_secs_f64(), b));
                let notes: usize = project.patterns.iter().flat_map(|p| p.lanes.iter()).map(|l| l.notes.len()).sum();
                samples.push(serde_json::json!({
                    "t": started.elapsed().as_secs_f64(),
                    "rss_mb": b as f64 / 1e6,
                    "live_heap_mb": crate::live_heap_bytes() as f64 / 1e6,
                    "notes": notes,
                    "project_json_kb": rawdawg_project::persist::project_to_json(&project).len() as f64 / 1e3,
                    "undo_depth": history.undo_len(),
                    "inserts": project.mixer.channels.iter().map(|m| m.inserts.len()).sum::<usize>(),
                    "channels": project.channels.len(),
                    "late_blocks": stats.late_blocks.load(Ordering::Relaxed),
                    "voices": status.active_voices.load(Ordering::Relaxed),
                }));
            }
            next_rss += Duration::from_secs(5);
        }
        while handle.poll_recorded().is_some() {
            recorded_events += 1;
        }
        // release live notes whose time is up
        let now = Instant::now();
        held.retain(|(ch, id, pitch, until)| {
            if now >= *until {
                let _ = handle.send(EngineCommand::Live { channel: *ch, event: TimedEvent::note_off(0, EventSource::TypingKeyboard, 0, *id, *pitch) });
                false
            } else {
                true
            }
        });
        // periodic stuck-note check: everything released and stopped → voices must reach 0
        if now >= next_stuck_check {
            for (ch, id, pitch, _) in held.drain(..) {
                let _ = handle.send(EngineCommand::Live { channel: ch, event: TimedEvent::note_off(0, EventSource::TypingKeyboard, 0, id, pitch) });
            }
            let _ = handle.send(EngineCommand::SetRecording(false));
            recording = false;
            let _ = handle.send(EngineCommand::Stop);
            let t = Instant::now();
            let release = longest_release(&project);
            let limit = Duration::from_secs_f64((release + 2.0).clamp(2.0, 40.0));
            let mut voices = status.active_voices.load(Ordering::Relaxed);
            while t.elapsed() < limit {
                std::thread::sleep(Duration::from_millis(50));
                voices = status.active_voices.load(Ordering::Relaxed);
                if voices == 0 {
                    break;
                }
            }
            stuck.push(StuckCheck {
                at_seconds: started.elapsed().as_secs_f64(),
                voices_after_stop: voices,
                waited_ms: t.elapsed().as_millis() as u64,
                longest_release_s: release,
            });
            let _ = handle.send(EngineCommand::Play);
            next_stuck_check = Instant::now() + Duration::from_secs(60);
        }

        // one random action
        let kind = rng.below(100);
        let mut cmd: Option<Command> = None;
        let label: &'static str;
        match kind {
            0..=24 => {
                // note edits keep each lane between 32 and 96 notes: the soak must measure a
                // constant workload, not a song that keeps growing for half an hour
                let (ch, _) = synth_channels[rng.below(synth_channels.len())];
                let lane_len = project.patterns[0].lanes.iter().find(|l| l.channel_id == ch).map(|l| l.notes.len()).unwrap_or(0);
                let add = lane_len < 32 || (lane_len < 96 && rng.below(2) == 0);
                if add {
                    label = "add notes";
                    let len = project.patterns[0].length_ticks.0;
                    let notes: Vec<Note> = (0..1 + rng.below(6))
                        .map(|_| {
                            let dur = PPQ / 4 * (1 + rng.below(8) as i64);
                            let tick = (rng.below(((len - dur) / (PPQ / 4)).max(1) as usize) as i64) * (PPQ / 4);
                            Note { id: NoteId::new(), tick: Tick(tick), duration_ticks: Tick(dur), pitch: 36 + rng.below(48) as u8, velocity: 0.2 + 0.7 * rng.unit(), channel: 0 }
                        })
                        .collect();
                    cmd = Some(Command::AddNotes { pattern_id: pattern, channel_id: ch, notes });
                } else {
                    label = "remove notes";
                    if let Some(lane) = project.patterns[0].lanes.iter().find(|l| l.channel_id == ch) {
                        let ids: Vec<NoteId> =
                            (0..1 + rng.below(6)).map(|_| lane.notes[rng.below(lane.notes.len())].id).collect::<std::collections::BTreeSet<_>>().into_iter().collect();
                        cmd = Some(Command::RemoveNotes { pattern_id: pattern, channel_id: ch, note_ids: ids });
                    }
                }
            }
            25..=36 => {
                label = "synth parameter";
                let (_, inst) = synth_channels[rng.below(synth_channels.len())];
                if let Some(i) = project.instance(inst) {
                    let mut s = rawdawg_synthesizer::Synth::default();
                    if s.load_state(&serde_json::to_vec(&i.state)?).is_ok() {
                        let d: Vec<_> = s.descriptors().iter().filter(|d| !d.flags.hidden && d.flags.automatable).collect();
                        let pick = d[rng.below(d.len())];
                        s.set_param(pick.id, rng.unit());
                        cmd = Some(Command::SetInstanceState { instance_id: inst, state_version: 1, state: serde_json::from_slice(&s.save_state())? });
                    }
                }
            }
            37..=46 => {
                label = "effect parameter";
                let inserts: Vec<&Instance> = project.mixer.channels.iter().flat_map(|m| m.inserts.iter()).collect();
                let i = inserts[rng.below(inserts.len())];
                if let Ok(mut fx) = rawdawg_effects::make_effect(i.type_id.as_str(), Some(&serde_json::to_vec(&i.state)?)) {
                    let d: Vec<_> = fx.descriptors().iter().filter(|d| !d.flags.hidden).copied().collect();
                    if !d.is_empty() {
                        let pick = d[rng.below(d.len())];
                        fx.set_param(pick.id, rng.unit());
                        cmd = Some(Command::SetInstanceState { instance_id: i.id, state_version: 1, state: serde_json::from_slice(&fx.save_state())? });
                    }
                }
            }
            47..=51 => {
                label = "add/remove effect";
                let added_inserts: Vec<(MixerChannelId, InstanceId)> =
                    project.mixer.channels.iter().flat_map(|m| m.inserts.iter().filter(|i| !original_inserts.contains(&i.id)).map(move |i| (m.id, i.id))).collect();
                if added_inserts.len() < 12 && rng.below(2) == 0 {
                    let listed: Vec<_> = rawdawg_effects::CATALOG.iter().filter(|i| i.listed).collect();
                    let info = listed[rng.below(listed.len())];
                    let mut inst = Instance::native(info.type_id, serde_json::Value::Null);
                    inst.display_name = info.name.into();
                    let m = track_mixers[rng.below(track_mixers.len())];
                    cmd = Some(Command::AddInsert { mixer_id: m, instance: inst, index: None });
                } else if !added_inserts.is_empty() {
                    let (m, id) = added_inserts[rng.below(added_inserts.len())];
                    cmd = Some(Command::RemoveInsert { mixer_id: m, instance_id: id });
                }
            }
            52..=56 => {
                label = "mixer gain";
                let m = track_mixers[rng.below(track_mixers.len())];
                cmd = Some(Command::SetMixerGain { id: m, gain_db: Db(-18.0 + 12.0 * f64::from(rng.unit())) });
            }
            57..=59 => {
                label = "sample import + channel";
                let added_channels: Vec<ChannelId> = project.channels.iter().filter(|c| !original_channels.contains(&c.id)).map(|c| c.id).collect();
                if added_channels.len() < 6 && rng.below(3) != 0 {
                    let file = fixtures[rng.below(fixtures.len())];
                    let asset = rawdawg_project::assets::import_asset(&dir, &root.join("fixtures").join(file)).context("importing a sample")?;
                    let mut cmds = Vec::new();
                    if !project.assets.iter().any(|a| a.id == asset.id) {
                        cmds.push(Command::AddAsset { asset: asset.clone() });
                    }
                    let patch = rawdawg_sampler::SamplerPatch { asset_id: Some(asset.id.clone()), name: file.into(), ..Default::default() };
                    let mut inst = Instance::native(rawdawg_types::ids::native_types::SAMPLER, serde_json::to_value(&patch)?);
                    inst.display_name = file.into();
                    let id = ChannelId::new();
                    let ch = rawdawg_project::Channel {
                        id,
                        name: format!("Import {file}"),
                        color: None,
                        instrument: inst,
                        gain_db: Db(-12.0),
                        pan: rawdawg_types::units::Pan(0.0),
                        mute: false,
                        solo: false,
                        mixer_id: track_mixers[0],
                    };
                    cmds.push(Command::AddChannel { channel: ch, index: None });
                    cmds.push(Command::AddNotes {
                        pattern_id: pattern,
                        channel_id: id,
                        notes: (0..4).map(|b| Note { id: NoteId::new(), tick: Tick(b * PPQ * 2), duration_ticks: Tick(PPQ / 2), pitch: 60, velocity: 0.7, channel: 0 }).collect(),
                    });
                    cmd = Some(Command::Batch { commands: cmds });
                } else if !added_channels.is_empty() {
                    cmd = Some(Command::RemoveChannel { id: added_channels[rng.below(added_channels.len())] });
                }
            }
            60..=62 => {
                label = "tempo";
                cmd = Some(Command::SetTempo { index: 0, us_per_quarter: UsPerQuarter((60_000_000.0 / (90.0 + 70.0 * f64::from(rng.unit()))) as u32) });
            }
            63..=69 => {
                label = "undo/redo";
                if rng.below(3) != 0 {
                    let _ = history.undo(&mut project);
                } else {
                    let _ = history.redo(&mut project);
                }
            }
            70..=77 => {
                label = "transport";
                let c = match rng.below(6) {
                    0 => EngineCommand::Stop,
                    1 | 2 => EngineCommand::Play,
                    3 => EngineCommand::SeekTick((rng.below(64) as i64) * PPQ * 4),
                    4 => EngineCommand::SetMode(if rng.below(2) == 0 { TransportMode::Song } else { TransportMode::Pattern }),
                    _ => EngineCommand::SetLoopEnabled(rng.below(2) == 0),
                };
                let _ = handle.send(c);
            }
            78..=93 => {
                label = "live note";
                if held.len() < 16 {
                    let (ch, _) = synth_channels[rng.below(synth_channels.len())];
                    let pitch = 40 + rng.below(40) as u8;
                    next_note_id += 1;
                    let _ = handle
                        .send(EngineCommand::Live { channel: ch, event: TimedEvent::note_on(0, EventSource::TypingKeyboard, 0, next_note_id, pitch, 0.4 + 0.5 * rng.unit()) });
                    held.push((ch, next_note_id, pitch, Instant::now() + Duration::from_millis(50 + rng.below(1500) as u64)));
                }
            }
            _ => {
                label = "recording";
                recording = !recording;
                let _ = handle.send(EngineCommand::SetRecording(recording));
            }
        }
        *counts.entry(label).or_default() += 1;
        let changed = if let Some(c) = cmd { history.apply(&mut project, c).is_ok() } else { label == "undo/redo" };
        if changed {
            revision += 1;
            cache.sync(Some(dir.clone()), &project);
            match rawdawg_audio_engine::compile(&project, revision, BLOCK, &limits, &live, &cache) {
                Ok(out) => {
                    handle.collect_retired();
                    match handle.publish_plan(out.plan) {
                        Ok(_) => {
                            live = out.fingerprints;
                            _worker = out.stream_worker;
                        }
                        Err(_) => refused += 1,
                    }
                }
                Err(e) => {
                    if compile_errors.len() < 20 {
                        compile_errors.push(e.to_string());
                    }
                }
            }
        }
        std::thread::sleep(Duration::from_millis(20 + rng.below(80) as u64));
    }

    // ---- final checks
    for (ch, id, pitch, _) in held.drain(..) {
        let _ = handle.send(EngineCommand::Live { channel: ch, event: TimedEvent::note_off(0, EventSource::TypingKeyboard, 0, id, pitch) });
    }
    let _ = handle.send(EngineCommand::SetRecording(false));
    let _ = handle.send(EngineCommand::Stop);
    let t = Instant::now();
    let release = longest_release(&project);
    let limit = Duration::from_secs_f64((release + 2.0).clamp(2.0, 40.0));
    let mut final_voices = status.active_voices.load(Ordering::Relaxed);
    while t.elapsed() < limit && final_voices != 0 {
        std::thread::sleep(Duration::from_millis(50));
        final_voices = status.active_voices.load(Ordering::Relaxed);
    }
    stuck.push(StuckCheck { at_seconds: started.elapsed().as_secs_f64(), voices_after_stop: final_voices, waited_ms: t.elapsed().as_millis() as u64, longest_release_s: release });
    stop.store(true, Ordering::Relaxed);
    let audio_alive = !audio.is_finished();
    let _engine = audio.join().map_err(|_| anyhow::anyhow!("the audio thread panicked"))?;
    handle.collect_retired();

    // memory: growth after warm-up (first 5 minutes or a third of the run, whichever is shorter)
    let warm = (minutes * 60.0 / 3.0).min(300.0);
    let after: Vec<(f64, u64)> = rss.iter().copied().filter(|(t, _)| *t >= warm).collect();
    let (warm_rss, end_rss, max_rss) = match (after.first(), after.last()) {
        (Some(a), Some(b)) => (a.1, b.1, after.iter().map(|x| x.1).max().unwrap_or(0)),
        _ => (0, 0, 0),
    };
    let growth = if warm_rss > 0 { (end_rss as f64 - warm_rss as f64) / warm_rss as f64 * 100.0 } else { 0.0 };
    // least-squares slope after warm-up, MB per hour
    let slope_mb_h = if after.len() >= 2 {
        let n = after.len() as f64;
        let mx = after.iter().map(|x| x.0).sum::<f64>() / n;
        let my = after.iter().map(|x| x.1 as f64).sum::<f64>() / n;
        let num: f64 = after.iter().map(|x| (x.0 - mx) * (x.1 as f64 - my)).sum();
        let den: f64 = after.iter().map(|x| (x.0 - mx).powi(2)).sum();
        if den > 0.0 { num / den * 3600.0 / 1e6 } else { 0.0 }
    } else {
        0.0
    };
    let blocks = stats.blocks.load(Ordering::Relaxed);
    let report = serde_json::json!({
        "minutes": minutes,
        "seed": seed,
        "build": if cfg!(debug_assertions) { "debug" } else { "release" },
        "actions": counts,
        "plan_publishes_refused": refused,
        "engine_refused_swaps": status.refused_swaps.get(),
        "compile_errors": compile_errors,
        "audio_blocks": blocks,
        "audio_seconds": blocks as f64 * BLOCK as f64 / f64::from(SR),
        "late_blocks": stats.late_blocks.load(Ordering::Relaxed),
        "max_render_ms": stats.max_render_ns.load(Ordering::Relaxed) as f64 / 1e6,
        "nonfinite_blocks": stats.nonfinite.load(Ordering::Relaxed),
        "engine_nonfinite_blocks": status.nonfinite_blocks.get(),
        "dropped_events": status.dropped_events.get(),
        "command_overflows": status.command_overflows.get(),
        "record_overflows": status.record_overflows.get(),
        "stream_dropouts": status.stream_dropouts.get(),
        "recorded_events": recorded_events,
        "undo_depth": history.undo_len(),
        "stuck_note_checks": stuck,
        "rss_samples": rss,
        "samples": samples,
        "rss_warmup_seconds": warm,
        "rss_after_warmup_mb": warm_rss as f64 / 1e6,
        "rss_end_mb": end_rss as f64 / 1e6,
        "rss_max_after_warmup_mb": max_rss as f64 / 1e6,
        "rss_growth_percent": growth,
        "rss_slope_mb_per_hour": slope_mb_h,
        "audio_thread_alive_at_end": audio_alive,
    });
    std::fs::write(out.join("soak.json"), serde_json::to_vec_pretty(&report)?)?;
    let stuck_fail: Vec<&StuckCheck> = stuck.iter().filter(|s| s.voices_after_stop != 0).collect();
    println!(
        "soak {minutes:.1} min: {} actions, {:.0} s audio, late blocks {}, max render {:.2} ms, non-finite blocks {}, refused publishes {refused}, compile errors {}, stuck-note checks {}/{} clean, RSS {:.1} → {:.1} MB after warm-up ({growth:+.1}%, slope {slope_mb_h:+.1} MB/h)",
        counts.values().sum::<u64>(),
        blocks as f64 * BLOCK as f64 / f64::from(SR),
        stats.late_blocks.load(Ordering::Relaxed),
        stats.max_render_ns.load(Ordering::Relaxed) as f64 / 1e6,
        stats.nonfinite.load(Ordering::Relaxed),
        compile_errors.len(),
        stuck.len() - stuck_fail.len(),
        stuck.len(),
        warm_rss as f64 / 1e6,
        end_rss as f64 / 1e6,
    );
    println!("report: {}", out.join("soak.json").display());
    if !stuck_fail.is_empty() {
        bail!("voices still sounding after stop: {:?}", stuck_fail.iter().map(|s| (s.at_seconds, s.voices_after_stop)).collect::<Vec<_>>());
    }
    if stats.nonfinite.load(Ordering::Relaxed) > 0 {
        bail!("non-finite output blocks");
    }
    if !compile_errors.is_empty() {
        bail!("compile errors during the soak: {compile_errors:?}");
    }
    Ok(())
}
