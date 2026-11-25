//! Device and host enumeration into plain, serializable descriptions.

use cpal::traits::{DeviceTrait, HostTrait};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioBackend {
    Alsa,
    Jack,
    PipeWire,
    Wasapi,
    Asio,
    Other,
}

impl AudioBackend {
    pub fn label(self) -> &'static str {
        match self {
            AudioBackend::Alsa => "ALSA",
            AudioBackend::Jack => "JACK",
            AudioBackend::PipeWire => "PipeWire",
            AudioBackend::Wasapi => "WASAPI",
            AudioBackend::Asio => "ASIO",
            AudioBackend::Other => "Other",
        }
    }
}

pub fn backend_of(id: cpal::HostId) -> AudioBackend {
    let name = format!("{id:?}").to_ascii_lowercase();
    if name.contains("alsa") {
        AudioBackend::Alsa
    } else if name.contains("jack") {
        AudioBackend::Jack
    } else if name.contains("pipewire") {
        AudioBackend::PipeWire
    } else if name.contains("wasapi") {
        AudioBackend::Wasapi
    } else if name.contains("asio") {
        AudioBackend::Asio
    } else {
        AudioBackend::Other
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SupportedRange {
    pub channels: u16,
    pub min_rate_hz: u32,
    pub max_rate_hz: u32,
    pub sample_format: String,
    pub buffer_min: Option<u32>,
    pub buffer_max: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DeviceInfo {
    pub backend: AudioBackend,
    /// Stable id string (host + device identifier) for preferences.
    pub id: String,
    pub name: String,
    pub is_default_output: bool,
    pub is_default_input: bool,
    pub output: Vec<SupportedRange>,
    pub input: Vec<SupportedRange>,
    pub default_output_rate_hz: Option<u32>,
    pub default_output_channels: Option<u16>,
    pub default_input_rate_hz: Option<u32>,
    pub default_input_channels: Option<u16>,
}

impl DeviceInfo {
    pub fn supports_output(&self) -> bool {
        !self.output.is_empty()
    }
    pub fn supports_input(&self) -> bool {
        !self.input.is_empty()
    }
    pub fn supports_output_rate(&self, rate: u32, channels: u16) -> bool {
        self.output.iter().any(|r| r.channels >= channels && r.min_rate_hz <= rate && rate <= r.max_rate_hz)
    }
    /// Project rates offered by this output device.
    pub fn supported_project_rates(&self) -> Vec<u32> {
        rawdawg_types::SampleRate::SUPPORTED.iter().copied().filter(|r| self.supports_output_rate(*r, 1)).collect()
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HostInfo {
    pub backend: AudioBackend,
    pub name: String,
    pub available: bool,
    pub error: Option<String>,
}

/// Everything the UI needs, captured at one moment off the RT thread.
#[derive(Clone, Debug, PartialEq, Default, Serialize, Deserialize)]
pub struct DeviceSnapshot {
    pub hosts: Vec<HostInfo>,
    pub devices: Vec<DeviceInfo>,
    pub default_backend: Option<AudioBackend>,
}

impl DeviceSnapshot {
    pub fn default_output(&self) -> Option<&DeviceInfo> {
        self.devices.iter().find(|d| d.is_default_output && d.supports_output()).or_else(|| self.devices.iter().find(|d| d.supports_output()))
    }
    pub fn find(&self, id: &str) -> Option<&DeviceInfo> {
        self.devices.iter().find(|d| d.id == id)
    }
    pub fn has_output(&self) -> bool {
        self.devices.iter().any(|d| d.supports_output())
    }
}

fn range_of(r: &cpal::SupportedStreamConfigRange) -> SupportedRange {
    let (bmin, bmax) = match r.buffer_size() {
        cpal::SupportedBufferSize::Range { min, max } => (Some(*min), Some(*max)),
        cpal::SupportedBufferSize::Unknown => (None, None),
    };
    SupportedRange {
        channels: r.channels(),
        min_rate_hz: r.min_sample_rate(),
        max_rate_hz: r.max_sample_rate(),
        sample_format: format!("{:?}", r.sample_format()),
        buffer_min: bmin,
        buffer_max: bmax,
    }
}

/// Enumerate all hosts and devices. Never panics; errors are recorded per host.
pub fn enumerate() -> DeviceSnapshot {
    let mut snap = DeviceSnapshot { default_backend: Some(backend_of(cpal::default_host().id())), ..Default::default() };
    for host_id in cpal::available_hosts() {
        let backend = backend_of(host_id);
        let host = match cpal::host_from_id(host_id) {
            Ok(h) => h,
            Err(e) => {
                snap.hosts.push(HostInfo { backend, name: format!("{host_id:?}"), available: false, error: Some(e.to_string()) });
                continue;
            }
        };
        snap.hosts.push(HostInfo { backend, name: format!("{host_id:?}"), available: true, error: None });
        let default_out = host.default_output_device().and_then(|d| d.id().ok());
        let default_in = host.default_input_device().and_then(|d| d.id().ok());
        let devices = match host.devices() {
            Ok(d) => d,
            Err(e) => {
                log::warn!("device enumeration failed on {host_id:?}: {e}");
                continue;
            }
        };
        for dev in devices {
            let Ok(id) = dev.id() else { continue };
            let name = dev.description().map(|d| d.name().to_owned()).unwrap_or_else(|_| id.to_string());
            let output: Vec<SupportedRange> = dev.supported_output_configs().map(|c| c.map(|r| range_of(&r)).collect()).unwrap_or_default();
            let input: Vec<SupportedRange> = dev.supported_input_configs().map(|c| c.map(|r| range_of(&r)).collect()).unwrap_or_default();
            if output.is_empty() && input.is_empty() {
                continue;
            }
            let dout = dev.default_output_config().ok();
            let din = dev.default_input_config().ok();
            snap.devices.push(DeviceInfo {
                backend,
                id: id.to_string(),
                name,
                is_default_output: default_out.as_ref() == Some(&id),
                is_default_input: default_in.as_ref() == Some(&id),
                output,
                input,
                default_output_rate_hz: dout.as_ref().map(|c| c.sample_rate()),
                default_output_channels: dout.as_ref().map(|c| c.channels()),
                default_input_rate_hz: din.as_ref().map(|c| c.sample_rate()),
                default_input_channels: din.as_ref().map(|c| c.channels()),
            });
        }
    }
    snap
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enumeration_never_panics_and_is_serializable() {
        // Headless CI has no devices: the snapshot must still be well-formed.
        let snap = enumerate();
        let json = serde_json::to_string(&snap).unwrap();
        let back: DeviceSnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(back, snap);
        for d in &snap.devices {
            assert!(!d.id.is_empty());
            assert!(!d.name.is_empty());
        }
    }

    #[test]
    fn device_rate_support_logic() {
        let d = DeviceInfo {
            backend: AudioBackend::Alsa,
            id: "x".into(),
            name: "x".into(),
            is_default_output: true,
            is_default_input: false,
            output: vec![SupportedRange { channels: 2, min_rate_hz: 44_100, max_rate_hz: 96_000, sample_format: "F32".into(), buffer_min: Some(32), buffer_max: Some(4096) }],
            input: vec![],
            default_output_rate_hz: Some(48_000),
            default_output_channels: Some(2),
            default_input_rate_hz: None,
            default_input_channels: None,
        };
        assert!(d.supports_output_rate(48_000, 2));
        assert!(!d.supports_output_rate(192_000, 2));
        assert!(!d.supports_output_rate(48_000, 4));
        assert_eq!(d.supported_project_rates(), vec![44_100, 48_000, 88_200, 96_000]);
        assert!(!d.supports_input());
    }
}
