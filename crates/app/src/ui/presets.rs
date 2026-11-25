//! Preset browser (SYN-08): factory presets by character, user presets in the
//! per-user data dir, audition on click (live preview), save / rename /
//! favorite, import/export as `.rawdawgpreset`.

use super::App;
use crate::theme;
use rawdawg_dsp::processor::Processor;
use rawdawg_synthesizer::Patch;
use rawdawg_synthesizer::presets::{Character, factory_presets};
use rawdawg_types::InstanceId;
use std::path::PathBuf;

pub(crate) struct UserPreset {
    pub path: PathBuf,
    pub name: String,
    pub character: String,
    pub patch: Patch,
}

#[derive(Default)]
pub(crate) struct PresetBrowserState {
    pub search: String,
    pub character: Option<Character>,
    pub favorites_only: bool,
    pub user: Vec<UserPreset>,
    pub scanned: bool,
    pub save_name: String,
    pub save_character: usize,
    pub show_save: bool,
    pub import_path: String,
    pub export_path: String,
    pub preview_backup: Option<(InstanceId, Vec<u8>)>,
}

pub(crate) fn user_preset_dir() -> Option<PathBuf> {
    directories::ProjectDirs::from("", "", rawdawg_types::PACKAGE_NAME).map(|d| d.data_dir().join("presets"))
}

fn scan_user_presets(st: &mut PresetBrowserState) {
    st.user.clear();
    st.scanned = true;
    let Some(dir) = user_preset_dir() else { return };
    let Ok(rd) = std::fs::read_dir(&dir) else { return };
    let mut entries: Vec<PathBuf> = rd.filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.extension().and_then(|e| e.to_str()).is_some_and(|e| e == "rawdawgpreset")).collect();
    entries.sort();
    for path in entries {
        match rawdawg_project::preset::read_preset(&path) {
            Ok(p) if p.manifest.type_id == rawdawg_synthesizer::TYPE_ID => {
                if let Ok(patch) = Patch::from_json(&serde_json::to_vec(&p.manifest.state).unwrap_or_default()) {
                    st.user.push(UserPreset { path, name: p.manifest.name.clone(), character: p.manifest.tags.first().cloned().unwrap_or_default(), patch });
                }
            }
            Ok(_) => {}
            Err(e) => log::warn!("skipping preset {}: {e}", path.display()),
        }
    }
}

fn preview(app: &mut App, instance: InstanceId, patch: &Patch) {
    if app.synth_panel.presets.preview_backup.is_none() {
        app.synth_panel.presets.preview_backup = Some((instance, app.synth_mirrors[&instance].save_state()));
    }
    let value = serde_json::to_value(patch).unwrap_or(serde_json::Value::Null);
    if let Some(m) = app.synth_mirrors.get_mut(&instance) {
        let _ = m.set_patch(patch.clone());
    }
    if let Some(ch) = app.project.channels.iter_mut().find(|c| c.instrument.id == instance) {
        ch.instrument.state = value;
    }
    app.recompile();
    app.status_line = format!("Previewing “{}” — play to hear it; Load keeps it (undoable)", patch.name);
}

fn load(app: &mut App, instance: InstanceId, patch: &Patch) {
    if let Some((_, before)) = app.synth_panel.presets.preview_backup.take()
        && let Ok(p) = Patch::from_json(&before)
        && let Some(ch) = app.project.channels.iter_mut().find(|c| c.instrument.id == instance)
    {
        ch.instrument.state = serde_json::to_value(&p).unwrap_or(serde_json::Value::Null);
    }
    super::synth_panel::commit_patch(app, instance, patch.clone());
    app.status_line = format!("Loaded “{}”", patch.name);
}

fn cancel_preview(app: &mut App) {
    if let Some((instance, before)) = app.synth_panel.presets.preview_backup.take()
        && let Ok(p) = Patch::from_json(&before)
    {
        if let Some(m) = app.synth_mirrors.get_mut(&instance) {
            let _ = m.set_patch(p.clone());
        }
        if let Some(ch) = app.project.channels.iter_mut().find(|c| c.instrument.id == instance) {
            ch.instrument.state = serde_json::to_value(&p).unwrap_or(serde_json::Value::Null);
        }
        app.recompile();
    }
}

fn write_user_preset(patch: &Patch, name: &str, character: &str) -> Result<PathBuf, String> {
    let dir = user_preset_dir().ok_or("no user data directory")?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let safe: String = name.chars().map(|c| if c.is_alphanumeric() || c == '-' || c == '_' || c == ' ' { c } else { '_' }).collect();
    let mut path = dir.join(format!("{}.rawdawgpreset", safe.trim()));
    let mut n = 2;
    while path.exists() {
        path = dir.join(format!("{} {n}.rawdawgpreset", safe.trim()));
        n += 1;
    }
    let mut p = patch.clone();
    p.name = name.to_owned();
    let manifest = rawdawg_project::preset::PresetManifest::new_synth(name, &p, &[character.to_owned()]);
    rawdawg_project::preset::write_preset(&path, &manifest, &[]).map_err(|e| e.to_string())?;
    Ok(path)
}

pub(crate) fn show(app: &mut App, ui: &mut egui::Ui, instance: InstanceId) {
    if !app.synth_panel.presets.scanned {
        scan_user_presets(&mut app.synth_panel.presets);
    }
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Presets").strong());
        ui.add(egui::TextEdit::singleline(&mut app.synth_panel.presets.search).hint_text("search").desired_width(120.0));
        let st = &mut app.synth_panel.presets;
        egui::ComboBox::from_id_salt("preset-char").selected_text(st.character.map(|c| c.label()).unwrap_or("All")).show_ui(ui, |ui| {
            ui.selectable_value(&mut st.character, None, "All");
            for c in Character::ALL {
                ui.selectable_value(&mut st.character, Some(c), c.label());
            }
        });
        ui.checkbox(&mut st.favorites_only, "★ only");
        if ui.button("Save as preset…").clicked() {
            st.show_save = !st.show_save;
            st.save_name = app.synth_mirrors[&instance].patch().name.clone();
        }
        if ui.button("Rescan").clicked() {
            scan_user_presets(st);
        }
    });
    if app.synth_panel.presets.show_save {
        ui.horizontal(|ui| {
            let st = &mut app.synth_panel.presets;
            ui.label("Name");
            ui.text_edit_singleline(&mut st.save_name);
            egui::ComboBox::from_id_salt("preset-save-char").selected_text(Character::ALL[st.save_character].label()).show_ui(ui, |ui| {
                for (i, c) in Character::ALL.iter().enumerate() {
                    ui.selectable_value(&mut st.save_character, i, c.label());
                }
            });
            if ui.button("Save").clicked() && !st.save_name.trim().is_empty() {
                let patch = app.synth_mirrors[&instance].patch().clone();
                let name = st.save_name.clone();
                let ch = Character::ALL[st.save_character].key().to_owned();
                match write_user_preset(&patch, &name, &ch) {
                    Ok(path) => {
                        app.status_line = format!("Saved preset to {}", path.display());
                        st.show_save = false;
                        scan_user_presets(st);
                    }
                    Err(e) => app.status_line = format!("Could not save preset: {e}"),
                }
            }
        });
        ui.horizontal(|ui| {
            let st = &mut app.synth_panel.presets;
            ui.label("Export to");
            ui.add(egui::TextEdit::singleline(&mut st.export_path).hint_text("/path/My Sound.rawdawgpreset").desired_width(260.0));
            if ui.button("Export").clicked() && !st.export_path.trim().is_empty() {
                let patch = app.synth_mirrors[&instance].patch().clone();
                let manifest = rawdawg_project::preset::PresetManifest::new_synth(&patch.name, &patch, &[]);
                match rawdawg_project::preset::write_preset(std::path::Path::new(st.export_path.trim()), &manifest, &[]) {
                    Ok(()) => app.status_line = format!("Exported {}", st.export_path),
                    Err(e) => app.status_line = format!("Export failed: {e}"),
                }
            }
            ui.label("Import");
            ui.add(egui::TextEdit::singleline(&mut st.import_path).hint_text("/path/Sound.rawdawgpreset").desired_width(260.0));
            if ui.button("Import").clicked() && !st.import_path.trim().is_empty() {
                let path = PathBuf::from(st.import_path.trim());
                match rawdawg_project::preset::read_preset(&path)
                    .map_err(|e| e.to_string())
                    .and_then(|p| Patch::from_json(&serde_json::to_vec(&p.manifest.state).unwrap_or_default()))
                {
                    Ok(patch) => {
                        load(app, instance, &patch);
                    }
                    Err(e) => app.status_line = format!("Import failed: {e}"),
                }
            }
        });
    }
    let search = app.synth_panel.presets.search.to_lowercase();
    let character = app.synth_panel.presets.character;
    let favorites_only = app.synth_panel.presets.favorites_only;
    let fav_names: Vec<String> = app.synth_panel.evolve.favorites.iter().map(|p| p.name.clone()).collect();
    let mut to_preview: Option<Patch> = None;
    let mut to_load: Option<Patch> = None;
    let mut to_fav: Option<Patch> = None;
    egui::ScrollArea::horizontal().id_salt("preset-list").max_height(96.0).show(ui, |ui| {
        ui.horizontal_wrapped(|ui| {
            for fp in factory_presets() {
                if character.is_some_and(|c| c != fp.character)
                    || (!search.is_empty() && !fp.name.to_lowercase().contains(&search) && !fp.description.to_lowercase().contains(&search))
                {
                    continue;
                }
                let fav = fav_names.iter().any(|n| n == fp.name);
                if favorites_only && !fav {
                    continue;
                }
                let label = format!("{}{}", if fav { "★ " } else { "" }, fp.name);
                let r = ui.button(label).on_hover_text(format!("{} — {}\nClick: preview · Double-click: load · Right-click: favorite", fp.character.label(), fp.description));
                if r.double_clicked() {
                    to_load = Some(fp.patch.clone());
                } else if r.clicked() {
                    to_preview = Some(fp.patch.clone());
                }
                if r.secondary_clicked() {
                    to_fav = Some(fp.patch.clone());
                }
            }
            for up in &app.synth_panel.presets.user {
                if !search.is_empty() && !up.name.to_lowercase().contains(&search) {
                    continue;
                }
                if character.is_some_and(|c| c.key() != up.character) {
                    continue;
                }
                let r = ui.button(format!("👤 {}", up.name)).on_hover_text(format!("User preset {}\nClick: preview · Double-click: load", up.path.display()));
                if r.double_clicked() {
                    to_load = Some(up.patch.clone());
                } else if r.clicked() {
                    to_preview = Some(up.patch.clone());
                }
            }
        });
    });
    if let Some(p) = to_fav {
        let favs = &mut app.synth_panel.evolve.favorites;
        if let Some(i) = favs.iter().position(|f| f.name == p.name) {
            favs.remove(i);
        } else {
            favs.push(p);
        }
    }
    if let Some(p) = to_load {
        load(app, instance, &p);
    } else if let Some(p) = to_preview {
        preview(app, instance, &p);
    }
    if app.synth_panel.presets.preview_backup.is_some() {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Previewing a preset").color(theme::WARNING));
            if ui.button("Load").clicked() {
                let patch = app.synth_mirrors[&instance].patch().clone();
                load(app, instance, &patch);
            }
            if ui.button("Cancel").clicked() {
                cancel_preview(app);
            }
        });
    }
}
