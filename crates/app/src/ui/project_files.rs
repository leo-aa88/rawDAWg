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
    /// Which offered candidate is chosen (the list's selection; Compare shows this one).
    pub recovery_selected: usize,
    /// A never-saved recovery offer's line per candidate (name and what it holds).
    pub recovery_labels: Vec<String>,
    /// Recover was clicked while the project has unsaved changes: ask before replacing them.
    pub recover_confirm: bool,
    /// Offer never-saved projects of earlier sessions once the current offer is answered.
    pub unsaved_offer_pending: bool,
    /// The autosave files this session wrote (or took over by recovering) for this project:
    /// the only ones removed when its work is saved or given up.
    pub session_autosaves: Vec<PathBuf>,
    /// Why "Save and close" / "Save and open" could not save (the prompt stays open).
    pub prompt_note: Option<String>,
    /// The window is asked to close (set when rawDAWg sends the close request; tests read it).
    pub closing: bool,
    pub last_autosave: Option<Instant>,
    /// The last autosave: when (unix ms) and where, or why it failed.
    pub last_autosave_note: Option<Result<(u64, PathBuf), String>>,
    /// The Rename project window, with the name being typed.
    pub rename: Option<String>,
    /// Give the file dialog's text field the keyboard focus (once, when it opens).
    pub focus_field: bool,
    /// The same for the Rename project window.
    pub focus_rename: bool,
    /// The file dialog and Rename window drawn last frame: Enter on the frame one opens (say,
    /// the Enter that chose the menu entry) does not confirm it.
    shown_dialog: Option<FileDialog>,
    shown_rename: bool,
    /// Relink: asset id → path typed by the user.
    pub relink: Option<(rawdawg_types::AssetId, String)>,
    /// The recovery offer's Compare view: the chosen autosave (`recovery_selected`), what it
    /// changes, and which version the engine plays.
    pub compare: Option<RecoveryCompare>,
}

pub(crate) struct RecoveryCompare {
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
        // a folder another window holds is not ours to write into
        self.files.lock.as_ref().map(|_| dir)
    }

    /// The never-saved project's per-user folder (where its media and autosaves are, or would be).
    fn unsaved_folder(&self) -> Option<PathBuf> {
        if !self.is_never_saved() {
            return None;
        }
        self.project_path.clone().or_else(|| self.unsaved_dir())
    }

    /// Delete a never-saved project's per-user folder; nothing outside the per-user area.
    fn remove_unsaved_folder(&self, dir: &Path) {
        let Some(root) = &self.unsaved_root else { return };
        if dir.starts_with(root) && dir != root.as_path() && dir.extension().is_some_and(|e| e == "rawdawg") {
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    /// Delete the autosaves this session wrote: the work they protect was saved or given up.
    /// Older ones (a crash the user chose to keep) stay.
    fn forget_session_autosaves(&mut self) {
        for p in std::mem::take(&mut self.files.session_autosaves) {
            let _ = std::fs::remove_file(p);
        }
    }

    /// The work since the last save is given up (discarded, or the window closed with nothing
    /// unsaved): this session's autosaves go, and a never-saved project's folder with them, so
    /// the next start or open offers nothing as a crash.
    pub(crate) fn give_up_session_work(&mut self) {
        self.forget_session_autosaves();
        if let Some(dir) = self.unsaved_folder() {
            self.files.lock = None;
            self.remove_unsaved_folder(&dir);
        }
    }

    /// Never-saved projects of earlier sessions that ended without closing (their folder still
    /// holds the lock of a process that is gone) are offered for recovery, one line each.
    pub(crate) fn offer_unsaved_recovery(&mut self) {
        let Some(root) = self.unsaved_root.clone() else { return };
        let own = self.unsaved_dir();
        let mut found = Vec::new();
        for e in std::fs::read_dir(&root).into_iter().flatten().flatten() {
            let dir = e.path();
            if !dir.is_dir() || Some(&dir) == own.as_ref() {
                continue;
            }
            // a clean exit removes the folder (or at least its lock); a running rawDAWg holds it
            if ProjectLock::read(&dir).is_none_or(|l| rawdawg_project::lock::is_live(&l)) {
                continue;
            }
            if let Some(c) = rawdawg_project::persist::latest_snapshot(&dir) {
                let label = rawdawg_project::persist::load_candidate(&c).map_or_else(|_| "A project".to_owned(), |p| summary(&p));
                found.push((c, label));
            }
        }
        found.sort_by_key(|(c, _)| std::cmp::Reverse(c.saved_unix_ms));
        if !found.is_empty() {
            (self.files.recovery, self.files.recovery_labels) = found.into_iter().unzip();
            self.files.recovery_unsaved = true;
            self.files.recovery_selected = 0;
            self.files.recover_confirm = false;
        }
    }

    /// Open a project folder: lock it, load, then look for newer autosaves. Whether it opened.
    pub(crate) fn open_project_checked(&mut self, path: PathBuf) -> bool {
        let dir = match rawdawg_project::persist::resolve_project_dir(&path) {
            Ok(d) => crate::prefs::absolute(&d),
            Err(e) => {
                self.status_line = format!("Could not open {}: {e}", path.display());
                self.files.error = Some(e.to_string());
                return false;
            }
        };
        // reopening this project keeps our lock (a second one would take the file with it if
        // the load failed); another project's lock is taken first, the current one kept if the
        // open fails
        let reopen = self.project_path.as_ref() == Some(&dir) && self.files.lock.is_some();
        let acquired = (!reopen).then(|| ProjectLock::acquire(&dir, false));
        // what this session leaves behind once the other project is open
        let left_autosaves = std::mem::take(&mut self.files.session_autosaves);
        let left_unsaved = self.unsaved_folder().filter(|d| *d != dir);
        // the engine must play the opened project, not an autosave being compared
        let compare = self.files.compare.take();
        if !self.open_project(dir.clone()) {
            self.files.session_autosaves = left_autosaves;
            self.files.compare = compare;
            self.files.error = Some(self.status_line.clone());
            return false;
        }
        // the work left behind was saved, or the user chose to discard it
        for p in left_autosaves {
            let _ = std::fs::remove_file(p);
        }
        match acquired {
            Some(Ok(l)) => (self.files.lock, self.files.read_only) = (Some(l), None),
            Some(Err(e)) => (self.files.lock, self.files.read_only) = (None, Some(e.to_string())),
            None => {}
        }
        if let Some(old) = left_unsaved {
            self.remove_unsaved_folder(&old);
        }
        // a copy open read-only beside a running one: that one's autosaves are not a crash
        self.files.recovery = if self.files.read_only.is_some() { Vec::new() } else { rawdawg_project::recovery_candidates(&dir) };
        self.files.recovery_unsaved = false;
        self.files.recovery_labels.clear();
        self.files.recovery_selected = 0;
        self.files.recover_confirm = false;
        self.files.open_prompt = None;
        self.files.prompt_note = None;
        self.files.last_autosave = Some(Instant::now());
        self.files.last_autosave_note = None;
        self.files.error = None;
        self.files.dialog = None;
        true
    }

    /// Take over a lock held elsewhere (the user's explicit choice).
    pub(crate) fn take_over_lock(&mut self) {
        let Some(dir) = self.project_path.clone() else { return };
        if let Ok(l) = ProjectLock::acquire(&dir, true) {
            self.files.lock = Some(l);
            self.files.read_only = None;
            self.status_line = "Project lock taken over; saving enabled".into();
            // the holder's autosaves are offered now, as after a crash (it may be one elsewhere)
            if self.files.recovery.is_empty() && !self.is_never_saved() {
                self.files.recovery = rawdawg_project::recovery_candidates(&dir);
                self.files.recovery_unsaved = false;
                self.files.recovery_selected = 0;
            }
        }
    }

    /// Read a recovery candidate (large plugin states inlined from its folder's plugin-state/).
    fn load_candidate(&self, candidate: &RecoveryCandidate) -> Result<Project, String> {
        rawdawg_project::persist::load_candidate(candidate).map_err(|e| e.to_string())
    }

    /// Open the Compare view on recovery candidate `index` (what it changes; listen to either).
    pub(crate) fn compare_recovery(&mut self, index: usize) {
        let Some(c) = self.files.recovery.get(index).cloned() else { return };
        let listen_autosave = self.files.compare.as_ref().is_some_and(|x| x.listen_autosave);
        match self.load_candidate(&c) {
            Ok(autosave) => {
                let changes = rawdawg_project::compare::describe_changes(&self.project, &autosave);
                self.files.compare = Some(RecoveryCompare { autosave, changes, listen_autosave });
                self.files.recovery_selected = index;
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
    pub(crate) fn end_compare(&mut self) {
        if self.files.compare.take().is_some_and(|c| c.listen_autosave) {
            self.send(EngineCommand::Panic { source: None });
            self.recompile();
        }
    }

    /// Use a recovery candidate as the working project (dirty; the saved manifest stays until Save).
    pub(crate) fn recover_from(&mut self, candidate: &RecoveryCandidate) {
        let unsaved = self.files.recovery_unsaved;
        // a never-saved project's folder is taken with its lock or not at all: two windows
        // offered the same crash, and the other one recovered it first
        let lock = if unsaved {
            match ProjectLock::acquire(&candidate.dir, false) {
                Ok(l) => Some(l),
                Err(_) => {
                    self.status_line = "That project was recovered in another rawDAWg window".to_owned();
                    self.drop_offered(&candidate.dir);
                    return;
                }
            }
        } else {
            None
        };
        self.end_compare();
        match self.load_candidate(candidate) {
            Ok(p) => {
                self.send(EngineCommand::Panic { source: None });
                if unsaved {
                    // the project it replaces is given up (the user confirmed when it had changes)
                    self.give_up_session_work();
                    self.files.lock = lock;
                    self.files.read_only = None;
                    // its folder (media, autosaves) is this session's now; Save asks where to put it
                    self.project_path = Some(candidate.dir.clone());
                    self.files.session_autosaves = autosave_paths(&candidate.dir);
                } else {
                    // the crash's autosaves are taken over: saving or discarding this work removes them
                    let offered: Vec<PathBuf> = self.files.recovery.iter().map(|c| c.path.clone()).collect();
                    self.files.session_autosaves.extend(offered);
                }
                // later edits (and autosaves) number after the saved, the recovered and every kept autosave
                let floor = self.project.revision.max(p.revision).max(rawdawg_project::persist::highest_autosave_revision(&candidate.dir));
                self.history = History::continuing_from(floor);
                self.history.mark_dirty();
                self.project = p;
                self.selected_channel = self.project.channels.first().map(|c| c.id);
                self.rebuild_mirrors();
                self.recompile();
                self.files.last_autosave = Some(Instant::now());
                self.files.last_autosave_note = None;
                self.status_line = if unsaved {
                    "Recovered the never-saved project; File › Save As… to keep it".to_owned()
                } else {
                    format!("Recovered the autosave from {}; save to keep it", ago(candidate.saved_unix_ms))
                };
            }
            Err(e) => self.status_line = format!("Could not recover: {e}"),
        }
        if unsaved {
            // other never-saved projects stay offered
            self.drop_offered(&candidate.dir);
        } else {
            self.files.recovery.clear();
        }
        self.files.recover_confirm = false;
    }

    /// Remove one folder's entries from the recovery offer.
    fn drop_offered(&mut self, dir: &Path) {
        let keep: Vec<bool> = self.files.recovery.iter().map(|c| c.dir != dir).collect();
        let mut i = 0;
        self.files.recovery.retain(|_| (keep[i], i += 1).0);
        if self.files.recovery_labels.len() == keep.len() {
            let mut j = 0;
            self.files.recovery_labels.retain(|_| (keep[j], j += 1).0);
        }
        self.files.recovery_selected = 0;
        self.files.recover_confirm = false;
    }

    /// Discard autosaves: of this project, the offered ones; of never-saved projects, the
    /// selected one only (its whole folder).
    pub(crate) fn discard_recovery(&mut self) {
        self.end_compare();
        if self.files.recovery_unsaved {
            let Some(c) = self.files.recovery.get(self.files.recovery_selected).cloned() else { return };
            // not if another window took it meanwhile
            if ProjectLock::read(&c.dir).is_none_or(|l| !rawdawg_project::lock::is_live(&l)) {
                self.remove_unsaved_folder(&c.dir);
            }
            self.drop_offered(&c.dir);
        } else {
            for c in std::mem::take(&mut self.files.recovery) {
                let _ = std::fs::remove_file(&c.path);
            }
        }
        self.files.recover_confirm = false;
    }

    /// New empty project (unsaved working folder); the current one must be clean or confirmed.
    pub(crate) fn new_project(&mut self) {
        self.send(EngineCommand::Panic { source: None });
        self.end_compare();
        // the user chose to lose the unsaved work: no crash offer for it later
        self.give_up_session_work();
        self.files.lock = None;
        self.files.read_only = None;
        self.project = Project::new_default("Untitled", serde_json::to_value(rawdawg_synthesizer::Patch::default()).unwrap_or(serde_json::Value::Null));
        self.history = History::new();
        self.project_path = None;
        self.selected_channel = self.project.channels.first().map(|c| c.id);
        self.files.recovery.clear();
        self.files.recovery_unsaved = false;
        self.files.last_autosave_note = None;
        self.rebuild_mirrors();
        self.recompile();
        self.seek(0);
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
            // an empty field: never the per-user folder, nor a path typed for something else
            self.files.path_input.clear();
            self.files.error = None;
            self.files.dialog = Some(FileDialog::SaveAs);
            self.files.focus_field = true;
            return;
        };
        let revision = self.history.revision();
        self.project.revision = revision;
        match rawdawg_project::save_project_dir(&path, &self.project) {
            Ok(()) => {
                self.history.mark_saved_at(revision);
                self.forget_session_autosaves();
                self.files.last_autosave_note = None;
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
        let unsaved_before = self.unsaved_folder();
        // a project still called "Untitled" (or after its old folder) takes the new folder's name
        let old_name = self.project.name.clone();
        let old_folder = src.as_ref().filter(|_| !self.is_never_saved()).and_then(|p| p.file_stem()).map(|s| s.to_string_lossy().into_owned());
        if let Some(stem) = dst.file_stem().map(|s| s.to_string_lossy().into_owned())
            && (old_name == "Untitled" || old_folder.as_deref() == Some(old_name.as_str()))
            && !stem.trim().is_empty()
        {
            self.project.name = stem;
        }
        match rawdawg_project::save_as(src.as_deref(), &dst, &self.project) {
            Ok(()) => {
                // saved for real now: this session's autosaves (in the old folder) are not a crash
                self.forget_session_autosaves();
                self.files.lock = ProjectLock::acquire(&dst, false).ok();
                self.files.read_only = None;
                self.files.last_autosave_note = None;
                // a never-saved project's per-user folder is done with (its media were copied)
                if let Some(old) = unsaved_before {
                    self.remove_unsaved_folder(&old);
                }
                self.project_path = Some(dst.clone());
                self.history.mark_saved_at(revision);
                self.prefs.remember_project(dst.clone());
                self.save_prefs();
                self.assets.sync(self.project_path.clone(), &self.project);
                self.files.error = None;
                self.files.dialog = None;
                self.status_line = format!("Saved as {}", dst.display());
            }
            Err(e) => {
                self.project.name = old_name;
                self.files.error = Some(e.to_string());
            }
        }
    }

    /// Every frame: autosave when dirty and due; handle a close request while dirty.
    pub(crate) fn poll_project_files(&mut self, ctx: &egui::Context) {
        // a never-saved project autosaves too (into its per-user folder); a copy open read-only
        // beside a running one does not write into that one's folder
        if self.autosave_enabled
            && self.files.read_only.is_none()
            && self.history.is_dirty()
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
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0);
        self.files.last_autosave_note = Some(match rawdawg_project::autosave(dir, &self.project) {
            Ok(p) => {
                // the ring prunes old files: keep the list to the ones still there
                self.files.session_autosaves.retain(|f| f.is_file());
                self.files.session_autosaves.push(p.clone());
                Ok((now, p))
            }
            Err(e) => Err(e.to_string()),
        });
    }

    /// The File menu's autosave line: short ("Autosaved 2 min ago"), the file (or the error) on
    /// hover, and whether it failed.
    pub(crate) fn autosave_summary(&self) -> Option<(String, String, bool)> {
        self.files.last_autosave_note.as_ref().map(|n| match n {
            Ok((at, path)) => (format!("Autosaved {}", ago(*at)), path.display().to_string(), false),
            Err(e) => ("Autosave failed (hover for why)".to_owned(), e.clone(), true),
        })
    }

    /// Rename the project (one undo step); the name proposes export file names and titles MIDI files.
    pub(crate) fn rename_project(&mut self, name: &str) {
        let name = name.trim();
        if !name.is_empty() && name != self.project.name {
            self.apply(Command::RenameProject { name: name.to_owned() });
        }
    }

    /// Close the frontmost dialog (Escape): prompts first, then file dialogs, Settings, the
    /// add-effect search and Export. Whether one was open.
    pub(crate) fn close_top_dialog(&mut self) -> bool {
        if self.files.open_prompt.take().is_some() {
            return true;
        }
        if self.files.close_prompt {
            self.files.close_prompt = false;
            return true;
        }
        if self.files.rename.take().is_some() {
            return true;
        }
        if self.files.dialog.take().is_some() {
            self.files.close_after_save = false;
            self.files.open_after_save = None;
            return true;
        }
        if self.show_settings {
            self.show_settings = false;
            return true;
        }
        // the add-effect search is a dialog only where it is drawn
        if self.mixer.add_open && self.editor_view == EditorView::Mixer {
            self.mixer.add_open = false;
            return true;
        }
        if self.export.open && !self.export.running() {
            self.export.open = false;
            return true;
        }
        false
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
    // a prompt's note belongs to that prompt
    if app.files.open_prompt.is_none() && !app.files.close_prompt {
        app.files.prompt_note = None;
    }
    // "Save and close" belongs to the Save As it opened: leaving that for another dialog (New,
    // Open) forgets it, or a later ordinary Save As would quit
    if !matches!(app.files.dialog, Some(FileDialog::SaveAs)) {
        app.files.close_after_save = false;
        app.files.open_after_save = None;
    }
    // ---- file dialog (dark, in-app: a path field plus the recent list)
    let just_opened = (app.files.dialog != app.files.shown_dialog).then_some(());
    app.files.shown_dialog = app.files.dialog;
    let rename_just_opened = app.files.rename.is_some() && !app.files.shown_rename;
    app.files.shown_rename = app.files.rename.is_some();
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
                    // no Enter here: it throws the unsaved changes away, so only a click does
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
                    let field = ui.add(egui::TextEdit::singleline(&mut app.files.path_input).hint_text("/path/to/Song.rawdawg").desired_width(f32::INFINITY));
                    if std::mem::take(&mut app.files.focus_field) {
                        field.request_focus();
                    }
                    // Enter in the field confirms (not on the frame the dialog opened); Escape cancels
                    let enter = just_opened.is_none() && ui.input(|i| i.key_pressed(egui::Key::Enter)) && (field.lost_focus() || field.has_focus());
                    ui.horizontal(|ui| {
                        if (ui.button(if kind == FileDialog::Open { "Open" } else { "Save as" }).clicked() || enter) && !app.files.path_input.trim().is_empty() {
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
    // ---- recovery offer (another of never-saved projects may follow once this one is answered)
    if app.files.recovery.is_empty() && std::mem::take(&mut app.files.unsaved_offer_pending) {
        app.offer_unsaved_recovery();
    }
    if !app.files.recovery.is_empty() {
        let mut keep_saved = false;
        let mut recover: Option<RecoveryCandidate> = None;
        let mut discard = false;
        let mut compare_index: Option<usize> = None;
        let mut pick: Option<usize> = None;
        let mut listen: Option<bool> = None;
        let unsaved = app.files.recovery_unsaved;
        let title = if unsaved { "Recover a project that was never saved?" } else { "Recover unsaved changes?" };
        egui::Window::new(title).collapsible(false).resizable(false).show(ctx, |ui| {
            let selected = app.files.recovery_selected.min(app.files.recovery.len() - 1);
            let c = app.files.recovery[selected].clone();
            let label = |i: usize, r: &RecoveryCandidate| match app.files.recovery_labels.get(i).filter(|_| unsaved) {
                Some(l) => format!("{l} · {}", ago(r.saved_unix_ms)),
                None => format!("{} (revision {})", ago(r.saved_unix_ms), r.revision),
            };
            ui.label(if unsaved {
                format!(
                    "rawDAWg did not close normally: {} never saved, last autosaved {}. Recover one (then Save As to keep it), leave them for the next start, or discard the chosen one.",
                    if app.files.recovery.len() == 1 { "a project was".to_owned() } else { format!("{} projects were", app.files.recovery.len()) },
                    ago(c.saved_unix_ms)
                )
            } else {
                format!(
                    "rawDAWg did not close normally: an autosave from {} has changes that are not in the saved project ({} autosave(s)). Compare them, recover it, keep the saved version, or discard the autosaves.",
                    ago(c.saved_unix_ms),
                    app.files.recovery.len()
                )
            });
            if app.files.recovery.len() > 1 || unsaved {
                egui::ComboBox::from_id_salt("recovery-candidate").width(420.0).selected_text(label(selected, &c)).show_ui(ui, |ui| {
                    for (i, r) in app.files.recovery.iter().enumerate() {
                        if ui.selectable_label(i == selected, label(i, r)).clicked() {
                            pick = Some(i);
                        }
                    }
                });
            }
            // comparing a never-saved project with the current one says nothing useful
            match &app.files.compare {
                None if unsaved => {}
                None => {
                    if ui.button("Compare").on_hover_text("What the autosave changes, and listen to either version before choosing").clicked() {
                        compare_index = Some(selected);
                    }
                }
                Some(cmp) => {
                    ui.separator();
                    if cmp.changes.is_empty() {
                        ui.label(egui::RichText::new("No difference from the current project.").color(theme::TEXT_SECONDARY));
                    } else {
                        ui.label(egui::RichText::new("The autosave, compared with the current project:").strong());
                        egui::ScrollArea::vertical().max_height(180.0).show(ui, |ui| {
                            for line in &cmp.changes {
                                ui.label(format!("• {line}"));
                            }
                        });
                    }
                    ui.horizontal(|ui| {
                        ui.label("Listen to");
                        if ui.selectable_label(!cmp.listen_autosave, "Saved").on_hover_text("The project as it is now (the saved version plus any edits since opening)").clicked() {
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
            if app.files.recover_confirm {
                ui.colored_label(theme::WARNING, "The current project has unsaved changes: recovering replaces them.");
            }
            ui.horizontal(|ui| {
                let recover_label = if app.files.recover_confirm { "Replace my changes and recover" } else { "Recover" };
                if ui.button(recover_label).clicked() {
                    // edits made while this offer was open are not replaced without asking
                    if app.history.is_dirty() && !app.files.recover_confirm {
                        app.files.recover_confirm = true;
                    } else {
                        recover = Some(c.clone());
                    }
                }
                if ui.button(if unsaved { "Not now (ask at the next start)" } else { "Keep saved version (keep autosaves)" }).clicked() {
                    keep_saved = true;
                }
                if ui.button(if unsaved { "Discard this project" } else { "Discard autosaves" }).clicked() {
                    discard = true;
                }
            });
        });
        if let Some(i) = pick {
            app.files.recovery_selected = i;
            app.files.recover_confirm = false;
            if app.files.compare.is_some() {
                app.compare_recovery(i);
            }
        }
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
            app.files.recover_confirm = false;
        } else if discard {
            app.discard_recovery();
        }
        if app.files.recovery.is_empty() {
            app.files.recovery_unsaved = false;
            app.files.recovery_labels.clear();
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
                    app.files.close_prompt = false;
                    if !app.history.is_dirty() {
                        app.files.closing = true;
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    } else if app.files.dialog == Some(FileDialog::SaveAs) {
                        // a never-saved project: close once Save As succeeds
                        app.files.close_after_save = true;
                    } else {
                        // not saved (read-only, or the save failed): stay, and say why
                        app.files.close_prompt = true;
                        app.files.prompt_note = Some(app.status_line.clone());
                    }
                }
                if ui.button("Discard and close").clicked() {
                    app.give_up_session_work();
                    app.history.mark_saved();
                    app.files.close_prompt = false;
                    app.files.closing = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
                if ui.button("Cancel").clicked() {
                    app.files.close_prompt = false;
                }
            });
            if let Some(n) = &app.files.prompt_note {
                ui.colored_label(theme::WARNING, n);
            }
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
                    } else {
                        // not saved (read-only, or the save failed): stay, and say why
                        app.files.open_prompt = Some(next.clone());
                        app.files.prompt_note = Some(app.status_line.clone());
                    }
                }
                // the current work goes only once the other project opened (see open_project_checked)
                if ui.button("Discard and open").clicked() && !app.open_project_checked(next.clone()) {
                    app.files.open_prompt = Some(next.clone());
                    app.files.prompt_note = Some(app.status_line.clone());
                }
                if ui.button("Cancel").clicked() {
                    app.files.open_prompt = None;
                }
            });
            if let Some(n) = &app.files.prompt_note {
                ui.colored_label(theme::WARNING, n);
            }
        });
    }
    // ---- rename the project
    if let Some(mut name) = app.files.rename.clone() {
        let mut open = true;
        let mut done = false;
        egui::Window::new("Rename project").open(&mut open).collapsible(false).resizable(false).show(ctx, |ui| {
            ui.label("The name proposes export file names and titles exported MIDI files.");
            let field = ui.add(egui::TextEdit::singleline(&mut name).desired_width(320.0));
            if std::mem::take(&mut app.files.focus_rename) {
                field.request_focus();
            }
            let enter = !rename_just_opened && ui.input(|i| i.key_pressed(egui::Key::Enter)) && (field.lost_focus() || field.has_focus());
            ui.horizontal(|ui| {
                if ui.button("Rename").clicked() || enter {
                    app.rename_project(&name);
                    done = true;
                }
                if ui.button("Cancel").clicked() {
                    done = true;
                }
            });
        });
        app.files.rename = if done || !open { None } else { Some(name) };
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
}

/// One line for an offered never-saved project: its name and what it holds.
fn summary(p: &Project) -> String {
    let notes: usize = p.patterns.iter().flat_map(|pat| pat.lanes.iter()).map(|l| l.notes.len()).sum();
    let plural = |n: usize, one: &str| format!("{n} {one}{}", if n == 1 { "" } else { "s" });
    format!("{} — {}, {}, {}", p.name, plural(p.channels.len(), "channel"), plural(notes, "note"), plural(p.clips.len(), "clip"))
}

/// Every autosave file in a project folder.
fn autosave_paths(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir.join(rawdawg_project::persist::AUTOSAVE_DIR))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect()
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
