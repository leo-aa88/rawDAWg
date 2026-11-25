//! Audio file decoding (WAV/FLAC/MP3 via Symphonia) into a [`SampleAsset`].
//! Non-RT: runs on a worker; never mutates the source file.

use crate::asset::SampleAsset;
use std::path::Path;
use std::sync::Arc;
use symphonia::core::codecs::CodecParameters;
use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;

/// Decoded frames are capped to keep memory bounded (≈ 1 h stereo at 48 kHz).
pub const MAX_DECODED_FRAMES: usize = 48_000 * 3600;

#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    #[error("could not open {0}: {1}")]
    Open(String, std::io::Error),
    #[error("unsupported or unrecognized audio file: {0}")]
    Unsupported(String),
    #[error("decode error: {0}")]
    Decode(String),
    #[error("file has no audio track")]
    NoAudioTrack,
    #[error("file is longer than the supported maximum ({0} frames)")]
    TooLong(usize),
    #[error("decoded audio is empty")]
    Empty,
}

/// Decode a whole file. Channels beyond the first two are dropped (documented
/// stereo-first editor); mono stays mono.
pub fn decode_file(path: &Path) -> Result<Arc<SampleAsset>, DecodeError> {
    let file = std::fs::File::open(path).map_err(|e| DecodeError::Open(path.display().to_string(), e))?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let mut format =
        symphonia::default::get_probe().probe(&hint, mss, FormatOptions::default(), MetadataOptions::default()).map_err(|e| DecodeError::Unsupported(e.to_string()))?;
    let track = format.default_track(TrackType::Audio).ok_or(DecodeError::NoAudioTrack)?;
    let track_id = track.id;
    let Some(CodecParameters::Audio(params)) = track.codec_params.clone() else { return Err(DecodeError::NoAudioTrack) };
    let mut decoder = symphonia::default::get_codecs().make_audio_decoder(&params, &AudioDecoderOptions::default()).map_err(|e| DecodeError::Unsupported(e.to_string()))?;
    let mut rate = 0u32;
    let mut channels: Vec<Vec<f32>> = Vec::new();
    let mut scratch: Vec<Vec<f32>> = Vec::new();
    loop {
        let packet = match format.next_packet() {
            Ok(Some(p)) => p,
            Ok(None) => break,
            Err(symphonia::core::errors::Error::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(DecodeError::Decode(e.to_string())),
        };
        if packet.track_id != track_id {
            continue;
        }
        let buf = match decoder.decode(&packet) {
            Ok(b) => b,
            Err(symphonia::core::errors::Error::DecodeError(e)) => {
                log::warn!("skipping undecodable packet: {e}");
                continue;
            }
            Err(e) => return Err(DecodeError::Decode(e.to_string())),
        };
        let spec = buf.spec();
        rate = spec.rate();
        let nch = spec.channels().count().clamp(1, 2);
        buf.copy_to_vecs_planar::<f32>(&mut scratch);
        if channels.len() < nch {
            channels.resize(nch, Vec::new());
        }
        for (c, dst) in channels.iter_mut().enumerate() {
            if let Some(src) = scratch.get(c) {
                dst.extend_from_slice(src);
            }
        }
        if channels.first().map(|c| c.len()).unwrap_or(0) > MAX_DECODED_FRAMES {
            return Err(DecodeError::TooLong(MAX_DECODED_FRAMES));
        }
    }
    if channels.is_empty() || channels[0].is_empty() || rate == 0 {
        return Err(DecodeError::Empty);
    }
    let frames = channels[0].len();
    for c in &mut channels {
        c.resize(frames, 0.0);
        for s in c.iter_mut() {
            if !s.is_finite() {
                *s = 0.0;
            }
        }
    }
    let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    Ok(Arc::new(SampleAsset { name, sample_rate: rate, channels }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_a_generated_stereo_wav_exactly() {
        let dir = std::env::temp_dir().join(format!("rawdawg-decode-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.wav");
        let spec = hound::WavSpec { channels: 2, sample_rate: 44_100, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
        let mut w = hound::WavWriter::create(&path, spec).unwrap();
        for i in 0..1000i32 {
            w.write_sample((i % 100 - 50) * 100).unwrap();
            w.write_sample(-(i % 100 - 50) * 100).unwrap();
        }
        w.finalize().unwrap();
        let asset = decode_file(&path).unwrap();
        assert_eq!(asset.sample_rate, 44_100);
        assert_eq!(asset.channels.len(), 2);
        assert_eq!(asset.frames(), 1000);
        assert!((asset.channels[0][7] - (7 - 50) as f32 * 100.0 / 32768.0).abs() < 1e-6);
        assert!((asset.channels[1][7] + (7 - 50) as f32 * 100.0 / 32768.0).abs() < 1e-6);
        assert!(asset.is_valid());
        std::fs::write(dir.join("garbage.wav"), b"not audio at all").unwrap();
        assert!(decode_file(&dir.join("garbage.wav")).is_err());
        assert!(decode_file(&dir.join("missing.wav")).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
