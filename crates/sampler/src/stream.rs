//! Streaming decoder for long audio files (ARCHITECTURE §6): sequential reads with
//! accurate seeks, planar stereo float output, no whole-file allocation. Used by the
//! engine's clip streams (worker thread) — never on the audio thread.

use crate::decode::DecodeError;
use std::path::{Path, PathBuf};
use symphonia::core::audio::GenericAudioBufferRef;
use symphonia::core::codecs::CodecParameters;
use symphonia::core::codecs::audio::{AudioDecoder, AudioDecoderOptions};
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::units::Timestamp;

/// What a file is, without decoding it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StreamInfo {
    pub path: PathBuf,
    pub sample_rate: u32,
    pub channels: u16,
    /// Frames according to the container (counted by decoding when absent).
    pub frames: u64,
}

pub struct StreamReader {
    format: Box<dyn FormatReader>,
    decoder: Box<dyn AudioDecoder>,
    track_id: u32,
    pub info: StreamInfo,
    /// Source frame of the next sample `read_into` returns.
    cursor: u64,
    /// Decoded samples not yet handed out (planar L/R) and the read offset into them.
    pend_l: Vec<f32>,
    pend_r: Vec<f32>,
    pend_pos: usize,
    scratch: Vec<Vec<f32>>,
    eof: bool,
}

/// Container reader, track id, codec parameters and container frame count.
type Opened = (Box<dyn FormatReader>, u32, symphonia::core::codecs::audio::AudioCodecParameters, Option<u64>);

fn open_format(path: &Path) -> Result<Opened, DecodeError> {
    let file = std::fs::File::open(path).map_err(|e| DecodeError::Open(path.display().to_string(), e))?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let format = symphonia::default::get_probe().probe(&hint, mss, FormatOptions::default(), MetadataOptions::default()).map_err(|e| DecodeError::Unsupported(e.to_string()))?;
    let track = format.default_track(TrackType::Audio).ok_or(DecodeError::NoAudioTrack)?;
    let track_id = track.id;
    let frames = track.num_frames;
    let Some(CodecParameters::Audio(params)) = track.codec_params.clone() else { return Err(DecodeError::NoAudioTrack) };
    Ok((format, track_id, params, frames))
}

/// Probe rate/channels/length. Files without a frame count in the container are counted
/// by decoding once (sequential, bounded memory).
pub fn probe(path: &Path) -> Result<StreamInfo, DecodeError> {
    let mut r = StreamReader::open(path)?;
    if r.info.frames == 0 {
        let mut l = vec![0.0f32; 8192];
        let mut rr = vec![0.0f32; 8192];
        let mut n = 0u64;
        loop {
            let got = r.read_into(&mut l, &mut rr)?;
            if got == 0 {
                break;
            }
            n += got as u64;
        }
        r.info.frames = n;
    }
    Ok(r.info)
}

impl StreamReader {
    pub fn open(path: &Path) -> Result<Self, DecodeError> {
        let (format, track_id, params, frames) = open_format(path)?;
        let decoder = symphonia::default::get_codecs().make_audio_decoder(&params, &AudioDecoderOptions::default()).map_err(|e| DecodeError::Unsupported(e.to_string()))?;
        let sample_rate = params.sample_rate.unwrap_or(0);
        let channels = params.channels.as_ref().map(|c| c.count() as u16).unwrap_or(0);
        if sample_rate == 0 {
            return Err(DecodeError::Unsupported("unknown sample rate".into()));
        }
        Ok(Self {
            format,
            decoder,
            track_id,
            info: StreamInfo { path: path.to_path_buf(), sample_rate, channels: channels.max(1), frames: frames.unwrap_or(0) },
            cursor: 0,
            pend_l: Vec::new(),
            pend_r: Vec::new(),
            pend_pos: 0,
            scratch: Vec::new(),
            eof: false,
        })
    }

    pub fn cursor(&self) -> u64 {
        self.cursor
    }

    /// Position the reader so the next `read_into` starts exactly at `frame`.
    pub fn seek(&mut self, frame: u64) -> Result<(), DecodeError> {
        let seeked = self
            .format
            .seek(SeekMode::Accurate, SeekTo::Timestamp { ts: Timestamp::new(frame as i64), track_id: self.track_id })
            .map_err(|e| DecodeError::Decode(e.to_string()))?;
        self.decoder.reset();
        self.pend_l.clear();
        self.pend_r.clear();
        self.pend_pos = 0;
        self.eof = false;
        let actual = seeked.actual_ts.get().max(0) as u64;
        self.cursor = actual;
        // skip up to the exact frame
        let mut skip = frame.saturating_sub(actual);
        let mut l = vec![0.0f32; 4096];
        let mut r = vec![0.0f32; 4096];
        while skip > 0 {
            let n = (skip as usize).min(4096);
            let got = self.read_into(&mut l[..n], &mut r[..n])?;
            if got == 0 {
                break;
            }
            skip -= got as u64;
        }
        Ok(())
    }

    /// Fill `l`/`r` (equal length) with the next frames; returns how many (0 at the end).
    /// Mono sources feed both channels; extra channels are dropped.
    pub fn read_into(&mut self, l: &mut [f32], r: &mut [f32]) -> Result<usize, DecodeError> {
        let want = l.len().min(r.len());
        let mut done = 0usize;
        while done < want {
            if self.pend_pos >= self.pend_l.len() {
                if self.eof || !self.decode_next()? {
                    self.eof = true;
                    break;
                }
                continue;
            }
            let n = (self.pend_l.len() - self.pend_pos).min(want - done);
            l[done..done + n].copy_from_slice(&self.pend_l[self.pend_pos..self.pend_pos + n]);
            r[done..done + n].copy_from_slice(&self.pend_r[self.pend_pos..self.pend_pos + n]);
            self.pend_pos += n;
            done += n;
        }
        self.cursor += done as u64;
        Ok(done)
    }

    fn decode_next(&mut self) -> Result<bool, DecodeError> {
        loop {
            let packet = match self.format.next_packet() {
                Ok(Some(p)) => p,
                Ok(None) => return Ok(false),
                Err(symphonia::core::errors::Error::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(false),
                Err(e) => return Err(DecodeError::Decode(e.to_string())),
            };
            if packet.track_id != self.track_id {
                continue;
            }
            let buf: GenericAudioBufferRef<'_> = match self.decoder.decode(&packet) {
                Ok(b) => b,
                Err(symphonia::core::errors::Error::DecodeError(_)) => continue,
                Err(e) => return Err(DecodeError::Decode(e.to_string())),
            };
            buf.copy_to_vecs_planar::<f32>(&mut self.scratch);
            let n = self.scratch.first().map(|c| c.len()).unwrap_or(0);
            self.pend_l.clear();
            self.pend_r.clear();
            self.pend_l.extend_from_slice(&self.scratch[0][..n]);
            if self.scratch.len() > 1 {
                self.pend_r.extend_from_slice(&self.scratch[1][..n]);
            } else {
                self.pend_r.extend_from_slice(&self.scratch[0][..n]);
            }
            for v in self.pend_l.iter_mut().chain(self.pend_r.iter_mut()) {
                if !v.is_finite() {
                    *v = 0.0;
                }
            }
            self.pend_pos = 0;
            return Ok(n > 0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sequential_reads_and_accurate_seeks_match_the_file() {
        let dir = std::env::temp_dir().join(format!("rawdawg-stream-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("ramp.wav");
        let spec = hound::WavSpec { channels: 2, sample_rate: 48_000, bits_per_sample: 32, sample_format: hound::SampleFormat::Float };
        let mut w = hound::WavWriter::create(&path, spec).unwrap();
        let n = 100_000u32;
        for i in 0..n {
            w.write_sample(i as f32 / n as f32).unwrap();
            w.write_sample(-(i as f32) / n as f32).unwrap();
        }
        w.finalize().unwrap();
        let info = probe(&path).unwrap();
        assert_eq!((info.sample_rate, info.channels, info.frames), (48_000, 2, 100_000));
        let mut r = StreamReader::open(&path).unwrap();
        let mut l = vec![0.0f32; 1000];
        let mut rr = vec![0.0f32; 1000];
        assert_eq!(r.read_into(&mut l, &mut rr).unwrap(), 1000);
        assert_eq!(l[999], 999.0 / n as f32);
        assert_eq!(rr[999], -999.0 / n as f32);
        assert_eq!(r.cursor(), 1000);
        r.seek(77_777).unwrap();
        assert_eq!(r.cursor(), 77_777);
        assert_eq!(r.read_into(&mut l[..10], &mut rr[..10]).unwrap(), 10);
        assert_eq!(l[0], 77_777.0 / n as f32, "accurate seek");
        r.seek(99_990).unwrap();
        assert_eq!(r.read_into(&mut l, &mut rr).unwrap(), 10, "reads stop at the end");
        assert_eq!(r.read_into(&mut l, &mut rr).unwrap(), 0);
        r.seek(0).unwrap();
        assert_eq!(r.read_into(&mut l[..1], &mut rr[..1]).unwrap(), 1);
        assert_eq!(l[0], 0.0);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
