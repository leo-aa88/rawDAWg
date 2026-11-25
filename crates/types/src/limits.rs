//! Engine budgets (ARCHITECTURE.md §5). Configurable with a rebuild outside the
//! real-time thread; enforced before compilation, never silently truncated.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct EngineLimits {
    pub max_mixer_channels: u32,
    pub max_processing_nodes: u32,
    pub max_voices_per_synth: u32,
    pub default_voices_per_synth: u32,
    pub max_layers_per_voice: u32,
    pub max_unison_per_layer: u32,
    /// Total simultaneous voice×oscillator budget for one synth instance.
    pub max_voice_oscillators: u32,
    pub max_scheduled_events_per_block: u32,
    pub max_control_messages_per_queue: u32,
    pub max_block_frames: u32,
    pub max_compensated_latency_seconds: f32,
    /// Internal feedback processing quantum in frames.
    pub feedback_quantum_frames: u32,
}

impl EngineLimits {
    pub const DEFAULT: EngineLimits = EngineLimits {
        max_mixer_channels: 128,
        max_processing_nodes: 512,
        max_voices_per_synth: 64,
        default_voices_per_synth: 32,
        max_layers_per_voice: 4,
        max_unison_per_layer: 8,
        max_voice_oscillators: 256,
        max_scheduled_events_per_block: 4096,
        max_control_messages_per_queue: 4096,
        max_block_frames: 8192,
        max_compensated_latency_seconds: 2.0,
        feedback_quantum_frames: 64,
    };

    /// Supported host block sizes (devices may negotiate other actual values;
    /// oversized callbacks are split into prepared chunks).
    pub const PREFERRED_BLOCK_SIZES: [u32; 7] = [32, 64, 128, 256, 512, 1024, 2048];
    pub const PREFERRED_BLOCK: u32 = 256;

    pub fn is_valid(&self) -> bool {
        self.max_mixer_channels >= 1
            && self.max_processing_nodes >= 2
            && self.max_voices_per_synth >= 1
            && self.default_voices_per_synth >= 1
            && self.default_voices_per_synth <= self.max_voices_per_synth
            && (1..=8).contains(&self.max_layers_per_voice)
            && (1..=16).contains(&self.max_unison_per_layer)
            && self.max_voice_oscillators >= 1
            && self.max_scheduled_events_per_block >= 16
            && self.max_control_messages_per_queue >= 16
            && self.max_block_frames >= 32
            && self.max_block_frames.is_power_of_two()
            && self.max_compensated_latency_seconds > 0.0
            && self.feedback_quantum_frames >= 1
            && self.feedback_quantum_frames.is_power_of_two()
    }
}

impl Default for EngineLimits {
    fn default() -> Self {
        Self::DEFAULT
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn defaults_valid() {
        assert!(EngineLimits::DEFAULT.is_valid());
        let mut l = EngineLimits::DEFAULT;
        l.max_block_frames = 1000;
        assert!(!l.is_valid());
    }
}
