//! The effect wrapper shared by every format adapter: latency-aligned true bypass,
//! wet/dry mix with a dry path delayed by the reported latency, the two wrapper
//! parameters (`Bypass`, `Mix`) and the versioned state envelope.
//!
//! Instruments use none of this (they have no dry path); adapters call
//! [`wrapper_descriptors`] to know which wrapper parameters to expose.

use rawdawg_dsp::AudioBlock;
use rawdawg_dsp::delayline::DelayLine;
use rawdawg_dsp::processor::{ProcessContext, Processor, Tail, TransportView};
use rawdawg_dsp::smooth::Smoother;
use rawdawg_types::events::{EventKind, TimedEvent};
use rawdawg_types::params::ParamCategory;
use rawdawg_types::{ParamDescriptor, ParamMapping, ParamUnit};

/// Wrapper parameter ids (outside the native descriptor id space, which is small).
pub const WRAP_BYPASS: u32 = 0xFFFF_0000;
pub const WRAP_MIX: u32 = 0xFFFF_0001;
/// Upper bound on the latency the dry path compensates.
pub const MAX_DRY_LATENCY: usize = 1 << 20;
/// Key that marks the versioned state envelope (shared by CLAP and VST3 exports).
pub const STATE_KEY: &str = "rawdawg_state";
pub const STATE_VERSION: u32 = 1;

pub static WRAP_DESCRIPTORS: [ParamDescriptor; 2] = [
    {
        let mut d = ParamDescriptor::continuous(WRAP_BYPASS, "wrap.bypass", "Bypass", ParamUnit::None, ParamMapping::Toggle, 0.0, ParamCategory::Mix).stepped();
        d.flags.is_bypass = true;
        d
    },
    ParamDescriptor::continuous(WRAP_MIX, "wrap.mix", "Mix", ParamUnit::Percent, ParamMapping::Linear { min: 0.0, max: 1.0 }, 1.0, ParamCategory::Mix),
];

/// Wrapper descriptors a plugin exposes (none for instruments; `Bypass` for every
/// effect; `Mix` where the catalogue marks the effect as mixable).
pub fn wrapper_descriptors(is_synth: bool, has_mix: bool) -> &'static [ParamDescriptor] {
    match (is_synth, has_mix) {
        (true, _) => &[],
        (false, true) => &WRAP_DESCRIPTORS,
        (false, false) => &WRAP_DESCRIPTORS[..1],
    }
}

pub fn is_wrapper_param(id: u32) -> bool {
    id == WRAP_BYPASS || id == WRAP_MIX
}

/// Realtime half of the wrapper. Allocates only in [`EffectWrapper::prepare`].
pub struct EffectWrapper {
    dry: [DelayLine; 2],
    latency: usize,
    mix: Smoother,
    pub bypass: bool,
    has_mix: bool,
}

impl EffectWrapper {
    pub fn new(has_mix: bool) -> Self {
        Self { dry: [DelayLine::new(1), DelayLine::new(1)], latency: 0, mix: Smoother::new(1.0, 0.01, 48_000.0), bypass: false, has_mix }
    }

    /// Size the dry path for `latency` frames at `sample_rate`; `mix` is snapped.
    pub fn prepare(&mut self, latency: u32, sample_rate: f64, mix: f32) {
        self.latency = (latency as usize).min(MAX_DRY_LATENCY);
        let len = (self.latency + 1).next_power_of_two();
        self.dry = [DelayLine::new(len), DelayLine::new(len)];
        self.mix = Smoother::new(mix, 0.01, sample_rate as f32);
        self.mix.snap(mix);
    }

    pub fn reset(&mut self, mix: f32) {
        self.dry[0].reset();
        self.dry[1].reset();
        self.mix.snap(mix);
    }

    pub fn latency(&self) -> usize {
        self.latency
    }
    pub fn has_mix(&self) -> bool {
        self.has_mix
    }
    pub fn mix_target(&self) -> f32 {
        self.mix.target()
    }
    pub fn set_mix(&mut self, value: f32) {
        if self.has_mix {
            self.mix.set_target(value.clamp(0.0, 1.0));
        }
    }

    /// Apply a wrapper parameter; `false` when `id` is not a wrapper parameter.
    pub fn apply_param(&mut self, id: u32, value: f32) -> bool {
        match id {
            WRAP_BYPASS => {
                self.bypass = value >= 0.5;
                true
            }
            WRAP_MIX => {
                self.set_mix(value);
                true
            }
            _ => false,
        }
    }

    /// Apply and remove wrapper parameter events from a block's event list (no allocation).
    pub fn take_wrapper_events(&mut self, events: &mut Vec<TimedEvent>) {
        for e in events.iter() {
            if let EventKind::Param { id, value } = e.kind {
                self.apply_param(id.0, value);
            }
        }
        events.retain(|e| !matches!(e.kind, EventKind::Param { id, .. } if is_wrapper_param(id.0)));
    }

    /// Render one prepared chunk: `inputs` (and `sidechain` when fed) → `outputs`, with
    /// the dry path, mix and bypass. `n` frames; all slices must hold at least `n`.
    #[allow(clippy::too_many_arguments)]
    pub fn process_chunk(
        &mut self,
        processor: &mut dyn Processor,
        n: usize,
        events: &[TimedEvent],
        transport: &TransportView,
        inputs: [&[f32]; 2],
        sidechain: Option<[&[f32]; 2]>,
        outputs: [&mut [f32]; 2],
    ) {
        let [out_l, out_r] = outputs;
        let [in_l, in_r] = inputs;
        if self.bypass {
            // parameters still arrive so the effect's state stays current
            for ev in events {
                if let EventKind::Param { id, value } = ev.kind {
                    processor.set_param(id, value);
                }
            }
            out_l[..n].copy_from_slice(&in_l[..n]);
            out_r[..n].copy_from_slice(&in_r[..n]);
            if self.latency > 0 {
                self.dry[0].delay_block(&mut out_l[..n], self.latency);
                self.dry[1].delay_block(&mut out_r[..n], self.latency);
            }
            return;
        }
        {
            let ins: [&[f32]; 2] = [&in_l[..n], &in_r[..n]];
            let sc: [&[f32]; 2] = match sidechain {
                Some([l, r]) => [&l[..n], &r[..n]],
                None => [&[], &[]],
            };
            let mut outs: [&mut [f32]; 2] = [&mut out_l[..n], &mut out_r[..n]];
            let mut ctx =
                ProcessContext { audio: AudioBlock { frames: n, inputs: &ins, outputs: &mut outs, sidechain: if sidechain.is_some() { &sc } else { &[] } }, events, transport };
            processor.process(&mut ctx);
        }
        let lat = self.latency;
        if self.has_mix {
            for i in 0..n {
                let m = self.mix.next();
                self.dry[0].push(in_l[i]);
                self.dry[1].push(in_r[i]);
                let (dl, dr) = (self.dry[0].read(lat), self.dry[1].read(lat));
                out_l[i] = out_l[i] * m + dl * (1.0 - m);
                out_r[i] = out_r[i] * m + dr * (1.0 - m);
            }
        } else {
            for i in 0..n {
                self.dry[0].push(in_l[i]);
                self.dry[1].push(in_r[i]);
            }
        }
    }

    /// Tail the host should expect after input stops: `Some(frames)` or `None` for unbounded.
    pub fn tail_frames(&self, processor: &dyn Processor) -> Option<u32> {
        if self.bypass {
            return Some(0);
        }
        match processor.tail() {
            Tail::None => Some(0),
            Tail::Frames(n) => Some(n.saturating_add(self.latency as u32)),
            Tail::Unknown | Tail::Infinite => None,
        }
    }
}

/// Parsed state: the processor's own bytes plus the wrapper values when present.
pub struct ParsedState {
    pub processor: Vec<u8>,
    pub bypass: Option<bool>,
    pub mix: Option<f32>,
}

/// Serialize an effect's state as the versioned envelope (synths save the raw patch).
pub fn save_envelope(type_id: &str, processor_bytes: &[u8], bypass: bool, mix: f32) -> Result<Vec<u8>, String> {
    let processor: serde_json::Value = serde_json::from_slice(processor_bytes).map_err(|_| "processor state is not JSON".to_owned())?;
    let env = serde_json::json!({ STATE_KEY: STATE_VERSION, "type_id": type_id, "bypass": bypass, "mix": mix, "processor": processor });
    serde_json::to_vec(&env).map_err(|e| e.to_string())
}

/// Accepts the envelope (checking `type_id`) or a bare processor state (app preset).
pub fn parse_state(type_id: &str, bytes: &[u8]) -> Result<ParsedState, String> {
    if let Ok(serde_json::Value::Object(map)) = serde_json::from_slice::<serde_json::Value>(bytes)
        && map.contains_key(STATE_KEY)
    {
        if let Some(t) = map.get("type_id").and_then(|v| v.as_str())
            && t != type_id
        {
            return Err(format!("state belongs to {t}, not {type_id}"));
        }
        let processor = map.get("processor").ok_or_else(|| "state envelope lacks processor state".to_owned())?;
        return Ok(ParsedState {
            processor: serde_json::to_vec(processor).map_err(|e| e.to_string())?,
            bypass: map.get("bypass").and_then(|v| v.as_bool()),
            mix: map.get("mix").and_then(|v| v.as_f64()).map(|m| m as f32).filter(|m| m.is_finite()),
        });
    }
    Ok(ParsedState { processor: bytes.to_vec(), bypass: None, mix: None })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rawdawg_dsp::BusLayout;
    use rawdawg_dsp::processor::PrepareSpec;

    #[test]
    fn bypass_is_latency_aligned_and_mix_zero_is_dry() {
        let mut p = rawdawg_effects::make_effect(rawdawg_effects::pitch::TYPE_ID, None).unwrap();
        p.prepare(&PrepareSpec::new(48_000.0, 64, BusLayout::Stereo)).unwrap();
        let latency = p.latency_frames();
        assert!(latency > 0, "the pitch shifter reports its frame latency");
        let mut w = EffectWrapper::new(true);
        w.prepare(latency, 48_000.0, 1.0);
        const BLOCKS: usize = 80;
        let input: Vec<f32> = (0..64 * BLOCKS).map(|i| ((i * 7919) % 101) as f32 / 101.0 - 0.5).collect();
        let transport = TransportView::stopped(48_000.0);
        let run = |w: &mut EffectWrapper, p: &mut dyn Processor| {
            let mut out = Vec::new();
            for b in 0..BLOCKS {
                let (mut l, mut r) = (vec![0.0f32; 64], vec![0.0f32; 64]);
                let x = &input[b * 64..(b + 1) * 64];
                w.process_chunk(p, 64, &[], &transport, [x, x], None, [&mut l, &mut r]);
                out.extend_from_slice(&l);
            }
            out
        };
        w.apply_param(WRAP_BYPASS, 1.0);
        let out = run(&mut w, p.as_mut());
        let l = latency as usize;
        for i in 0..64 * BLOCKS - l {
            assert!((out[i + l] - input[i]).abs() < 1e-6, "bypass delays the dry signal by the latency");
        }
        w.apply_param(WRAP_BYPASS, 0.0);
        w.apply_param(WRAP_MIX, 0.0);
        w.reset(0.0);
        p.reset();
        let out = run(&mut w, p.as_mut());
        for i in 64 * 10..64 * BLOCKS - l {
            assert!((out[i + l] - input[i]).abs() < 1e-3, "mix 0 is the delayed dry signal");
        }
        assert_eq!(w.tail_frames(p.as_ref()).map(|t| t >= latency), Some(true));
    }

    #[test]
    fn envelope_round_trips_and_rejects_other_plugins() {
        let p = rawdawg_effects::make_effect("native.fx.stereo", None).unwrap();
        let bytes = p.save_state();
        let env = save_envelope("native.fx.stereo", &bytes, true, 0.25).unwrap();
        let v: serde_json::Value = serde_json::from_slice(&env).unwrap();
        assert_eq!(v[STATE_KEY], STATE_VERSION);
        let parsed = parse_state("native.fx.stereo", &env).unwrap();
        assert_eq!(parsed.bypass, Some(true));
        assert_eq!(parsed.mix, Some(0.25));
        assert_eq!(serde_json::from_slice::<serde_json::Value>(&parsed.processor).unwrap(), serde_json::from_slice::<serde_json::Value>(&bytes).unwrap());
        assert!(parse_state("native.fx.delay", &env).is_err());
        let bare = parse_state("native.fx.stereo", &bytes).unwrap();
        assert_eq!(bare.bypass, None);
        assert_eq!(bare.processor, bytes);
        let mut events = vec![
            TimedEvent::new(0, rawdawg_types::events::EventSource::Host, 0, EventKind::Param { id: rawdawg_types::ParamId(WRAP_MIX), value: 0.5 }),
            TimedEvent::new(0, rawdawg_types::events::EventSource::Host, 0, EventKind::Param { id: rawdawg_types::ParamId(3), value: 0.5 }),
        ];
        let mut w = EffectWrapper::new(true);
        w.take_wrapper_events(&mut events);
        assert_eq!(events.len(), 1);
        assert_eq!(w.mix_target(), 0.5);
    }
}
