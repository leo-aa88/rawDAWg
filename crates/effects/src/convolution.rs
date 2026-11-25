//! Convolution reverb (`native.fx.convolution`): a user impulse response from a
//! project asset, non-uniformly partitioned FFT convolution, optional trim/normalize
//! applied off-thread when the IR is set, IR length bounded (30 s). Mono IRs feed both
//! channels; stereo IRs map L→L, R→R (true-stereo four-channel IRs are not supported in v1).
//!
//! Two stages, exact (equal to direct convolution up to float rounding):
//! - **head**: the first [`HEAD_LEN`] samples of the IR in [`PARTITION`]-sample partitions
//!   (uniformly partitioned overlap-save; the partition is the reported latency);
//! - **tail**: the rest in [`TAIL_PART`]-sample partitions. The tail filter starts two tail
//!   chunks into the IR, so each chunk's FFT, multiply-accumulate and inverse FFT can be spread
//!   evenly over the [`SPREAD`] small blocks while the next chunk arrives: no block carries the
//!   whole tail's cost. With a 30 s IR this measured about 17× cheaper per block than
//!   256-sample partitions throughout (0.31 ms vs 5.47 ms mean at 48 kHz/256 frames;
//!   `cargo xtask bench-audio`, `docs/verification/benchmarks.md`).

use crate::common::{Params, Preset};
use rawdawg_dsp::biquad::{Biquad, BiquadKind, Coeffs};
use rawdawg_dsp::delayline::DelayLine;
use rawdawg_dsp::processor::{PrepareError, PrepareSpec, ProcessContext, Processor, Tail};
use rawdawg_types::events::EventKind;
use rawdawg_types::params::{ParamCategory, ParamMapping, ParamUnit};
use rawdawg_types::{ParamDescriptor, ParamId};
use realfft::num_complex::Complex;
use realfft::{ComplexToReal, RealFftPlanner, RealToComplex};
use std::sync::Arc;

pub const TYPE_ID: &str = "native.fx.convolution";
pub const DISPLAY_NAME: &str = "Convolution";
pub const STATE_VERSION: u32 = 1;
pub const PARTITION: usize = 256;
/// Tail partition size (FFT size twice this).
pub const TAIL_PART: usize = 4096;
/// The head covers two tail partitions, which gives the tail one full chunk of time to compute.
pub const HEAD_LEN: usize = 2 * TAIL_PART;
/// Small blocks per tail chunk: the tail work is spread over this many `convolve_block` calls.
pub const SPREAD: usize = TAIL_PART / PARTITION;
pub const MAX_IR_SECONDS: f32 = 30.0;

pub const P_MIX: u32 = 1;
pub const P_PREDELAY: u32 = 2;
pub const P_GAIN: u32 = 3;
pub const P_LOWCUT: u32 = 4;
pub const P_HIGHCUT: u32 = 5;
pub const P_NORMALIZE: u32 = 6;
pub const P_TRIM: u32 = 7;

pub static DESCRIPTORS: [ParamDescriptor; 7] = [
    ParamDescriptor::continuous(P_MIX, "mix", "Mix", ParamUnit::Percent, ParamMapping::Linear { min: 0.0, max: 100.0 }, 0.3, ParamCategory::Mix).with_smoothing(20.0),
    ParamDescriptor::continuous(P_PREDELAY, "predelay", "Pre-delay", ParamUnit::Milliseconds, ParamMapping::Linear { min: 0.0, max: 200.0 }, 0.0, ParamCategory::Effects)
        .with_smoothing(100.0),
    ParamDescriptor::continuous(P_GAIN, "gain", "IR gain", ParamUnit::Decibel, ParamMapping::Linear { min: -24.0, max: 24.0 }, 0.5, ParamCategory::Effects).with_smoothing(20.0),
    ParamDescriptor::continuous(P_LOWCUT, "lowcut", "Low cut", ParamUnit::Hertz, ParamMapping::Log { min: 20.0, max: 1000.0 }, 0.0, ParamCategory::Effects).with_smoothing(50.0),
    ParamDescriptor::continuous(P_HIGHCUT, "highcut", "High cut", ParamUnit::Hertz, ParamMapping::Log { min: 1000.0, max: 20_000.0 }, 1.0, ParamCategory::Effects)
        .with_smoothing(50.0),
    ParamDescriptor::continuous(P_NORMALIZE, "normalize", "Normalize IR", ParamUnit::None, ParamMapping::Toggle, 1.0, ParamCategory::Effects).stepped(),
    ParamDescriptor::continuous(P_TRIM, "trim", "Trim IR silence", ParamUnit::None, ParamMapping::Toggle, 1.0, ParamCategory::Effects).stepped(),
];

pub static PRESETS: [Preset; 3] = [
    Preset { name: "Natural blend", description: "30 % wet, normalized IR", values: &[("mix", 30.0)] },
    Preset { name: "Wet only", description: "100 % wet for sends", values: &[("mix", 100.0)] },
    Preset { name: "Distant", description: "Dark, 40 ms pre-delay", values: &[("mix", 45.0), ("predelay", 40.0), ("highcut", 4000.0), ("lowcut", 120.0)] },
];

/// An impulse response ready for the effect (effect-crate owned; the engine converts assets).
#[derive(Clone, Debug)]
pub struct ImpulseResponse {
    pub name: String,
    pub sample_rate: u32,
    pub channels: Vec<Vec<f32>>,
}

/// Information about the loaded IR for the UI.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct IrInfo {
    pub name: String,
    pub source_rate: u32,
    pub source_frames: usize,
    pub channels: usize,
    pub used_frames: usize,
    pub truncated: bool,
    pub resampled: bool,
}

struct ChannelConv {
    /// Partition spectra of the IR (K × (P+1)).
    h: Vec<Vec<Complex<f32>>>,
    /// Frequency-delay line of input spectra (K × (P+1)).
    fdl: Vec<Vec<Complex<f32>>>,
    fdl_pos: usize,
    in_fifo: Vec<f32>,
    out_fifo: Vec<f32>,
    fifo_pos: usize,
    time: Vec<f32>,
    spec: Vec<Complex<f32>>,
    acc: Vec<Complex<f32>>,
    prev_block: Vec<f32>,
    pre: DelayLine,
    lowcut: Biquad,
    highcut: Biquad,
    tail: TailConv,
}

/// Tail stage of one channel (see the module notes).
struct TailConv {
    /// Tail partition spectra (K × (TAIL_PART + 1)); empty when the IR fits in the head.
    h: Vec<Vec<Complex<f32>>>,
    fdl: Vec<Vec<Complex<f32>>>,
    fdl_pos: usize,
    /// Input chunks m-1 and m (the pair transformed at slot 0) and the chunk being collected.
    prev: Vec<f32>,
    last: Vec<f32>,
    collect: Vec<f32>,
    time: Vec<f32>,
    spec: Vec<Complex<f32>>,
    acc: Vec<Complex<f32>>,
    /// Output segment being read and the one being produced.
    cur: Vec<f32>,
    next: Vec<f32>,
    /// Small block index within the current chunk (0..SPREAD).
    slot: usize,
}

impl TailConv {
    fn new() -> Self {
        let zc = || vec![Complex::new(0.0, 0.0); TAIL_PART + 1];
        Self {
            h: Vec::new(),
            fdl: Vec::new(),
            fdl_pos: 0,
            prev: vec![0.0; TAIL_PART],
            last: vec![0.0; TAIL_PART],
            collect: vec![0.0; TAIL_PART],
            time: vec![0.0; 2 * TAIL_PART],
            spec: zc(),
            acc: zc(),
            cur: vec![0.0; TAIL_PART],
            next: vec![0.0; TAIL_PART],
            slot: 0,
        }
    }
    fn reset(&mut self) {
        for f in &mut self.fdl {
            f.fill(Complex::new(0.0, 0.0));
        }
        self.fdl_pos = 0;
        for b in [&mut self.prev, &mut self.last, &mut self.collect, &mut self.cur, &mut self.next] {
            b.fill(0.0);
        }
        self.acc.fill(Complex::new(0.0, 0.0));
        self.slot = 0;
    }
}

impl ChannelConv {
    fn new(sr: f32) -> Self {
        Self {
            h: Vec::new(),
            fdl: Vec::new(),
            fdl_pos: 0,
            in_fifo: vec![0.0; PARTITION],
            out_fifo: vec![0.0; PARTITION],
            fifo_pos: 0,
            time: vec![0.0; 2 * PARTITION],
            spec: vec![Complex::new(0.0, 0.0); PARTITION + 1],
            acc: vec![Complex::new(0.0, 0.0); PARTITION + 1],
            prev_block: vec![0.0; PARTITION],
            pre: DelayLine::new((0.2 * sr) as usize + 8),
            lowcut: Biquad::new(),
            highcut: Biquad::new(),
            tail: TailConv::new(),
        }
    }
    fn reset(&mut self) {
        for f in &mut self.fdl {
            f.fill(Complex::new(0.0, 0.0));
        }
        self.fdl_pos = 0;
        self.in_fifo.fill(0.0);
        self.out_fifo.fill(0.0);
        self.fifo_pos = 0;
        self.prev_block.fill(0.0);
        self.pre.reset();
        self.lowcut.reset();
        self.highcut.reset();
        self.tail.reset();
    }
}

pub struct Convolution {
    params: Params,
    sr: f32,
    ir_asset: Option<String>,
    info: Option<IrInfo>,
    ch: [ChannelConv; 2],
    fft: Arc<dyn RealToComplex<f32>>,
    ifft: Arc<dyn ComplexToReal<f32>>,
    scratch_f: Vec<Complex<f32>>,
    scratch_i: Vec<Complex<f32>>,
    tail_fft: Arc<dyn RealToComplex<f32>>,
    tail_ifft: Arc<dyn ComplexToReal<f32>>,
    tail_scratch_f: Vec<Complex<f32>>,
    tail_scratch_i: Vec<Complex<f32>>,
    ir_len: usize,
}

impl Default for Convolution {
    fn default() -> Self {
        Self::new()
    }
}

impl Convolution {
    pub fn new() -> Self {
        let mut planner = RealFftPlanner::<f32>::new();
        let fft = planner.plan_fft_forward(2 * PARTITION);
        let ifft = planner.plan_fft_inverse(2 * PARTITION);
        let scratch_f = fft.make_scratch_vec();
        let scratch_i = ifft.make_scratch_vec();
        let tail_fft = planner.plan_fft_forward(2 * TAIL_PART);
        let tail_ifft = planner.plan_fft_inverse(2 * TAIL_PART);
        let tail_scratch_f = tail_fft.make_scratch_vec();
        let tail_scratch_i = tail_ifft.make_scratch_vec();
        Self {
            params: Params::new(&DESCRIPTORS),
            sr: 48_000.0,
            ir_asset: None,
            info: None,
            ch: [ChannelConv::new(48_000.0), ChannelConv::new(48_000.0)],
            fft,
            ifft,
            scratch_f,
            scratch_i,
            tail_fft,
            tail_ifft,
            tail_scratch_f,
            tail_scratch_i,
            ir_len: 0,
        }
    }

    pub fn ir_asset_id(&self) -> Option<&str> {
        self.ir_asset.as_deref()
    }
    pub fn set_ir_asset_id(&mut self, id: Option<String>) {
        self.ir_asset = id;
    }
    pub fn info(&self) -> Option<&IrInfo> {
        self.info.as_ref()
    }
    pub fn has_ir(&self) -> bool {
        self.ir_len > 0
    }

    /// Non-RT: install (or clear) the impulse response; resamples, trims, normalizes and partitions.
    pub fn set_ir(&mut self, ir: Option<Arc<ImpulseResponse>>) {
        self.ir_len = 0;
        for c in &mut self.ch {
            c.h.clear();
            c.fdl.clear();
            c.tail.h.clear();
            c.tail.fdl.clear();
            c.tail.reset();
        }
        let Some(ir) = ir else {
            self.info = None;
            return;
        };
        let max_frames = (MAX_IR_SECONDS * self.sr) as usize;
        let resampled = ir.sample_rate != self.sr as u32;
        let mut info = IrInfo {
            name: ir.name.clone(),
            source_rate: ir.sample_rate,
            source_frames: ir.channels.first().map(|c| c.len()).unwrap_or(0),
            channels: ir.channels.len(),
            used_frames: 0,
            truncated: false,
            resampled,
        };
        let mut chans: Vec<Vec<f32>> = ir
            .channels
            .iter()
            .take(2)
            .map(|c| {
                if !resampled {
                    return c.clone();
                }
                // linear resampling to the project rate (slight high-frequency loss; documented)
                let ratio = f64::from(ir.sample_rate) / f64::from(self.sr);
                let n = (c.len() as f64 / ratio).floor() as usize;
                (0..n)
                    .map(|i| {
                        let pos = i as f64 * ratio;
                        let k = pos.floor() as usize;
                        let f = (pos - k as f64) as f32;
                        let a = c.get(k).copied().unwrap_or(0.0);
                        let b = c.get(k + 1).copied().unwrap_or(0.0);
                        a + (b - a) * f
                    })
                    .collect()
            })
            .collect();
        if chans.is_empty() {
            self.info = None;
            return;
        }
        if self.params.as_bool(P_TRIM) {
            // drop trailing silence below −80 dBFS (keep at least one partition)
            for c in &mut chans {
                let last = c.iter().rposition(|v| v.abs() > 1e-4).unwrap_or(0) + 1;
                c.truncate(last.max(PARTITION.min(c.len())));
            }
        }
        for c in &mut chans {
            if c.len() > max_frames {
                c.truncate(max_frames);
                info.truncated = true;
            }
        }
        if self.params.as_bool(P_NORMALIZE) {
            // unity energy: a dry impulse through the IR keeps its loudness
            let energy: f32 = chans.iter().map(|c| c.iter().map(|v| v * v).sum::<f32>()).sum::<f32>() / chans.len() as f32;
            if energy > 1e-12 {
                let g = 1.0 / energy.sqrt();
                for c in &mut chans {
                    for v in c.iter_mut() {
                        *v *= g;
                    }
                }
            }
        }
        let len = chans.iter().map(|c| c.len()).max().unwrap_or(0);
        info.used_frames = len;
        self.ir_len = len;
        // head: the first HEAD_LEN samples in small partitions
        let k = len.min(HEAD_LEN).div_ceil(PARTITION).max(1);
        // tail: the rest in TAIL_PART partitions, starting at HEAD_LEN
        let k_tail = len.saturating_sub(HEAD_LEN).div_ceil(TAIL_PART);
        let mut input = vec![0.0f32; 2 * PARTITION];
        let mut tail_input = vec![0.0f32; 2 * TAIL_PART];
        for (ci, conv) in self.ch.iter_mut().enumerate() {
            let src = &chans[ci.min(chans.len() - 1)];
            conv.h = Vec::with_capacity(k);
            for part in 0..k {
                input.fill(0.0);
                let start = part * PARTITION;
                for (i, v) in input.iter_mut().take(PARTITION).enumerate() {
                    if start + i < HEAD_LEN {
                        *v = src.get(start + i).copied().unwrap_or(0.0);
                    }
                }
                let mut spec = self.fft.make_output_vec();
                let _ = self.fft.process_with_scratch(&mut input, &mut spec, &mut self.scratch_f);
                conv.h.push(spec);
            }
            conv.fdl = (0..k).map(|_| vec![Complex::new(0.0, 0.0); PARTITION + 1]).collect();
            conv.fdl_pos = 0;
            conv.tail.h = Vec::with_capacity(k_tail);
            for part in 0..k_tail {
                tail_input.fill(0.0);
                let start = HEAD_LEN + part * TAIL_PART;
                for (i, v) in tail_input.iter_mut().take(TAIL_PART).enumerate() {
                    *v = src.get(start + i).copied().unwrap_or(0.0);
                }
                let mut spec = self.tail_fft.make_output_vec();
                let _ = self.tail_fft.process_with_scratch(&mut tail_input, &mut spec, &mut self.tail_scratch_f);
                conv.tail.h.push(spec);
            }
            conv.tail.fdl = (0..k_tail).map(|_| vec![Complex::new(0.0, 0.0); TAIL_PART + 1]).collect();
            conv.tail.reset();
        }
        self.info = Some(info);
    }

    /// Process one partition for channel `c` (input block already in `in_fifo`): writes `out_fifo`.
    fn convolve_block(&mut self, c: usize) {
        let ch = &mut self.ch[c];
        if ch.h.is_empty() {
            ch.out_fifo.fill(0.0);
            return;
        }
        let k = ch.h.len();
        // time buffer = [previous block, current block]
        ch.time[..PARTITION].copy_from_slice(&ch.prev_block);
        ch.time[PARTITION..].copy_from_slice(&ch.in_fifo);
        ch.prev_block.copy_from_slice(&ch.in_fifo);
        let _ = self.fft.process_with_scratch(&mut ch.time, &mut ch.spec, &mut self.scratch_f);
        ch.fdl[ch.fdl_pos].copy_from_slice(&ch.spec);
        for v in ch.acc.iter_mut() {
            *v = Complex::new(0.0, 0.0);
        }
        for p in 0..k {
            let idx = (ch.fdl_pos + k - p) % k;
            let x = &ch.fdl[idx];
            let h = &ch.h[p];
            for b in 0..=PARTITION {
                ch.acc[b] += x[b] * h[b];
            }
        }
        ch.fdl_pos = (ch.fdl_pos + 1) % k;
        let _ = self.ifft.process_with_scratch(&mut ch.acc, &mut ch.time, &mut self.scratch_i);
        let norm = 1.0 / (2 * PARTITION) as f32;
        for i in 0..PARTITION {
            ch.out_fifo[i] = ch.time[PARTITION + i] * norm;
        }
        // ---- tail stage
        let t = &mut ch.tail;
        let kt = t.h.len();
        if kt == 0 {
            return;
        }
        let slot = t.slot;
        // this block's output: add the tail segment computed during the previous chunk
        for i in 0..PARTITION {
            ch.out_fifo[i] += t.cur[slot * PARTITION + i];
        }
        // collect this block's input into the chunk being gathered
        t.collect[slot * PARTITION..(slot + 1) * PARTITION].copy_from_slice(&ch.in_fifo);
        // one share of the work for the previous chunk
        if slot == 0 {
            t.time[..TAIL_PART].copy_from_slice(&t.prev);
            t.time[TAIL_PART..].copy_from_slice(&t.last);
            let _ = self.tail_fft.process_with_scratch(&mut t.time, &mut t.spec, &mut self.tail_scratch_f);
            t.fdl[t.fdl_pos].copy_from_slice(&t.spec);
            t.acc.fill(Complex::new(0.0, 0.0));
        }
        let (from, to) = (slot * kt / SPREAD, (slot + 1) * kt / SPREAD);
        for p in from..to {
            let x = &t.fdl[(t.fdl_pos + kt - p) % kt];
            let h = &t.h[p];
            for b in 0..=TAIL_PART {
                t.acc[b] += x[b] * h[b];
            }
        }
        if slot == SPREAD - 1 {
            t.fdl_pos = (t.fdl_pos + 1) % kt;
            let _ = self.tail_ifft.process_with_scratch(&mut t.acc, &mut t.time, &mut self.tail_scratch_i);
            let norm = 1.0 / (2 * TAIL_PART) as f32;
            for i in 0..TAIL_PART {
                t.next[i] = t.time[TAIL_PART + i] * norm;
            }
            // next round: read the new segment, transform [last, collected]
            std::mem::swap(&mut t.cur, &mut t.next);
            std::mem::swap(&mut t.prev, &mut t.last);
            std::mem::swap(&mut t.last, &mut t.collect);
            t.slot = 0;
        } else {
            t.slot = slot + 1;
        }
    }
}

impl Processor for Convolution {
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
        self.ch = [ChannelConv::new(self.sr), ChannelConv::new(self.sr)];
        self.ir_len = 0;
        self.info = None;
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
        let mix = (self.params.phys_smoothed(P_MIX, frames) / 100.0).clamp(0.0, 1.0);
        let gain = rawdawg_dsp::math::db_to_lin(self.params.phys_smoothed(P_GAIN, frames));
        let pre = (self.params.phys_smoothed(P_PREDELAY, frames) / 1000.0 * self.sr).clamp(0.0, self.ch[0].pre.capacity() as f32 - 4.0);
        let lowcut_hz = self.params.phys_smoothed(P_LOWCUT, frames);
        let highcut_hz = self.params.phys_smoothed(P_HIGHCUT, frames);
        // the filters are true bypasses at their extremes (no colouration of the IR)
        let lc_on = lowcut_hz > 21.0;
        let hc_on = highcut_hz < 19_900.0;
        let lc = Coeffs::design(BiquadKind::HighPass, lowcut_hz, 0.707, 0.0, self.sr);
        let hc = Coeffs::design(BiquadKind::LowPass, highcut_hz.min(self.sr * 0.45), 0.707, 0.0, self.sr);
        for c in &mut self.ch {
            c.lowcut.coeffs = lc;
            c.highcut.coeffs = hc;
            if !lc_on {
                c.lowcut.reset();
            }
            if !hc_on {
                c.highcut.reset();
            }
        }
        let (first, rest) = ctx.audio.outputs.split_at_mut(1);
        let (l, r) = (&mut first[0][..frames], &mut rest[0][..frames]);
        let inputs = ctx.audio.inputs;
        for i in 0..frames {
            let xs = [inputs[0][i], inputs[1][i]];
            let mut wet = [0.0f32; 2];
            for c in 0..2 {
                let pos = self.ch[c].fifo_pos;
                // the output FIFO holds the convolution of the previous partition: P frames of latency
                wet[c] = self.ch[c].out_fifo[pos];
                self.ch[c].in_fifo[pos] = xs[c];
            }
            // advance both channels together
            let pos = self.ch[0].fifo_pos + 1;
            if pos >= PARTITION {
                self.convolve_block(0);
                self.convolve_block(1);
                self.ch[0].fifo_pos = 0;
                self.ch[1].fifo_pos = 0;
            } else {
                self.ch[0].fifo_pos = pos;
                self.ch[1].fifo_pos = pos;
            }
            for (c, w_out) in wet.iter_mut().enumerate() {
                let ch = &mut self.ch[c];
                ch.pre.push(*w_out);
                let mut w = ch.pre.read_frac(pre) * gain;
                if lc_on {
                    w = ch.lowcut.process(w);
                }
                if hc_on {
                    w = ch.highcut.process(w);
                }
                *w_out = w;
            }
            l[i] = xs[0] + (wet[0] - xs[0]) * mix;
            r[i] = xs[1] + (wet[1] - xs[1]) * mix;
        }
    }
    fn reset(&mut self) {
        for c in &mut self.ch {
            c.reset();
        }
        self.params.snap_all();
    }
    fn latency_frames(&self) -> u32 {
        PARTITION as u32
    }
    fn tail(&self) -> Tail {
        Tail::Frames(self.ir_len as u32 + PARTITION as u32 + (0.2 * self.sr) as u32)
    }
    fn param(&self, id: ParamId) -> Option<f32> {
        self.params.param(id)
    }
    fn set_param(&mut self, id: ParamId, normalized: f32) -> bool {
        self.params.set(id, normalized)
    }
    fn save_state(&self) -> Vec<u8> {
        let mut v: serde_json::Value = serde_json::from_slice(&self.params.save(STATE_VERSION)).unwrap_or(serde_json::Value::Null);
        if let Some(obj) = v.as_object_mut() {
            obj.insert("ir_asset".into(), self.ir_asset.clone().map(serde_json::Value::String).unwrap_or(serde_json::Value::Null));
        }
        serde_json::to_vec(&v).unwrap_or_default()
    }
    fn load_state(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.params.load(bytes, STATE_VERSION)?;
        let v: serde_json::Value = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
        self.ir_asset = v.get("ir_asset").and_then(|x| x.as_str()).map(|s| s.to_owned());
        Ok(())
    }
}
