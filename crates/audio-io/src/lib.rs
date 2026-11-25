//! Audio device layer (PLAT-02). Enumeration and negotiation happen off the
//! real-time thread; the output stream callback only pulls prepared f32 frames
//! from a [`RenderSource`] and converts sample formats at the edge.

pub mod devices;
pub mod stream;

pub use devices::{AudioBackend, DeviceInfo, DeviceSnapshot, HostInfo, SupportedRange, enumerate};
pub use stream::{OutputStream, RenderSource, StreamConfigRequest, StreamError, StreamStatus};
