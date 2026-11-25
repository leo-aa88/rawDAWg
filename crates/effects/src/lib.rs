//! rawDAWg native effect suite (FX-01/02/03). Every effect is a [`Processor`]
//! with stable parameter descriptors, JSON state keyed by parameter key, factory
//! presets and RT-safe processing. The same code runs in the app, the offline
//! renderer and the plugin exports.

pub mod common;
pub mod delay;
pub mod dynamics;
pub mod eq;
pub mod reverb;
pub mod stereo;
pub mod testfx;

pub use common::{Params, Preset};
use rawdawg_dsp::processor::Processor;

/// Catalogue entry for browsers and menus.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EffectInfo {
    pub type_id: &'static str,
    pub name: &'static str,
    pub family: &'static str,
    pub description: &'static str,
    /// Wet/dry mix is meaningful for this effect.
    pub has_mix: bool,
    /// Listed in menus (test effects are not).
    pub listed: bool,
}

pub static CATALOG: [EffectInfo; 3] = [
    EffectInfo {
        type_id: eq::TYPE_ID,
        name: eq::DISPLAY_NAME,
        family: "EQ",
        description: "8-band parametric equalizer with draggable bands and a response plot",
        has_mix: false,
        listed: true,
    },
    EffectInfo {
        type_id: stereo::TYPE_ID,
        name: stereo::DISPLAY_NAME,
        family: "Stereo",
        description: "Gain, pan, balance, width, mono and phase with a correlation meter",
        has_mix: false,
        listed: true,
    },
    EffectInfo { type_id: testfx::TYPE_ID, name: "Latency test", family: "Test", description: "Fixed delay reporting its latency (tests only)", has_mix: true, listed: false },
];

pub fn info(type_id: &str) -> Option<&'static EffectInfo> {
    CATALOG.iter().find(|e| e.type_id == type_id)
}

/// Instantiate an effect by type id (non-RT). `state` is applied when present.
pub fn make_effect(type_id: &str, state: Option<&[u8]>) -> Result<Box<dyn Processor>, String> {
    let mut p: Box<dyn Processor> = match type_id {
        eq::TYPE_ID => Box::new(eq::ParametricEq::new()),
        delay::TYPE_ID => Box::new(delay::Delay::new()),
        reverb::TYPE_ID => Box::new(reverb::Reverb::new()),
        dynamics::compressor::TYPE_ID => Box::new(dynamics::compressor::Compressor::new()),
        dynamics::limiter::TYPE_ID => Box::new(dynamics::limiter::Limiter::new()),
        dynamics::gate::TYPE_ID => Box::new(dynamics::gate::Gate::new()),
        stereo::TYPE_ID => Box::new(stereo::StereoProcessor::new()),
        testfx::TYPE_ID => Box::new(testfx::LatencyTest::new()),
        other => return Err(format!("unknown native effect {other}")),
    };
    if let Some(bytes) = state
        && !bytes.is_empty()
    {
        p.load_state(bytes)?;
    }
    Ok(p)
}

/// Factory presets of an effect.
pub fn presets(type_id: &str) -> &'static [Preset] {
    match type_id {
        eq::TYPE_ID => &eq::PRESETS,
        delay::TYPE_ID => &delay::PRESETS,
        reverb::TYPE_ID => &reverb::PRESETS,
        dynamics::compressor::TYPE_ID => &dynamics::compressor::PRESETS,
        dynamics::limiter::TYPE_ID => &dynamics::limiter::PRESETS,
        dynamics::gate::TYPE_ID => &dynamics::gate::PRESETS,
        stereo::TYPE_ID => &stereo::PRESETS,
        _ => &[],
    }
}

/// State bytes of a preset (non-RT): a fresh instance with the preset applied.
pub fn preset_state(type_id: &str, preset: &Preset) -> Option<Vec<u8>> {
    let mut p = make_effect(type_id, None).ok()?;
    let descs = p.descriptors();
    let mut params = Params::new(descs);
    common::apply_preset(&mut params, preset);
    for d in descs {
        p.set_param(d.id, params.get(d.id.0));
    }
    Some(p.save_state())
}
