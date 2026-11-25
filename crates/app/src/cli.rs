//! Command-line interface. Headless operations never open a GUI or an audio device.

use anyhow::{Context, bail};
use clap::Parser;
use rawdawg_audio_engine::commands::TransportMode;
use rawdawg_audio_engine::offline::{OfflineOptions, WavFormat, render_project_to_wav};
use rawdawg_types::units::Tick;
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(name = "rawdawg", version, about = "rawDAWg — Less thinking. More noise.", long_about = None)]
pub struct Args {
    /// Open a project folder (or its project.json) in the GUI.
    #[arg(long, value_name = "PATH")]
    pub open: Option<PathBuf>,

    /// Render a project to an audio file without GUI or audio device.
    #[arg(long, value_name = "PROJECT", requires = "output")]
    pub headless_render: Option<PathBuf>,

    /// Output file for --headless-render (.wav).
    #[arg(long, value_name = "FILE")]
    pub output: Option<PathBuf>,

    /// Render mode: pattern (selected pattern, one pass) or song.
    #[arg(long, value_enum, default_value = "pattern")]
    pub mode: RenderMode,

    /// Output sample format for WAV.
    #[arg(long, value_enum, default_value = "float32")]
    pub format: WavFormatArg,

    /// Tail (seconds) rendered after the musical end.
    #[arg(long, default_value_t = 1.0)]
    pub tail: f64,

    /// Override the project sample rate (Hz) for the render.
    #[arg(long, value_name = "HZ")]
    pub sample_rate: Option<u32>,

    /// Start tick of the render range.
    #[arg(long, default_value_t = 0)]
    pub start_tick: i64,

    /// End tick of the render range (default: pattern/song end).
    #[arg(long)]
    pub end_tick: Option<i64>,

    /// Disable master output protection for the render (unprotected float).
    #[arg(long)]
    pub unprotected: bool,

    /// Start without loading third-party plugins (project state preserved as placeholders).
    #[arg(long)]
    pub safe_mode: bool,

    /// List audio hosts/devices as JSON and exit.
    #[arg(long)]
    pub list_audio_devices: bool,

    /// Open the default (or --device) output and play a 1 s A4 test tone, then exit.
    #[arg(long)]
    pub audio_smoke: bool,

    /// Audio device id (from --list-audio-devices) for --audio-smoke / GUI startup.
    #[arg(long)]
    pub device: Option<String>,

    /// Requested buffer size in frames for --audio-smoke / GUI startup.
    #[arg(long)]
    pub buffer: Option<u32>,

    /// Start the GUI without opening any audio device.
    #[arg(long)]
    pub no_audio: bool,

    /// Capture a PNG screenshot of the main window after `--screenshot-delay-ms`, then exit
    /// (GUI verification evidence).
    #[arg(long, value_name = "FILE.png")]
    pub screenshot: Option<PathBuf>,

    /// Delay before the screenshot is captured.
    #[arg(long, default_value_t = 2000)]
    pub screenshot_delay_ms: u64,

    /// Start with a UI state preset (verification aid): `advanced` opens the synth's
    /// advanced sections, `graph` additionally focuses the modular graph editor.
    #[arg(long, value_name = "NAME")]
    pub ui_preset: Option<String>,
}

#[derive(clap::ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum RenderMode {
    Pattern,
    Song,
}

#[derive(clap::ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum WavFormatArg {
    Pcm16,
    Pcm24,
    Float32,
}

pub fn run(args: Args) -> anyhow::Result<i32> {
    if args.list_audio_devices {
        let snap = rawdawg_audio_io::enumerate();
        println!("{}", serde_json::to_string_pretty(&snap)?);
        return Ok(0);
    }
    if args.audio_smoke {
        return crate::audio::smoke_test(args.device.clone(), args.buffer);
    }
    if let Some(project_path) = &args.headless_render {
        let output = args.output.clone().context("--output is required")?;
        if output.extension().and_then(|e| e.to_str()).is_none_or(|e| !e.eq_ignore_ascii_case("wav")) {
            bail!("only .wav output is supported by --headless-render at this milestone (got {})", output.display());
        }
        let project = rawdawg_project::load_project_dir(project_path).with_context(|| format!("loading project {}", project_path.display()))?;
        let sample_rate = match args.sample_rate {
            Some(hz) => Some(rawdawg_types::SampleRate::supported(hz).with_context(|| format!("{hz} Hz is not a supported rate (44100, 48000, 88200, 96000, 176400, 192000)"))?),
            None => None,
        };
        if args.start_tick < 0 {
            bail!("--start-tick must be nonnegative");
        }
        let opts = OfflineOptions {
            mode: match args.mode {
                RenderMode::Pattern => TransportMode::Pattern,
                RenderMode::Song => TransportMode::Song,
            },
            start_tick: Tick(args.start_tick),
            end_tick: args.end_tick.map(Tick),
            tail_seconds: args.tail,
            block_frames: 256,
            sample_rate,
            unprotected: args.unprotected,
            metronome: false,
            project_dir: rawdawg_project::persist::resolve_project_dir(project_path).ok(),
        };
        let format = match args.format {
            WavFormatArg::Pcm16 => WavFormat::Pcm16,
            WavFormatArg::Pcm24 => WavFormat::Pcm24,
            WavFormatArg::Float32 => WavFormat::Float32,
        };
        if let Some(parent) = output.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
        }
        let stats = render_project_to_wav(&project, &opts, format, &output).context("render failed")?;
        println!(
            "rendered {} frames at {} Hz to {} (peak {:.3}, clipped {}, nonfinite blocks {}, dropped events {}, {:.1}x realtime)",
            stats.frames,
            stats.sample_rate,
            output.display(),
            stats.peak,
            stats.clipped_samples,
            stats.nonfinite_blocks,
            stats.dropped_events,
            stats.realtime_factor
        );
        if stats.nonfinite_blocks > 0 || stats.dropped_events > 0 {
            eprintln!("warning: render reported engine health issues (see counts above)");
            return Ok(3);
        }
        return Ok(0);
    }
    crate::ui::run_gui(crate::ui::GuiOptions {
        open: args.open.clone(),
        safe_mode: args.safe_mode,
        device: args.device.clone(),
        buffer: args.buffer,
        no_audio: args.no_audio,
        screenshot: args.screenshot.clone().map(|p| (p, std::time::Duration::from_millis(args.screenshot_delay_ms))),
        ui_preset: args.ui_preset.clone(),
    })?;
    Ok(0)
}
