//! The exported plugin catalogue: the flagship synth plus every listed effect family.

use rawdawg_effects::EffectInfo;

/// Stable external CLAP id of the flagship synth.
pub const SYNTH_CLAP_ID: &str = "rawdawg.synth";
/// Prefix of every exported effect id (`rawdawg.fx.reverb`, …).
pub const FX_CLAP_ID_PREFIX: &str = "rawdawg.fx.";
pub const NATIVE_FX_PREFIX: &str = "native.fx.";
pub const VENDOR: &str = "rawDAWg";
pub const URL: &str = "https://github.com/leonardo-aa/rawDAWg";

/// Stable VST3 class-id slots per plugin slug. **Append only, never renumber**: a slot
/// becomes the plugin's processor/controller class ids (`vst3_class_ids`) and must stay
/// identical across releases so hosts keep recognising saved projects.
pub const VST3_SLOTS: &[(&str, u32)] = &[
    ("synth", 1),
    ("eq", 2),
    ("compressor", 3),
    ("limiter", 4),
    ("gate", 5),
    ("distortion", 6),
    ("multiband", 7),
    ("delay", 8),
    ("chorus", 9),
    ("flanger", 10),
    ("phaser", 11),
    ("pitch", 12),
    ("granular", 13),
    ("spectral", 14),
    ("reverb", 15),
    ("convolution", 16),
    ("stereo", 17),
];

/// Four 32-bit words of a VST3 class id (`vst3::uid(a, b, c, d)` on the adapter side).
pub type Vst3Uid = (u32, u32, u32, u32);

/// Processor and controller class ids for a slot (recorded scheme: vendor word
/// `0x7A57D4A6`, kind word `0x1000_0000 | slot` for processors and `0x2000_0000 | slot`
/// for controllers, product word `0xB0A53E11`, tail `0x9F2C_0000 | 2·slot(+1)`).
pub fn vst3_class_ids(slot: u32) -> (Vst3Uid, Vst3Uid) {
    ((0x7A57_D4A6, 0x1000_0000 | slot, 0xB0A5_3E11, 0x9F2C_0000 | (slot * 2)), (0x7A57_D4A6, 0x2000_0000 | slot, 0xB0A5_3E11, 0x9F2C_0000 | (slot * 2 + 1)))
}

/// What an exported plugin wraps.
#[derive(Clone, Copy, Debug)]
pub enum Kind {
    Synth,
    Effect(&'static EffectInfo),
}

/// One plugin the exports expose.
#[derive(Clone, Debug)]
pub struct ExportedPlugin {
    /// `synth`, `reverb`, … (the part after `rawdawg.fx.`).
    pub slug: &'static str,
    pub clap_id: String,
    /// The native processor type id (`native.synth`, `native.fx.reverb`, …).
    pub native_type_id: &'static str,
    pub kind: Kind,
    /// Display name as hosts see it (`rawDAWg Synth`, `rawDAWg Reverb`).
    pub name: String,
    pub description: &'static str,
    /// Effects whose descriptors include a `sidechain` toggle get a second input bus.
    pub has_sidechain: bool,
    pub has_mix: bool,
    pub vst3_slot: u32,
}

impl ExportedPlugin {
    pub fn is_synth(&self) -> bool {
        matches!(self.kind, Kind::Synth)
    }
    /// A fresh, unprepared native processor for this plugin.
    pub fn make(&self) -> Result<Box<dyn rawdawg_dsp::processor::Processor>, String> {
        match self.kind {
            Kind::Synth => Ok(Box::new(rawdawg_synthesizer::Synth::default())),
            Kind::Effect(info) => rawdawg_effects::make_effect(info.type_id, None),
        }
    }
    /// VST3 (processor, controller) class ids.
    pub fn vst3_class_ids(&self) -> (Vst3Uid, Vst3Uid) {
        vst3_class_ids(self.vst3_slot)
    }
    /// CLAP feature strings (`audio-effect`, `reverb`, `stereo`, …).
    pub fn clap_features(&self) -> Vec<&'static str> {
        if self.is_synth() {
            return vec!["instrument", "synthesizer", "stereo"];
        }
        let specific: &[&'static str] = match self.slug {
            "eq" => &["equalizer"],
            "compressor" => &["compressor"],
            "limiter" => &["limiter", "mastering"],
            "gate" => &["gate"],
            "distortion" | "multiband" => &["distortion"],
            "delay" => &["delay"],
            "chorus" => &["chorus"],
            "flanger" => &["flanger"],
            "phaser" => &["phaser"],
            "pitch" => &["pitch-shifter"],
            "granular" => &["granular", "glitch"],
            "spectral" => &["phase-vocoder", "glitch"],
            "reverb" | "convolution" => &["reverb"],
            "stereo" => &["utility", "mixing"],
            _ => &[],
        };
        let mut v = vec!["audio-effect"];
        v.extend_from_slice(specific);
        v.push("stereo");
        v
    }
    /// VST3 sub-categories string (`Fx|Reverb`, `Instrument|Synth`).
    pub fn vst3_subcategories(&self) -> &'static str {
        if self.is_synth() {
            return "Instrument|Synth";
        }
        match self.slug {
            "eq" => "Fx|EQ",
            "compressor" | "limiter" | "gate" => "Fx|Dynamics",
            "distortion" | "multiband" => "Fx|Distortion",
            "delay" => "Fx|Delay",
            "chorus" | "flanger" | "phaser" => "Fx|Modulation",
            "pitch" => "Fx|Pitch Shift",
            "granular" | "spectral" => "Fx|Generator",
            "reverb" | "convolution" => "Fx|Reverb",
            "stereo" => "Fx|Spatial",
            _ => "Fx",
        }
    }
}

/// `native.fx.reverb` → `rawdawg.fx.reverb`; `native.synth` → `rawdawg.synth`.
pub fn clap_id_for_native(type_id: &str) -> Option<String> {
    if type_id == rawdawg_synthesizer::TYPE_ID { Some(SYNTH_CLAP_ID.to_owned()) } else { type_id.strip_prefix(NATIVE_FX_PREFIX).map(|slug| format!("{FX_CLAP_ID_PREFIX}{slug}")) }
}

/// Inverse of [`clap_id_for_native`].
pub fn native_type_id_for_clap(clap_id: &str) -> Option<String> {
    if clap_id == SYNTH_CLAP_ID { Some(rawdawg_synthesizer::TYPE_ID.to_owned()) } else { clap_id.strip_prefix(FX_CLAP_ID_PREFIX).map(|slug| format!("{NATIVE_FX_PREFIX}{slug}")) }
}

fn slot_for(slug: &str) -> Option<u32> {
    VST3_SLOTS.iter().find(|(s, _)| *s == slug).map(|(_, n)| *n)
}

/// Every exported plugin: the synth first, then the listed effects in catalogue order.
/// An effect without a recorded VST3 slot is **not** exported (add the slot first).
pub fn exported_plugins() -> Vec<ExportedPlugin> {
    let mut out = Vec::with_capacity(1 + rawdawg_effects::CATALOG.len());
    out.push(ExportedPlugin {
        slug: "synth",
        clap_id: SYNTH_CLAP_ID.to_owned(),
        native_type_id: rawdawg_synthesizer::TYPE_ID,
        kind: Kind::Synth,
        name: rawdawg_synthesizer::DISPLAY_NAME.to_owned(),
        description: "rawDAWg flagship experimental synthesizer. Less thinking. More noise.",
        has_sidechain: false,
        has_mix: false,
        vst3_slot: slot_for("synth").expect("synth slot recorded"),
    });
    for info in rawdawg_effects::CATALOG.iter().filter(|i| i.listed) {
        let Some(slug) = info.type_id.strip_prefix(NATIVE_FX_PREFIX) else { continue };
        let Some(vst3_slot) = slot_for(slug) else { continue };
        let has_sidechain = rawdawg_effects::make_effect(info.type_id, None).map(|p| p.descriptors().iter().any(|d| d.key == "sidechain")).unwrap_or(false);
        out.push(ExportedPlugin {
            slug,
            clap_id: format!("{FX_CLAP_ID_PREFIX}{slug}"),
            native_type_id: info.type_id,
            kind: Kind::Effect(info),
            name: format!("rawDAWg {}", info.name),
            description: info.description,
            has_sidechain,
            has_mix: info.has_mix,
            vst3_slot,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_listed_effect_is_exported_with_a_unique_stable_slot() {
        let all = exported_plugins();
        let listed = rawdawg_effects::CATALOG.iter().filter(|i| i.listed).count();
        assert_eq!(all.len(), listed + 1, "add a VST3 slot for every new effect family");
        let mut slots: Vec<u32> = all.iter().map(|p| p.vst3_slot).collect();
        slots.sort_unstable();
        slots.dedup();
        assert_eq!(slots.len(), all.len(), "slots are unique");
        let mut ids: Vec<Vst3Uid> = all.iter().flat_map(|p| [p.vst3_class_ids().0, p.vst3_class_ids().1]).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), all.len() * 2, "class ids are unique");
        for p in &all {
            assert_eq!(clap_id_for_native(p.native_type_id).as_deref(), Some(p.clap_id.as_str()));
            assert_eq!(native_type_id_for_clap(&p.clap_id).as_deref(), Some(p.native_type_id));
            assert!(p.clap_features().contains(&if p.is_synth() { "instrument" } else { "audio-effect" }));
            assert!(p.vst3_subcategories().starts_with(if p.is_synth() { "Instrument" } else { "Fx" }));
        }
        assert_eq!(vst3_class_ids(1).0, (0x7A57_D4A6, 0x1000_0001, 0xB0A5_3E11, 0x9F2C_0002));
    }
}
