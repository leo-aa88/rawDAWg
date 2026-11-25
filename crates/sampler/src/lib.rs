//! Native sample instrument (`native.sampler`) and SoundFont 2 player
//! (`native.sf2`). Decoding/resampling happens off-thread; processors only read
//! prepared, immutable sample data.

pub mod asset;
pub mod decode;
pub mod sampler;
pub mod sf2;
pub mod sf2_player;

pub use asset::SampleAsset;
pub use decode::{DecodeError, decode_file};
pub use sampler::{Sampler, SamplerPatch};
pub use sf2_player::{Sf2Patch, Sf2Player};

pub const SAMPLER_TYPE_ID: &str = rawdawg_types::ids::native_types::SAMPLER;
pub const SF2_TYPE_ID: &str = rawdawg_types::ids::native_types::SF2;
pub const SAMPLER_DISPLAY_NAME: &str = "rawDAWg Sampler";
pub const SF2_DISPLAY_NAME: &str = "rawDAWg SoundFont";
