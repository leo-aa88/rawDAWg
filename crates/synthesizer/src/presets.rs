//! Original factory presets (SYN-08): programmatic patches across the required
//! characters. Read-only; edited copies get new ids when saved.

use crate::patch::*;
use rawdawg_dsp::envelope::AdsrParams;
use rawdawg_dsp::filter::FilterMode;
use rawdawg_dsp::oscillator::BasicWave;
use rawdawg_dsp::wavetable::{CYCLE_LEN, cycles};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Character {
    Bass,
    Pad,
    Lead,
    Keys,
    Percussive,
    Texture,
    Noise,
    Experimental,
}

impl Character {
    pub const ALL: [Character; 8] =
        [Character::Bass, Character::Pad, Character::Lead, Character::Keys, Character::Percussive, Character::Texture, Character::Noise, Character::Experimental];
    pub fn label(self) -> &'static str {
        match self {
            Character::Bass => "Bass",
            Character::Pad => "Pad",
            Character::Lead => "Lead",
            Character::Keys => "Keys",
            Character::Percussive => "Percussive",
            Character::Texture => "Texture",
            Character::Noise => "Noise",
            Character::Experimental => "Experimental",
        }
    }
    pub fn key(self) -> &'static str {
        match self {
            Character::Bass => "bass",
            Character::Pad => "pad",
            Character::Lead => "lead",
            Character::Keys => "keys",
            Character::Percussive => "percussive",
            Character::Texture => "texture",
            Character::Noise => "noise",
            Character::Experimental => "experimental",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct FactoryPreset {
    pub name: &'static str,
    pub character: Character,
    pub description: &'static str,
    /// Marks the deliberately unusual sounds (SYN-08: at least 8).
    pub unusual: bool,
    pub patch: Patch,
}

fn env(a: f32, d: f32, s: f32, r: f32) -> AdsrParams {
    AdsrParams { attack_s: a, decay_s: d, sustain: s, release_s: r }
}

fn base(name: &str) -> Patch {
    Patch { name: name.to_owned(), ..Patch::default() }
}

fn layer(g: Generator) -> Layer {
    Layer::with_generator(g)
}

fn route(source: ModSource, target: ModTarget, depth: f32) -> ModRoute {
    ModRoute { source, target, depth, enabled: true }
}

fn lfo(shape: LfoShape, rate_hz: f32, depth: f32, per_voice: bool) -> Lfo {
    Lfo { shape, rate_hz, sync_beats: 0.0, depth, per_voice }
}

fn cycle_from(f: impl Fn(f64) -> f64) -> Vec<f32> {
    (0..CYCLE_LEN).map(|i| f(i as f64 / CYCLE_LEN as f64) as f32).collect()
}

fn noise_cycle(seed: u64) -> Vec<f32> {
    let mut r = rawdawg_dsp::rng::DeterministicRng::new(seed, 0x0E);
    (0..CYCLE_LEN).map(|_| r.bipolar_f32()).collect()
}

/// All factory presets in catalog order.
pub fn factory_presets() -> Vec<FactoryPreset> {
    let mut out: Vec<FactoryPreset> = Vec::new();
    let mut add = |name: &'static str, character: Character, description: &'static str, unusual: bool, patch: Patch| {
        out.push(FactoryPreset { name, character, description, unusual, patch });
    };

    // ---- Bass
    {
        let mut p = base("Init");
        p.layers = vec![Layer::default_soft_saw()];
        add("Init", Character::Keys, "The default: one soft saw, open filter, fast envelope.", false, p);
    }
    {
        let mut p = base("Sub Pressure");
        p.layers = vec![
            layer(Generator::Basic { wave: BasicWave::Sine, pulse_width: 0.5 }),
            Layer { coarse_semitones: 12, level_db: -14.0, ..layer(Generator::Basic { wave: BasicWave::Triangle, pulse_width: 0.5 }) },
        ];
        p.amp_env = env(0.004, 0.2, 0.9, 0.12);
        p.voice_mode = VoiceMode::MonoLegato;
        p.portamento_s = 0.04;
        add("Sub Pressure", Character::Bass, "Mono sine sub with a quiet octave for presence; glides between notes.", false, p);
    }
    {
        let mut p = base("Reese Grit");
        p.layers = vec![Layer { unison: 4, unison_detune_cents: 22.0, unison_spread: 0.6, ..layer(Generator::Basic { wave: BasicWave::Saw, pulse_width: 0.5 }) }];
        p.filter = FilterSettings { mode: FilterMode::LowPass, cutoff_hz: 900.0, resonance: 1.2, env_octaves: 1.5, key_track: 0.3, enabled: true };
        p.filter_env = env(0.01, 0.35, 0.2, 0.3);
        p.amp_env = env(0.005, 0.1, 1.0, 0.15);
        add("Reese Grit", Character::Bass, "Detuned saws through a slow-opening low-pass.", false, p);
    }
    {
        let mut p = base("Pluck Bass");
        p.layers = vec![layer(Generator::Pluck(PluckSettings { damping: 0.35, brightness: 0.5, excitation: Excitation::Pick }))];
        p.amp_env = env(0.001, 0.6, 0.0, 0.3);
        p.filter.cutoff_hz = 3000.0;
        add("Pluck Bass", Character::Bass, "A plucked string low down: short, woody, dynamic.", false, p);
    }
    {
        let mut p = base("FM Wobble Bass");
        let mut fm = FmSettings::default();
        fm.operators[1] = FmOperator { ratio: 1.0, level: 0.5, detune_cents: 0.0 };
        p.layers = vec![layer(Generator::Fm(fm))];
        p.lfos[0] = lfo(LfoShape::Sine, 3.0, 1.0, false);
        p.mod_routes = vec![route(ModSource::Lfo1, ModTarget::FmIndex, 0.6), route(ModSource::Lfo1, ModTarget::Cutoff, 0.3)];
        p.filter.cutoff_hz = 2500.0;
        p.amp_env = env(0.005, 0.1, 1.0, 0.12);
        add("FM Wobble Bass", Character::Bass, "Phase-modulation bass whose brightness wobbles with an LFO.", false, p);
    }

    // ---- Pads
    {
        let mut p = base("Glass Pad");
        p.layers = vec![
            Layer { unison: 6, unison_detune_cents: 14.0, unison_spread: 1.0, ..Layer::default_soft_saw() },
            Layer { coarse_semitones: 12, level_db: -10.0, ..layer(Generator::Basic { wave: BasicWave::Triangle, pulse_width: 0.5 }) },
        ];
        p.amp_env = env(0.9, 1.0, 0.8, 1.6);
        p.filter = FilterSettings { mode: FilterMode::LowPass, cutoff_hz: 2200.0, resonance: 0.8, env_octaves: 1.0, key_track: 0.2, enabled: true };
        p.filter_env = env(1.5, 2.0, 0.6, 1.5);
        p.lfos[0] = lfo(LfoShape::Sine, 0.3, 1.0, false);
        p.mod_routes = vec![route(ModSource::Lfo1, ModTarget::Cutoff, 0.15)];
        add("Glass Pad", Character::Pad, "Wide detuned soft saws that bloom slowly.", false, p);
    }
    {
        let mut p = base("Breathing Grains");
        p.layers = vec![layer(Generator::Granular {
            settings: GranularSettings { size_ms: 120.0, density_hz: 25.0, jitter: 0.2, pitch_spread_semitones: 0.1, window: GrainWindow::Hann },
            source: cycles::soft_saw(),
        })];
        p.amp_env = env(1.2, 1.0, 0.9, 2.0);
        p.filter.cutoff_hz = 5000.0;
        p.lfos[1] = lfo(LfoShape::Triangle, 0.15, 1.0, false);
        p.mod_routes = vec![route(ModSource::Lfo2, ModTarget::GrainPosition, 0.5), route(ModSource::Lfo2, ModTarget::Amp, 0.2)];
        add("Breathing Grains", Character::Pad, "Granular cloud over a soft saw, slowly shifting position.", false, p);
    }
    {
        let mut p = base("Hollow Choir");
        p.layers = vec![
            layer(Generator::Additive(AdditiveSettings { amplitudes: vec![1.0, 0.0, 0.6, 0.0, 0.3, 0.0, 0.15, 0.0, 0.08], phases: vec![0.0; 9], stretch: 0.0, detuned: false })),
            Layer { coarse_semitones: 7, level_db: -9.0, unison: 3, unison_detune_cents: 9.0, ..layer(Generator::Basic { wave: BasicWave::Triangle, pulse_width: 0.5 }) },
        ];
        p.amp_env = env(0.6, 0.5, 0.9, 1.2);
        p.lfos[0] = lfo(LfoShape::Sine, 5.0, 1.0, true);
        p.mod_routes = vec![route(ModSource::Lfo1, ModTarget::Pitch, 0.004)];
        add("Hollow Choir", Character::Pad, "Odd-harmonic additive voice with a fifth and gentle vibrato.", false, p);
    }

    // ---- Leads
    {
        let mut p = base("Square Lead");
        p.layers = vec![layer(Generator::Basic { wave: BasicWave::Pulse, pulse_width: 0.5 })];
        p.voice_mode = VoiceMode::MonoLegato;
        p.portamento_s = 0.06;
        p.amp_env = env(0.01, 0.2, 0.85, 0.2);
        p.filter = FilterSettings { mode: FilterMode::LowPass, cutoff_hz: 4000.0, resonance: 1.5, env_octaves: 1.0, key_track: 0.5, enabled: true };
        p.lfos[0] = lfo(LfoShape::Sine, 5.5, 1.0, true);
        p.mod_routes = vec![route(ModSource::ModWheel, ModTarget::Pitch, 0.01), route(ModSource::Lfo1, ModTarget::Pitch, 0.0)];
        add("Square Lead", Character::Lead, "Classic mono square with glide; the mod wheel adds vibrato.", false, p);
    }
    {
        let mut p = base("PWM Lead");
        p.layers = vec![Layer { unison: 2, unison_detune_cents: 8.0, ..layer(Generator::Basic { wave: BasicWave::Pulse, pulse_width: 0.3 }) }];
        p.lfos[0] = lfo(LfoShape::Triangle, 0.7, 1.0, false);
        p.mod_routes = vec![route(ModSource::Lfo1, ModTarget::PulseWidth, 0.5)];
        p.amp_env = env(0.01, 0.3, 0.8, 0.25);
        p.filter.cutoff_hz = 6000.0;
        add("PWM Lead", Character::Lead, "Pulse width sweeping under an LFO.", false, p);
    }
    {
        let mut p = base("Bell Tine Lead");
        let fm = FmSettings {
            operators: [
                FmOperator { ratio: 1.0, level: 1.0, detune_cents: 0.0 },
                FmOperator { ratio: 3.5, level: 0.45, detune_cents: 3.0 },
                FmOperator { ratio: 1.0, level: 0.0, detune_cents: 0.0 },
                FmOperator { ratio: 1.0, level: 0.0, detune_cents: 0.0 },
            ],
            algorithm: FmAlgorithm::Serial,
            feedback: 0.0,
        };
        p.layers = vec![layer(Generator::Fm(fm))];
        p.amp_env = env(0.002, 1.2, 0.3, 0.8);
        p.mod_routes = vec![route(ModSource::AmpEnv, ModTarget::FmIndex, 0.8), route(ModSource::Velocity, ModTarget::FmIndex, 0.4)];
        add("Bell Tine Lead", Character::Lead, "Inharmonic FM tine whose brightness follows the envelope.", false, p);
    }

    // ---- Keys
    {
        let mut p = base("Electric Piano");
        let fm = FmSettings {
            operators: [
                FmOperator { ratio: 1.0, level: 1.0, detune_cents: 0.0 },
                FmOperator { ratio: 14.0, level: 0.18, detune_cents: 0.0 },
                FmOperator { ratio: 1.0, level: 0.6, detune_cents: 0.0 },
                FmOperator { ratio: 1.0, level: 0.25, detune_cents: 0.0 },
            ],
            algorithm: FmAlgorithm::Pairs,
            feedback: 0.1,
        };
        p.layers = vec![layer(Generator::Fm(fm))];
        p.amp_env = env(0.002, 1.8, 0.2, 0.5);
        p.mod_routes =
            vec![route(ModSource::AmpEnv, ModTarget::FmIndex, 0.5), route(ModSource::Velocity, ModTarget::FmIndex, 0.5), route(ModSource::Velocity, ModTarget::Cutoff, 0.3)];
        p.filter.cutoff_hz = 8000.0;
        add("Electric Piano", Character::Keys, "Two FM pairs: bark on attack, mellow sustain, velocity-sensitive.", false, p);
    }
    {
        let mut p = base("Clavi Pluck");
        p.layers = vec![layer(Generator::Pluck(PluckSettings { damping: 0.5, brightness: 0.85, excitation: Excitation::Noise }))];
        p.amp_env = env(0.001, 0.4, 0.0, 0.2);
        p.filter = FilterSettings { mode: FilterMode::HighPass, cutoff_hz: 150.0, resonance: 0.7, env_octaves: 0.0, key_track: 0.0, enabled: true };
        add("Clavi Pluck", Character::Keys, "Bright plucked string, short and snappy.", false, p);
    }
    {
        let mut p = base("Organ Drawbars");
        p.layers =
            vec![layer(Generator::Additive(AdditiveSettings { amplitudes: vec![1.0, 0.8, 0.5, 0.6, 0.0, 0.4, 0.0, 0.3], phases: vec![0.0; 8], stretch: 0.0, detuned: false }))];
        p.amp_env = env(0.003, 0.01, 1.0, 0.03);
        p.lfos[0] = lfo(LfoShape::Sine, 6.5, 1.0, false);
        p.mod_routes = vec![route(ModSource::ModWheel, ModTarget::Amp, 0.0), route(ModSource::Lfo1, ModTarget::Amp, 0.08)];
        add("Organ Drawbars", Character::Keys, "Additive drawbar organ with a light tremolo.", false, p);
    }

    // ---- Percussive
    {
        let mut p = base("Kick Drum");
        p.layers = vec![layer(Generator::Basic { wave: BasicWave::Sine, pulse_width: 0.5 })];
        p.amp_env = env(0.001, 0.35, 0.0, 0.1);
        p.filter_env = env(0.001, 0.08, 0.0, 0.05);
        p.mod_routes = vec![route(ModSource::FilterEnv, ModTarget::Pitch, 1.0)];
        p.tuning.transpose_semitones = -24;
        p.master_db = -3.0;
        add("Kick Drum", Character::Percussive, "Sine with a fast pitch drop from the filter envelope. Play low C.", false, p);
    }
    {
        let mut p = base("Noise Snare");
        p.layers = vec![
            layer(Generator::Wavetable { cycle: noise_cycle(7), remove_dc: true, frames: vec![], position: 0.0 }),
            Layer { level_db: -4.0, coarse_semitones: -12, ..layer(Generator::Basic { wave: BasicWave::Triangle, pulse_width: 0.5 }) },
        ];
        p.amp_env = env(0.001, 0.18, 0.0, 0.08);
        p.master_db = -3.0;
        p.filter = FilterSettings { mode: FilterMode::BandPass, cutoff_hz: 2200.0, resonance: 0.9, env_octaves: 0.0, key_track: 0.0, enabled: true };
        add("Noise Snare", Character::Percussive, "A noise cycle plus a short body tone through a band-pass.", false, p);
    }
    {
        let mut p = base("Metal Hit");
        let fm = FmSettings {
            operators: [
                FmOperator { ratio: 1.0, level: 1.0, detune_cents: 0.0 },
                FmOperator { ratio: 5.7, level: 0.9, detune_cents: 11.0 },
                FmOperator { ratio: 2.3, level: 0.7, detune_cents: -7.0 },
                FmOperator { ratio: 9.1, level: 0.5, detune_cents: 0.0 },
            ],
            algorithm: FmAlgorithm::Fan,
            feedback: 0.5,
        };
        p.layers = vec![layer(Generator::Fm(fm))];
        p.amp_env = env(0.001, 0.5, 0.0, 0.3);
        p.master_db = -4.0;
        add("Metal Hit", Character::Percussive, "Three inharmonic modulators into one carrier: clangs and gongs.", false, p);
    }

    // ---- Texture
    {
        let mut p = base("Shimmer Dust");
        p.layers = vec![layer(Generator::Granular {
            settings: GranularSettings { size_ms: 18.0, density_hz: 110.0, jitter: 0.6, pitch_spread_semitones: 12.0, window: GrainWindow::Triangle },
            source: cycles::sine(),
        })];
        p.amp_env = env(0.4, 0.5, 0.8, 1.5);
        p.filter = FilterSettings { mode: FilterMode::HighPass, cutoff_hz: 600.0, resonance: 0.7, env_octaves: 0.0, key_track: 0.0, enabled: true };
        add("Shimmer Dust", Character::Texture, "Tiny grains scattered an octave wide.", false, p);
    }
    {
        let mut p = base("Tape Drift");
        p.layers = vec![Layer { unison: 3, unison_detune_cents: 5.0, ..Layer::default_soft_saw() }];
        p.lfos[0] = lfo(LfoShape::SampleHold, 2.5, 1.0, true);
        p.lfos[1] = lfo(LfoShape::Sine, 0.9, 1.0, false);
        p.mod_routes = vec![route(ModSource::Lfo1, ModTarget::Pitch, 0.006), route(ModSource::Lfo2, ModTarget::Cutoff, 0.4)];
        p.filter.cutoff_hz = 1800.0;
        p.amp_env = env(0.3, 0.4, 0.8, 0.9);
        add("Tape Drift", Character::Texture, "Wobbly, unstable pitch like a worn tape.", false, p);
    }

    // ---- Noise
    {
        let mut p = base("White Wash");
        p.layers = vec![layer(Generator::Wavetable { cycle: noise_cycle(1), remove_dc: true, frames: vec![], position: 0.0 })];
        p.amp_env = env(0.5, 0.5, 1.0, 1.0);
        p.filter = FilterSettings { mode: FilterMode::LowPass, cutoff_hz: 4000.0, resonance: 0.7, env_octaves: 0.0, key_track: 0.0, enabled: true };
        p.lfos[0] = lfo(LfoShape::Triangle, 0.2, 1.0, false);
        p.mod_routes = vec![route(ModSource::Lfo1, ModTarget::Cutoff, 0.8)];
        add("White Wash", Character::Noise, "Noise cycle with a slowly sweeping filter; pitch changes the grain.", false, p);
    }
    {
        let mut p = base("Resonant Hiss");
        p.layers = vec![layer(Generator::Wavetable { cycle: noise_cycle(3), remove_dc: true, frames: vec![], position: 0.0 })];
        p.filter = FilterSettings { mode: FilterMode::BandPass, cutoff_hz: 1200.0, resonance: 8.0, env_octaves: 0.0, key_track: 1.0, enabled: true };
        p.amp_env = env(0.05, 0.3, 0.7, 0.4);
        p.master_db = 4.0;
        add("Resonant Hiss", Character::Noise, "Keytracked band-pass turns noise into a pitched whistle.", false, p);
    }

    // ---- Experimental / unusual
    {
        let mut p = base("Folded Sine");
        p.layers = vec![layer(Generator::Wavetable {
            cycle: cycle_from(|t| {
                let x = (t * std::f64::consts::TAU).sin() * 3.0;
                let m = (x + 1.0).rem_euclid(4.0);
                if m < 2.0 { m - 1.0 } else { 3.0 - m }
            }),
            remove_dc: true,
            frames: vec![],
            position: 0.0,
        })];
        p.amp_env = env(0.01, 0.3, 0.8, 0.4);
        add("Folded Sine", Character::Experimental, "A sine folded over itself three times.", true, p);
    }
    {
        let mut p = base("Motion Morph");
        let frames = vec![cycles::sine(), cycles::triangle(), cycles::square(), cycles::saw()];
        p.layers = vec![layer(Generator::Wavetable { cycle: cycles::sine(), remove_dc: true, frames, position: 0.0 })];
        p.lfos[0] = lfo(LfoShape::Triangle, 0.4, 1.0, false);
        p.mod_routes = vec![route(ModSource::Lfo1, ModTarget::WavePosition, 0.5)];
        p.amp_env = env(0.05, 0.3, 0.9, 0.5);
        add("Motion Morph", Character::Experimental, "Four waveform frames; an LFO travels between them.", true, p);
    }
    {
        let mut p = base("Feedback Growl");
        let fm = FmSettings {
            operators: [
                FmOperator { ratio: 1.0, level: 1.0, detune_cents: 0.0 },
                FmOperator { ratio: 0.5, level: 0.7, detune_cents: 0.0 },
                FmOperator { ratio: 1.0, level: 0.0, detune_cents: 0.0 },
                FmOperator { ratio: 1.0, level: 0.9, detune_cents: 0.0 },
            ],
            algorithm: FmAlgorithm::Serial,
            feedback: 0.95,
        };
        p.layers = vec![layer(Generator::Fm(fm))];
        p.filter.cutoff_hz = 3000.0;
        p.amp_env = env(0.02, 0.2, 0.9, 0.3);
        add("Feedback Growl", Character::Experimental, "Operator 4 eating its own tail at near-maximum feedback.", true, p);
    }
    {
        let mut p = base("Inharmonic Bank");
        p.layers = vec![layer(Generator::Additive(AdditiveSettings {
            amplitudes: (0..24).map(|k| 1.0 / (k as f32 + 1.0).sqrt()).collect(),
            phases: vec![0.0; 24],
            stretch: 0.03,
            detuned: true,
        }))];
        p.amp_env = env(0.01, 0.8, 0.6, 0.9);
        add("Inharmonic Bank", Character::Experimental, "24 partials stretched apart like a struck bar.", true, p);
    }
    {
        let mut p = base("Random Walk");
        p.layers = vec![layer(Generator::Basic { wave: BasicWave::Saw, pulse_width: 0.5 })];
        p.lfos[0] = lfo(LfoShape::SampleHold, 8.0, 1.0, true);
        p.lfos[1] = lfo(LfoShape::SampleHold, 3.0, 1.0, true);
        p.mod_routes = vec![route(ModSource::Lfo1, ModTarget::Cutoff, 0.7), route(ModSource::Lfo2, ModTarget::Pan, 0.8), route(ModSource::Random, ModTarget::Cutoff, 0.3)];
        p.filter.cutoff_hz = 600.0;
        p.filter.resonance = 4.0;
        p.amp_env = env(0.01, 0.2, 0.9, 0.3);
        add("Random Walk", Character::Experimental, "Sample-and-hold steps the filter and pan around; every note starts differently.", true, p);
    }
    {
        let mut p = base("Stretched String");
        p.layers = vec![
            layer(Generator::Pluck(PluckSettings { damping: 0.02, brightness: 0.95, excitation: Excitation::Impulse })),
            Layer { level_db: -8.0, coarse_semitones: 19, ..layer(Generator::Pluck(PluckSettings { damping: 0.1, brightness: 0.3, excitation: Excitation::Noise })) },
        ];
        p.amp_env = env(0.001, 2.0, 0.9, 1.5);
        add("Stretched String", Character::Experimental, "Two strings, one bright and endless, one dull a twelfth up.", true, p);
    }
    {
        let mut p = base("Grain Freeze");
        p.layers = vec![layer(Generator::Granular {
            settings: GranularSettings { size_ms: 400.0, density_hz: 6.0, jitter: 0.0, pitch_spread_semitones: 0.0, window: GrainWindow::Rect },
            source: cycles::square(),
        })];
        p.amp_env = env(0.2, 0.2, 1.0, 0.6);
        add("Grain Freeze", Character::Experimental, "Long rectangular grains at a slow rate: a stuttering square.", true, p);
    }
    {
        let mut p = base("Chaos Comb");
        p.layers = vec![Layer { unison: 8, unison_detune_cents: 90.0, unison_spread: 1.0, ..layer(Generator::Basic { wave: BasicWave::Pulse, pulse_width: 0.12 }) }];
        p.filter = FilterSettings { mode: FilterMode::Notch, cutoff_hz: 800.0, resonance: 6.0, env_octaves: 2.0, key_track: 0.0, enabled: true };
        p.filter_env = env(0.3, 1.0, 0.2, 0.5);
        p.amp_env = env(0.02, 0.3, 0.8, 0.5);
        add("Chaos Comb", Character::Experimental, "Eight wildly detuned narrow pulses through a moving notch.", true, p);
    }
    {
        let mut p = base("Haunt");
        p.layers = vec![
            layer(Generator::Granular {
                settings: GranularSettings { size_ms: 90.0, density_hz: 30.0, jitter: 0.5, pitch_spread_semitones: 7.0, window: GrainWindow::Hann },
                source: noise_cycle(11),
            }),
            Layer { level_db: -6.0, ..layer(Generator::Basic { wave: BasicWave::Sine, pulse_width: 0.5 }) },
        ];
        p.filter = FilterSettings { mode: FilterMode::LowPass, cutoff_hz: 1500.0, resonance: 3.0, env_octaves: 0.0, key_track: 0.5, enabled: true };
        p.amp_env = env(0.8, 0.5, 0.8, 2.0);
        p.lfos[0] = lfo(LfoShape::Sine, 0.11, 1.0, false);
        p.mod_routes = vec![route(ModSource::Lfo1, ModTarget::Cutoff, 0.5)];
        add("Haunt", Character::Experimental, "Noise grains smeared across a fifth with a sine ghost underneath.", true, p);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audition::{AuditionSpec, SoundFeatures, render_patch};

    #[test]
    fn catalog_covers_every_character_with_enough_originals() {
        let all = factory_presets();
        assert!(all.len() >= 24, "{} presets", all.len());
        assert!(all.iter().filter(|p| p.unusual).count() >= 8);
        for c in Character::ALL {
            assert!(all.iter().any(|p| p.character == c), "no preset for {c:?}");
        }
        let mut names: Vec<&str> = all.iter().map(|p| p.name).collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), all.len(), "duplicate preset names");
        for p in &all {
            assert!(p.patch.validate().is_ok(), "{}: {:?}", p.name, p.patch.validate());
            assert_eq!(p.patch.name, p.name);
        }
    }

    #[test]
    fn presets_are_audible_finite_and_actually_different() {
        let all = factory_presets();
        let aud = AuditionSpec { hold_seconds: 0.3, tail_seconds: 0.1, pitch: 48, ..Default::default() };
        let mut feats = Vec::new();
        let mut problems = Vec::new();
        for p in &all {
            let pitch = if p.name == "Kick Drum" { 36 } else { aud.pitch };
            let buf = render_patch(&p.patch, &AuditionSpec { pitch, ..aud }).unwrap();
            assert!(buf.iter().all(|s| s.is_finite()), "{}", p.name);
            let f = SoundFeatures::of(&buf, aud.sample_rate);
            if f.rms <= 0.003 {
                problems.push(format!("{} is silent (rms {})", p.name, f.rms));
            }
            if f.peak > 1.5 {
                problems.push(format!("{} too loud ({})", p.name, f.peak));
            }
            feats.push((p.name, f, buf));
        }
        assert!(problems.is_empty(), "{problems:?}");
        for i in 0..feats.len() {
            for j in (i + 1)..feats.len() {
                let d = feats[i].1.distance(&feats[j].1);
                let samples_differ = feats[i].2.iter().zip(&feats[j].2).any(|(a, b)| (a - b).abs() > 1e-3);
                assert!(samples_differ && (d > 0.05 || feats[i].2 != feats[j].2), "{} and {} sound the same (distance {d})", feats[i].0, feats[j].0);
            }
        }
    }
}
