//! rawDAWg native effect suite (FX-01/02/03). Every effect is a [`Processor`]
//! with stable parameter descriptors, JSON state keyed by parameter key, factory
//! presets and RT-safe processing. The same code runs in the app, the offline
//! renderer and the plugin exports.

pub mod common;
pub mod convolution;
pub mod delay;
pub mod distortion;
pub mod dynamics;
pub mod eq;
pub mod modulation;
pub mod oversample;
pub mod pitch;
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

const fn entry(type_id: &'static str, name: &'static str, family: &'static str, description: &'static str, has_mix: bool) -> EffectInfo {
    EffectInfo { type_id, name, family, description, has_mix, listed: true }
}

/// Every effect the menus offer. [`make_effect`] must accept each entry (tested).
pub static CATALOG: [EffectInfo; 15] = [
    entry(eq::TYPE_ID, eq::DISPLAY_NAME, "EQ", "8-band parametric equalizer with draggable bands and a response plot", false),
    entry(dynamics::compressor::TYPE_ID, dynamics::compressor::DISPLAY_NAME, "Dynamics", "RMS/peak compressor with sidechain input, knee and gain-reduction meter", true),
    entry(dynamics::limiter::TYPE_ID, dynamics::limiter::DISPLAY_NAME, "Dynamics", "True-peak lookahead limiter with ceiling, release and dBTP meter", false),
    entry(dynamics::gate::TYPE_ID, dynamics::gate::DISPLAY_NAME, "Dynamics", "Noise gate with hysteresis, hold, range and sidechain", false),
    entry(distortion::single::TYPE_ID, distortion::single::DISPLAY_NAME, "Distortion", "Tube, tape, soft/hard clip, wavefolder and bitcrusher with oversampling and tone", true),
    entry(distortion::multiband::TYPE_ID, distortion::multiband::DISPLAY_NAME, "Distortion", "Four Linkwitz-Riley bands with drive, mix and gain painted per band", true),
    entry(delay::TYPE_ID, delay::DISPLAY_NAME, "Delay", "Stereo, ping-pong, tape, multitap and reverse delay with tempo sync", true),
    entry(modulation::chorus::TYPE_ID, modulation::chorus::DISPLAY_NAME, "Modulation", "Up to four interpolated delay voices with stereo spread and tempo sync", true),
    entry(modulation::flanger::TYPE_ID, modulation::flanger::DISPLAY_NAME, "Modulation", "Short modulated comb with bounded positive or negative feedback", true),
    entry(modulation::phaser::TYPE_ID, modulation::phaser::DISPLAY_NAME, "Modulation", "2 to 12 all-pass stages swept around a centre frequency with feedback", true),
    entry(pitch::TYPE_ID, pitch::DISPLAY_NAME, "Pitch", "Phase-vocoder pitch shift with cepstral formant shifting and formant preservation", true),
    entry(reverb::TYPE_ID, reverb::DISPLAY_NAME, "Reverb", "Room, hall, cathedral, plate, spring and shimmer with a decay-shape editor", true),
    entry(convolution::TYPE_ID, convolution::DISPLAY_NAME, "Reverb", "Partitioned convolution with impulse responses from the project's samples", true),
    entry(stereo::TYPE_ID, stereo::DISPLAY_NAME, "Stereo", "Gain, pan, balance, width, mono and phase with a correlation meter", false),
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
        modulation::chorus::TYPE_ID => Box::new(modulation::chorus::Chorus::new()),
        modulation::flanger::TYPE_ID => Box::new(modulation::flanger::Flanger::new()),
        modulation::phaser::TYPE_ID => Box::new(modulation::phaser::Phaser::new()),
        pitch::TYPE_ID => Box::new(pitch::PitchShift::new()),
        reverb::TYPE_ID => Box::new(reverb::Reverb::new()),
        distortion::single::TYPE_ID => Box::new(distortion::single::Distortion::new()),
        distortion::multiband::TYPE_ID => Box::new(distortion::multiband::Multiband::new()),
        convolution::TYPE_ID => Box::new(convolution::Convolution::new()),
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
        modulation::chorus::TYPE_ID => &modulation::chorus::PRESETS,
        modulation::flanger::TYPE_ID => &modulation::flanger::PRESETS,
        modulation::phaser::TYPE_ID => &modulation::phaser::PRESETS,
        pitch::TYPE_ID => &pitch::PRESETS,
        reverb::TYPE_ID => &reverb::PRESETS,
        distortion::single::TYPE_ID => &distortion::single::PRESETS,
        distortion::multiband::TYPE_ID => &distortion::multiband::PRESETS,
        convolution::TYPE_ID => &convolution::PRESETS,
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
