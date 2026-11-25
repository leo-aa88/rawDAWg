//! Disk writer for audio capture (IO-04): a worker thread drains the capture queue into a
//! temporary WAV (32-bit float, stereo) inside the project's recovery folder and keeps a
//! journal beside it (frames written, lost frames, rate, start frame); lost blocks become
//! explicit silence (flagged). Stop finalizes the header; a crash leaves a recoverable
//! take whose header is rebuilt from the data size.

use crate::capture::{BLOCK_FRAMES, CaptureBlock};
use rawdawg_rt::queue::QueueConsumer;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// Journal written next to the take while recording (`recovery/takes/<id>.json`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TakeJournal {
    pub id: String,
    pub wav: String,
    pub sample_rate: u32,
    pub channels: u16,
    /// Project frame (output clock) the take was started at, after latency alignment.
    pub start_project_frame: i64,
    pub frames_written: u64,
    pub lost_frames: u64,
    pub started_unix_ms: u64,
    pub finished: bool,
}

/// Where takes live inside a project folder.
pub fn takes_dir(project_dir: &Path) -> PathBuf {
    project_dir.join("recovery").join("takes")
}

/// What the writer thread hands back on stop: the journal and the capture queue.
pub type WriterOutcome = Result<(TakeJournal, QueueConsumer<CaptureBlock>), String>;

pub struct CaptureWriter {
    stop: Arc<AtomicBool>,
    frames: Arc<AtomicU64>,
    lost: Arc<AtomicU64>,
    handle: Option<std::thread::JoinHandle<WriterOutcome>>,
    pub journal_path: PathBuf,
    pub wav_path: PathBuf,
}

impl CaptureWriter {
    /// Start writing `queue` into a new take in `project_dir`. Non-RT.
    pub fn start(project_dir: &Path, queue: QueueConsumer<CaptureBlock>, sample_rate: u32, start_project_frame: i64) -> Result<Self, String> {
        let dir = takes_dir(project_dir);
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let started_unix_ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0);
        let id = format!("take-{started_unix_ms}-{}", std::process::id());
        let wav_path = dir.join(format!("{id}.wav"));
        let journal_path = dir.join(format!("{id}.json"));
        let mut journal = TakeJournal {
            id: id.clone(),
            wav: format!("{id}.wav"),
            sample_rate,
            channels: 2,
            start_project_frame,
            frames_written: 0,
            lost_frames: 0,
            started_unix_ms,
            finished: false,
        };
        write_journal(&journal_path, &journal)?;
        let spec = hound::WavSpec { channels: 2, sample_rate, bits_per_sample: 32, sample_format: hound::SampleFormat::Float };
        let mut writer = hound::WavWriter::create(&wav_path, spec).map_err(|e| e.to_string())?;
        let stop = Arc::new(AtomicBool::new(false));
        let frames = Arc::new(AtomicU64::new(0));
        let lost = Arc::new(AtomicU64::new(0));
        let (stop2, frames2, lost2, jp) = (stop.clone(), frames.clone(), lost.clone(), journal_path.clone());
        let mut queue = queue;
        let handle = std::thread::Builder::new()
            .name("rawdawg-capture-writer".into())
            .spawn(move || {
                let mut expected: Option<u64> = None;
                let mut last_flush = Instant::now();
                loop {
                    let mut idle = true;
                    while let Some(block) = queue.pop() {
                        idle = false;
                        // explicit silence for frames lost between blocks (queue overflow)
                        if let Some(exp) = expected
                            && block.stream_frame > exp
                        {
                            let gap = (block.stream_frame - exp).min(10 * 60 * u64::from(sample_rate)); // bounded (10 min)
                            for _ in 0..gap {
                                writer.write_sample(0.0f32).map_err(|e| e.to_string())?;
                                writer.write_sample(0.0f32).map_err(|e| e.to_string())?;
                            }
                            lost2.fetch_add(gap, Ordering::Relaxed);
                            frames2.fetch_add(gap, Ordering::Relaxed);
                        }
                        let n = usize::from(block.frames).min(BLOCK_FRAMES);
                        for i in 0..n {
                            writer.write_sample(block.left[i]).map_err(|e| e.to_string())?;
                            writer.write_sample(block.right[i]).map_err(|e| e.to_string())?;
                        }
                        frames2.fetch_add(n as u64, Ordering::Relaxed);
                        expected = Some(block.stream_frame + n as u64);
                    }
                    if last_flush.elapsed() >= Duration::from_secs(1) {
                        writer.flush().map_err(|e| e.to_string())?;
                        journal.frames_written = frames2.load(Ordering::Relaxed);
                        journal.lost_frames = lost2.load(Ordering::Relaxed);
                        let _ = write_journal(&jp, &journal);
                        last_flush = Instant::now();
                    }
                    if stop2.load(Ordering::Relaxed) && queue.is_empty() {
                        break;
                    }
                    if idle {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                }
                writer.finalize().map_err(|e| e.to_string())?;
                journal.frames_written = frames2.load(Ordering::Relaxed);
                journal.lost_frames = lost2.load(Ordering::Relaxed);
                journal.finished = true;
                write_journal(&jp, &journal)?;
                Ok((journal, queue))
            })
            .map_err(|e| e.to_string())?;
        Ok(Self { stop, frames, lost, handle: Some(handle), journal_path, wav_path })
    }

    pub fn frames_written(&self) -> u64 {
        self.frames.load(Ordering::Relaxed)
    }
    pub fn lost_frames(&self) -> u64 {
        self.lost.load(Ordering::Relaxed)
    }

    /// Stop: drain what is queued, finalize the header, return the journal and the queue
    /// (so the next take can reuse the capture stream).
    pub fn stop(mut self) -> WriterOutcome {
        self.stop.store(true, Ordering::Relaxed);
        match self.handle.take() {
            Some(h) => h.join().map_err(|_| "capture writer panicked".to_owned())?,
            None => Err("writer already stopped".into()),
        }
    }
}

fn write_journal(path: &Path, j: &TakeJournal) -> Result<(), String> {
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(j).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())
}

/// Unfinished takes left by a crash: journals with `finished == false` whose WAV exists.
pub fn scan_unfinished(project_dir: &Path) -> Vec<(PathBuf, TakeJournal)> {
    let dir = takes_dir(project_dir);
    let Ok(rd) = std::fs::read_dir(&dir) else { return Vec::new() };
    let mut out = Vec::new();
    for e in rd.flatten() {
        let p = e.path();
        if p.extension().and_then(|x| x.to_str()) != Some("json") {
            continue;
        }
        let Ok(bytes) = std::fs::read(&p) else { continue };
        let Ok(j) = serde_json::from_slice::<TakeJournal>(&bytes) else { continue };
        if !j.finished && dir.join(&j.wav).is_file() {
            out.push((p, j));
        }
    }
    out.sort_by_key(|(_, j)| j.started_unix_ms);
    out
}

/// Rebuild the RIFF/data sizes of a WAV whose writer never finalized (crash), from the
/// file size. Returns the number of frames in the data chunk.
pub fn finalize_from_data(path: &Path) -> Result<u64, String> {
    let mut bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    if bytes.len() < 12 || &bytes[..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err("not a RIFF/WAVE file".into());
    }
    // walk chunks to the data chunk
    let mut pos = 12usize;
    let mut channels = 2u64;
    let mut bytes_per_sample = 4u64;
    while pos + 8 <= bytes.len() {
        let id = &bytes[pos..pos + 4];
        let size = u32::from_le_bytes([bytes[pos + 4], bytes[pos + 5], bytes[pos + 6], bytes[pos + 7]]) as usize;
        if id == b"fmt " && pos + 8 + 16 <= bytes.len() {
            channels = u64::from(u16::from_le_bytes([bytes[pos + 10], bytes[pos + 11]])).max(1);
            bytes_per_sample = (u64::from(u16::from_le_bytes([bytes[pos + 22], bytes[pos + 23]])) / 8).max(1);
        }
        if id == b"data" {
            let data_len = (bytes.len() - (pos + 8)) as u64;
            let frames = data_len / (channels * bytes_per_sample);
            let data_bytes = frames * channels * bytes_per_sample;
            bytes.truncate(pos + 8 + data_bytes as usize);
            let riff = (bytes.len() as u64 - 8) as u32;
            bytes[4..8].copy_from_slice(&riff.to_le_bytes());
            bytes[pos + 4..pos + 8].copy_from_slice(&(data_bytes as u32).to_le_bytes());
            std::fs::write(path, &bytes).map_err(|e| e.to_string())?;
            return Ok(frames);
        }
        pos += 8 + size + (size & 1);
    }
    Err("no data chunk".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::{CaptureProducer, InputChannels};

    fn temp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("rawdawg-capture-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn writer_records_blocks_inserts_silence_for_lost_ranges_and_journals() {
        let dir = temp("writer");
        // 2 000 frames of a mono ramp through a real producer
        let (mut p, q) = CaptureProducer::new(1, InputChannels::Mono(0));
        let w = CaptureWriter::start(&dir, q.writer, 48_000, 1234).unwrap();
        let ramp: Vec<f32> = (0..2000).map(|i| i as f32 / 2000.0).collect();
        p.push_interleaved(&ramp);
        p.flush_partial();
        std::thread::sleep(Duration::from_millis(30));
        let (j, _q) = w.stop().unwrap();
        assert_eq!(j.frames_written, 2000);
        assert_eq!(j.lost_frames, 0);
        assert!(j.finished);
        assert_eq!(j.start_project_frame, 1234);
        let mut r = hound::WavReader::open(j_path(&dir, &j)).unwrap();
        assert_eq!(r.spec().channels, 2);
        assert_eq!(r.len() / 2, 2000);
        let samples: Vec<f32> = r.samples::<f32>().map(|s| s.unwrap()).collect();
        assert_eq!(samples[2 * 1000], 0.5, "mono input feeds both channels");
        assert_eq!(samples[2 * 1000 + 1], 0.5);
        // gap detection: block A at stream frame 0, block B at 1 024 → 768 silent frames, flagged
        let (mut p3, q3) = rawdawg_rt::queue::ControlQueue::create::<CaptureBlock>(8);
        let mut a = CaptureBlock { stream_frame: 0, frames: 256, ..CaptureBlock::default() };
        a.left.fill(0.25);
        a.right.fill(0.25);
        let mut b = CaptureBlock { stream_frame: 1024, frames: 256, ..CaptureBlock::default() };
        b.left.fill(0.75);
        b.right.fill(0.75);
        p3.push(a).ok();
        p3.push(b).ok();
        let w3 = CaptureWriter::start(&dir, q3, 48_000, 0).unwrap();
        std::thread::sleep(Duration::from_millis(50));
        let (j3, _q3) = w3.stop().unwrap();
        assert_eq!(j3.frames_written, 256 + 768 + 256);
        assert_eq!(j3.lost_frames, 768);
        let mut r = hound::WavReader::open(j_path(&dir, &j3)).unwrap();
        let samples: Vec<f32> = r.samples::<f32>().map(|s| s.unwrap()).collect();
        assert_eq!(samples[0], 0.25);
        assert_eq!(samples[2 * 300], 0.0, "lost range is silence");
        assert_eq!(samples[2 * 1024], 0.75);
        assert_eq!(scan_unfinished(&dir).len(), 0, "finished takes are not recovery candidates");
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn j_path(dir: &Path, j: &TakeJournal) -> PathBuf {
        takes_dir(dir).join(&j.wav)
    }

    #[test]
    fn unfinished_take_is_found_and_its_header_rebuilt_from_the_data() {
        let dir = temp("recover");
        let (mut p, q) = CaptureProducer::new(2, InputChannels::Stereo(0, 1));
        let w = CaptureWriter::start(&dir, q.writer, 44_100, 99).unwrap();
        p.push_interleaved(&vec![0.5f32; 2 * 1000]);
        p.flush_partial();
        std::thread::sleep(Duration::from_millis(1100)); // past the 1 s flush
        // "crash": forget the writer without stop (thread keeps the file open until dropped)
        let wav = w.wav_path.clone();
        std::mem::forget(w);
        let found = scan_unfinished(&dir);
        assert_eq!(found.len(), 1);
        assert!(!found[0].1.finished);
        assert_eq!(found[0].1.start_project_frame, 99);
        assert!(found[0].1.frames_written >= 768, "journal flushed: {}", found[0].1.frames_written);
        // the header still carries placeholder sizes: rebuild from the data
        let frames = finalize_from_data(&wav).unwrap();
        assert!((768..=1000).contains(&frames), "{frames}");
        let r = hound::WavReader::open(&wav).unwrap();
        assert_eq!(r.spec().sample_rate, 44_100);
        assert_eq!(u64::from(r.len()) / 2, frames);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
