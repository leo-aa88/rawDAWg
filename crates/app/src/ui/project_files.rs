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
    /// "Save and close" opened Save As: close once that save succeeds.
    pub close_after_save: bool,
    /// A project to open once the user answered what happens to unsaved changes.
    pub open_prompt: Option<PathBuf>,
    /// "Save and open" opened Save As: open this once that save succeeds.
    pub open_after_save: Option<PathBuf>,
    /// The recovery offer is for never-saved projects of an earlier session (found at start).
    pub recovery_unsaved: bool,
    /// The window is asked to close (set when rawDAWg sends the close request; tests read it).
    pub closing: bool,
    pub last_autosave: Option<Instant>,
    pub last_autosave_note: Option<String>,
    /// Relink: asset id → path typed by the user.
    pub relink: Option<(rawdawg_types::AssetId, String)>,
    /// The recovery offer's Compare view: the chosen autosave, what it changes, and which
    /// version the engine plays.
    pub compare: Option<RecoveryCompare>,
}

pub(crate) struct RecoveryCompare {
    /// Index into `recovery`.
    pub candidate: usize,
    pub autosave: Project,
    pub changes: Vec<String>,
    /// The engine plays the autosave instead of the saved project (listening only).
    pub listen_autosave: bool,
}

impl App {
    /// Open the project the user picked (Projects list, File › Open, Recent): with unsaved
    /// changes, ask first (Save / Discard / Cancel) instead of dropping them.
    pub(crate) fn request_open(&mut self, path: PathBuf) {
        if self.history.is_dirty() {
            self.files.open_prompt = Some(path);
        } else {
            self.open_project_checked(path);
        }
    }

    /// This never-saved project's folder in the per-user area (its media and autosaves).
    pub(crate) fn unsaved_dir(&self) -> Option<PathBuf> {
        Some(self.unsaved_root.as_ref()?.join(format!("{}.rawdawg", self.project.project_id)))
    }

    /// The project has no folder of its own yet: none, or only its per-user working folder.
    pub(crate) fn is_never_saved(&self) -> bool {
        match &self.project_path {
            None => true,
            Some(p) => self.unsaved_root.as_ref().is_some_and(|r| p.starts_with(r)),
        }
    }

    /// Where autosaves go: the project's folder, or its per-user folder when never saved
    /// (locked there, so another running rawDAWg never takes it for a crashed session).
    fn autosave_dir(&mut self) -> Option<PathBuf> {
        if let Some(p) = &self.project_path {
            return Some(p.clone());
        }
        let dir = self.unsaved_dir()?;
        if self.files.lock.is_none() {
            self.files.lock = ProjectLock::acquire(&dir, false).ok();
        }
        Some(dir)
    }

    /// The work since the last save is given up (discarded, or the window closed cleanly):
    /// its autosaves go too, so the next start or open does not offer them as a crash.
    pub(crate) fn forget_unsaved_autosaves(&mut self) {
        let dir = if self.is_never_saved() { self.project_path.clone().or_else(|| self.unsaved_dir()) } else { self.project_path.clone() };
        if let Some(dir) = dir {
            rawdawg_project::persist::clear_autosaves(&dir);
        }
    }

    /// At start: never-saved projects of earlier sessions that ended without closing (their
    /// autosaves are still there and no running rawDAWg holds them) are offered for recovery.
    pub(crate) fn offer_unsaved_recovery(&mut self) {
        let Some(root) = self.unsaved_root.clone() else { return };
        let own = self.unsaved_dir();
        let mut found = Vec::new();
        for e in std::fs::read_dir(&root).into_iter().flatten().flatten() {
            let dir = e.path();
            if !dir.is_dir() || Some(&dir) == own.as_ref() {
                continue;
            }
            if ProjectLock::read(&dir).is_some_and(|l| rawdawg_project::lock::is_live(&l)) {
                continue;
            }
            found.extend(rawdawg_project::recovery_candidates(&dir));
        }
        found.sort_by_key(|c| std::cmp::Reverse(c.saved_unix_ms));
        if !found.is_empty() {
            self.files.recovery = found;
            self.files.recovery_unsaved = true;
        }
    }

    /// Open a project folder: lock it, load, then look for newer autosaves.
    pub(crate) fn open_project_checked(&mut self, path: PathBuf) {
        let dir = match rawdawg_project::persist::resolve_project_dir(&path) {
            Ok(d) => d,
            Err(e) => {
                self.status_line = format!("Could not open {}: {e}", path.display());
                self.files.error = Some(e.to_string());
                return;
            }
        };
        // the new project's lock first; the current project keeps its own if the open fails
        let (lock, read_only) = match ProjectLock::acquire(&dir, false) {
            Ok(l) => (Some(l), None),
            Err(e) => (None, Some(e.to_string())),
        };
        if !self.open_project(dir.clone()) {
            self.files.error = Some(self.status_line.clone());
            return;
        }
        // the previous project's lock is released here (replaced), the new one held
        self.files.lock = lock;
        self.files.read_only = read_only;
        self.files.recovery = rawdawg_project::recovery_candidates(&dir);
        self.files.recovery_unsaved = false;
        self.files.open_prompt = None;
        self.files.compare = None;
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

    /// Read a recovery candidate (large plugin states inlined from its folder's plugin-state/).
    fn load_candidate(&self, candidate: &RecoveryCandidate) -> Result<Project, String> {
        let mut p = std::fs::read(&candidate.path).map_err(|e| e.to_string()).and_then(|b| rawdawg_project::persist::parse_project(&b).map_err(|e| e.to_string()))?;
        if let Some(dir) = rawdawg_project::persist::candidate_project_dir(candidate) {
            let _ = rawdawg_project::plugin_state::inline(&dir, &mut p);
        }
        Ok(p)
    }

    /// Open the Compare view on recovery candidate `index` (what it changes; listen to either).
    pub(crate) fn compare_recovery(&mut self, index: usize) {
        let Some(c) = self.files.recovery.get(index).cloned() else { return };
        let listen_autosave = self.files.compare.as_ref().is_some_and(|x| x.listen_autosave);
        match self.load_candidate(&c) {
            Ok(autosave) => {
                let changes = rawdawg_project::compare::describe_changes(&self.project, &autosave);
                self.files.compare = Some(RecoveryCompare { candidate: index, autosave, changes, listen_autosave });
                if listen_autosave {
                    self.recompile();
                }
            }
            Err(e) => self.status_line = format!("Could not read the autosave: {e}"),
        }
    }

    /// Switch which version the engine plays while comparing (the document is not touched).
    pub(crate) fn listen_to_autosave(&mut self, autosave: bool) {
        if let Some(c) = self.files.compare.as_mut()
            && c.listen_autosave != autosave
        {
            c.listen_autosave = autosave;
            self.send(EngineCommand::Panic { source: None });
            self.recompile();
        }
    }

    /// Leave the Compare view; the engine plays the document again.
    fn end_compare(&mut self) {
        if self.files.compare.take().is_some_and(|c| c.listen_autosave) {
            self.send(EngineCommand::Panic { source: None });
            self.recompile();
        }
    }

    /// Use a recovery candidate as the working project (dirty; the saved manifest stays until Save).
    pub(crate) fn recover_from(&mut self, candidate: &RecoveryCandidate) {
        self.end_compare();
        match self.load_candidate(candidate) {
            Ok(p) => {
                self.send(EngineCommand::Panic { source: None });
                // later edits (and autosaves) number after both the saved and the recovered work
                self.history = History::continuing_from(self.project.revision.max(p.revision));
                self.history.mark_dirty();
                self.project = p;
                if self.files.recovery_unsaved {
                    // the crashed session's own folder: its media live there; Save asks where to put it
                    let dir = rawdawg_project::persist::candidate_project_dir(candidate);
                    self.files.lock = dir.as_ref().and_then(|d| ProjectLock::acquire(d, false).ok());
                    self.files.read_only = None;
                    self.project_path = dir;
                }
                self.selected_channel = self.project.channels.first().map(|c| c.id);
                self.rebuild_mirrors();
                self.recompile();
                self.status_line = if self.files.recovery_unsaved {
                    "Recovered the never-saved project; File › Save As… to keep it".to_owned()
                } else {
                    format!("Recovered the autosave from {}; save to keep it", ago(candidate.saved_unix_ms))
                };
            }
            Err(e) => self.status_line = format!("Could not recover: {e}"),
        }
        self.files.recovery.clear();
    }

    pub(crate) fn discard_recovery(&mut self) {
        self.end_compare();
        // the folders the offered autosaves belong to (the project's, or never-saved ones)
        let dirs: Vec<PathBuf> = self.files.recovery.iter().filter_map(rawdawg_project::persist::candidate_project_dir).collect();
        for dir in dirs {
            rawdawg_project::persist::clear_autosaves(&dir);
        }
        self.files.recovery.clear();
    }

    /// New empty project (unsaved working folder); the current one must be clean or confirmed.
    pub(crate) fn new_project(&mut self) {
        self.send(EngineCommand::Panic { source: None });
        // the user chose to lose the unsaved work: no crash offer for it later
        self.forget_unsaved_autosaves();
        self.files.lock = None;
        self.files.read_only = None;
        self.project = Project::new_default("Untitled", serde_json::to_value(rawdawg_synthesizer::Patch::default()).unwrap_or(serde_json::Value::Null));
        self.history = History::new();
        self.project_path = None;
        self.selected_channel = self.project.channels.first().map(|c| c.id);
        self.files.recovery.clear();
        self.files.compare = None;
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
        // a never-saved project (also one with media in its per-user folder) asks where to go
        let Some(path) = self.project_path.clone().filter(|_| !self.is_never_saved()) else {
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
        // an absolute path, so the project (and its recent entry) opens from any folder later
        let dst = crate::prefs::absolute(&dst);
        // "Song v1.2" becomes "Song v1.2.rawdawg" (not "Song v1.rawdawg")
        let dst = if dst.extension().is_some_and(|e| e == "rawdawg") {
            dst
        } else {
            let mut name = dst.clone().into_os_string();
            name.push(".rawdawg");
            PathBuf::from(name)
        };
        let revision = self.history.revision();
        self.project.revision = revision;
        let src = self.project_path.clone();
        let unsaved_before = self.is_never_saved().then(|| self.project_path.clone().or_else(|| self.unsaved_dir())).flatten();
        match rawdawg_project::save_as(src.as_deref(), &dst, &self.project) {
            Ok(()) => {
                // saved for real now: the per-user autosaves of the never-saved project are not a crash
                if let Some(old) = unsaved_before {
                    rawdawg_project::persist::clear_autosaves(&old);
                }
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
        // a never-saved project autosaves too (into its per-user folder)
        if self.history.is_dirty()
            && self.files.last_autosave.is_none_or(|t| t.elapsed() >= AUTOSAVE_EVERY)
            && let Some(dir) = self.autosave_dir()
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
    // "Save and close" belongs to the Save As it opened: leaving that for another dialog (New,
    // Open) forgets it, or a later ordinary Save As would quit
    if !matches!(app.files.dialog, Some(FileDialog::SaveAs)) {
        app.files.close_after_save = false;
        app.files.open_after_save = None;
    }
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
                                FileDialog::Open => app.request_open(p),
                                _ => {
                                    app.save_project_as(p);
                                    if app.files.close_after_save && !app.history.is_dirty() {
                                        app.files.close_after_save = false;
                                        app.files.closing = true;
                                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                                    } else if !app.history.is_dirty()
                                        && let Some(next) = app.files.open_after_save.take()
                                    {
                                        app.open_project_checked(next);
                                    }
                                }
                            }
                        }
                        if ui.button("Cancel").clicked() {
                            app.files.dialog = None;
                            // the save was cancelled: rawDAWg stays open, nothing else opens
                            app.files.close_after_save = false;
                            app.files.open_after_save = None;
                        }
                    });
                    if kind == FileDialog::Open && !app.prefs.recent_projects.is_empty() {
                        ui.separator();
                        ui.label(egui::RichText::new("Recent").strong());
                        let recent = app.prefs.recent_projects.clone();
                        for p in recent {
                            if ui.button(p.display().to_string()).clicked() {
                                app.request_open(p);
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
            app.files.close_after_save = false;
            app.files.open_after_save = None;
        }
    }
    // ---- recovery offer
    if !app.files.recovery.is_empty() {
        let mut keep_saved = false;
        let mut recover: Option<RecoveryCandidate> = None;
        let mut discard = false;
        let mut compare_index: Option<usize> = None;
        let mut listen: Option<bool> = None;
        let unsaved = app.files.recovery_unsaved;
        let title = if unsaved { "Recover a project that was never saved?" } else { "Recover unsaved changes?" };
        egui::Window::new(title).collapsible(false).resizable(false).show(ctx, |ui| {
            let selected = app.files.compare.as_ref().map_or(0, |c| c.candidate);
            let c = app.files.recovery[selected.min(app.files.recovery.len() - 1)].clone();
            ui.label(if unsaved {
                format!(
                    "rawDAWg did not close normally: a project that was never saved has autosaved work from {} ({} autosave(s)). Compare it, recover it (then Save As to keep it), leave it for the next start, or discard it.",
                    ago(c.saved_unix_ms),
                    app.files.recovery.len()
                )
            } else {
                format!(
                    "rawDAWg did not close normally: an autosave from {} has changes that are not in the saved project ({} autosave(s)). Compare them, recover it, keep the saved version, or discard the autosaves.",
                    ago(c.saved_unix_ms),
                    app.files.recovery.len()
                )
            });
            if app.files.recovery.len() > 1 {
                let label = |r: &RecoveryCandidate| format!("{} (revision {})", ago(r.saved_unix_ms), r.revision);
                egui::ComboBox::from_id_salt("recovery-candidate").selected_text(label(&c)).show_ui(ui, |ui| {
                    for (i, r) in app.files.recovery.iter().enumerate() {
                        if ui.selectable_label(i == selected, label(r)).clicked() {
                            compare_index = Some(i);
                        }
                    }
                });
            }
            match &app.files.compare {
                None => {
                    if ui.button("Compare").on_hover_text("What the autosave changes, and listen to either version before choosing").clicked() {
                        compare_index = Some(selected);
                    }
                }
                Some(cmp) => {
                    ui.separator();
                    if cmp.changes.is_empty() {
                        ui.label(egui::RichText::new("No musical difference: only bookkeeping changed.").color(theme::TEXT_SECONDARY));
                    } else {
                        ui.label(egui::RichText::new("The autosave, compared with the saved project:").strong());
                        egui::ScrollArea::vertical().max_height(180.0).show(ui, |ui| {
                            for line in &cmp.changes {
                                ui.label(format!("• {line}"));
                            }
                        });
                    }
                    ui.horizontal(|ui| {
                        ui.label("Listen to");
                        if ui.selectable_label(!cmp.listen_autosave, "Saved").clicked() {
                            listen = Some(false);
                        }
                        if ui.selectable_label(cmp.listen_autosave, "Autosave").clicked() {
                            listen = Some(true);
                        }
                        ui.label(egui::RichText::new("(Play/Stop as usual; nothing is changed until you choose)").small().color(theme::TEXT_SECONDARY));
                    });
                    ui.separator();
                }
            }
            ui.horizontal(|ui| {
                if ui.button("Recover").clicked() {
                    recover = Some(c.clone());
                }
                if ui.button(if unsaved { "Not now (ask at the next start)" } else { "Keep saved version (keep autosaves)" }).clicked() {
                    keep_saved = true;
                }
                if ui.button("Discard autosaves").clicked() {
                    discard = true;
                }
            });
        });
        if let Some(i) = compare_index {
            app.compare_recovery(i);
        }
        if let Some(l) = listen {
            app.listen_to_autosave(l);
        }
        if let Some(c) = recover {
            app.recover_from(&c);
        } else if keep_saved {
            app.end_compare();
            app.files.recovery.clear();
        } else if discard {
            app.discard_recovery();
        }
        if app.files.recovery.is_empty() {
            app.files.recovery_unsaved = false;
        }
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
                        app.files.closing = true;
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    } else if app.files.dialog == Some(FileDialog::SaveAs) {
                        // a never-saved project: close once Save As succeeds
                        app.files.close_after_save = true;
                    }
                    app.files.close_prompt = false;
                }
                if ui.button("Discard and close").clicked() {
                    app.forget_unsaved_autosaves();
                    app.history.mark_saved();
                    app.files.close_prompt = false;
                    app.files.closing = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
                if ui.button("Cancel").clicked() {
                    app.files.close_prompt = false;
                }
            });
        });
    }
    // ---- unsaved changes before opening another project
    if let Some(next) = app.files.open_prompt.clone() {
        let name = next.file_name().map_or_else(|| next.display().to_string(), |n| n.to_string_lossy().into_owned());
        egui::Window::new("Unsaved changes").id(egui::Id::unique("open-unsaved-prompt")).collapsible(false).resizable(false).show(ctx, |ui| {
            ui.label(format!("Save the current project before opening {name}?"));
            ui.horizontal(|ui| {
                if ui.button("Save and open").clicked() {
                    app.files.open_prompt = None;
                    app.save_project_checked();
                    if !app.history.is_dirty() {
                        app.open_project_checked(next.clone());
                    } else if app.files.dialog == Some(FileDialog::SaveAs) {
                        // a never-saved project: open once Save As succeeds
                        app.files.open_after_save = Some(next.clone());
                    }
                }
                if ui.button("Discard and open").clicked() {
                    app.files.open_prompt = None;
                    app.forget_unsaved_autosaves();
                    app.open_project_checked(next.clone());
                }
                if ui.button("Cancel").clicked() {
                    app.files.open_prompt = None;
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

/// "3 min ago" for a unix-millisecond time stamp.
fn ago(unix_ms: u64) -> String {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(unix_ms);
    let s = now.saturating_sub(unix_ms) / 1000;
    match s {
        0..=59 => format!("{s} s ago"),
        60..=3599 => format!("{} min ago", s / 60),
        3600..=86_399 => format!("{} h ago", s / 3600),
        _ => format!("{} days ago", s / 86_400),
    }
}
