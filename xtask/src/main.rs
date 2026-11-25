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
mod fixtures;
mod package;
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
    BenchAudio {
        #[arg(long, default_value_t = 60.0)]
        seconds: f64,
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
        Cmd::BenchAudio { seconds } => bench::run(&root, seconds),
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
