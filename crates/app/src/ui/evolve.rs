//! Randomize / Chaos / Locks, Evolution (3×3 descendants, breed, history) and
//! XY morph panels. Candidates are real patches; selecting a card loads it live
//! into the instrument as a preview; "Use sound" commits one undo entry.

use super::App;
use crate::theme;
use rawdawg_dsp::processor::Processor;
use rawdawg_synthesizer::audition::{AuditionSpec, SoundFeatures, patch_hash, render_patch};
use rawdawg_synthesizer::morph;
use rawdawg_synthesizer::mutate::{self, MutationSpec};
use rawdawg_synthesizer::patch::Locks;
use rawdawg_synthesizer::{Patch, params};
use rawdawg_types::InstanceId;
use rawdawg_types::params::ParamCategory;
use std::collections::HashMap;

pub(crate) struct Candidate {
    pub patch: Patch,
    pub features: Option<SoundFeatures>,
    pub thumb: Vec<f32>,
}

#[derive(Default)]
pub(crate) struct Generation {
    pub parent: Option<Patch>,
    pub seed: u64,
    pub number: u32,
    pub cards: Vec<Candidate>,
}

#[derive(Default)]
pub(crate) struct EvolveState {
    pub chaos: f32,
    pub locks: Locks,
    pub show_locks: bool,
    pub show_evolve: bool,
    pub show_morph: bool,
    pub history: Vec<Generation>,
    pub cursor: usize,
    pub selected: Option<usize>,
    pub second: Option<usize>,
    /// State before a live preview started, restored on Cancel.
    pub preview_backup: Option<(InstanceId, Vec<u8>)>,
    pub favorites: Vec<Patch>,
    pub cache: HashMap<u64, Vec<f32>>,
    pub seed_counter: u64,
    pub morph_corners: [Option<Patch>; 4],
    pub morph_xy: (f32, f32),
    pub morph_recording: Vec<(f32, f32, f64)>,
    pub morph_last: Option<Patch>,
    /// Play previews about as loud as the sound before the preview (on by default).
    pub match_levels: bool,
    /// Gain the engine applies to the previewed channel while a variation plays (never
    /// saved, never part of the sound); `None` when nothing is level-matched.
    pub preview_trim_db: Option<f32>,
}

impl EvolveState {
    pub fn new() -> Self {
        Self { chaos: 35.0, locks: Locks::default(), seed_counter: 1, show_evolve: true, match_levels: true, ..Default::default() }
    }
}

fn audition() -> AuditionSpec {
    AuditionSpec { hold_seconds: 0.3, tail_seconds: 0.1, ..Default::default() }
}

fn thumbnail(buf: &[f32]) -> Vec<f32> {
    // 256-point envelope-ish thumbnail of the first 40 ms
    let n = buf.len().clamp(1, 1920);
    (0..256).map(|i| buf.get(i * n / 256).copied().unwrap_or(0.0)).collect()
}

fn make_candidate(st: &mut EvolveState, patch: Patch) -> Candidate {
    let h = patch_hash(&patch);
    let aud = audition();
    let buf = st.cache.entry(h).or_insert_with(|| render_patch(&patch, &aud).unwrap_or_default()).clone();
    let features = if buf.is_empty() { None } else { Some(SoundFeatures::of(&buf, aud.sample_rate)) };
    Candidate { patch, features, thumb: thumbnail(&buf) }
}

/// Largest correction a level-matched preview applies, either way.
pub(crate) const MAX_PREVIEW_TRIM_DB: f32 = 12.0;

/// The bounded gain that plays a sound with `candidate_rms` about as loud as one with
/// `reference_rms` (both from the same offline audition); 0 dB when either is silent.
pub(crate) fn match_gain_db(reference_rms: f32, candidate_rms: f32) -> f32 {
    if !(reference_rms > 1e-6 && candidate_rms > 1e-6) || !reference_rms.is_finite() || !candidate_rms.is_finite() {
        return 0.0;
    }
    (20.0 * (reference_rms / candidate_rms).log10()).clamp(-MAX_PREVIEW_TRIM_DB, MAX_PREVIEW_TRIM_DB)
}

/// Loudness of a patch's offline audition (cached by patch hash).
fn audition_rms(st: &mut EvolveState, patch: &Patch) -> Option<f32> {
    let aud = audition();
    let buf = st.cache.entry(patch_hash(patch)).or_insert_with(|| render_patch(patch, &aud).unwrap_or_default());
    (!buf.is_empty()).then(|| SoundFeatures::of(buf, aud.sample_rate).rms)
}

/// Work out the preview's level match: the previewed sound against the one before the preview.
fn refresh_trim(app: &mut App, instance: InstanceId) {
    let st = &mut app.synth_panel.evolve;
    st.preview_trim_db = None;
    if !st.match_levels {
        return;
    }
    let Some((backup_instance, before)) = st.preview_backup.as_ref() else { return };
    if *backup_instance != instance {
        return;
    }
    let Ok(reference) = Patch::from_json(before) else { return };
    let Some(current) = app.synth_mirrors.get(&instance).map(|m| m.patch().clone()) else { return };
    let st = &mut app.synth_panel.evolve;
    if let (Some(r), Some(c)) = (audition_rms(st, &reference), audition_rms(st, &current)) {
        st.preview_trim_db = Some(match_gain_db(r, c));
    }
}

fn next_seed(st: &mut EvolveState) -> u64 {
    st.seed_counter = st.seed_counter.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
    st.seed_counter
}

/// Load a patch live without committing (restores on cancel), level-matched to the sound
/// before the preview when "Match levels" is on.
pub(crate) fn preview(app: &mut App, instance: InstanceId, patch: &Patch) {
    if app.synth_panel.evolve.preview_backup.is_none() {
        let before = app.synth_mirrors[&instance].save_state();
        app.synth_panel.evolve.preview_backup = Some((instance, before));
    }
    let value = serde_json::to_value(patch).unwrap_or(serde_json::Value::Null);
    if let Some(m) = app.synth_mirrors.get_mut(&instance) {
        let _ = m.set_patch(patch.clone());
    }
    if let Some(ch) = app.project.channels.iter_mut().find(|c| c.instrument.id == instance) {
        ch.instrument.state = value;
    }
    refresh_trim(app, instance);
    app.recompile();
}

pub(crate) fn cancel_preview(app: &mut App) {
    app.synth_panel.evolve.preview_trim_db = None;
    if let Some((instance, before)) = app.synth_panel.evolve.preview_backup.take() {
        if let Ok(p) = Patch::from_json(&before) {
            let value = serde_json::to_value(&p).unwrap_or(serde_json::Value::Null);
            if let Some(m) = app.synth_mirrors.get_mut(&instance) {
                let _ = m.set_patch(p);
            }
            if let Some(ch) = app.project.channels.iter_mut().find(|c| c.instrument.id == instance) {
                ch.instrument.state = value;
            }
        }
        app.recompile();
    }
}

/// Commit the previewed patch as one undo entry. The level match ends: the sound plays at its
/// own level, as it will everywhere else.
pub(crate) fn use_sound(app: &mut App, instance: InstanceId, patch: &Patch) {
    app.synth_panel.evolve.preview_trim_db = None;
    if let Some((_, before)) = app.synth_panel.evolve.preview_backup.take() {
        // restore the project to the pre-preview state so the command's inverse is exact
        if let Ok(p) = Patch::from_json(&before)
            && let Some(ch) = app.project.channels.iter_mut().find(|c| c.instrument.id == instance)
        {
            ch.instrument.state = serde_json::to_value(&p).unwrap_or(serde_json::Value::Null);
        }
    }
    super::synth_panel::commit_patch(app, instance, patch.clone());
}

pub(crate) fn randomize_bar(app: &mut App, ui: &mut egui::Ui, instance: InstanceId) {
    ui.horizontal(|ui| {
        if ui
            .add(egui::Button::new(egui::RichText::new("Randomize sound").strong()).fill(theme::ACCENT.linear_multiply(0.35)))
            .on_hover_text("One click: a new, usable variation (undo restores exactly)")
            .clicked()
        {
            let parent = app.synth_mirrors[&instance].patch().clone();
            let seed = next_seed(&mut app.synth_panel.evolve);
            let spec = MutationSpec { seed, chaos: app.synth_panel.evolve.chaos, locks: app.synth_panel.evolve.locks.clone(), audition: Some(audition()) };
            let r = mutate::mutate(&parent, &spec);
            if r.chaos_used == 0.0 && app.synth_panel.evolve.chaos > 0.0 {
                app.status_line = "No valid variation found for these locks; try lowering Chaos".into();
            } else if r.patch != parent {
                super::synth_panel::commit_patch(app, instance, r.patch);
                app.status_line = format!("Randomized (chaos {:.0}, seed {seed}, {} attempt(s))", r.chaos_used, r.attempts);
            }
        }
        ui.add(egui::Slider::new(&mut app.synth_panel.evolve.chaos, 0.0..=100.0).text("Chaos").fixed_decimals(0));
        if ui.selectable_label(app.synth_panel.evolve.show_locks, "Locks").clicked() {
            app.synth_panel.evolve.show_locks = !app.synth_panel.evolve.show_locks;
        }
        if ui.selectable_label(app.synth_panel.evolve.show_evolve, "Evolve").clicked() {
            app.synth_panel.evolve.show_evolve = !app.synth_panel.evolve.show_evolve;
        }
        if ui.selectable_label(app.synth_panel.evolve.show_morph, "XY Morph").clicked() {
            app.synth_panel.evolve.show_morph = !app.synth_panel.evolve.show_morph;
        }
    });
    if app.synth_panel.evolve.show_locks {
        locks_panel(app, ui);
    }
    if app.synth_panel.evolve.show_evolve {
        evolve_panel(app, ui, instance);
    }
    if app.synth_panel.evolve.show_morph {
        morph_panel(app, ui, instance);
    }
}

fn locks_panel(app: &mut App, ui: &mut egui::Ui) {
    ui.group(|ui| {
        ui.label(egui::RichText::new("Locked (randomization leaves these alone)").strong());
        ui.horizontal_wrapped(|ui| {
            for (c, label) in [
                (ParamCategory::Wave, "Wave"),
                (ParamCategory::Pitch, "Pitch"),
                (ParamCategory::Envelope, "Envelope"),
                (ParamCategory::Filter, "Filter"),
                (ParamCategory::Modulation, "Modulation"),
                (ParamCategory::Effects, "Effects"),
                (ParamCategory::Topology, "Topology"),
                (ParamCategory::Mix, "Mix"),
                (ParamCategory::Safety, "Output safety"),
                (ParamCategory::Routing, "Routing"),
            ] {
                let mut on = app.synth_panel.evolve.locks.is_category_locked(c);
                if ui.toggle_value(&mut on, label).changed() {
                    app.synth_panel.evolve.locks.set_category(c, on);
                }
            }
            if ui.button("Defaults").clicked() {
                app.synth_panel.evolve.locks = Locks::default();
            }
        });
        let locked: Vec<u32> = app.synth_panel.evolve.locks.params.clone();
        if !locked.is_empty() {
            ui.horizontal_wrapped(|ui| {
                ui.label("Parameters:");
                for id in locked {
                    let name = params::DESCRIPTORS.iter().find(|d| d.id.0 == id).map(|d| d.name).unwrap_or("?");
                    if ui.small_button(format!("{name} ✕")).clicked() {
                        app.synth_panel.evolve.locks.set_param(id, false);
                    }
                }
            });
        }
        ui.label(egui::RichText::new("Right-click any parameter in Advanced to lock it individually.").color(theme::TEXT_SECONDARY));
    });
}

fn evolve_panel(app: &mut App, ui: &mut egui::Ui, instance: InstanceId) {
    ui.group(|ui| {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Evolution").strong());
            if ui.button("New generation from current").on_hover_text("Nine real descendants of the current sound").clicked() {
                let parent = app.synth_mirrors[&instance].patch().clone();
                let seed = next_seed(&mut app.synth_panel.evolve);
                spawn_generation(app, parent, seed);
            }
            let has_sel = app.synth_panel.evolve.selected.is_some();
            if ui.add_enabled(has_sel, egui::Button::new("Evolve selected")).clicked()
                && let Some(i) = app.synth_panel.evolve.selected
                && let Some(g) = app.synth_panel.evolve.history.get(app.synth_panel.evolve.cursor)
                && let Some(c) = g.cards.get(i)
            {
                let parent = c.patch.clone();
                let seed = next_seed(&mut app.synth_panel.evolve);
                spawn_generation(app, parent, seed);
            }
            let two = app.synth_panel.evolve.selected.is_some() && app.synth_panel.evolve.second.is_some();
            if ui.add_enabled(two, egui::Button::new("Breed selected two")).on_hover_text("Ctrl-click a second card to pick the other parent").clicked()
                && let (Some(a), Some(b)) = (app.synth_panel.evolve.selected, app.synth_panel.evolve.second)
                && let Some(g) = app.synth_panel.evolve.history.get(app.synth_panel.evolve.cursor)
                && let (Some(ca), Some(cb)) = (g.cards.get(a), g.cards.get(b))
            {
                let (pa, pb) = (ca.patch.clone(), cb.patch.clone());
                let seed = next_seed(&mut app.synth_panel.evolve);
                let locks = app.synth_panel.evolve.locks.clone();
                let child = mutate::breed(&pa, &pb, seed, &locks);
                spawn_generation(app, child, seed);
            }
            let st = &mut app.synth_panel.evolve;
            if ui.add_enabled(st.cursor > 0, egui::Button::new("◀ Previous")).clicked() {
                st.cursor -= 1;
                st.selected = None;
                st.second = None;
            }
            if ui.add_enabled(st.cursor + 1 < st.history.len(), egui::Button::new("Next ▶")).clicked() {
                st.cursor += 1;
                st.selected = None;
                st.second = None;
            }
            if let Some(g) = st.history.get(st.cursor) {
                ui.label(egui::RichText::new(format!("generation {} · seed {}", g.number, g.seed)).color(theme::TEXT_SECONDARY));
            }
        });
        let Some(gen_idx) = app.synth_panel.evolve.history.get(app.synth_panel.evolve.cursor).map(|_| app.synth_panel.evolve.cursor) else {
            ui.label(
                egui::RichText::new("Click “New generation from current” to grow nine variations. Clicking a card plays it; hold keys to hear it.").color(theme::TEXT_SECONDARY),
            );
            return;
        };
        let mut clicked: Option<(usize, bool)> = None;
        let card_w = (ui.available_width() / 3.0 - 8.0).max(120.0);
        egui::Grid::new("evolve-grid").num_columns(3).spacing([6.0, 6.0]).show(ui, |ui| {
            let st = &app.synth_panel.evolve;
            let g = &st.history[gen_idx];
            for (i, c) in g.cards.iter().enumerate() {
                let selected = st.selected == Some(i);
                let second = st.second == Some(i);
                let (rect, resp) = ui.allocate_exact_size(egui::vec2(card_w, 64.0), egui::Sense::click());
                let painter = ui.painter_at(rect);
                let fill = if selected {
                    theme::ACCENT.linear_multiply(0.25)
                } else if second {
                    theme::WARNING.linear_multiply(0.25)
                } else {
                    theme::RAISED
                };
                painter.rect_filled(rect, 4.0, fill);
                if selected || second {
                    painter.rect_stroke(rect, 4.0, egui::Stroke::new(1.5, if selected { theme::ACCENT } else { theme::WARNING }), egui::StrokeKind::Inside);
                }
                let mid = rect.center().y + 6.0;
                let pts: Vec<egui::Pos2> =
                    c.thumb.iter().enumerate().map(|(k, v)| egui::pos2(rect.min.x + 4.0 + (rect.width() - 8.0) * k as f32 / 255.0, mid - v.clamp(-1.0, 1.0) * 22.0)).collect();
                painter.add(egui::Shape::line(pts, egui::Stroke::new(1.0, theme::TEXT)));
                let tags = c.features.map(|f| f.tags().join(" · ")).unwrap_or_else(|| "no preview".into());
                painter.text(rect.min + egui::vec2(6.0, 4.0), egui::Align2::LEFT_TOP, format!("{} — {}", i + 1, tags), egui::FontId::proportional(11.0), theme::TEXT_SECONDARY);
                if resp.clicked() {
                    clicked = Some((i, ui.input(|inp| inp.modifiers.ctrl)));
                }
                if (i + 1) % 3 == 0 {
                    ui.end_row();
                }
            }
        });
        if let Some((i, ctrl)) = clicked {
            if ctrl && app.synth_panel.evolve.selected.is_some_and(|s| s != i) {
                app.synth_panel.evolve.second = Some(i);
            } else {
                app.synth_panel.evolve.selected = Some(i);
                app.synth_panel.evolve.second = None;
                let patch = app.synth_panel.evolve.history[gen_idx].cards[i].patch.clone();
                preview(app, instance, &patch);
                app.status_line = format!("Previewing variation {} — play keys to hear it; Use sound to keep it", i + 1);
            }
        }
        ui.horizontal(|ui| {
            let sel = app.synth_panel.evolve.selected;
            if ui.add_enabled(sel.is_some(), egui::Button::new("Use sound")).clicked()
                && let Some(i) = sel
            {
                let patch = app.synth_panel.evolve.history[gen_idx].cards[i].patch.clone();
                use_sound(app, instance, &patch);
                app.status_line = format!("Kept variation {}", i + 1);
            }
            let parent = app.synth_panel.evolve.history.get(gen_idx).and_then(|g| g.parent.clone());
            if ui.add_enabled(parent.is_some(), egui::Button::new("Compare parent")).on_hover_text("Play the sound these variations came from").clicked()
                && let Some(p) = parent
            {
                preview(app, instance, &p);
                app.synth_panel.evolve.selected = None;
            }
            if ui.add_enabled(app.synth_panel.evolve.preview_backup.is_some(), egui::Button::new("Cancel preview")).clicked() {
                cancel_preview(app);
                app.synth_panel.evolve.selected = None;
            }
            let mut matched = app.synth_panel.evolve.match_levels;
            if ui
                .checkbox(&mut matched, "Match levels")
                .on_hover_text(
                    "Play each variation about as loud as the sound you started from (at most ±12 dB), so louder never just sounds better. The sound itself keeps its own level.",
                )
                .changed()
            {
                app.synth_panel.evolve.match_levels = matched;
                if app.synth_panel.evolve.preview_backup.is_some() {
                    refresh_trim(app, instance);
                    app.recompile();
                }
            }
            if let Some(t) = app.synth_panel.evolve.preview_trim_db
                && app.synth_panel.evolve.preview_backup.is_some()
            {
                ui.label(egui::RichText::new(format!("preview {t:+.1} dB")).small().color(theme::TEXT_SECONDARY))
                    .on_hover_text("Level match for listening only: Use sound keeps the variation at its own level");
            }
            if ui.add_enabled(sel.is_some(), egui::Button::new("★ Favorite")).clicked()
                && let Some(i) = sel
            {
                let patch = app.synth_panel.evolve.history[gen_idx].cards[i].patch.clone();
                app.synth_panel.evolve.favorites.push(patch);
            }
            let favs = app.synth_panel.evolve.favorites.len();
            if favs > 0 {
                ui.label(egui::RichText::new(format!("{favs} favorite(s)")).color(theme::TEXT_SECONDARY));
            }
        });
    });
}

fn spawn_generation(app: &mut App, parent: Patch, seed: u64) {
    let st = &mut app.synth_panel.evolve;
    let number = st.history.get(st.cursor).map(|g| g.number + 1).unwrap_or(1);
    let chaos = st.chaos.max(5.0);
    let locks = st.locks.clone();
    let mut cards = Vec::with_capacity(9);
    for i in 0..9u32 {
        let r = mutate::evolve(&parent, seed, number, i, chaos, &locks, Some(audition()));
        cards.push(make_candidate(st, r.patch));
    }
    st.history.truncate(st.cursor + 1);
    st.history.push(Generation { parent: Some(parent), seed, number, cards });
    st.cursor = st.history.len() - 1;
    st.selected = None;
    st.second = None;
    if st.history.len() > 32 {
        st.history.remove(0);
        st.cursor = st.history.len() - 1;
    }
    app.status_line = format!("Generation {number}: nine variations ready");
}

fn morph_panel(app: &mut App, ui: &mut egui::Ui, instance: InstanceId) {
    ui.group(|ui| {
        ui.label(egui::RichText::new("XY Morph — four corner sounds").strong());
        ui.horizontal(|ui| {
            for (i, label) in ["A (top-left)", "B (top-right)", "C (bottom-left)", "D (bottom-right)"].iter().enumerate() {
                let has = app.synth_panel.evolve.morph_corners[i].is_some();
                let text = if has { format!("{label}: set") } else { format!("{label}: use current") };
                if ui.button(text).on_hover_text("Stores the current sound in this corner").clicked() {
                    app.synth_panel.evolve.morph_corners[i] = Some(app.synth_mirrors[&instance].patch().clone());
                }
            }
        });
        let corners: Vec<Option<Patch>> = app.synth_panel.evolve.morph_corners.to_vec();
        if corners.iter().any(|c| c.is_none()) {
            ui.label(egui::RichText::new("Set all four corners (Randomize between them for variety).").color(theme::TEXT_SECONDARY));
            return;
        }
        let refs: [&Patch; 4] = std::array::from_fn(|i| corners[i].as_ref().expect("checked"));
        if !morph::compatible(&refs) {
            ui.label(
                egui::RichText::new("These corners have different structures (layer kinds or counts), so they cannot be blended into one sound. A morph rack plays all four and crossfades them instead.")
                    .color(theme::TEXT_SECONDARY),
            );
            if ui.button("Crossfade them in a morph rack").on_hover_text("Replaces this channel's instrument with a morph rack holding the four corners (undoable); X and Y can be automated").clicked() {
                make_morph_rack(app, instance, refs);
            }
            return;
        }
        let size = egui::vec2(220.0, 220.0);
        let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click_and_drag());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 4.0, theme::BACKGROUND);
        let (x, y) = app.synth_panel.evolve.morph_xy;
        let p = egui::pos2(rect.min.x + x * rect.width(), rect.min.y + y * rect.height());
        painter.circle_filled(p, 6.0, theme::ACCENT);
        for (i, corner) in ["A", "B", "C", "D"].iter().enumerate() {
            let pos = [rect.left_top(), rect.right_top(), rect.left_bottom(), rect.right_bottom()][i];
            let anchor = [egui::Align2::LEFT_TOP, egui::Align2::RIGHT_TOP, egui::Align2::LEFT_BOTTOM, egui::Align2::RIGHT_BOTTOM][i];
            painter.text(pos, anchor, *corner, egui::FontId::proportional(12.0), theme::TEXT_SECONDARY);
        }
        if (resp.dragged() || resp.clicked())
            && let Some(pos) = resp.interact_pointer_pos()
        {
            let nx = ((pos.x - rect.min.x) / rect.width()).clamp(0.0, 1.0);
            let ny = ((pos.y - rect.min.y) / rect.height()).clamp(0.0, 1.0);
            app.synth_panel.evolve.morph_xy = (nx, ny);
            if let Some(m) = morph::morph(&refs, nx, ny) {
                // live: the structure is compatible, so parameters go through Param events and
                // the waveforms through the table path (no processor rebuild)
                apply_morph_live(app, instance, &m);
                if app.synth_panel.evolve.preview_backup.is_none() {
                    app.synth_panel.evolve.preview_backup = Some((instance, app.synth_mirrors[&instance].save_state()));
                }
                app.synth_panel.evolve.morph_last = Some(m);
                let t = ui.input(|i| i.time);
                app.synth_panel.evolve.morph_recording.push((nx, ny, t));
            }
        }
        ui.horizontal(|ui| {
            if ui.button("Freeze as current sound").on_hover_text("Commits the morphed sound (one undo entry)").clicked()
                && let Some(m) = app.synth_panel.evolve.morph_last.clone()
            {
                use_sound(app, instance, &m);
            }
            if ui
                .button("Morph rack")
                .on_hover_text("Keep all four corners in the channel as a morph rack: it crossfades them, and its X and Y can be automated or MIDI-learned (undoable)")
                .clicked()
            {
                make_morph_rack(app, instance, refs);
            }
            if ui.button("Clear corners").clicked() {
                app.synth_panel.evolve.morph_corners = [None, None, None, None];
                app.synth_panel.evolve.morph_recording.clear();
            }
            let n = app.synth_panel.evolve.morph_recording.len();
            ui.label(egui::RichText::new(format!("x {:.2} · y {:.2} · {n} gesture points recorded", x, y)).color(theme::TEXT_SECONDARY));
            if n > 1 && ui.button("Write gesture as automation").on_hover_text("Bakes the recorded pad movement into automation curves for every parameter it changed (Automation track, Song mode)").clicked() {
                let gesture = app.synth_panel.evolve.morph_recording.clone();
                let written = super::automation::bake_morph_gesture(app, instance, refs, &gesture);
                app.status_line = if written == 0 { "The gesture changed no continuous parameter".into() } else { format!("Wrote {written} automation curve(s) from the morph gesture") };
            }
        });
    });
}

/// Replace the synth on `instance`'s channel with a morph rack holding the four corners at the
/// pad's current position (one undo step). Refused with a message when the corners together
/// exceed one synth's oscillator budget.
pub(crate) fn make_morph_rack(app: &mut App, instance: InstanceId, corners: [&Patch; 4]) {
    use rawdawg_synthesizer::morph_rack::MorphRackState;
    let Some(channel_id) = app.project.channels.iter().find(|c| c.instrument.id == instance).map(|c| c.id) else { return };
    let (x, y) = app.synth_panel.evolve.morph_xy;
    let state = MorphRackState::new(corners.map(Clone::clone), x, y);
    if let Err(e) = state.validate() {
        app.status_line = format!("No morph rack: {e}");
        return;
    }
    // a live preview ends here; the rack starts from the saved sound's corners
    if app.synth_panel.evolve.preview_backup.is_some() {
        cancel_preview(app);
    }
    let value = serde_json::to_value(&state).unwrap_or(serde_json::Value::Null);
    let rack = rawdawg_project::Instance::native(rawdawg_types::ids::native_types::MORPH, value);
    app.apply(rawdawg_project::Command::SetInstrument { channel_id, instance: rack });
    app.status_line = "Morph rack: drag the pad or automate X and Y (Undo goes back to the synth)".into();
}

/// Apply a compatible morph result live: parameters as events, waveforms via tables.
fn apply_morph_live(app: &mut App, instance: InstanceId, m: &Patch) {
    // the morph is heard at its own level: a variation's level match ends here
    if app.synth_panel.evolve.preview_trim_db.take().is_some() {
        app.recompile();
    }
    use rawdawg_types::events::{EventKind, EventSource, TimedEvent};
    let Some(ch) = app.project.channels.iter().find(|c| c.instrument.id == instance).map(|c| c.id) else { return };
    let mut cycles: Vec<(usize, Vec<f32>)> = Vec::new();
    {
        let Some(mirror) = app.synth_mirrors.get_mut(&instance) else { return };
        let current = mirror.patch().clone();
        for d in params::DESCRIPTORS.iter() {
            if d.flags.read_only {
                continue;
            }
            let target = rawdawg_synthesizer::Synth::new(m.clone()).param(d.id);
            if let Some(v) = target
                && mirror.param(d.id).is_some_and(|c| (c - v).abs() > 1e-6)
            {
                mirror.set_param(d.id, v);
                let _ = app_send_param(&mut app.audio, ch, d.id, v);
            }
        }
        for (li, layer) in m.layers.iter().enumerate() {
            let changed = current.layers.get(li).map(|l| l.generator != layer.generator).unwrap_or(true);
            if changed {
                match &layer.generator {
                    rawdawg_synthesizer::patch::Generator::Wavetable { cycle, frames, .. } => cycles.push((li, frames.first().cloned().unwrap_or_else(|| cycle.clone()))),
                    rawdawg_synthesizer::patch::Generator::Granular { source, .. } => cycles.push((li, source.clone())),
                    _ => {}
                }
            }
        }
        let _ = mirror.set_patch(m.clone());
    }
    for (li, cycle) in cycles {
        let table = Box::new(app.synth_panel.waveform_builder().build(&cycle, true));
        if let Some(s) = app.table_senders.get_mut(&instance) {
            let _ = s.send(li, 0, table);
        }
    }
    let _ = (EventKind::Reset, EventSource::Mouse, TimedEvent::new(0, EventSource::Mouse, 0, EventKind::Reset));
}

fn app_send_param(audio: &mut super::AudioState, channel: rawdawg_types::ChannelId, id: rawdawg_types::ParamId, value: f32) -> bool {
    use rawdawg_types::events::{EventKind, EventSource, TimedEvent};
    if let super::AudioState::Running(sys) = audio {
        return sys.handle.send(rawdawg_audio_engine::EngineCommand::Live { channel, event: TimedEvent::new(0, EventSource::Mouse, 0, EventKind::Param { id, value }) }).is_ok();
    }
    false
}
