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
    /// What the user typed; the extension follows the format when writing (see [`audio_path`]).
    pub path: String,
    /// The project folder `path` belongs to: another project gets its own default path.
    path_for: Option<Option<PathBuf>>,
    /// An export waiting for "Replace" because its files exist.
    confirm: Option<(Pending, Vec<PathBuf>)>,
    job: Option<Job>,
    pub last_result: Option<String>,
}

/// What waits for the overwrite confirmation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pending {
    Audio,
    Midi,
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
            path_for: None,
            confirm: None,
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

/// The typed path without a known export extension (`mix.wav`, `mix.mid` → `mix`).
fn stem(typed: &str) -> Option<PathBuf> {
    let t = typed.trim();
    if t.is_empty() {
        return None;
    }
    let p = PathBuf::from(t);
    let known = p.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).is_some_and(|e| matches!(e.as_str(), "wav" | "flac" | "mid" | "midi"));
    Some(if known { p.with_extension("") } else { p })
}

fn with_ext(stem: &std::path::Path, ext: &str) -> PathBuf {
    PathBuf::from(format!("{}.{ext}", stem.display()))
}

/// The audio file an export writes: the typed path with the format's extension.
pub(crate) fn audio_path(st: &ExportState) -> Option<PathBuf> {
    stem(&st.path).map(|s| with_ext(&s, st.format.extension()))
}

/// The MIDI file "Export MIDI" writes: the typed path with `.mid`.
pub(crate) fn midi_path(st: &ExportState) -> Option<PathBuf> {
    stem(&st.path).map(|s| with_ext(&s, "mid"))
}

/// The stems folder next to the audio file.
fn stems_dir(audio: &std::path::Path) -> PathBuf {
    audio
        .parent()
        .map(|d| d.join(format!("{}-stems", audio.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "export".into()))))
        .unwrap_or_else(|| PathBuf::from("stems"))
}

/// The export path proposed for the open project (empty for an unsaved one).
fn default_path(app: &App) -> String {
    app.project_path.as_ref().map(|dir| dir.join(format!("{}.{}", app.project.name.replace('/', "_"), app.export.format.extension())).display().to_string()).unwrap_or_default()
}

/// Export audio, asking first when it would replace files.
pub(crate) fn request_audio(app: &mut App) {
    let Some(audio) = audio_path(&app.export) else {
        app.export.last_result = Some("Choose an output path first".into());
        return;
    };
    let mut existing: Vec<PathBuf> = vec![audio.clone()];
    if app.export.stems {
        existing.push(stems_dir(&audio));
    }
    existing.retain(|p| p.exists());
    if existing.is_empty() {
        start(app);
    } else {
        app.export.confirm = Some((Pending::Audio, existing));
    }
}

/// Export MIDI for the chosen scope, refusing an empty result and asking before replacing.
pub(crate) fn request_midi(app: &mut App) {
    let Some(path) = midi_path(&app.export) else {
        app.export.last_result = Some("Choose an output path first".into());
        return;
    };
    let notes = if app.export.song {
        rawdawg_project::smf::song_note_count(&app.project)
    } else {
        app.project.selected_pattern.map_or(0, |p| rawdawg_project::smf::pattern_note_count(&app.project, p))
    };
    if notes == 0 {
        app.export.last_result = Some(if app.export.song {
            "Nothing to export: no pattern on the playlist has notes. Choose Scope Pattern to export the selected pattern.".into()
        } else {
            "Nothing to export: the selected pattern has no notes.".into()
        });
        return;
    }
    if path.exists() {
        app.export.confirm = Some((Pending::Midi, vec![path]));
    } else {
        write_midi(app, &path);
    }
}

fn write_midi(app: &mut App, path: &std::path::Path) {
    let bytes = if app.export.song {
        rawdawg_project::smf::export_song(&app.project)
    } else {
        match app.project.selected_pattern {
            Some(p) => rawdawg_project::smf::export_pattern(&app.project, p),
            None => Err(rawdawg_project::smf::SmfError::Empty),
        }
    };
    app.export.last_result = Some(match bytes {
        Ok(bytes) => match std::fs::write(path, bytes) {
            Ok(()) => format!("Wrote {}", path.display()),
            Err(e) => format!("Could not write {}: {e}", path.display()),
        },
        Err(e) => format!("MIDI export failed: {e}"),
    });
}

/// Start the export on a worker thread (the project is snapshotted; the UI stays live).
pub(crate) fn start(app: &mut App) {
    let Some(path) = audio_path(&app.export) else {
        app.export.last_result = Some("Choose an output path first".into());
        return;
    };
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
                let dir = stems_dir(&p2);
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
            // the path belongs to the open project: another project starts from its own default
            match &app.export.path_for {
                Some(owner) if *owner == app.project_path => {}
                Some(_) => {
                    app.export.path = default_path(app);
                    app.export.confirm = None;
                }
                None => {}
            }
            app.export.path_for = Some(app.project_path.clone());
            if app.export.path.trim().is_empty() {
                app.export.path = default_path(app);
            }
            ui.horizontal(|ui| {
                ui.label("File");
                ui.add(egui::TextEdit::singleline(&mut app.export.path).hint_text("/path/to/mix").desired_width(f32::INFINITY));
            });
            // what will be written (the extension follows the format; MIDI gets .mid)
            if let (Some(a), Some(m)) = (audio_path(&app.export), midi_path(&app.export)) {
                let stems = if app.export.stems { format!(" + {}", stems_dir(&a).display()) } else { String::new() };
                ui.label(egui::RichText::new(format!("Audio: {}{stems} · MIDI: {}", a.display(), m.display())).small().color(theme::TEXT_SECONDARY));
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
            match app.export.confirm.clone() {
                Some((pending, existing)) => {
                    let names: Vec<String> = existing.iter().map(|p| p.display().to_string()).collect();
                    ui.colored_label(theme::WARNING, format!("{} already exist{}. Replace?", names.join(" and "), if names.len() == 1 { "s" } else { "" }));
                    ui.horizontal(|ui| {
                        if ui.button("Replace").clicked() {
                            app.export.confirm = None;
                            match pending {
                                Pending::Audio => start(app),
                                Pending::Midi => {
                                    if let Some(p) = midi_path(&app.export) {
                                        write_midi(app, &p);
                                    }
                                }
                            }
                        }
                        if ui.button("Keep the existing file").clicked() {
                            app.export.confirm = None;
                            app.export.last_result = Some("Nothing written: choose another file name".into());
                        }
                    });
                }
                None => {
                    ui.horizontal(|ui| {
                        if ui.button("Export").clicked() {
                            request_audio(app);
                        }
                        let what = if app.export.song { "The song (pattern clips on the playlist)" } else { "The selected pattern" };
                        if ui.button("Export MIDI (type 1)").on_hover_text(format!("{what} as a Standard MIDI File")).clicked() {
                            request_midi(app);
                        }
                    });
                }
            }
        }
        if let Some(r) = &app.export.last_result {
            ui.label(egui::RichText::new(r).color(if r.starts_with("Export failed") || r.starts_with("Could not") || r.starts_with("Nothing") {
                theme::ERROR
            } else {
                theme::TEXT_SECONDARY
            }));
        }
    });
    app.export.open = open;
}
