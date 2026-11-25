//! Export dialog (IO-06): scope, range, format, rate, tail, stems and path. The render runs
//! on a worker thread with the same offline engine as playback; progress and cancel are
//! shared through atomics, the result (clipped samples, path) is shown when done. No
//! normalization, no hidden processing.

use super::{App, TransportMode};
use crate::theme;
use rawdawg_audio_engine::offline::{ExportFormat, OfflineOptions, RenderStats, StemFile, export_project, export_stems};
use rawdawg_project::Project;
use rawdawg_types::units::Tick;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ExportRange {
    Whole,
    LoopRange,
}

pub(crate) struct ExportState {
    pub open: bool,
    pub song: bool,
    pub range: ExportRange,
    pub format: ExportFormat,
    pub sample_rate: Option<u32>,
    pub tail_seconds: f64,
    pub stems: bool,
    pub unprotected: bool,
    pub path: String,
    job: Option<Job>,
    pub last_result: Option<String>,
}

impl ExportState {
    pub fn running(&self) -> bool {
        self.job.is_some()
    }
}

impl Default for ExportState {
    fn default() -> Self {
        Self {
            open: false,
            song: true,
            range: ExportRange::Whole,
            format: ExportFormat::Wav(rawdawg_audio_engine::offline::WavFormat::Pcm24),
            sample_rate: None,
            tail_seconds: 1.0,
            stems: false,
            unprotected: false,
            path: String::new(),
            job: None,
            last_result: None,
        }
    }
}

type JobResult = Result<(RenderStats, Vec<StemFile>), String>;

/// One running export.
struct Job {
    done: Arc<AtomicUsize>,
    total: Arc<AtomicUsize>,
    cancel: Arc<AtomicBool>,
    result: Arc<Mutex<Option<JobResult>>>,
    path: PathBuf,
    handle: Option<std::thread::JoinHandle<()>>,
}

pub(crate) fn options_for(app: &App, st: &ExportState) -> OfflineOptions {
    let (start_tick, end_tick) = match (st.range, app.project.r#loop) {
        (ExportRange::LoopRange, Some(l)) => (l.start_tick, Some(l.end_tick)),
        _ => (Tick::ZERO, None),
    };
    OfflineOptions {
        mode: if st.song { TransportMode::Song } else { TransportMode::Pattern },
        start_tick,
        end_tick,
        tail_seconds: st.tail_seconds.max(0.0),
        block_frames: 256,
        sample_rate: st.sample_rate.and_then(rawdawg_types::SampleRate::supported),
        unprotected: st.unprotected,
        metronome: false,
        project_dir: app.project_path.clone(),
    }
}

/// Start the export on a worker thread (the project is snapshotted; the UI stays live).
pub(crate) fn start(app: &mut App) {
    let path = PathBuf::from(app.export.path.trim());
    if path.as_os_str().is_empty() {
        app.export.last_result = Some("Choose an output path first".into());
        return;
    }
    let project: Project = app.project.clone();
    let opts = options_for(app, &app.export);
    let format = app.export.format;
    let stems = app.export.stems;
    let done = Arc::new(AtomicUsize::new(0));
    let total = Arc::new(AtomicUsize::new(1));
    let cancel = Arc::new(AtomicBool::new(false));
    let result = Arc::new(Mutex::new(None));
    let (d2, t2, c2, r2, p2) = (done.clone(), total.clone(), cancel.clone(), result.clone(), path.clone());
    let handle = std::thread::Builder::new()
        .name("rawdawg-export".into())
        .spawn(move || {
            let mut progress = |d: usize, t: usize| {
                d2.store(d, Ordering::Relaxed);
                t2.store(t.max(1), Ordering::Relaxed);
                !c2.load(Ordering::Relaxed)
            };
            let out = if stems {
                let dir = p2
                    .parent()
                    .map(|d| d.join(format!("{}-stems", p2.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "export".into()))))
                    .unwrap_or_else(|| PathBuf::from("stems"));
                export_project(&project, &opts, format, &p2, &mut progress)
                    .and_then(|stats| export_stems(&project, &opts, format, &dir, &mut |_, _| !c2.load(Ordering::Relaxed)).map(|(files, _)| (stats, files)))
            } else {
                export_project(&project, &opts, format, &p2, &mut progress).map(|stats| (stats, Vec::new()))
            };
            if let Ok(mut r) = r2.lock() {
                *r = Some(out.map_err(|e| e.to_string()));
            }
        })
        .ok();
    app.export.job = Some(Job { done, total, cancel, result, path, handle });
    app.export.last_result = None;
}

/// Poll the running job (called every frame).
pub(crate) fn poll(app: &mut App) {
    let Some(job) = app.export.job.as_mut() else { return };
    let finished = job.result.lock().ok().map(|r| r.is_some()).unwrap_or(false);
    if !finished {
        return;
    }
    let mut job = app.export.job.take().unwrap();
    if let Some(h) = job.handle.take() {
        let _ = h.join();
    }
    let outcome = job.result.lock().ok().and_then(|mut r| r.take());
    app.export.last_result = Some(match outcome {
        Some(Ok((stats, files))) => {
            let stems = if files.is_empty() {
                String::new()
            } else {
                format!(", {} stems in {}", files.len(), files[0].path.parent().map(|p| p.display().to_string()).unwrap_or_default())
            };
            format!(
                "Exported {} frames at {} Hz to {} (peak {:.3}, clipped samples {}{}){}",
                stats.frames,
                stats.sample_rate,
                job.path.display(),
                stats.peak,
                stats.clipped_samples,
                if stats.clipped_samples > 0 { " — lower the master or use float" } else { "" },
                stems
            )
        }
        Some(Err(e)) => format!("Export failed: {e}"),
        None => "Export ended without a result".into(),
    });
}

pub(crate) fn show(app: &mut App, ctx: &egui::Context) {
    let mut open = app.export.open;
    egui::Window::new("Export").open(&mut open).default_size([520.0, 360.0]).resizable(true).collapsible(false).show(ctx, |ui| {
        let running = app.export.running();
        ui.add_enabled_ui(!running, |ui| {
            ui.horizontal(|ui| {
                ui.label("Scope");
                ui.selectable_value(&mut app.export.song, true, "Song");
                ui.selectable_value(&mut app.export.song, false, "Pattern");
                ui.separator();
                ui.label("Range");
                ui.selectable_value(&mut app.export.range, ExportRange::Whole, "Whole");
                ui.add_enabled_ui(app.project.r#loop.is_some(), |ui| {
                    ui.selectable_value(&mut app.export.range, ExportRange::LoopRange, "Loop range");
                });
            });
            ui.horizontal(|ui| {
                ui.label("Format");
                egui::ComboBox::from_id_salt("export-format").selected_text(app.export.format.label()).show_ui(ui, |ui| {
                    for f in ExportFormat::ALL {
                        ui.selectable_value(&mut app.export.format, f, f.label());
                    }
                });
                ui.label("Rate");
                let label = app.export.sample_rate.map(|r| format!("{r} Hz")).unwrap_or_else(|| format!("Project ({} Hz)", app.project.sample_rate_hz.0));
                egui::ComboBox::from_id_salt("export-rate").selected_text(label).show_ui(ui, |ui| {
                    ui.selectable_value(&mut app.export.sample_rate, None, "Project rate");
                    for r in [44_100u32, 48_000, 88_200, 96_000] {
                        ui.selectable_value(&mut app.export.sample_rate, Some(r), format!("{r} Hz"));
                    }
                });
            });
            ui.horizontal(|ui| {
                ui.label("Tail");
                ui.add(egui::DragValue::new(&mut app.export.tail_seconds).range(0.0..=30.0).speed(0.1).suffix(" s"));
                ui.checkbox(&mut app.export.stems, "Stems (one file per mixer channel, post-fader)")
                    .on_hover_text("Written next to the main file in a “<name>-stems” folder; stems of channels that share sends need not sum to the master");
            });
            ui.checkbox(&mut app.export.unprotected, "Unprotected (bypass the master limiter; float output recommended)");
            ui.horizontal(|ui| {
                ui.label("File");
                ui.add(egui::TextEdit::singleline(&mut app.export.path).hint_text("/path/to/mix.wav").desired_width(f32::INFINITY));
            });
            // keep the extension in step with the format
            let ext = app.export.format.extension();
            if !app.export.path.trim().is_empty() && !app.export.path.to_lowercase().ends_with(&format!(".{ext}")) {
                let p = PathBuf::from(app.export.path.trim()).with_extension(ext);
                app.export.path = p.display().to_string();
            }
            if app.export.path.trim().is_empty()
                && let Some(dir) = &app.project_path
            {
                app.export.path = dir.join(format!("{}.{ext}", app.project.name.replace('/', "_"))).display().to_string();
            }
        });
        ui.separator();
        if running {
            let job = app.export.job.as_ref().unwrap();
            let (d, t) = (job.done.load(Ordering::Relaxed), job.total.load(Ordering::Relaxed).max(1));
            ui.add(egui::ProgressBar::new(d as f32 / t as f32).show_percentage());
            if ui.button("Cancel").clicked() {
                job.cancel.store(true, Ordering::Relaxed);
            }
        } else {
            ui.horizontal(|ui| {
                if ui.button("Export").clicked() {
                    start(app);
                }
                if ui.button("Export MIDI (type 1)").on_hover_text("The arrangement as a Standard MIDI File next to the audio path").clicked() {
                    let p = PathBuf::from(app.export.path.trim()).with_extension("mid");
                    app.export.last_result = Some(match rawdawg_project::smf::export_song(&app.project) {
                        Ok(bytes) => match std::fs::write(&p, bytes) {
                            Ok(()) => format!("Wrote {}", p.display()),
                            Err(e) => format!("Could not write {}: {e}", p.display()),
                        },
                        Err(e) => format!("MIDI export failed: {e}"),
                    });
                }
            });
        }
        if let Some(r) = &app.export.last_result {
            ui.label(egui::RichText::new(r).color(if r.starts_with("Export failed") || r.starts_with("Could not") { theme::ERROR } else { theme::TEXT_SECONDARY }));
        }
    });
    app.export.open = open;
}
