//! Shared parameter storage for native effects: normalized values keyed by stable
//! descriptor ids, JSON state keyed by descriptor `key` (forward compatible),
//! per-parameter one-pole smoothing for continuous parameters.

use rawdawg_dsp::smooth::Smoother;
use rawdawg_types::{ParamDescriptor, ParamId};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EffectState {
    pub version: u32,
    /// Normalized values by descriptor key.
    pub params: serde_json::Map<String, serde_json::Value>,
}

/// Parameter bank for one effect instance.
#[derive(Clone, Debug)]
pub struct Params {
    descriptors: &'static [ParamDescriptor],
    normalized: Vec<f32>,
    smoothers: Vec<Smoother>,
    sample_rate: f32,
}

impl Params {
    pub fn new(descriptors: &'static [ParamDescriptor]) -> Self {
        let normalized: Vec<f32> = descriptors.iter().map(|d| d.default_normalized).collect();
        let smoothers = descriptors.iter().map(|d| Smoother::new(d.default_normalized, d.smoothing_ms / 1000.0, 48_000.0)).collect();
        Self { descriptors, normalized, smoothers, sample_rate: 48_000.0 }
    }

    pub fn descriptors(&self) -> &'static [ParamDescriptor] {
        self.descriptors
    }

    pub fn set_sample_rate(&mut self, sr: f32) {
        self.sample_rate = sr;
        for (s, d) in self.smoothers.iter_mut().zip(self.descriptors) {
            s.set_time(d.smoothing_ms / 1000.0, sr);
        }
    }

    fn index(&self, id: ParamId) -> Option<usize> {
        self.descriptors.iter().position(|d| d.id == id)
    }

    /// Normalized value (target).
    pub fn get(&self, id: u32) -> f32 {
        self.index(ParamId(id)).map(|i| self.normalized[i]).unwrap_or(0.0)
    }

    /// Physical value (target) through the descriptor mapping.
    pub fn phys(&self, id: u32) -> f32 {
        match self.index(ParamId(id)) {
            Some(i) => self.descriptors[i].to_physical(self.normalized[i]) as f32,
            None => 0.0,
        }
    }

    /// Smoothed physical value, advancing the smoother by `frames`.
    pub fn phys_smoothed(&mut self, id: u32, frames: usize) -> f32 {
        match self.index(ParamId(id)) {
            Some(i) => {
                let s = &mut self.smoothers[i];
                s.skip(frames.saturating_sub(1));
                let n = if frames > 0 { s.next() } else { s.current() };
                self.descriptors[i].to_physical(n) as f32
            }
            None => 0.0,
        }
    }

    pub fn as_bool(&self, id: u32) -> bool {
        self.get(id) >= 0.5
    }

    pub fn as_index(&self, id: u32) -> usize {
        self.phys(id).round().max(0.0) as usize
    }

    /// RT-safe set (normalized). Returns false for unknown ids / non-finite values.
    pub fn set(&mut self, id: ParamId, normalized: f32) -> bool {
        if !normalized.is_finite() {
            return false;
        }
        let Some(i) = self.index(id) else { return false };
        let v = normalized.clamp(0.0, 1.0);
        self.normalized[i] = v;
        if self.descriptors[i].smoothing_ms > 0.0 {
            self.smoothers[i].set_target(v);
        } else {
            self.smoothers[i].snap(v);
        }
        true
    }

    pub fn param(&self, id: ParamId) -> Option<f32> {
        self.index(id).map(|i| self.normalized[i])
    }

    pub fn snap_all(&mut self) {
        for (s, v) in self.smoothers.iter_mut().zip(&self.normalized) {
            s.snap(*v);
        }
    }

    pub fn save(&self, version: u32) -> Vec<u8> {
        let mut params = serde_json::Map::new();
        for (d, v) in self.descriptors.iter().zip(&self.normalized) {
            params.insert(d.key.to_owned(), serde_json::json!(v));
        }
        serde_json::to_vec(&EffectState { version, params }).unwrap_or_default()
    }

    /// Load state by key; unknown keys are ignored, missing keys keep their defaults.
    pub fn load(&mut self, bytes: &[u8], max_version: u32) -> Result<(), String> {
        let st: EffectState = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
        if st.version == 0 || st.version > max_version {
            return Err(format!("unsupported effect state version {}", st.version));
        }
        for (i, d) in self.descriptors.iter().enumerate() {
            self.normalized[i] = d.default_normalized;
            if let Some(v) = st.params.get(d.key).and_then(|v| v.as_f64()) {
                if !v.is_finite() {
                    return Err(format!("parameter {} is not finite", d.key));
                }
                self.normalized[i] = (v as f32).clamp(0.0, 1.0);
            }
        }
        self.snap_all();
        Ok(())
    }

    /// Set a parameter by key from a physical value (presets).
    pub fn set_phys_by_key(&mut self, key: &str, phys: f64) {
        if let Some(i) = self.descriptors.iter().position(|d| d.key == key) {
            let n = self.descriptors[i].to_normalized(phys);
            self.normalized[i] = n;
            self.smoothers[i].snap(n);
        }
    }
}

/// A factory preset: name + physical values by key.
#[derive(Clone, Debug)]
pub struct Preset {
    pub name: &'static str,
    pub description: &'static str,
    pub values: &'static [(&'static str, f64)],
}

pub fn apply_preset(params: &mut Params, preset: &Preset) {
    for d in params.descriptors() {
        params.set_phys_by_key(d.key, d.to_physical(d.default_normalized));
    }
    for (k, v) in preset.values {
        params.set_phys_by_key(k, *v);
    }
}

/// Wet/dry crossfade helper (equal-gain linear; `mix` 0 = dry, 1 = wet).
#[inline]
pub fn mix_wet(dry: f32, wet: f32, mix: f32) -> f32 {
    dry + (wet - dry) * mix
}
