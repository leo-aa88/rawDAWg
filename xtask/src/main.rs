//! `cargo xtask` — rawDAWg developer tasks (TESTING.md §2).
//!
//! Every subcommand exits nonzero on failure. `verify` prints a PASS/FAIL/
//! BLOCKED/NOT RUN table and writes a JSON report under `target/verification/`.

use anyhow::{Context, bail};
use clap::{Parser, Subcommand, ValueEnum};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

mod bench;
use rawdawg_fixtures as fixtures;
mod package;
mod soak;
mod verify;

#[derive(Parser)]
#[command(name = "xtask", about = "rawDAWg developer tasks")]
struct Args {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Generate deterministic test fixtures under fixtures/.
    Fixtures,
    /// Run a verification suite and write evidence to target/verification/.
    Verify {
        #[arg(long, value_enum, default_value = "headless")]
        suite: Suite,
    },
    /// Headless render of a project to WAV (same engine as playback).
    Render {
        #[arg(long)]
        project: PathBuf,
        #[arg(long)]
        output: PathBuf,
        #[arg(long, default_value = "pattern")]
        mode: String,
    },
    /// Offline engine throughput and block-time distribution on the reference workload.
    /// Seeded mutation fuzzing of every untrusted input surface (see crates/fuzz).
    Fuzz {
        /// Only this target (project, preset, smf, sf2, audio, expr, patch, effect-state, plugin-envelope, midi-messages).
        #[arg(long)]
        target: Option<String>,
        #[arg(long, default_value_t = 2000)]
        iterations: u64,
        /// Time budget per target.
        #[arg(long, default_value_t = 60)]
        seconds: u64,
        #[arg(long, default_value_t = 1)]
        seed: u64,
        /// Re-run one saved input (requires --target).
        #[arg(long)]
        replay: Option<PathBuf>,
    },
    /// Headless stress soak: random edits, plan swaps, transport, live notes and recording
    /// against a real-time-paced engine; memory growth and stuck-note checks.
    Soak {
        #[arg(long, default_value_t = 30.0)]
        minutes: f64,
        #[arg(long, default_value_t = 1)]
        seed: u64,
    },
    BenchAudio {
        #[arg(long, default_value_t = 60.0)]
        seconds: f64,
        /// Only the single-unit measurements (effects, voices, convolution, morph).
        #[arg(long)]
        units_only: bool,
    },
    /// Build a native package (deb on Ubuntu; msi/portable on Windows).
    Package {
        #[arg(long, value_enum)]
        format: PackageFormat,
    },
    /// Run fmt check, clippy, tests and release build (what CI runs).
    Ci,
}

#[derive(Clone, Copy, ValueEnum, PartialEq, Eq, Debug)]
pub enum Suite {
    Headless,
    Plugins,
    All,
}

#[derive(Clone, Copy, ValueEnum, PartialEq, Eq, Debug)]
enum PackageFormat {
    Deb,
    Msi,
    Portable,
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).parent().expect("xtask lives in the workspace").to_path_buf()
}

fn run(cmd: &mut Command) -> anyhow::Result<()> {
    let desc = format!("{cmd:?}");
    let status = cmd.status().with_context(|| format!("spawning {desc}"))?;
    if !status.success() {
        bail!("{desc} exited with {status}");
    }
    Ok(())
}

/// Counts live heap bytes so the soak can tell growing data from allocator retention
/// (resident memory alone cannot). One relaxed atomic add per allocation.
struct CountingAlloc;

static LIVE_HEAP: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);

// SAFETY: forwards every call unchanged to the system allocator; only a counter is updated.
unsafe impl std::alloc::GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: std::alloc::Layout) -> *mut u8 {
        // SAFETY: same contract as the caller's.
        let p = unsafe { std::alloc::System.alloc(layout) };
        if !p.is_null() {
            LIVE_HEAP.fetch_add(layout.size() as i64, std::sync::atomic::Ordering::Relaxed);
        }
        p
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: std::alloc::Layout) {
        // SAFETY: same contract as the caller's.
        unsafe { std::alloc::System.dealloc(ptr, layout) };
        LIVE_HEAP.fetch_sub(layout.size() as i64, std::sync::atomic::Ordering::Relaxed);
    }
    unsafe fn alloc_zeroed(&self, layout: std::alloc::Layout) -> *mut u8 {
        // SAFETY: same contract as the caller's.
        let p = unsafe { std::alloc::System.alloc_zeroed(layout) };
        if !p.is_null() {
            LIVE_HEAP.fetch_add(layout.size() as i64, std::sync::atomic::Ordering::Relaxed);
        }
        p
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: std::alloc::Layout, new_size: usize) -> *mut u8 {
        // SAFETY: same contract as the caller's.
        let p = unsafe { std::alloc::System.realloc(ptr, layout, new_size) };
        if !p.is_null() {
            LIVE_HEAP.fetch_add(new_size as i64 - layout.size() as i64, std::sync::atomic::Ordering::Relaxed);
        }
        p
    }
}

#[global_allocator]
static GLOBAL: CountingAlloc = CountingAlloc;

/// Live heap bytes (all threads) as counted by the global allocator.
pub fn live_heap_bytes() -> i64 {
    LIVE_HEAP.load(std::sync::atomic::Ordering::Relaxed)
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let root = workspace_root();
    std::env::set_current_dir(&root)?;
    match args.cmd {
        Cmd::Fixtures => fixtures::generate(&root.join("fixtures")),
        Cmd::Verify { suite } => verify::run(&root, suite),
        Cmd::Render { project, output, mode } => {
            let mode = match mode.as_str() {
                "pattern" => rawdawg_audio_engine::TransportMode::Pattern,
                "song" => rawdawg_audio_engine::TransportMode::Song,
                other => bail!("unknown mode {other} (pattern|song)"),
            };
            let p = rawdawg_project::load_project_dir(&project)?;
            if let Some(parent) = output.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let opts = rawdawg_audio_engine::OfflineOptions { mode, project_dir: rawdawg_project::persist::resolve_project_dir(&project).ok(), ..Default::default() };
            let t = Instant::now();
            let stats = rawdawg_audio_engine::offline::render_project_to_wav(&p, &opts, rawdawg_audio_engine::offline::WavFormat::Float32, &output)?;
            println!(
                "rendered {} frames @ {} Hz → {} in {:.2}s (peak {:.3}, {:.1}x realtime)",
                stats.frames,
                stats.sample_rate,
                output.display(),
                t.elapsed().as_secs_f64(),
                stats.peak,
                stats.realtime_factor
            );
            if stats.nonfinite_blocks > 0 || stats.dropped_events > 0 {
                bail!("render reported health issues: nonfinite blocks {}, dropped events {}", stats.nonfinite_blocks, stats.dropped_events);
            }
            Ok(())
        }
        Cmd::BenchAudio { seconds, units_only } => bench::run(&root, seconds, units_only),
        Cmd::Soak { minutes, seed } => soak::run(&root, minutes, seed),
        Cmd::Fuzz { target, iterations, seconds, seed, replay } => fuzz(&root, target, iterations, seconds, seed, replay),
        Cmd::Package { format } => package(&root, format),
        Cmd::Ci => {
            run(Command::new("cargo").args(["fmt", "--all", "--", "--check"]))?;
            run(Command::new("cargo").args(["clippy", "--workspace", "--all-targets", "--locked", "--", "-D", "warnings"]))?;
            run(Command::new("cargo").args(["test", "--workspace", "--locked"]))?;
            run(Command::new("cargo").args(["build", "--workspace", "--release", "--locked"]))?;
            fixtures::generate(&root.join("fixtures"))?;
            verify::run(&root, Suite::All)
        }
    }
}

fn package(root: &Path, format: PackageFormat) -> anyhow::Result<()> {
    let f = match format {
        PackageFormat::Deb => package::Format::Deb,
        PackageFormat::Msi => package::Format::Msi,
        PackageFormat::Portable => package::Format::Portable,
    };
    let produced = package::package(root, f)?;
    for a in &produced.artifacts {
        println!("artifact: {}", a.display());
    }
    println!("staging: {}", produced.staging.display());
    for w in &produced.warnings {
        println!("warning: {w}");
    }
    Ok(())
}

fn fuzz(root: &Path, target: Option<String>, iterations: u64, seconds: u64, seed: u64, replay: Option<PathBuf>) -> anyhow::Result<()> {
    let out = root.join("target").join("fuzz");
    if let Some(file) = replay {
        let name = target.ok_or_else(|| anyhow::anyhow!("--replay needs --target"))?;
        let t = rawdawg_fuzz::targets::by_name(&name).ok_or_else(|| anyhow::anyhow!("unknown target {name}"))?;
        let input = std::fs::read(&file)?;
        match rawdawg_fuzz::replay(&t, &input) {
            Some(msg) => bail!("{name}: still fails: {msg}"),
            None => println!("{name}: {} passes", file.display()),
        }
        return Ok(());
    }
    let targets: Vec<_> = rawdawg_fuzz::targets::all().into_iter().filter(|t| target.as_deref().is_none_or(|n| n == t.name)).collect();
    if targets.is_empty() {
        bail!("unknown target {target:?}");
    }
    let mut findings = 0;
    let mut summary = Vec::new();
    for t in &targets {
        let r = rawdawg_fuzz::run_campaign(t, iterations, seed, std::time::Duration::from_secs(seconds), &out, 16, std::time::Duration::from_secs(30));
        println!(
            "{:<16} {:>7} inputs in {:>6.1} s, parsed {:>6} ({:>5.1}%), seeds accepted {}/{}, slowest {:.1} ms, findings {}",
            r.target,
            r.executed,
            r.elapsed.as_secs_f64(),
            r.parsed,
            r.parsed as f64 / r.executed.max(1) as f64 * 100.0,
            r.accepted_seeds,
            t.seeds.len(),
            r.slowest_ms,
            r.findings.len()
        );
        for f in &r.findings {
            println!("    iteration {}: {}\n      reproducer {}", f.iteration, f.message, f.input.display());
        }
        findings += r.findings.len();
        summary.push(serde_json::json!({ "target": r.target, "inputs": r.executed, "parsed": r.parsed, "seconds": r.elapsed.as_secs_f64(), "seeds": t.seeds.len(), "seeds_accepted": r.accepted_seeds, "slowest_ms": r.slowest_ms, "findings": r.findings.iter().map(|f| serde_json::json!({"iteration": f.iteration, "message": f.message, "input": f.input})).collect::<Vec<_>>() }));
    }
    std::fs::create_dir_all(&out)?;
    std::fs::write(
        out.join("report.json"),
        serde_json::to_vec_pretty(
            &serde_json::json!({ "seed": seed, "iterations": iterations, "build": if cfg!(debug_assertions) { "debug" } else { "release" }, "targets": summary }),
        )?,
    )?;
    if findings > 0 {
        bail!("{findings} finding(s); reproducers under {}", out.display());
    }
    Ok(())
}
