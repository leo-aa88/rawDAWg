//! Project files (DATA-01): New / Open / Save / Save As through dark in-app dialogs,
//! autosave every two minutes while dirty, crash recovery offer on open, advisory lock
//! (read-only when held elsewhere, with take-over), unsaved-changes prompt on close, and
//! asset relink. Everything here goes through `rawdawg_project::persist`.

use super::{App, EditorView, EngineCommand};
use crate::theme;
use rawdawg_project::lock::ProjectLock;
use rawdawg_project::persist::RecoveryCandidate;
use rawdawg_project::{Command, History, Project};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub const AUTOSAVE_EVERY: Duration = Duration::from_secs(120);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FileDialog {
    Open,
    SaveAs,
    New,
}

#[derive(Default)]
pub(crate) struct ProjectFilesState {
    pub dialog: Option<FileDialog>,
    pub path_input: String,
    pub error: Option<String>,
    /// Recovery candidates found when the project was opened (newest first).
    pub recovery: Vec<RecoveryCandidate>,
    pub lock: Option<ProjectLock>,
    /// Another process holds the lock: edits allowed, saving refused until taken over.
    pub read_only: Option<String>,
    pub close_prompt: bool,
    pub last_autosave: Option<Instant>,
    pub last_autosave_note: Option<String>,
    /// Relink: asset id → path typed by the user.
    pub relink: Option<(rawdawg_types::AssetId, String)>,
}

impl App {
    /// Open a project folder: lock it, load, then look for newer autosaves.
    pub(crate) fn open_project_checked(&mut self, path: PathBuf) {
        let dir = match rawdawg_project::persist::resolve_project_dir(&path) {
            Ok(d) => d,
            Err(e) => {
                self.files.error = Some(e.to_string());
                return;
            }
        };
        self.files.lock = None;
        self.files.read_only = None;
        match ProjectLock::acquire(&dir, false) {
            Ok(l) => self.files.lock = Some(l),
            Err(e) => self.files.read_only = Some(e.to_string()),
        }
        self.open_project(dir.clone());
        self.files.recovery = rawdawg_project::recovery_candidates(&dir);
        self.files.last_autosave = Some(Instant::now());
        self.files.error = None;
        self.files.dialog = None;
    }

    /// Take over a lock held elsewhere (the user's explicit choice).
    pub(crate) fn take_over_lock(&mut self) {
        let Some(dir) = self.project_path.clone() else { return };
        if let Ok(l) = ProjectLock::acquire(&dir, true) {
            self.files.lock = Some(l);
            self.files.read_only = None;
            self.status_line = "Project lock taken over; saving enabled".into();
        }
    }

    /// Use a recovery candidate as the working project (dirty; the saved manifest stays until Save).
    pub(crate) fn recover_from(&mut self, candidate: &RecoveryCandidate) {
        match std::fs::read(&candidate.path).map_err(|e| e.to_string()).and_then(|b| rawdawg_project::persist::parse_project(&b).map_err(|e| e.to_string())) {
            Ok(mut p) => {
                if let Some(dir) = &self.project_path {
                    // large plugin states of the autosave live in the project's plugin-state/
                    let _ = rawdawg_project::plugin_state::inline(dir, &mut p);
                }
                self.send(EngineCommand::Panic { source: None });
                self.project = p;
                self.history = History::new();
                self.history.mark_dirty();
                self.rebuild_mirrors();
                self.recompile();
                self.status_line = format!("Recovered revision {} from {}; save to keep it", candidate.revision, candidate.path.display());
            }
            Err(e) => self.status_line = format!("Could not recover: {e}"),
        }
        self.files.recovery.clear();
    }

    pub(crate) fn discard_recovery(&mut self) {
        if let Some(dir) = &self.project_path {
            rawdawg_project::persist::clear_autosaves(dir);
        }
        self.files.recovery.clear();
    }

    /// New empty project (unsaved working folder); the current one must be clean or confirmed.
    pub(crate) fn new_project(&mut self) {
        self.send(EngineCommand::Panic { source: None });
        self.files.lock = None;
        self.files.read_only = None;
        self.project = Project::new_default("Untitled", serde_json::to_value(rawdawg_synthesizer::Patch::default()).unwrap_or(serde_json::Value::Null));
        self.history = History::new();
        self.project_path = None;
        self.selected_channel = self.project.channels.first().map(|c| c.id);
        self.files.recovery.clear();
        self.rebuild_mirrors();
        self.recompile();
        self.status_line = "New project (unsaved — File › Save As… to choose a folder)".into();
    }

    /// Save to the current folder (revision stamped; autosaves cleared on success).
    pub(crate) fn save_project_checked(&mut self) {
        if let Some(r) = &self.files.read_only {
            self.status_line = format!("Read-only: {r} — take over the lock to save");
            return;
        }
        let Some(path) = self.project_path.clone() else {
            self.files.dialog = Some(FileDialog::SaveAs);
            return;
        };
        let revision = self.history.revision();
        self.project.revision = revision;
        match rawdawg_project::save_project_dir(&path, &self.project) {
            Ok(()) => {
                self.history.mark_saved_at(revision);
                rawdawg_project::persist::clear_autosaves(&path);
                self.status_line = format!("Saved {}", path.display());
            }
            Err(e) => self.status_line = format!("Save failed: {e}"),
        }
    }

    /// Save As into a new folder (assets copied); the new folder becomes the project path.
    pub(crate) fn save_project_as(&mut self, dst: PathBuf) {
        let dst = if dst.extension().is_some_and(|e| e == "rawdawg") { dst } else { dst.with_extension("rawdawg") };
        let revision = self.history.revision();
        self.project.revision = revision;
        let src = self.project_path.clone();
        match rawdawg_project::save_as(src.as_deref(), &dst, &self.project) {
            Ok(()) => {
                self.files.lock = ProjectLock::acquire(&dst, false).ok();
                self.files.read_only = None;
                self.project_path = Some(dst.clone());
                self.history.mark_saved_at(revision);
                self.prefs.remember_project(dst.clone());
                self.save_prefs();
                self.assets.sync(self.project_path.clone(), &self.project);
                self.files.error = None;
                self.files.dialog = None;
                self.status_line = format!("Saved as {}", dst.display());
            }
            Err(e) => self.files.error = Some(e.to_string()),
        }
    }

    /// Every frame: autosave when dirty and due; handle a close request while dirty.
    pub(crate) fn poll_project_files(&mut self, ctx: &egui::Context) {
        if self.history.is_dirty()
            && let Some(dir) = self.project_path.clone()
            && self.files.last_autosave.is_none_or(|t| t.elapsed() >= AUTOSAVE_EVERY)
        {
            self.autosave_now(&dir);
        }
        if ctx.input(|i| i.viewport().close_requested()) && self.history.is_dirty() && !self.files.close_prompt {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.files.close_prompt = true;
        }
    }

    pub(crate) fn autosave_now(&mut self, dir: &Path) {
        self.project.revision = self.history.revision();
        self.files.last_autosave = Some(Instant::now());
        self.files.last_autosave_note = Some(match rawdawg_project::autosave(dir, &self.project) {
            Ok(p) => format!("Autosaved revision {} to {}", self.project.revision, p.display()),
            Err(e) => format!("Autosave failed: {e}"),
        });
    }

    /// Relink a missing asset: same content restores the file; different content replaces
    /// the asset and rewrites every reference (undoable).
    pub(crate) fn relink_asset(&mut self, id: &rawdawg_types::AssetId, source: &Path) {
        let Some(dir) = self.working_dir() else { return };
        match rawdawg_project::assets::import_asset(&dir, source) {
            Ok(asset) if &asset.id == id => {
                self.assets.sync(self.project_path.clone(), &self.project);
                self.recompile();
                self.status_line = format!("Relinked {} (same content)", asset.source_name);
            }
            Ok(asset) => {
                let name = asset.source_name.clone();
                self.apply(Command::ReplaceAsset { old: id.clone(), new: asset });
                self.status_line = format!("Replaced the missing asset with {name} (different content; undoable)");
            }
            Err(e) => self.status_line = format!("Relink failed: {e}"),
        }
        self.files.relink = None;
    }

    pub(crate) fn asset_is_missing(&self, asset: &rawdawg_project::Asset) -> bool {
        match &self.project_path {
            Some(dir) => rawdawg_project::assets::asset_path(dir, asset).is_none_or(|p| !p.is_file()),
            None => true,
        }
    }
}

pub(crate) fn show(app: &mut App, ctx: &egui::Context) {
    // ---- file dialog (dark, in-app: a path field plus the recent list)
    if let Some(kind) = app.files.dialog {
        let title = match kind {
            FileDialog::Open => "Open project",
            FileDialog::SaveAs => "Save project as",
            FileDialog::New => "New project",
        };
        let mut open = true;
        egui::Window::new(title).open(&mut open).collapsible(false).resizable(true).default_size([520.0, 300.0]).show(ctx, |ui| {
            match kind {
                FileDialog::New => {
                    ui.label("Start a new empty project? Unsaved changes in the current project will be lost.");
                    ui.horizontal(|ui| {
                        if ui.button("New project").clicked() {
                            app.new_project();
                            app.files.dialog = None;
                        }
                        if ui.button("Cancel").clicked() {
                            app.files.dialog = None;
                        }
                    });
                }
                FileDialog::Open | FileDialog::SaveAs => {
                    ui.label(if kind == FileDialog::Open {
                        "Project folder (…/Song.rawdawg) or its project.json:"
                    } else {
                        "New folder for the project (…/Song.rawdawg); assets are copied, the current folder is left as is:"
                    });
                    ui.add(egui::TextEdit::singleline(&mut app.files.path_input).hint_text("/path/to/Song.rawdawg").desired_width(f32::INFINITY));
                    ui.horizontal(|ui| {
                        if ui.button(if kind == FileDialog::Open { "Open" } else { "Save as" }).clicked() && !app.files.path_input.trim().is_empty() {
                            let p = PathBuf::from(app.files.path_input.trim());
                            match kind {
                                FileDialog::Open => app.open_project_checked(p),
                                _ => app.save_project_as(p),
                            }
                        }
                        if ui.button("Cancel").clicked() {
                            app.files.dialog = None;
                        }
                    });
                    if kind == FileDialog::Open && !app.prefs.recent_projects.is_empty() {
                        ui.separator();
                        ui.label(egui::RichText::new("Recent").strong());
                        let recent = app.prefs.recent_projects.clone();
                        for p in recent {
                            if ui.button(p.display().to_string()).clicked() {
                                app.open_project_checked(p);
                            }
                        }
                    }
                }
            }
            if let Some(e) = &app.files.error {
                ui.colored_label(theme::ERROR, e);
            }
        });
        if !open {
            app.files.dialog = None;
        }
    }
    // ---- recovery offer
    if !app.files.recovery.is_empty() {
        egui::Window::new("Recover unsaved changes?").collapsible(false).resizable(false).show(ctx, |ui| {
            let c = app.files.recovery[0].clone();
            ui.label(format!(
                "An autosave newer than the saved project was found (revision {} vs {}, {} autosave(s)). Recover it, keep the saved version, or discard the autosaves.",
                c.revision,
                app.project.revision,
                app.files.recovery.len()
            ));
            ui.horizontal(|ui| {
                if ui.button("Recover").clicked() {
                    app.recover_from(&c);
                }
                if ui.button("Keep saved version (keep autosaves)").clicked() {
                    app.files.recovery.clear();
                }
                if ui.button("Discard autosaves").clicked() {
                    app.discard_recovery();
                }
            });
        });
    }
    // ---- read-only banner
    if let Some(r) = app.files.read_only.clone() {
        egui::Window::new("Project is open elsewhere").collapsible(false).resizable(false).show(ctx, |ui| {
            ui.label(format!("{r}. You can look and edit, but saving is refused so the other copy is not overwritten."));
            ui.horizontal(|ui| {
                if ui.button("Take over the lock").clicked() {
                    app.take_over_lock();
                }
                if ui.button("Stay read-only").clicked() {
                    app.files.read_only = Some(format!("{r} (acknowledged)"));
                }
            });
        });
    }
    // ---- unsaved changes on close
    if app.files.close_prompt {
        egui::Window::new("Unsaved changes").collapsible(false).resizable(false).show(ctx, |ui| {
            ui.label("Save the project before closing?");
            ui.horizontal(|ui| {
                if ui.button("Save and close").clicked() {
                    app.save_project_checked();
                    if !app.history.is_dirty() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                    app.files.close_prompt = false;
                }
                if ui.button("Discard and close").clicked() {
                    app.history.mark_saved();
                    app.files.close_prompt = false;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
                if ui.button("Cancel").clicked() {
                    app.files.close_prompt = false;
                }
            });
        });
    }
    // ---- relink
    if let Some((id, mut path)) = app.files.relink.clone() {
        let name = app.project.assets.iter().find(|a| a.id == id).map(|a| a.source_name.clone()).unwrap_or_default();
        let mut open = true;
        egui::Window::new("Relink asset").open(&mut open).collapsible(false).show(ctx, |ui| {
            ui.label(format!("“{name}” is missing. Pick the file (same content restores it; different content replaces it everywhere, undoable):"));
            if ui.add(egui::TextEdit::singleline(&mut path).hint_text("/path/to/file.wav").desired_width(f32::INFINITY)).changed() {
                app.files.relink = Some((id.clone(), path.clone()));
            }
            if ui.button("Relink").clicked() && !path.trim().is_empty() {
                let p = PathBuf::from(path.trim());
                app.relink_asset(&id, &p);
            }
        });
        if !open {
            app.files.relink = None;
        }
    }
    let _ = EditorView::Rack;
}
