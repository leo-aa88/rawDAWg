//! Verification suites. Each check records PASS / FAIL / BLOCKED / NOT RUN with
//! evidence, and the run fails (nonzero) if any required check failed.

use crate::Suite;
use anyhow::bail;
use rawdawg_audio_engine::offline::{OfflineOptions, WavFormat, render_project, render_project_to_wav};
use rawdawg_dsp::analysis::dominant_frequency;
use serde::Serialize;
use std::path::Path;
use std::process::Command;
use std::time::Instant;

#[derive(Serialize, Clone, Copy, PartialEq, Eq, Debug)]
#[allow(dead_code)]
pub enum Status {
    Pass,
    Fail,
    Blocked,
    NotRun,
}

#[derive(Serialize, Debug)]
pub struct Check {
    pub id: String,
    pub requirement_ids: Vec<&'static str>,
    pub status: Status,
    pub detail: String,
    pub evidence: Option<String>,
}

#[derive(Serialize)]
struct Report {
    suite: String,
    os: String,
    arch: String,
    rustc: String,
    git_commit: String,
    started_utc_unix: u64,
    checks: Vec<Check>,
}

fn git_commit() -> String {
    Command::new("git")
        .args(["rev-parse", "--short=12", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        .unwrap_or_else(|| "unknown".into())
}

fn rustc_version() -> String {
    Command::new("rustc").arg("--version").output().ok().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned()).unwrap_or_default()
}

fn check(id: &str, reqs: &[&'static str], f: impl FnOnce() -> anyhow::Result<(String, Option<String>)>) -> Check {
    match f() {
        Ok((detail, evidence)) => Check { id: id.into(), requirement_ids: reqs.to_vec(), status: Status::Pass, detail, evidence },
        Err(e) => Check { id: id.into(), requirement_ids: reqs.to_vec(), status: Status::Fail, detail: format!("{e:#}"), evidence: None },
    }
}

fn blocked(id: &str, reqs: &[&'static str], why: &str) -> Check {
    Check { id: id.into(), requirement_ids: reqs.to_vec(), status: Status::Blocked, detail: why.into(), evidence: None }
}

fn headless_checks(root: &Path, out: &Path) -> Vec<Check> {
    let mut checks = Vec::new();
    let demo = root.join("fixtures/demo.rawdawg");
    checks.push(check("HL-01 demo project loads and validates", &["DATA-01"], || {
        let p = rawdawg_project::load_project_dir(&demo)?;
        Ok((format!("{} channels, {} patterns, schema v{}", p.channels.len(), p.patterns.len(), p.schema_version), Some(demo.display().to_string())))
    }));
    checks.push(check("HL-02 headless render demo (pattern mode) to WAV", &["IO-06", "QUAL-01", "CORE-01"], || {
        let p = rawdawg_project::load_project_dir(&demo)?;
        let path = out.join("demo.wav");
        let stats = render_project_to_wav(&p, &OfflineOptions::default(), WavFormat::Float32, &path)?;
        if stats.nonfinite_blocks > 0 || stats.dropped_events > 0 {
            bail!("health counters nonzero: nonfinite {} dropped {}", stats.nonfinite_blocks, stats.dropped_events);
        }
        if stats.peak <= 0.01 {
            bail!("render is silent (peak {})", stats.peak);
        }
        let reader = hound::WavReader::open(&path)?;
        if reader.duration() as usize != stats.frames {
            bail!("wav frame count mismatch");
        }
        Ok((format!("{} frames @ {} Hz, peak {:.3}, {:.1}x realtime", stats.frames, stats.sample_rate, stats.peak, stats.realtime_factor), Some(path.display().to_string())))
    }));
    checks.push(check("HL-03 A4 pitch accuracy at all six project rates", &["T-OSC", "QUAL-01", "SYN-01"], || {
        let mut p = rawdawg_project::Project::new_default("a4", serde_json::to_value(rawdawg_synthesizer::Patch::default())?);
        let ch = p.channels[0].id;
        p.patterns[0].lanes[0].notes.push(rawdawg_project::Note {
            id: rawdawg_types::NoteId::new(),
            tick: rawdawg_types::units::Tick(0),
            duration_ticks: rawdawg_types::units::Tick(3840),
            pitch: 69,
            velocity: 1.0,
            channel: 0,
        });
        let _ = ch;
        let mut details = Vec::new();
        for sr in rawdawg_types::SampleRate::SUPPORTED {
            let rate = rawdawg_types::SampleRate(sr);
            let (l, _, _) = render_project(&p, &OfflineOptions { sample_rate: Some(rate), tail_seconds: 0.0, ..Default::default() })?;
            let skip = sr as usize / 10;
            let f = dominant_frequency(&l[skip..], f64::from(sr));
            if (f - 440.0).abs() > 0.5 {
                bail!("{sr} Hz: measured {f:.3} Hz");
            }
            details.push(format!("{sr}:{f:.3}"));
        }
        Ok((details.join(" "), None))
    }));
    checks.push(check("HL-04 block-partition invariance (32..2048 incl. odd sizes)", &["T-TIME", "QUAL-01"], || {
        let p = rawdawg_project::load_project_dir(&demo)?;
        let reference = render_project(&p, &OfflineOptions { block_frames: 2048, tail_seconds: 0.5, ..Default::default() })?;
        let mut worst = 0.0f32;
        for block in [32usize, 64, 127, 128, 256, 511, 1024] {
            let other = render_project(&p, &OfflineOptions { block_frames: block, tail_seconds: 0.5, ..Default::default() })?;
            let err = reference.0.iter().zip(&other.0).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
            worst = worst.max(err);
            if err > 1e-5 {
                bail!("block {block}: max abs error {err}");
            }
        }
        Ok((format!("max abs error {worst:.2e} ≤ 1e-5"), None))
    }));
    checks.push(check("HL-05 repeated render identical (determinism)", &["DATA-02", "QUAL-01"], || {
        let p = rawdawg_project::load_project_dir(&demo)?;
        let a = render_project(&p, &OfflineOptions::default())?;
        let b = render_project(&p, &OfflineOptions::default())?;
        if a.0 != b.0 || a.1 != b.1 {
            bail!("renders differ");
        }
        Ok((format!("{} frames bit-identical", a.0.len()), None))
    }));
    checks.push(check("HL-06 project save/load round trip in temp dir", &["DATA-01", "DATA-02"], || {
        let p = rawdawg_project::load_project_dir(&demo)?;
        let dir = out.join("roundtrip.rawdawg");
        let _ = std::fs::remove_dir_all(&dir);
        rawdawg_project::save_project_dir(&dir, &p)?;
        let back = rawdawg_project::load_project_dir(&dir)?;
        if back != p {
            bail!("loaded project differs from saved");
        }
        let a = render_project(&p, &OfflineOptions::default())?;
        let b = render_project(&back, &OfflineOptions::default())?;
        if a.0 != b.0 {
            bail!("render differs after reload");
        }
        Ok(("identical model and render after reload".into(), Some(dir.display().to_string())))
    }));
    checks.push(check("HL-07 audio device enumeration (no device required)", &["PLAT-02"], || {
        let snap = rawdawg_audio_io::enumerate();
        let path = out.join("devices.json");
        std::fs::write(&path, serde_json::to_vec_pretty(&snap)?)?;
        Ok((format!("{} hosts, {} devices, output available: {}", snap.hosts.len(), snap.devices.len(), snap.has_output()), Some(path.display().to_string())))
    }));
    checks
}

fn plugin_checks(root: &Path, out: &Path) -> Vec<Check> {
    let mut checks = Vec::new();
    let profile_dir = root.join("target").join(if cfg!(debug_assertions) { "debug" } else { "release" });
    let clap_name = if cfg!(target_os = "windows") { "rawdawg_clap.dll" } else { "librawdawg_clap.so" };
    let vst3_name = if cfg!(target_os = "windows") { "rawdawg_vst3.dll" } else { "librawdawg_vst3.so" };
    checks.push(check("PL-01 CLAP and VST3 modules built", &["PLUG-01"], || {
        let c = profile_dir.join(clap_name);
        let v = profile_dir.join(vst3_name);
        if !c.is_file() || !v.is_file() {
            bail!("missing {} or {} (run cargo build --workspace)", c.display(), v.display());
        }
        Ok((format!("{} ({} bytes), {} ({} bytes)", clap_name, c.metadata()?.len(), vst3_name, v.metadata()?.len()), Some(profile_dir.display().to_string())))
    }));
    checks.push(check("PL-02 CLAP/VST3 ABI round-trip tests (cargo test)", &["PLUG-01", "T-PLUGIN"], || {
        let status = Command::new("cargo").args(["test", "-q", "-p", "rawdawg-plugin-clap", "-p", "rawdawg-plugin-vst3"]).status()?;
        if !status.success() {
            bail!("plugin tests failed");
        }
        Ok(("host load, instantiate, process, params, state: pass".into(), None))
    }));
    let validator = which("clap-validator");
    checks.push(match validator {
        Some(path) => check("PL-03 clap-validator on exported CLAP", &["PLUG-03", "T-PLUGIN"], || {
            let clap_path = out.join("rawDAWg.clap");
            std::fs::copy(profile_dir.join(clap_name), &clap_path)?;
            let output = Command::new(&path).args(["validate", clap_path.to_str().unwrap()]).output()?;
            let log = out.join("clap-validator.log");
            std::fs::write(&log, [&output.stdout[..], &output.stderr[..]].concat())?;
            if !output.status.success() {
                bail!("clap-validator failed, see {}", log.display());
            }
            Ok(("clap-validator passed".into(), Some(log.display().to_string())))
        }),
        None => blocked(
            "PL-03 clap-validator on exported CLAP",
            &["PLUG-03", "T-PLUGIN"],
            "clap-validator not installed on this machine; install from https://github.com/free-audio/clap-validator and rerun",
        ),
    });
    checks.push(blocked(
        "PL-04 VST3 SDK validator on exported VST3",
        &["PLUG-03", "T-PLUGIN"],
        "Steinberg validator requires the VST3 SDK build; scheduled for M7 with bundle layout",
    ));
    checks.push(blocked(
        "PL-05 independent host test (CLAP/VST3)",
        &["PLUG-01", "PLUG-02", "E2E-07"],
        "manual gate: load exported plugins in an independent host on Windows and Ubuntu (M7)",
    ));
    checks
}

fn which(bin: &str) -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|p| p.join(bin)).find(|p| p.is_file())
}

pub fn run(root: &Path, suite: Suite) -> anyhow::Result<()> {
    let out = root.join("target/verification");
    std::fs::create_dir_all(&out)?;
    let started = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs();
    let mut checks = Vec::new();
    if matches!(suite, Suite::Headless | Suite::All) {
        checks.extend(headless_checks(root, &out));
    }
    if matches!(suite, Suite::Plugins | Suite::All) {
        checks.extend(plugin_checks(root, &out));
    }
    // Gates that this suite can never establish by itself.
    if matches!(suite, Suite::All) {
        checks.push(blocked(
            "HW-01 real audio device playback/record matrix",
            &["PLAT-02", "IO-04", "QUAL-04"],
            "requires real hardware session on Windows and Ubuntu; run `rawdawg --audio-smoke` and record device/rate/buffer/xruns in docs/verification/hardware.md",
        ));
        checks.push(blocked("GUI-01 manual GUI verification at 1024x720/1440x900/1920x1080", &["PLAT-03", "QUAL-04"], "requires display session; take screenshots on both OSs"));
    }
    let report = Report {
        suite: format!("{suite:?}"),
        os: std::env::consts::OS.into(),
        arch: std::env::consts::ARCH.into(),
        rustc: rustc_version(),
        git_commit: git_commit(),
        started_utc_unix: started,
        checks,
    };
    let json = out.join(format!("verify-{}.json", format!("{suite:?}").to_lowercase()));
    std::fs::write(&json, serde_json::to_vec_pretty(&report)?)?;
    let mut failed = 0;
    println!("{:<64} {:<8} detail", "check", "status");
    for c in &report.checks {
        let s = match c.status {
            Status::Pass => "PASS",
            Status::Fail => {
                failed += 1;
                "FAIL"
            }
            Status::Blocked => "BLOCKED",
            Status::NotRun => "NOT RUN",
        };
        println!("{:<64} {:<8} {}", c.id, s, c.detail);
    }
    println!("report: {}", json.display());
    if failed > 0 {
        bail!("{failed} check(s) FAILED");
    }
    Ok(())
}

/// Offline throughput + block timing on the reference-style workload (M1 subset:
/// four synths playing 8-note chords). Prints realtime factor and block-time
/// percentiles; the full M8 workload adds samplers, effects and sends.
pub fn bench_audio(root: &Path, seconds: f64) -> anyhow::Result<()> {
    use rawdawg_audio_engine::commands::EngineCommand;
    use rawdawg_audio_engine::engine::Engine;
    use rawdawg_types::units::Tick;
    let mut p = rawdawg_project::Project::new_default("bench", serde_json::to_value(rawdawg_synthesizer::Patch::default())?);
    let base_channel = p.channels[0].clone();
    for i in 1..4 {
        let mut c = base_channel.clone();
        c.id = rawdawg_types::ChannelId::new();
        c.instrument.id = rawdawg_types::InstanceId::new();
        c.name = format!("Synth {}", i + 1);
        p.channels.push(c);
    }
    let ids: Vec<_> = p.channels.iter().map(|c| c.id).collect();
    p.patterns[0].lanes = ids
        .iter()
        .map(|id| rawdawg_project::Lane {
            channel_id: *id,
            notes: (0..8)
                .map(|k| rawdawg_project::Note { id: rawdawg_types::NoteId::new(), tick: Tick(0), duration_ticks: Tick(3840), pitch: 48 + k * 3, velocity: 0.8, channel: 0 })
                .collect(),
            controllers: vec![],
        })
        .collect();
    let limits = rawdawg_types::limits::EngineLimits::DEFAULT;
    let block = 256usize;
    let plan = rawdawg_audio_engine::plan::compile(&p, 1, block, &limits)?;
    let (mut engine, mut handle) = Engine::new(rawdawg_types::SampleRate::DEFAULT, block, &limits);
    engine.install_plan(plan);
    handle.send(EngineCommand::Play).ok();
    let mut l = vec![0.0f32; block];
    let mut r = vec![0.0f32; block];
    let blocks = ((seconds * 48_000.0) / block as f64) as usize;
    let mut times = Vec::with_capacity(blocks);
    let start = Instant::now();
    for _ in 0..blocks {
        let t = Instant::now();
        engine.render_block(&mut l, &mut r);
        times.push(t.elapsed().as_secs_f64() * 1e3);
    }
    let elapsed = start.elapsed().as_secs_f64();
    times.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let pct = |q: f64| times[((times.len() - 1) as f64 * q) as usize];
    let deadline_ms = block as f64 / 48.0;
    let voices = engine.status().active_voices.load(std::sync::atomic::Ordering::Relaxed);
    println!("workload: 4 synths × 8 voices = {voices} voices, 48 kHz, {block}-frame blocks, {blocks} blocks ({seconds:.0} s of audio)");
    println!("offline: {:.2} s wall, {:.1}x realtime", elapsed, seconds / elapsed);
    println!(
        "block time ms: p50 {:.3} p95 {:.3} p99 {:.3} max {:.3} (deadline {:.3} ms; p99 at {:.0}% of deadline)",
        pct(0.5),
        pct(0.95),
        pct(0.99),
        times[times.len() - 1],
        deadline_ms,
        pct(0.99) / deadline_ms * 100.0
    );
    let out = root.join("target/verification");
    std::fs::create_dir_all(&out)?;
    let report = serde_json::json!({"voices": voices, "block": block, "blocks": blocks, "seconds": seconds, "wall_seconds": elapsed, "p50_ms": pct(0.5), "p95_ms": pct(0.95), "p99_ms": pct(0.99), "max_ms": times[times.len()-1], "deadline_ms": deadline_ms, "build": if cfg!(debug_assertions) {"debug"} else {"release"}, "os": std::env::consts::OS});
    std::fs::write(out.join("bench-audio.json"), serde_json::to_vec_pretty(&report)?)?;
    Ok(())
}
