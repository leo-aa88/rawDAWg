//! Pitch / formant shifter (`native.fx.pitch`): a phase-vocoder pitch shifter (bins are
//! re-mapped by the ratio with their measured true frequencies, so steady tones land on
//! pitch and the duration never changes) combined with a cepstral spectral-envelope stage
//! that moves formants independently or holds them in place while the pitch moves.
//! Frame 2048, hop 512, Hann analysis and synthesis; latency one frame, constant.

use crate::common::{Params, Preset, mix_wet};
use rawdawg_dsp::processor::{PrepareError, PrepareSpec, ProcessContext, Processor, Tail};
use rawdawg_dsp::stft::{Stft, interp, princarg};
use rawdawg_types::events::EventKind;
use rawdawg_types::params::{ParamCategory, ParamMapping, ParamUnit};
use rawdawg_types::{ParamDescriptor, ParamId};
use realfft::num_complex::Complex;
use realfft::{ComplexToReal, RealFftPlanner, RealToComplex};
use std::sync::Arc;

pub const TYPE_ID: &str = "native.fx.pitch";
pub const DISPLAY_NAME: &str = "Pitch / formant";
pub const STATE_VERSION: u32 = 1;
/// STFT frame and hop (75 % overlap).
pub const FRAME: usize = 2048;
pub const HOP: usize = 512;
const BINS: usize = FRAME / 2 + 1;
/// Envelope gain correction is bounded (dB) so warping into noise floors cannot explode.
const MAX_BOOST_DB: f32 = 30.0;
const MAX_CUT_DB: f32 = 60.0;

pub const P_PITCH: u32 = 1;
pub const P_CENTS: u32 = 2;
pub const P_FORMANT: u32 = 3;
pub const P_PRESERVE: u32 = 4;
pub const P_MIX: u32 = 5;

pub static DESCRIPTORS: [ParamDescriptor; 5] = [
    ParamDescriptor::continuous(P_PITCH, "pitch", "Pitch", ParamUnit::Semitones, ParamMapping::Linear { min: -24.0, max: 24.0 }, 0.5, ParamCategory::Pitch).with_smoothing(50.0),
    ParamDescriptor::continuous(P_CENTS, "cents", "Fine", ParamUnit::Cents, ParamMapping::Linear { min: -100.0, max: 100.0 }, 0.5, ParamCategory::Pitch).with_smoothing(50.0),
    ParamDescriptor::continuous(P_FORMANT, "formant", "Formant", ParamUnit::Semitones, ParamMapping::Linear { min: -12.0, max: 12.0 }, 0.5, ParamCategory::Effects)
        .with_smoothing(50.0),
    ParamDescriptor::continuous(P_PRESERVE, "preserve", "Preserve formants", ParamUnit::None, ParamMapping::Toggle, 0.0, ParamCategory::Effects).stepped(),
    ParamDescriptor::continuous(P_MIX, "mix", "Mix", ParamUnit::Percent, ParamMapping::Linear { min: 0.0, max: 100.0 }, 1.0, ParamCategory::Mix).with_smoothing(20.0),
];

pub static PRESETS: [Preset; 7] = [
    Preset { name: "Octave up", description: "+12 semitones", values: &[("pitch", 12.0)] },
    Preset { name: "Octave down", description: "−12 semitones", values: &[("pitch", -12.0)] },
    Preset { name: "Fifth up", description: "+7 semitones, half wet", values: &[("pitch", 7.0), ("mix", 50.0)] },
    Preset { name: "Detune widen", description: "+8 cents blended with the dry signal", values: &[("cents", 8.0), ("mix", 50.0)] },
    Preset { name: "Chipmunk", description: "Formants up, pitch unchanged", values: &[("formant", 6.0)] },
    Preset { name: "Giant", description: "Formants down, pitch unchanged", values: &[("formant", -6.0)] },
    Preset { name: "Up a third, same voice", description: "+4 semitones keeping the formants", values: &[("pitch", 4.0), ("preserve", 1.0)] },
];

/// One channel: the STFT engine plus the vocoder state its frames are processed with
/// (kept as separate fields so both can be borrowed at once without any copy).
struct Channel {
    stft: Stft,
    voc: VocState,
}

/// Per-channel vocoder state (all buffers fixed at prepare time).
struct VocState {
    last_phase: Vec<f32>,
    phase_acc: Vec<f32>,
    mag: Vec<f32>,
    freq: Vec<f32>,
    mag_s: Vec<f32>,
    freq_s: Vec<f32>,
    logmag: Vec<Complex<f32>>,
    cep: Vec<f32>,
    env_c: Vec<Complex<f32>>,
    env: Vec<f32>,
}

fn channel() -> Channel {
    Channel { stft: Stft::new(FRAME, HOP), voc: voc_state() }
}

fn voc_state() -> VocState {
    VocState {
        last_phase: vec![0.0; BINS],
        phase_acc: vec![0.0; BINS],
        mag: vec![0.0; BINS],
        freq: vec![0.0; BINS],
        mag_s: vec![0.0; BINS],
        freq_s: vec![0.0; BINS],
        logmag: vec![Complex::new(0.0, 0.0); BINS],
        cep: vec![0.0; FRAME],
        env_c: vec![Complex::new(0.0, 0.0); BINS],
        env: vec![0.0; BINS],
    }
}

/// Cepstral envelope helper (its own FFT pair over the log magnitude).
struct Cepstrum {
    fft: Arc<dyn RealToComplex<f32>>,
    ifft: Arc<dyn ComplexToReal<f32>>,
    scratch_f: Vec<Complex<f32>>,
    scratch_i: Vec<Complex<f32>>,
    lifter: usize,
}

impl Cepstrum {
    fn new(sr: f32) -> Self {
        let mut planner = RealFftPlanner::<f32>::new();
        let fft = planner.plan_fft_forward(FRAME);
        let ifft = planner.plan_fft_inverse(FRAME);
        let scratch_f = fft.make_scratch_vec();
        let scratch_i = ifft.make_scratch_vec();
        // quefrency cut-off: envelope detail coarser than ~300 Hz (harmonic ripple removed)
        let lifter = ((sr / 300.0) as usize).clamp(16, FRAME / 4);
        Self { fft, ifft, scratch_f, scratch_i, lifter }
    }

    /// Cepstrally smoothed log-magnitude envelope of `spec` into `ch.env`.
    #[allow(clippy::needless_range_loop, reason = "bin index addresses several parallel spectra")]
    fn envelope(&mut self, ch: &mut VocState, spec: &[Complex<f32>]) {
        for k in 0..BINS {
            ch.logmag[k] = Complex::new((spec[k].norm() + 1e-9).ln(), 0.0);
        }
        let _ = self.ifft.process_with_scratch(&mut ch.logmag, &mut ch.cep, &mut self.scratch_i);
        let l = self.lifter;
        for i in l..FRAME - l {
            ch.cep[i] = 0.0;
        }
        let norm = 1.0 / FRAME as f32;
        for v in ch.cep.iter_mut() {
            *v *= norm;
        }
        let _ = self.fft.process_with_scratch(&mut ch.cep, &mut ch.env_c, &mut self.scratch_f);
        for k in 0..BINS {
            ch.env[k] = ch.env_c[k].re;
        }
    }
}

/// Shift the pitch of one analysed frame by `ratio` and impose the envelope warped by
/// `formant` (relative to the shifted spectrum's own envelope). Returns whether the frame
/// was resynthesised.
#[allow(clippy::needless_range_loop, reason = "bin index addresses several parallel spectra")]
fn vocode(ch: &mut VocState, cep: &mut Cepstrum, spec: &mut [Complex<f32>], ratio: f32, formant: f32) -> bool {
    let shift = (ratio - 1.0).abs() > 1e-4;
    let warp = (formant - 1.0).abs() > 1e-4;
    if warp {
        cep.envelope(ch, spec);
    }
    let expct = std::f32::consts::TAU * HOP as f32 / FRAME as f32;
    let osamp = (FRAME / HOP) as f32;
    let (lo, hi) = (-MAX_CUT_DB / 8.6859, MAX_BOOST_DB / 8.6859);
    if shift {
        // analysis: true frequency of every bin from the phase advance since the last hop
        for k in 0..BINS {
            let phase = spec[k].arg();
            let mut d = phase - ch.last_phase[k];
            ch.last_phase[k] = phase;
            d -= k as f32 * expct;
            d = princarg(d);
            ch.mag[k] = spec[k].norm();
            ch.freq[k] = k as f32 + osamp * d / std::f32::consts::TAU;
        }
        ch.mag_s.fill(0.0);
        ch.freq_s.fill(0.0);
        for k in 0..BINS {
            let idx = (k as f32 * ratio).round() as usize;
            if idx < BINS {
                ch.mag_s[idx] += ch.mag[k];
                ch.freq_s[idx] = ch.freq[k] * ratio;
            }
        }
        // envelope correction: the shifted spectrum carries env(k/ratio); impose env(k/(ratio·formant))
        for k in 0..BINS {
            let g = if warp {
                let have = interp(&ch.env, k as f32 / ratio);
                let want = interp(&ch.env, k as f32 / (ratio * formant));
                (want - have).clamp(lo, hi).exp()
            } else {
                1.0
            };
            ch.phase_acc[k] += ch.freq_s[k] * expct;
            let m = ch.mag_s[k] * g;
            spec[k] = Complex::new(m * ch.phase_acc[k].cos(), m * ch.phase_acc[k].sin());
        }
        true
    } else {
        // unity pitch: keep the analysed phases (exact reconstruction), sync the accumulators
        for k in 0..BINS {
            let phase = spec[k].arg();
            ch.last_phase[k] = phase;
            ch.phase_acc[k] = phase;
            if warp {
                let want = interp(&ch.env, k as f32 / formant);
                let g = (want - ch.env[k]).clamp(lo, hi).exp();
                spec[k] *= g;
            }
        }
        false
    }
}

pub struct PitchShift {
    params: Params,
    sr: f32,
    ch: [Channel; 2],
    cep: Cepstrum,
}

impl Default for PitchShift {
    fn default() -> Self {
        Self::new()
    }
}

impl PitchShift {
    pub fn new() -> Self {
        Self { params: Params::new(&DESCRIPTORS), sr: 48_000.0, ch: [channel(), channel()], cep: Cepstrum::new(48_000.0) }
    }
    /// Frequency ratio of the pitch stage.
    pub fn pitch_ratio(&self) -> f32 {
        2f32.powf((self.params.phys(P_PITCH) + self.params.phys(P_CENTS) / 100.0) / 12.0)
    }
}

impl Processor for PitchShift {
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
        self.sr = spec.sample_rate as f32;
        self.params.set_sample_rate(self.sr);
        self.ch = [channel(), channel()];
        self.cep = Cepstrum::new(self.sr);
        self.reset();
        Ok(())
    }
    fn process(&mut self, ctx: &mut ProcessContext<'_>) {
        let frames = ctx.audio.frames;
        for ev in ctx.events {
            if let EventKind::Param { id, value } = ev.kind {
                self.params.set(id, value);
            }
        }
        ctx.audio.copy_input_to_output();
        if ctx.audio.outputs.len() < 2 {
            return;
        }
        let semis = self.params.phys_smoothed(P_PITCH, frames) + self.params.phys_smoothed(P_CENTS, frames) / 100.0;
        let ratio = 2f32.powf(semis / 12.0);
        let formant_semis = self.params.phys_smoothed(P_FORMANT, frames);
        let preserve = self.params.as_bool(P_PRESERVE);
        // the shifted spectrum's envelope is already scaled by `ratio`; preserving means
        // warping it back, so the total envelope warp relative to the shifted one is formant/ratio
        let formant = 2f32.powf(formant_semis / 12.0) / if preserve { ratio } else { 1.0 };
        let mix = (self.params.phys_smoothed(P_MIX, frames) / 100.0).clamp(0.0, 1.0);
        let (first, rest) = ctx.audio.outputs.split_at_mut(1);
        let (l, r) = (&mut first[0][..frames], &mut rest[0][..frames]);
        let inputs = ctx.audio.inputs;
        for i in 0..frames {
            let xs = [inputs[0][i], inputs[1][i]];
            let mut ys = [0.0f32; 2];
            for c in 0..2 {
                let Channel { stft, voc } = &mut self.ch[c];
                let cep = &mut self.cep;
                let w = stft.push(xs[c], |spec| vocode(voc, cep, spec, ratio, formant));
                ys[c] = mix_wet(xs[c], w, mix);
            }
            l[i] = ys[0];
            r[i] = ys[1];
        }
    }
    fn reset(&mut self) {
        for c in &mut self.ch {
            c.stft.reset();
            c.voc.last_phase.fill(0.0);
            c.voc.phase_acc.fill(0.0);
        }
        self.params.snap_all();
    }
    fn latency_frames(&self) -> u32 {
        FRAME as u32
    }
    fn tail(&self) -> Tail {
        Tail::Frames(2 * FRAME as u32)
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
        self.params.load(bytes, STATE_VERSION)
    }
}
