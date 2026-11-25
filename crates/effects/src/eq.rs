//! Parametric EQ (`native.fx.eq`): 8 bands (bell, shelves, low/high-pass, notch),
//! output gain, stable coefficient interpolation per block, response for the UI.

use crate::common::{Params, Preset};
use rawdawg_dsp::biquad::{Biquad, BiquadKind, Coeffs};
use rawdawg_dsp::processor::{PrepareError, PrepareSpec, ProcessContext, Processor};
use rawdawg_types::params::{ParamCategory, ParamMapping, ParamUnit};
use rawdawg_types::{ParamDescriptor, ParamId};

pub const TYPE_ID: &str = "native.fx.eq";
pub const DISPLAY_NAME: &str = "Parametric EQ";
pub const BANDS: usize = 8;
pub const STATE_VERSION: u32 = 1;

pub const P_OUTPUT: u32 = 1;
/// Per band: enabled, kind, frequency, gain, Q at `10 + band*10 + offset`.
pub const B_ENABLED: u32 = 0;
pub const B_KIND: u32 = 1;
pub const B_FREQ: u32 = 2;
pub const B_GAIN: u32 = 3;
pub const B_Q: u32 = 4;
pub const fn band_param(band: usize, offset: u32) -> u32 {
    10 + band as u32 * 10 + offset
}

const KINDS: [BiquadKind; 6] = [BiquadKind::Peak, BiquadKind::LowShelf, BiquadKind::HighShelf, BiquadKind::LowPass, BiquadKind::HighPass, BiquadKind::Notch];
pub const KIND_LABELS: [&str; 6] = ["Bell", "Low shelf", "High shelf", "Low-pass", "High-pass", "Notch"];

macro_rules! band_descriptors {
    ($($b:expr),*) => {
        [
            ParamDescriptor::continuous(P_OUTPUT, "output", "Output", ParamUnit::Decibel, ParamMapping::Linear { min: -24.0, max: 24.0 }, 0.5, ParamCategory::Mix).with_smoothing(20.0),
            $(
                ParamDescriptor::continuous(band_param($b, B_ENABLED), concat!("band", $b, ".on"), concat!("Band ", $b, " on"), ParamUnit::None, ParamMapping::Toggle, if $b < 2 { 1.0 } else { 0.0 }, ParamCategory::Filter).stepped(),
                ParamDescriptor::continuous(band_param($b, B_KIND), concat!("band", $b, ".kind"), concat!("Band ", $b, " type"), ParamUnit::None, ParamMapping::Discrete { steps: 6 }, DEFAULT_KIND[$b], ParamCategory::Filter).stepped().with_labels(&KIND_LABELS),
                ParamDescriptor::continuous(band_param($b, B_FREQ), concat!("band", $b, ".freq"), concat!("Band ", $b, " freq"), ParamUnit::Hertz, ParamMapping::Log { min: 20.0, max: 20_000.0 }, DEFAULT_FREQ[$b], ParamCategory::Filter).with_smoothing(30.0),
                ParamDescriptor::continuous(band_param($b, B_GAIN), concat!("band", $b, ".gain"), concat!("Band ", $b, " gain"), ParamUnit::Decibel, ParamMapping::Linear { min: -24.0, max: 24.0 }, 0.5, ParamCategory::Filter).with_smoothing(30.0),
                ParamDescriptor::continuous(band_param($b, B_Q), concat!("band", $b, ".q"), concat!("Band ", $b, " Q"), ParamUnit::Ratio, ParamMapping::Log { min: 0.1, max: 18.0 }, 0.4675, ParamCategory::Filter).with_smoothing(30.0),
            )*
        ]
    };
}
// defaults: band 0 = high-pass at 30 Hz (on), band 1 = bell 1 kHz (on), others spread bells (off)
const DEFAULT_KIND: [f32; BANDS] = [4.0 / 5.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 2.0 / 5.0];
const DEFAULT_FREQ: [f32; BANDS] = [0.0587, 0.5663, 0.2, 0.35, 0.45, 0.65, 0.8, 0.9];

pub static DESCRIPTORS: [ParamDescriptor; 1 + BANDS * 5] = band_descriptors!(0, 1, 2, 3, 4, 5, 6, 7);

pub static PRESETS: [Preset; 5] = [
    Preset { name: "Flat", description: "All bands neutral", values: &[("band0.on", 0.0), ("band1.on", 0.0)] },
    Preset {
        name: "Low cut 80",
        description: "High-pass at 80 Hz, gentle air",
        values: &[("band0.on", 1.0), ("band0.kind", 4.0), ("band0.freq", 80.0), ("band1.on", 1.0), ("band1.kind", 2.0), ("band1.freq", 10_000.0), ("band1.gain", 2.0)],
    },
    Preset {
        name: "Smile",
        description: "Lows and highs up, mids down",
        values: &[
            ("band0.on", 1.0),
            ("band0.kind", 1.0),
            ("band0.freq", 120.0),
            ("band0.gain", 4.0),
            ("band1.on", 1.0),
            ("band1.freq", 800.0),
            ("band1.gain", -4.0),
            ("band1.q", 0.8),
            ("band2.on", 1.0),
            ("band2.kind", 2.0),
            ("band2.freq", 6000.0),
            ("band2.gain", 4.0),
        ],
    },
    Preset {
        name: "Telephone",
        description: "Narrow band 300 Hz – 3 kHz",
        values: &[
            ("band0.on", 1.0),
            ("band0.kind", 4.0),
            ("band0.freq", 300.0),
            ("band0.q", 1.0),
            ("band1.on", 1.0),
            ("band1.kind", 3.0),
            ("band1.freq", 3000.0),
            ("band1.q", 1.0),
            ("band2.on", 1.0),
            ("band2.freq", 1200.0),
            ("band2.gain", 6.0),
            ("band2.q", 1.2),
        ],
    },
    Preset {
        name: "Hum notch",
        description: "Notches at 50 Hz and 100 Hz",
        values: &[
            ("band0.on", 1.0),
            ("band0.kind", 5.0),
            ("band0.freq", 50.0),
            ("band0.q", 12.0),
            ("band1.on", 1.0),
            ("band1.kind", 5.0),
            ("band1.freq", 100.0),
            ("band1.q", 12.0),
        ],
    },
];

pub struct ParametricEq {
    params: Params,
    bands: [[Biquad; 2]; BANDS],
    sample_rate: f32,
    out_gain: f32,
}

impl Default for ParametricEq {
    fn default() -> Self {
        Self::new()
    }
}

impl ParametricEq {
    pub fn new() -> Self {
        Self { params: Params::new(&DESCRIPTORS), bands: [[Biquad::new(); 2]; BANDS], sample_rate: 48_000.0, out_gain: 1.0 }
    }

    pub fn params(&self) -> &Params {
        &self.params
    }

    fn band_target(params: &Params, b: usize, sr: f32) -> Option<Coeffs> {
        if !params.as_bool(band_param(b, B_ENABLED)) {
            return None;
        }
        let kind = KINDS[params.as_index(band_param(b, B_KIND)).min(KINDS.len() - 1)];
        Some(Coeffs::design(kind, params.phys(band_param(b, B_FREQ)), params.phys(band_param(b, B_Q)), params.phys(band_param(b, B_GAIN)), sr))
    }

    /// Magnitude response of the whole EQ (target values) at `freq_hz`, in dB (UI).
    pub fn response_db(&self, freq_hz: f32) -> f32 {
        let mut db = self.params.phys(P_OUTPUT);
        for b in 0..BANDS {
            if let Some(c) = Self::band_target(&self.params, b, self.sample_rate) {
                db += c.response_db(freq_hz, self.sample_rate);
            }
        }
        db
    }

    /// Response of one band alone (UI handles).
    pub fn band_response_db(&self, band: usize, freq_hz: f32) -> f32 {
        Self::band_target(&self.params, band, self.sample_rate).map(|c| c.response_db(freq_hz, self.sample_rate)).unwrap_or(0.0)
    }
}

impl Processor for ParametricEq {
    fn type_id(&self) -> &'static str {
        TYPE_ID
    }
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
    fn descriptors(&self) -> &'static [ParamDescriptor] {
        &DESCRIPTORS
    }
    fn prepare(&mut self, spec: &PrepareSpec) -> Result<(), PrepareError> {
        if !spec.is_valid() {
            return Err(PrepareError::UnsupportedSampleRate);
        }
        self.sample_rate = spec.sample_rate as f32;
        self.params.set_sample_rate(self.sample_rate);
        self.reset();
        Ok(())
    }
    fn process(&mut self, ctx: &mut ProcessContext<'_>) {
        let frames = ctx.audio.frames;
        for ev in ctx.events {
            if let rawdawg_types::events::EventKind::Param { id, value } = ev.kind {
                self.params.set(id, value);
            }
        }
        ctx.audio.copy_input_to_output();
        let out_db = self.params.phys_smoothed(P_OUTPUT, frames);
        let target_gain = rawdawg_dsp::math::db_to_lin(out_db);
        let chans = ctx.audio.outputs.len().min(2);
        // smoothed band targets (frequency/gain/Q smoothers advance once per block)
        let mut targets: [Option<Coeffs>; BANDS] = [None; BANDS];
        for (b, t) in targets.iter_mut().enumerate() {
            if self.params.as_bool(band_param(b, B_ENABLED)) {
                let kind = KINDS[self.params.as_index(band_param(b, B_KIND)).min(KINDS.len() - 1)];
                let f = self.params.phys_smoothed(band_param(b, B_FREQ), frames);
                let g = self.params.phys_smoothed(band_param(b, B_GAIN), frames);
                let q = self.params.phys_smoothed(band_param(b, B_Q), frames);
                *t = Some(Coeffs::design(kind, f, q, g, self.sample_rate));
            }
        }
        for c in 0..chans {
            let buf = &mut ctx.audio.outputs[c][..frames];
            for (b, t) in targets.iter().enumerate() {
                match t {
                    Some(coeffs) => self.bands[b][c].process_block_ramped(buf, *coeffs),
                    None => {
                        self.bands[b][c].coeffs = Coeffs::IDENTITY;
                        self.bands[b][c].reset();
                    }
                }
            }
            let g0 = self.out_gain;
            let n = frames.max(1) as f32;
            for (i, s) in buf.iter_mut().enumerate() {
                *s *= g0 + (target_gain - g0) * (i + 1) as f32 / n;
            }
        }
        self.out_gain = target_gain;
    }
    fn reset(&mut self) {
        for b in &mut self.bands {
            for c in b {
                c.reset();
            }
        }
        self.params.snap_all();
        self.out_gain = rawdawg_dsp::math::db_to_lin(self.params.phys(P_OUTPUT));
    }
    fn param(&self, id: ParamId) -> Option<f32> {
        self.params.param(id)
    }
    fn set_param(&mut self, id: ParamId, normalized: f32) -> bool {
        self.params.set(id, normalized)
    }
    fn save_state(&self) -> Vec<u8> {
        self.params.save(STATE_VERSION)
    }
    fn load_state(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.params.load(bytes, STATE_VERSION)?;
        self.out_gain = rawdawg_dsp::math::db_to_lin(self.params.phys(P_OUTPUT));
        Ok(())
    }
}
