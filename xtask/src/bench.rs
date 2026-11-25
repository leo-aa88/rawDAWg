//! Reference benchmarks (TESTING.md §"Performance", QUAL-04): the default workload,
//! per-effect and per-voice costs, and plan compilation time. Offline only: these are
//! block-render times of the engine on this machine, not device callback timings (HW-01).
//! Run as a release build: `cargo run --release -p xtask -- bench-audio`.

use anyhow::{Context, bail};
use rawdawg_audio_engine::commands::{EngineCommand, TransportMode};
use rawdawg_audio_engine::engine::Engine;
use rawdawg_dsp::processor::{PrepareSpec, ProcessContext, TransportView};
use rawdawg_dsp::{AudioBlock, BusLayout};
use rawdawg_project::{
    AutomationCurve, AutomationMode, AutomationPoint, AutomationTarget, Channel, Clip, ClipContent, Instance, Interpolation, Lane, MixerChannel, Note, Project, Send, SendTap,
    Track,
};
use rawdawg_types::limits::EngineLimits;
use rawdawg_types::units::{Db, PPQ, Pan, SampleRate, Tick};
use rawdawg_types::{ChannelId, ClipId, CurveId, MixerChannelId, NoteId, PointId, TrackId};
use std::path::Path;
use std::time::Instant;

const SR: u32 = 48_000;

fn fx(type_id: &str, name: &str, values: &[(&str, f64)]) -> anyhow::Result<Instance> {
    let mut p = rawdawg_effects::make_effect(type_id, None).map_err(anyhow::Error::msg)?;
    for (key, v) in values {
        let d = p.descriptors().iter().find(|d| d.key == *key).copied().with_context(|| format!("{type_id} has no param {key}"))?;
        p.set_param(d.id, d.to_normalized(*v));
    }
    let mut inst = Instance::native(type_id, serde_json::from_slice(&p.save_state())?);
    inst.display_name = name.to_owned();
    Ok(inst)
}

/// TESTING.md default fixture: 8 tracks — four native synths holding 8-note chords and
/// four sampler tracks — each on its own mixer channel with EQ + compressor, two send
/// reverbs (hall, plate), a master limiter, and automation of a synth filter and an EQ
/// gain across the song. Assets are imported into `dir`.
pub fn reference_project(root: &Path, dir: &Path, bars: i64) -> anyhow::Result<Project> {
    let _ = std::fs::remove_dir_all(dir);
    std::fs::create_dir_all(dir)?;
    let synth_patch = serde_json::to_value(rawdawg_synthesizer::Patch::default())?;
    let mut p = Project::new_default("bench-reference", synth_patch.clone());
    p.channels.clear();
    p.patterns[0].lanes.clear();
    let master = p.mixer.master_id;
    rawdawg_project::save_project_dir(dir, &p)?;
    let bus = |id: MixerChannelId, name: &str, inserts: Vec<Instance>, sends: Vec<Send>| MixerChannel {
        id,
        name: name.into(),
        color: None,
        gain_db: Db(-6.0),
        pan: Pan(0.0),
        mute: false,
        solo: false,
        inserts,
        sends,
        output_id: Some(master),
    };
    let (hall_id, plate_id) = (MixerChannelId::new(), MixerChannelId::new());
    p.mixer.channels.push(bus(hall_id, "Hall", vec![fx(rawdawg_effects::reverb::TYPE_ID, "Hall", &[("mode", 1.0), ("decay", 2.5), ("mix", 100.0)])?], vec![]));
    p.mixer.channels.push(bus(plate_id, "Plate", vec![fx(rawdawg_effects::reverb::TYPE_ID, "Plate", &[("mode", 3.0), ("decay", 1.2), ("mix", 100.0)])?], vec![]));
    let sends = || {
        vec![
            Send { target_id: hall_id, level_db: Db(-12.0), tap: SendTap::PostFader, sidechain: false },
            Send { target_id: plate_id, level_db: Db(-15.0), tap: SendTap::PostFader, sidechain: false },
        ]
    };
    let strip = || -> anyhow::Result<Vec<Instance>> {
        Ok(vec![
            fx(
                rawdawg_effects::eq::TYPE_ID,
                "EQ",
                &[("band0.on", 1.0), ("band0.freq", 250.0), ("band0.gain", -3.0), ("band1.on", 1.0), ("band1.freq", 4000.0), ("band1.gain", 2.0)],
            )?,
            fx(rawdawg_effects::dynamics::compressor::TYPE_ID, "Comp", &[("threshold", -18.0), ("ratio", 3.0)])?,
        ])
    };
    let bar = PPQ * 4;
    let pattern_len = bar * 4;
    p.patterns[0].length_ticks = Tick(pattern_len);
    let mut eq_of_first = None;
    let mut first_synth = None;
    // four synths × 8 held voices, retriggered every bar
    for s in 0..4u8 {
        let mix = MixerChannelId::new();
        let inserts = strip()?;
        if s == 0 {
            eq_of_first = Some(inserts[0].id);
        }
        p.mixer.channels.push(bus(mix, &format!("Synth {}", s + 1), inserts, sends()));
        let id = ChannelId::new();
        let mut inst = Instance::native(rawdawg_types::ids::native_types::SYNTH, synth_patch.clone());
        inst.display_name = format!("Synth {}", s + 1);
        if s == 0 {
            first_synth = Some(inst.id);
        }
        p.channels.push(Channel {
            id,
            name: format!("Synth {}", s + 1),
            color: None,
            instrument: inst,
            gain_db: Db(-12.0),
            pan: Pan(0.0),
            mute: false,
            solo: false,
            mixer_id: mix,
        });
        let notes = (0..4i64)
            .flat_map(|b| {
                (0..8u8).map(move |k| Note { id: NoteId::new(), tick: Tick(b * bar), duration_ticks: Tick(bar - PPQ / 8), pitch: 36 + s * 7 + k * 4, velocity: 0.7, channel: 0 })
            })
            .collect();
        p.patterns[0].lanes.push(Lane { channel_id: id, notes, controllers: vec![] });
    }
    // four sampler tracks playing the generated fixtures on every beat
    for (k, file) in ["sine_440_48k.wav", "noise_48k.wav", "sweep_20_20k_48k.wav", "impulse_48k.wav"].iter().enumerate() {
        let asset = rawdawg_project::assets::import_asset(dir, &root.join("fixtures").join(file))?;
        let patch = rawdawg_sampler::SamplerPatch { asset_id: Some(asset.id.clone()), name: (*file).to_owned(), ..Default::default() };
        p.assets.push(asset);
        let mix = MixerChannelId::new();
        p.mixer.channels.push(bus(mix, &format!("Sample {}", k + 1), strip()?, sends()));
        let id = ChannelId::new();
        let mut inst = Instance::native(rawdawg_types::ids::native_types::SAMPLER, serde_json::to_value(&patch)?);
        inst.display_name = format!("Sample {}", k + 1);
        p.channels.push(Channel {
            id,
            name: format!("Sample {}", k + 1),
            color: None,
            instrument: inst,
            gain_db: Db(-12.0),
            pan: Pan(0.0),
            mute: false,
            solo: false,
            mixer_id: mix,
        });
        let notes = (0..16i64).map(|beat| Note { id: NoteId::new(), tick: Tick(beat * PPQ), duration_ticks: Tick(PPQ / 2), pitch: 60, velocity: 0.8, channel: 0 }).collect();
        p.patterns[0].lanes.push(Lane { channel_id: id, notes, controllers: vec![] });
    }
    // master limiter
    p.mixer.channels.iter_mut().find(|m| m.id == master).context("master")?.inserts.push(fx(rawdawg_effects::dynamics::limiter::TYPE_ID, "Limiter", &[])?);
    // song: the pattern looped for `bars`, plus two automation clips
    let total = bars * bar;
    let track = TrackId::new();
    p.tracks.push(Track { id: track, name: "Song".into(), color: None, mute: false, solo: false, height: None, mixer_id: None });
    p.clips.push(Clip {
        id: ClipId::new(),
        track_id: track,
        start_tick: Tick(0),
        length_ticks: Tick(total),
        muted: false,
        content: ClipContent::Pattern { pattern_id: p.patterns[0].id, source_offset_ticks: Tick::ZERO, looped: true },
    });
    let auto_track = TrackId::new();
    p.tracks.push(Track { id: auto_track, name: "Automation".into(), color: None, mute: false, solo: false, height: None, mixer_id: None });
    for (instance_id, param_id, name) in [
        (first_synth.context("synth")?, rawdawg_synthesizer::params::FILTER_CUTOFF, "Cutoff"),
        (eq_of_first.context("eq")?, rawdawg_effects::eq::band_param(0, rawdawg_effects::eq::B_GAIN), "EQ gain"),
    ] {
        let curve = AutomationCurve {
            id: CurveId::new(),
            name: name.into(),
            target: AutomationTarget { instance_id, param_id },
            points: (0..=bars / 2)
                .map(|i| AutomationPoint { id: PointId::new(), tick: Tick(i * 2 * bar), value: if i % 2 == 0 { 0.35 } else { 0.7 }, interpolation: Interpolation::Linear })
                .collect(),
            mode: AutomationMode::Read,
            disabled: false,
        };
        let cid = curve.id;
        p.automation.push(curve);
        p.clips.push(Clip {
            id: ClipId::new(),
            track_id: auto_track,
            start_tick: Tick(0),
            length_ticks: Tick(total),
            muted: false,
            content: ClipContent::Automation { curve_id: cid, source_offset_ticks: Tick::ZERO, looped: false },
        });
    }
    rawdawg_project::validate(&p).map_err(|e| anyhow::anyhow!("reference project invalid: {e}"))?;
    rawdawg_project::save_project_dir(dir, &p)?;
    Ok(p)
}

#[derive(serde::Serialize, Clone)]
pub struct Distribution {
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
    pub max_ms: f64,
    pub mean_ms: f64,
}

fn distribution(times: &mut [f64]) -> Distribution {
    times.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let pct = |q: f64| times[((times.len() - 1) as f64 * q).round() as usize];
    Distribution { p50_ms: pct(0.5), p95_ms: pct(0.95), p99_ms: pct(0.99), max_ms: times[times.len() - 1], mean_ms: times.iter().sum::<f64>() / times.len() as f64 }
}

/// On-CPU time of the calling thread (`CLOCK_THREAD_CPUTIME_ID`, nanosecond resolution),
/// to tell slow blocks that did real work from blocks during which the thread was descheduled.
struct ThreadCpu;

impl ThreadCpu {
    fn new() -> Self {
        Self
    }
    #[cfg(unix)]
    fn now_ns(&mut self) -> Option<u64> {
        let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
        // SAFETY: `ts` is a valid, writable timespec; the clock id is a constant supported by Linux.
        let r = unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut ts) };
        (r == 0).then(|| ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64)
    }
    #[cfg(not(unix))]
    fn now_ns(&mut self) -> Option<u64> {
        None
    }
}

#[derive(serde::Serialize)]
pub struct SlowBlock {
    pub index: usize,
    pub wall_ms: f64,
    /// `None` when the platform offers no per-thread CPU accounting.
    pub on_cpu_ms: Option<f64>,
}

#[derive(serde::Serialize)]
pub struct WorkloadResult {
    pub block: usize,
    pub deadline_ms: f64,
    pub seconds: f64,
    pub wall_seconds: f64,
    pub realtime_factor: f64,
    pub blocks: Distribution,
    pub p99_percent_of_deadline: f64,
    pub max_percent_of_deadline: f64,
    pub peak_voices: u64,
    pub rms: f32,
    pub nonfinite_samples: usize,
    pub compile_ms: f64,
    /// Blocks whose wall time exceeded 50% of the deadline, with their on-CPU time.
    pub slow_blocks: Vec<SlowBlock>,
    pub blocks_over_deadline: usize,
    /// Distribution of on-CPU time per block (Linux only).
    pub on_cpu: Option<Distribution>,
}

/// Render the reference workload in song mode, timing every block.
pub fn run_workload(project: &Project, dir: &Path, seconds: f64, block: usize) -> anyhow::Result<WorkloadResult> {
    let limits = EngineLimits::DEFAULT;
    let cache = rawdawg_audio_engine::ProjectAssetCache::new(Some(dir.to_path_buf()));
    let mut cache = cache;
    cache.sync(Some(dir.to_path_buf()), project);
    let t0 = Instant::now();
    let compiled = rawdawg_audio_engine::compile(project, 1, block, &limits, &Default::default(), &cache)?;
    let compile_ms = t0.elapsed().as_secs_f64() * 1e3;
    let (mut engine, mut handle) = Engine::new(SampleRate(SR), block, &limits);
    let _worker = compiled.stream_worker;
    engine.install_plan(compiled.plan);
    handle.send(EngineCommand::SetMode(TransportMode::Song)).ok();
    handle.send(EngineCommand::Play).ok();
    let mut l = vec![0.0f32; block];
    let mut r = vec![0.0f32; block];
    // warm-up (caches, first allocations of lazily sized analysis buffers off the measured span)
    for _ in 0..(SR as usize / block) {
        l.fill(0.0);
        r.fill(0.0);
        engine.render_block(&mut l, &mut r);
    }
    let blocks = ((seconds * f64::from(SR)) / block as f64) as usize;
    let mut times = Vec::with_capacity(blocks);
    let mut peak_voices = 0;
    let mut sum_sq = 0.0f64;
    let mut nonfinite = 0usize;
    let mut cpu = ThreadCpu::new();
    let mut cpu_times: Vec<f64> = Vec::with_capacity(blocks);
    let deadline_ms = block as f64 * 1e3 / f64::from(SR);
    let mut slow_blocks = Vec::new();
    let start = Instant::now();
    for index in 0..blocks {
        l.fill(0.0);
        r.fill(0.0);
        let c0 = cpu.now_ns();
        let t = Instant::now();
        engine.render_block(&mut l, &mut r);
        let wall_ms = t.elapsed().as_secs_f64() * 1e3;
        let on_cpu_ms = match (c0, cpu.now_ns()) {
            (Some(a), Some(b)) => Some(b.saturating_sub(a) as f64 / 1e6),
            _ => None,
        };
        if let Some(c) = on_cpu_ms {
            cpu_times.push(c);
        }
        if wall_ms > deadline_ms * 0.5 && slow_blocks.len() < 64 {
            slow_blocks.push(SlowBlock { index, wall_ms, on_cpu_ms });
        }
        times.push(wall_ms);
        peak_voices = peak_voices.max(engine.status().active_voices.load(std::sync::atomic::Ordering::Relaxed));
        for v in l.iter().chain(r.iter()) {
            if v.is_finite() {
                sum_sq += f64::from(*v) * f64::from(*v);
            } else {
                nonfinite += 1;
            }
        }
    }
    let wall = start.elapsed().as_secs_f64();
    let blocks_over_deadline = times.iter().filter(|t| **t > deadline_ms).count();
    let on_cpu = (!cpu_times.is_empty()).then(|| distribution(&mut cpu_times));
    let d = distribution(&mut times);
    let rms = (sum_sq / (blocks * block * 2) as f64).sqrt() as f32;
    if rms < 1e-3 {
        bail!("reference workload is silent (rms {rms}); the benchmark would measure nothing");
    }
    Ok(WorkloadResult {
        block,
        deadline_ms,
        seconds,
        wall_seconds: wall,
        realtime_factor: seconds / wall,
        p99_percent_of_deadline: d.p99_ms / deadline_ms * 100.0,
        max_percent_of_deadline: d.max_ms / deadline_ms * 100.0,
        blocks: d,
        peak_voices,
        rms,
        nonfinite_samples: nonfinite,
        compile_ms,
        slow_blocks,
        blocks_over_deadline,
        on_cpu,
    })
}

#[derive(serde::Serialize)]
pub struct UnitCost {
    pub name: String,
    pub mean_us: f64,
    pub p99_us: f64,
    pub percent_of_deadline_p99: f64,
}

/// Each listed effect alone on a stereo signal (sine + noise), 256-frame blocks.
pub fn effect_costs(seconds: f64) -> anyhow::Result<Vec<UnitCost>> {
    let block = 256usize;
    let deadline_us = block as f64 * 1e6 / f64::from(SR);
    let mut out = Vec::new();
    let mut rng = 0x1234_5678u32;
    let input: Vec<f32> = (0..block * 64)
        .map(|i| {
            rng ^= rng << 13;
            rng ^= rng >> 17;
            rng ^= rng << 5;
            0.3 * (2.0 * std::f32::consts::PI * 220.0 * i as f32 / SR as f32).sin() + 0.05 * (rng as f32 / u32::MAX as f32 - 0.5)
        })
        .collect();
    let transport = TransportView { playing: true, ..TransportView::stopped(f64::from(SR)) };
    for info in rawdawg_effects::CATALOG.iter().filter(|i| i.listed) {
        let mut p = rawdawg_effects::make_effect(info.type_id, None).map_err(anyhow::Error::msg)?;
        p.prepare(&PrepareSpec::new(f64::from(SR), block, BusLayout::Stereo)).map_err(|e| anyhow::anyhow!("{}: {e}", info.name))?;
        let mut ol = vec![0.0f32; block];
        let mut or = vec![0.0f32; block];
        let n = ((seconds * f64::from(SR)) / block as f64) as usize;
        let mut times = Vec::with_capacity(n);
        for b in 0..n + 50 {
            let off = (b % 64) * block;
            let x = &input[off..off + block];
            let inputs: [&[f32]; 2] = [x, x];
            let mut outs: [&mut [f32]; 2] = [&mut ol, &mut or];
            let mut ctx = ProcessContext { audio: AudioBlock { frames: block, inputs: &inputs, outputs: &mut outs, sidechain: &[] }, events: &[], transport: &transport };
            let t = Instant::now();
            p.process(&mut ctx);
            if b >= 50 {
                times.push(t.elapsed().as_secs_f64() * 1e6);
            }
        }
        let d = distribution(&mut times);
        out.push(UnitCost { name: info.name.to_owned(), mean_us: d.mean_ms, p99_us: d.p99_ms, percent_of_deadline_p99: d.p99_ms / deadline_us * 100.0 });
    }
    Ok(out)
}

/// One default synth holding `voices` notes (256-frame blocks).
pub fn synth_voice_costs(seconds: f64) -> anyhow::Result<Vec<UnitCost>> {
    use rawdawg_types::events::{EventSource, TimedEvent};
    let block = 256usize;
    let deadline_us = block as f64 * 1e6 / f64::from(SR);
    let mut out = Vec::new();
    for voices in [1u32, 8, 32, 64] {
        let mut synth = rawdawg_synthesizer::Synth::default();
        rawdawg_dsp::processor::Processor::prepare(&mut synth, &PrepareSpec::new(f64::from(SR), block, BusLayout::Stereo)).map_err(|e| anyhow::anyhow!("{e}"))?;
        let ons: Vec<TimedEvent> = (0..voices).map(|k| TimedEvent::note_on(0, EventSource::Host, 0, k + 1, (24 + k) as u8, 0.7)).collect();
        let transport = TransportView::stopped(f64::from(SR));
        let mut ol = vec![0.0f32; block];
        let mut or = vec![0.0f32; block];
        let n = ((seconds * f64::from(SR)) / block as f64) as usize;
        let mut times = Vec::with_capacity(n);
        let mut sounding = 0;
        for b in 0..n + 20 {
            let events: &[TimedEvent] = if b == 0 { &ons } else { &[] };
            let mut outs: [&mut [f32]; 2] = [&mut ol, &mut or];
            let mut ctx = ProcessContext { audio: AudioBlock::output_only(block, &mut outs), events, transport: &transport };
            let t = Instant::now();
            rawdawg_dsp::processor::Processor::process(&mut synth, &mut ctx);
            if b >= 20 {
                times.push(t.elapsed().as_secs_f64() * 1e6);
            }
            sounding = rawdawg_dsp::processor::Processor::active_voices(&synth);
        }
        let d = distribution(&mut times);
        out.push(UnitCost {
            name: format!("{voices} requested, {sounding} sounding"),
            mean_us: d.mean_ms,
            p99_us: d.p99_ms,
            percent_of_deadline_p99: d.p99_ms / deadline_us * 100.0,
        });
    }
    Ok(out)
}

/// Convolution with impulse responses of growing length (exponentially decaying stereo noise),
/// 256-frame blocks. Uniform 256-frame partitions: cost should grow linearly with IR length.
pub fn convolution_costs(seconds: f64) -> anyhow::Result<Vec<UnitCost>> {
    use rawdawg_dsp::processor::Processor;
    use rawdawg_effects::convolution::{Convolution, ImpulseResponse};
    let block = 256usize;
    let deadline_us = block as f64 * 1e6 / f64::from(SR);
    let mut out = Vec::new();
    for ir_seconds in [1.0f32, 5.0, 10.0, rawdawg_effects::convolution::MAX_IR_SECONDS] {
        let n = (ir_seconds * SR as f32) as usize;
        let mut state = 0x2545_F491u32;
        let mut noise = || {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state as f32 / u32::MAX as f32 - 0.5
        };
        let decay = |i: usize| (-6.9 * i as f32 / n as f32).exp(); // −60 dB at the end
        let ch: Vec<Vec<f32>> = (0..2).map(|_| (0..n).map(|i| noise() * decay(i)).collect()).collect();
        let ir = std::sync::Arc::new(ImpulseResponse { name: format!("{ir_seconds} s noise"), sample_rate: SR, channels: ch });
        let mut conv = Convolution::new();
        conv.prepare(&PrepareSpec::new(f64::from(SR), block, BusLayout::Stereo)).map_err(|e| anyhow::anyhow!("{e}"))?;
        conv.set_ir(Some(ir));
        let d = conv.descriptors().iter().find(|d| d.key == "mix").copied().context("mix param")?;
        conv.set_param(d.id, d.to_normalized(100.0));
        let input: Vec<f32> = (0..block).map(|i| 0.3 * (i as f32 * 0.05).sin()).collect();
        let (mut ol, mut or) = (vec![0.0f32; block], vec![0.0f32; block]);
        let transport = TransportView::stopped(f64::from(SR));
        let blocks = ((seconds * f64::from(SR)) / block as f64) as usize;
        let mut times = Vec::with_capacity(blocks);
        for b in 0..blocks + 20 {
            let inputs: [&[f32]; 2] = [&input, &input];
            let mut outs: [&mut [f32]; 2] = [&mut ol, &mut or];
            let mut ctx = ProcessContext { audio: AudioBlock { frames: block, inputs: &inputs, outputs: &mut outs, sidechain: &[] }, events: &[], transport: &transport };
            let t = Instant::now();
            conv.process(&mut ctx);
            if b >= 20 {
                times.push(t.elapsed().as_secs_f64() * 1e6);
            }
        }
        if ol.iter().any(|v| !v.is_finite()) || ol.iter().all(|v| *v == 0.0) {
            bail!("convolution with a {ir_seconds} s IR produced no usable output");
        }
        let d = distribution(&mut times);
        out.push(UnitCost { name: format!("{ir_seconds:.0} s impulse response"), mean_us: d.mean_ms, p99_us: d.p99_ms, percent_of_deadline_p99: d.p99_ms / deadline_us * 100.0 });
    }
    Ok(out)
}

/// Four-corner morph (SYN-05): the blend itself (main thread, per pointer move) and synth
/// rendering while every block carries the changed parameters (8 held notes).
pub fn morph_costs(seconds: f64) -> anyhow::Result<Vec<UnitCost>> {
    use rawdawg_dsp::processor::Processor;
    use rawdawg_synthesizer::{Patch, Synth};
    use rawdawg_types::events::{EventSource, TimedEvent};
    let block = 256usize;
    let deadline_us = block as f64 * 1e6 / f64::from(SR);
    // four compatible corners: the default patch with different automatable parameter values
    let mut state = 77u64;
    let mut unit = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state >> 40) as f32 / (1u64 << 24) as f32
    };
    let mut corners: Vec<Patch> = Vec::new();
    let mut probe = Synth::default();
    let params: Vec<_> = probe.descriptors().iter().filter(|d| d.flags.automatable && !d.flags.hidden && !d.flags.stepped).copied().collect();
    for _ in 0..4 {
        let mut s = Synth::default();
        for d in &params {
            s.set_param(d.id, unit());
        }
        corners.push(Patch::from_json(&s.save_state()).map_err(anyhow::Error::msg)?);
    }
    let refs: [&Patch; 4] = [&corners[0], &corners[1], &corners[2], &corners[3]];
    if !rawdawg_synthesizer::morph::compatible(&refs) {
        bail!("benchmark corners are not compatible");
    }
    // 1. the blend: 2,000 positions along a circle
    let mut times = Vec::new();
    for i in 0..2000 {
        let a = i as f32 / 2000.0 * std::f32::consts::TAU;
        let t = Instant::now();
        let p = rawdawg_synthesizer::morph::morph(&refs, 0.5 + 0.45 * a.cos(), 0.5 + 0.45 * a.sin()).context("morph")?;
        times.push(t.elapsed().as_secs_f64() * 1e6);
        std::hint::black_box(p);
    }
    let d = distribution(&mut times);
    let mut out = vec![UnitCost { name: "morph blend (one pointer move)".into(), mean_us: d.mean_ms, p99_us: d.p99_ms, percent_of_deadline_p99: d.p99_ms / deadline_us * 100.0 }];
    // 2. rendering while the morph moves: every block applies the blended values as events
    let transport = TransportView::stopped(f64::from(SR));
    let ons: Vec<TimedEvent> = (0..8u32).map(|k| TimedEvent::note_on(0, EventSource::Host, 0, k + 1, (40 + k * 5) as u8, 0.7)).collect();
    let blocks = ((seconds * f64::from(SR)) / block as f64) as usize;
    // each corner's normalized value per parameter, read once
    let mut corner_values: Vec<[f32; 4]> = vec![[0.0; 4]; params.len()];
    for (c, patch) in corners.iter().enumerate() {
        let mut s = Synth::default();
        s.load_state(&serde_json::to_vec(patch)?).map_err(anyhow::Error::msg)?;
        for (i, d) in params.iter().enumerate() {
            corner_values[i][c] = s.param(d.id).unwrap_or(d.default_normalized);
        }
    }
    for moving in [false, true] {
        probe = Synth::default();
        probe.load_state(&serde_json::to_vec(&corners[0])?).map_err(anyhow::Error::msg)?;
        probe.prepare(&PrepareSpec::new(f64::from(SR), block, BusLayout::Stereo)).map_err(|e| anyhow::anyhow!("{e}"))?;
        let (mut ol, mut or) = (vec![0.0f32; block], vec![0.0f32; block]);
        let mut events: Vec<TimedEvent> = Vec::with_capacity(params.len() + 8);
        let mut times = Vec::with_capacity(blocks);
        for b in 0..blocks + 20 {
            events.clear();
            if b == 0 {
                events.extend_from_slice(&ons);
            }
            if moving {
                // the pad sweeps a circle every 4 s; the changed values arrive with the block
                let a = (b as f32 * block as f32 / SR as f32) / 4.0 * std::f32::consts::TAU;
                let (x, y) = (0.5 + 0.45 * a.cos(), 0.5 + 0.45 * a.sin());
                let w = rawdawg_synthesizer::morph::weights(x, y);
                for (d, cv) in params.iter().zip(&corner_values) {
                    let v: f32 = cv.iter().zip(w).map(|(c, w)| c * w).sum();
                    events.push(TimedEvent::new(0, EventSource::Host, 0, rawdawg_types::events::EventKind::Param { id: d.id, value: v }));
                }
            }
            let mut outs: [&mut [f32]; 2] = [&mut ol, &mut or];
            let mut ctx = ProcessContext { audio: AudioBlock::output_only(block, &mut outs), events: &events, transport: &transport };
            let t = Instant::now();
            probe.process(&mut ctx);
            if b >= 20 {
                times.push(t.elapsed().as_secs_f64() * 1e6);
            }
        }
        let d = distribution(&mut times);
        out.push(UnitCost {
            name: if moving { format!("synth, 8 notes, morph moving ({} params per block)", params.len()) } else { "synth, 8 notes, morph still".into() },
            mean_us: d.mean_ms,
            p99_us: d.p99_ms,
            percent_of_deadline_p99: d.p99_ms / deadline_us * 100.0,
        });
    }
    Ok(out)
}

fn machine() -> serde_json::Value {
    let cpu = std::fs::read_to_string("/proc/cpuinfo")
        .ok()
        .and_then(|s| s.lines().find(|l| l.starts_with("model name")).and_then(|l| l.split(':').nth(1)).map(|s| s.trim().to_owned()))
        .unwrap_or_else(|| "unknown".into());
    let os = std::fs::read_to_string("/etc/os-release")
        .ok()
        .and_then(|s| s.lines().find(|l| l.starts_with("PRETTY_NAME=")).map(|l| l.trim_start_matches("PRETTY_NAME=").trim_matches('"').to_owned()))
        .unwrap_or_else(|| std::env::consts::OS.into());
    let kernel = std::fs::read_to_string("/proc/sys/kernel/osrelease").map(|s| s.trim().to_owned()).unwrap_or_default();
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(0);
    serde_json::json!({ "cpu": cpu, "os": os, "kernel": kernel, "hardware_threads": threads, "build": if cfg!(debug_assertions) { "debug" } else { "release" } })
}

pub fn run(root: &Path, seconds: f64, units_only: bool) -> anyhow::Result<()> {
    if cfg!(debug_assertions) {
        eprintln!("warning: debug build — run `cargo run --release -p xtask -- bench-audio` for reference numbers");
    }
    let out = root.join("target/verification");
    std::fs::create_dir_all(&out)?;
    let dir = out.join("bench-reference.rawdawg");
    let bars = ((seconds + 2.0) / 2.0).ceil() as i64 + 2; // 120 BPM: 2 s per bar
    let (w256, w128) = if units_only {
        (None, None)
    } else {
        let project = reference_project(root, &dir, bars)?;
        (Some(run_workload(&project, &dir, seconds, 256)?), Some(run_workload(&project, &dir, seconds.min(60.0), 128)?))
    };
    let fx = effect_costs(5.0)?;
    let voices = synth_voice_costs(5.0)?;
    let conv = convolution_costs(5.0)?;
    let morph = morph_costs(5.0)?;
    let print_w = |w: &WorkloadResult| {
        println!(
            "reference workload @48 kHz/{} frames: {:.0} s audio in {:.2} s ({:.1}x realtime); block ms p50 {:.3} p95 {:.3} p99 {:.3} max {:.3} (deadline {:.3}; p99 {:.0}%, max {:.0}%); peak voices {}; rms {:.3}; non-finite {}; compile {:.1} ms",
            w.block,
            w.seconds,
            w.wall_seconds,
            w.realtime_factor,
            w.blocks.p50_ms,
            w.blocks.p95_ms,
            w.blocks.p99_ms,
            w.blocks.max_ms,
            w.deadline_ms,
            w.p99_percent_of_deadline,
            w.max_percent_of_deadline,
            w.peak_voices,
            w.rms,
            w.nonfinite_samples,
            w.compile_ms
        )
    };
    let print_slow = |w: &WorkloadResult| {
        if let Some(c) = &w.on_cpu {
            println!("  on-CPU ms per block: p50 {:.3} p99 {:.3} max {:.3}", c.p50_ms, c.p99_ms, c.max_ms);
        }
        println!("  blocks over deadline: {}; blocks over 50% of deadline: {}", w.blocks_over_deadline, w.slow_blocks.len());
        for b in w.slow_blocks.iter().take(8) {
            println!("    block {:>6}: wall {:.3} ms, on-CPU {}", b.index, b.wall_ms, b.on_cpu_ms.map(|c| format!("{c:.3} ms")).unwrap_or_else(|| "n/a".into()));
        }
    };
    for w in [&w256, &w128].into_iter().flatten() {
        print_w(w);
        print_slow(w);
    }
    println!("effects (256 frames, stereo, alone):");
    for c in &fx {
        println!("  {:<22} mean {:>7.1} µs  p99 {:>7.1} µs  ({:.1}% of deadline)", c.name, c.mean_us, c.p99_us, c.percent_of_deadline_p99);
    }
    println!("synth voices (default patch, 256 frames):");
    for c in &voices {
        println!("  {:<22} mean {:>7.1} µs  p99 {:>7.1} µs  ({:.1}% of deadline)", c.name, c.mean_us, c.p99_us, c.percent_of_deadline_p99);
    }
    println!("convolution, stereo, 100% wet (256 frames):");
    for c in &conv {
        println!("  {:<22} mean {:>7.1} µs  p99 {:>7.1} µs  ({:.1}% of deadline)", c.name, c.mean_us, c.p99_us, c.percent_of_deadline_p99);
    }
    println!("four-corner morph:");
    for c in &morph {
        println!("  {:<52} mean {:>7.1} µs  p99 {:>7.1} µs  ({:.1}% of a 256-frame deadline)", c.name, c.mean_us, c.p99_us, c.percent_of_deadline_p99);
    }
    let report =
        serde_json::json!({ "machine": machine(), "workload_256": w256, "workload_128": w128, "effects": fx, "synth_voices": voices, "convolution": conv, "morph": morph });
    std::fs::write(out.join("bench-audio.json"), serde_json::to_vec_pretty(&report)?)?;
    println!("report: {}", out.join("bench-audio.json").display());
    Ok(())
}
