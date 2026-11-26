//! Clip streaming (ARCHITECTURE §6): long audio assets are not decoded whole. A worker
//! thread decodes ahead into bounded SPSC queues; the audio thread keeps a fixed ring per
//! clip and reads from it, requesting a refill (via atomics) after seeks/loop wraps and
//! emitting silence — counted as a dropout — while data for the new position is missing.
//! A clip that is entered again after a loop wrap (it spans the loop start, or starts inside
//! the loop) keeps a short decoded **head** at that entry point, read off the audio thread
//! when the plan is built: after the wrap it plays from the head while the worker refills the
//! ring right behind it, so looping never gaps. Offline renders wait outside the audio path
//! until every stream is ready.

use rawdawg_rt::{ControlQueue, QueueConsumer, QueueProducer};
use rawdawg_sampler::stream::{StreamInfo, StreamReader};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// Frames per streamed block.
pub const STREAM_BLOCK: usize = 1024;
/// Blocks a stream queue holds (≈ 1.4 s at 48 kHz) and the RT ring length in frames.
pub const STREAM_QUEUE_BLOCKS: usize = 64;
pub const RING_FRAMES: usize = STREAM_BLOCK * 128;
/// Frames of a loop-entry head (≈ 0.68 s at 48 kHz): the time the worker has to seek and refill.
pub const HEAD_FRAMES: usize = STREAM_BLOCK * 32;
/// Source frame used when a stream has nothing requested yet.
const NO_WANT: u64 = u64::MAX;

pub struct StreamBlock {
    pub source_frame: u64,
    /// Request generation the worker served; the RT drops blocks of older generations.
    pub generation: u64,
    pub frames: u16,
    /// Inline sample storage: popping and dropping a block never touches the heap.
    pub left: [f32; STREAM_BLOCK],
    pub right: [f32; STREAM_BLOCK],
}

/// Shared between the RT ring and the worker: what the RT wants next.
pub struct StreamControl {
    /// Source frame the reader should continue from after a seek (`NO_WANT` = continue).
    pub want: AtomicU64,
    pub generation: AtomicU64,
    pub stop: AtomicBool,
}

/// RT side of one streamed clip: a fixed ring of contiguous source frames.
pub struct ClipStream {
    pub asset_frames: u64,
    queue: Option<QueueConsumer<StreamBlock>>,
    control: Arc<StreamControl>,
    /// Last source frame requested from the worker (avoids a request per sample).
    last_request: u64,
    ring_l: Vec<f32>,
    ring_r: Vec<f32>,
    /// Source frame of ring index 0 and how many contiguous frames follow it.
    ring_start: u64,
    ring_fill: usize,
    ring_head: usize,
    generation: u64,
    /// Decoded frames at the clip's loop entry point (empty when the clip is never re-entered).
    /// Shared with the head cache; plans are dropped off the audio thread, so the audio thread
    /// never frees them.
    head_start: u64,
    head_l: Arc<[f32]>,
    head_r: Arc<[f32]>,
}

/// Frames decoded at a clip's loop entry point.
#[derive(Clone)]
pub struct LoopHead {
    pub start: u64,
    pub left: Arc<[f32]>,
    pub right: Arc<[f32]>,
}

/// Decoded heads by (file, size, modification time, entry frame): recompiles (every edit) reuse
/// them instead of decoding again. Bounded; the oldest go first.
struct HeadCache {
    entries: std::collections::VecDeque<(HeadKey, LoopHead)>,
    /// Heads decoded (not taken from the cache) per file: diagnostics and tests.
    decoded: Vec<(std::path::PathBuf, u64)>,
}

type HeadKey = (std::path::PathBuf, u64, Option<std::time::SystemTime>, u64);

/// Heads kept (≈ 256 KB each at f32 stereo).
const HEAD_CACHE_ENTRIES: usize = 64;

static HEADS: std::sync::Mutex<HeadCache> = std::sync::Mutex::new(HeadCache { entries: std::collections::VecDeque::new(), decoded: Vec::new() });

fn head_key(path: &std::path::Path, entry: u64) -> HeadKey {
    let meta = std::fs::metadata(path).ok();
    (path.to_path_buf(), meta.as_ref().map_or(0, |m| m.len()), meta.and_then(|m| m.modified().ok()), entry)
}

/// How many loop heads of the file at `path` were decoded rather than reused.
pub fn heads_decoded(path: &std::path::Path) -> u64 {
    HEADS.lock().map(|c| c.decoded.iter().find(|(p, _)| p == path).map_or(0, |(_, n)| *n)).unwrap_or(0)
}

impl ClipStream {
    fn new(asset_frames: u64, queue: QueueConsumer<StreamBlock>, control: Arc<StreamControl>, head: Option<LoopHead>) -> Self {
        let head = head.unwrap_or(LoopHead { start: 0, left: Arc::from(Vec::new()), right: Arc::from(Vec::new()) });
        Self {
            asset_frames,
            queue: Some(queue),
            control,
            last_request: NO_WANT,
            ring_l: vec![0.0; RING_FRAMES],
            ring_r: vec![0.0; RING_FRAMES],
            ring_start: 0,
            ring_fill: 0,
            ring_head: 0,
            generation: 0,
            head_start: head.start,
            head_l: head.left,
            head_r: head.right,
        }
    }

    /// Sample at a fractional source position from the ring, or else from the loop-entry head;
    /// reading from the head asks the worker to continue the ring right after it (RT-safe).
    #[inline]
    pub fn read_or_head(&mut self, pos: f64) -> Option<(f32, f32)> {
        if let Some(v) = self.read(pos) {
            return Some(v);
        }
        let v = self.read_head(pos)?;
        // the ring resumes two frames before the head ends, so interpolating across the
        // hand-over always finds both neighbours; skip when that request is already in flight
        let resume = (self.head_start + self.head_l.len() as u64).saturating_sub(2);
        let in_flight = self.ring_fill == 0 && self.last_request == resume;
        if !in_flight && !self.has(resume, resume + 2) {
            self.last_request = resume;
            self.request(resume);
        }
        Some(v)
    }

    fn read_head(&self, pos: f64) -> Option<(f32, f32)> {
        if pos < self.head_start as f64 {
            return None;
        }
        let i = (pos.floor() as u64 - self.head_start) as usize;
        if i + 1 >= self.head_l.len() {
            return None;
        }
        let f = (pos - pos.floor()) as f32;
        Some((self.head_l[i] + (self.head_l[i + 1] - self.head_l[i]) * f, self.head_r[i] + (self.head_r[i + 1] - self.head_r[i]) * f))
    }

    /// Frames in the loop-entry head (0 without one).
    pub fn head_frames(&self) -> usize {
        self.head_l.len()
    }

    /// A stream with no worker yet (replaced once the worker starts); reads always miss.
    pub fn placeholder(asset_frames: u64) -> Self {
        let control = Arc::new(StreamControl { want: AtomicU64::new(NO_WANT), generation: AtomicU64::new(0), stop: AtomicBool::new(false) });
        Self {
            asset_frames,
            queue: None,
            control,
            last_request: NO_WANT,
            ring_l: Vec::new(),
            ring_r: Vec::new(),
            ring_start: 0,
            ring_fill: 0,
            ring_head: 0,
            generation: 0,
            head_start: 0,
            head_l: Arc::from(Vec::new()),
            head_r: Arc::from(Vec::new()),
        }
    }

    /// Request `source_frame` unless it was the last request already (RT-safe).
    pub fn request_once(&mut self, source_frame: u64) {
        if self.last_request != source_frame {
            self.last_request = source_frame;
            self.request(source_frame);
        }
    }

    /// Absorb delivered blocks that continue the ring (or start it after a seek). Blocks
    /// that do not fit stay queued (back-pressure on the worker); stale generations and
    /// out-of-order blocks are discarded. RT-safe.
    pub fn pump(&mut self) {
        let current_gen = self.control.generation.load(Ordering::Relaxed);
        let Some(queue) = self.queue.as_mut() else { return };
        while let Some(next) = queue.peek() {
            let n = usize::from(next.frames).min(STREAM_BLOCK);
            let stale = next.generation != current_gen;
            let expected_next = self.ring_start + self.ring_fill as u64;
            let out_of_order = self.ring_fill > 0 && next.source_frame != expected_next;
            if !stale && !out_of_order && self.ring_fill + n > RING_FRAMES {
                break; // wait for the reader to release frames
            }
            let Some(block) = queue.pop() else { break };
            if stale || out_of_order {
                continue;
            }
            if self.ring_fill == 0 {
                self.ring_start = block.source_frame;
                self.ring_head = 0;
            }
            for i in 0..n {
                let idx = (self.ring_head + self.ring_fill + i) % RING_FRAMES;
                self.ring_l[idx] = block.left[i];
                self.ring_r[idx] = block.right[i];
            }
            self.ring_fill += n;
        }
        self.generation = current_gen;
    }

    /// Whether source frames `[from, to)` are in the ring.
    pub fn has(&self, from: u64, to: u64) -> bool {
        self.ring_fill > 0 && from >= self.ring_start && to <= self.ring_start + self.ring_fill as u64
    }

    /// Ask the worker to restart at `source_frame` (seek/loop); clears the ring.
    pub fn request(&mut self, source_frame: u64) {
        self.ring_fill = 0;
        self.ring_head = 0;
        self.control.want.store(source_frame, Ordering::Relaxed);
        self.control.generation.fetch_add(1, Ordering::Relaxed);
    }

    /// Sample at a fractional source position (linear), `None` when not in the ring.
    #[inline]
    pub fn read(&self, pos: f64) -> Option<(f32, f32)> {
        let i = pos.floor() as u64;
        if self.ring_l.is_empty() || !self.has(i, i + 2) {
            return None;
        }
        let f = (pos - i as f64) as f32;
        let a = (self.ring_head + (i - self.ring_start) as usize) % RING_FRAMES;
        let b = (a + 1) % RING_FRAMES;
        Some((self.ring_l[a] + (self.ring_l[b] - self.ring_l[a]) * f, self.ring_r[a] + (self.ring_r[b] - self.ring_r[a]) * f))
    }

    /// Release frames before `source_frame` so the worker can run ahead.
    pub fn release_before(&mut self, source_frame: u64) {
        if self.ring_fill == 0 || source_frame <= self.ring_start {
            return;
        }
        let drop = ((source_frame - self.ring_start) as usize).min(self.ring_fill);
        self.ring_head = (self.ring_head + drop) % RING_FRAMES;
        self.ring_start += drop as u64;
        self.ring_fill -= drop;
    }

    pub fn ring_fill(&self) -> usize {
        self.ring_fill
    }
}

/// Worker side of one stream.
struct Feed {
    reader: StreamReader,
    producer: QueueProducer<StreamBlock>,
    control: Arc<StreamControl>,
    cursor: u64,
    seen_generation: u64,
    /// Frames the RT has committed to consume (worker stops `RING_FRAMES` ahead of it).
    done: bool,
}

/// Decoding worker for every streamed clip of a plan; stops when dropped.
pub struct StreamWorker {
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Drop for StreamWorker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// One streamed clip: where reading starts, and the loop entry point to keep a head of.
pub struct StreamRequest {
    pub info: StreamInfo,
    pub first_frame: u64,
    pub loop_entry: Option<u64>,
}

/// The head at `start` of the file at `path`: from the cache, or decoded `HEAD_FRAMES` (off the
/// audio thread) and cached.
fn loop_head(reader: &mut StreamReader, path: &std::path::Path, start: u64) -> Result<LoopHead, String> {
    let key = head_key(path, start);
    if let Ok(cache) = HEADS.lock()
        && let Some((_, h)) = cache.entries.iter().find(|(k, _)| *k == key)
    {
        return Ok(h.clone());
    }
    let head = decode_head(reader, start)?;
    if let Ok(mut cache) = HEADS.lock() {
        if cache.entries.len() >= HEAD_CACHE_ENTRIES {
            cache.entries.pop_front();
        }
        cache.entries.push_back((key, head.clone()));
        match cache.decoded.iter_mut().find(|(p, _)| p == path) {
            Some((_, n)) => *n += 1,
            None => cache.decoded.push((path.to_path_buf(), 1)),
        }
    }
    Ok(head)
}

/// Decode `HEAD_FRAMES` at `start` (off the audio thread).
fn decode_head(reader: &mut StreamReader, start: u64) -> Result<LoopHead, String> {
    reader.seek(start).map_err(|e| e.to_string())?;
    let (mut left, mut right) = (vec![0.0f32; HEAD_FRAMES], vec![0.0f32; HEAD_FRAMES]);
    let mut got = 0;
    while got < HEAD_FRAMES {
        let end = (got + STREAM_BLOCK).min(HEAD_FRAMES);
        let n = reader.read_into(&mut left[got..end], &mut right[got..end]).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        got += n;
    }
    left.truncate(got);
    right.truncate(got);
    Ok(LoopHead { start, left: Arc::from(left), right: Arc::from(right) })
}

/// Open readers for `requests` and start the worker. Returns the RT streams in order.
pub fn start_streams(requests: &[StreamRequest]) -> Result<(Vec<ClipStream>, StreamWorker), String> {
    let mut rts = Vec::with_capacity(requests.len());
    let mut feeds = Vec::with_capacity(requests.len());
    for StreamRequest { info, first_frame, loop_entry } in requests {
        let mut reader = StreamReader::open(&info.path).map_err(|e| e.to_string())?;
        // a head that cannot be read leaves the clip without one (it then gaps as before)
        let head = loop_entry.and_then(|e| loop_head(&mut reader, &info.path, e).map_err(|err| log::warn!("loop head for {}: {err}", info.path.display())).ok());
        reader.seek(*first_frame).map_err(|e| e.to_string())?;
        let (producer, consumer) = ControlQueue::create::<StreamBlock>(STREAM_QUEUE_BLOCKS);
        let control = Arc::new(StreamControl { want: AtomicU64::new(NO_WANT), generation: AtomicU64::new(0), stop: AtomicBool::new(false) });
        rts.push(ClipStream::new(info.frames, consumer, control.clone(), head));
        feeds.push(Feed { reader, producer, control, cursor: *first_frame, seen_generation: 0, done: false });
    }
    let stop = Arc::new(AtomicBool::new(false));
    let stop2 = stop.clone();
    let handle = std::thread::Builder::new()
        .name("rawdawg-stream".into())
        .spawn(move || {
            let mut feeds = feeds;
            while !stop2.load(Ordering::Relaxed) {
                let mut idle = true;
                for f in feeds.iter_mut() {
                    let current_gen = f.control.generation.load(Ordering::Relaxed);
                    if current_gen != f.seen_generation {
                        f.seen_generation = current_gen;
                        let want = f.control.want.load(Ordering::Relaxed);
                        if want != NO_WANT {
                            // drain anything already queued is impossible from here; the RT drops stale blocks
                            if f.reader.seek(want).is_ok() {
                                f.cursor = want;
                                f.done = false;
                            }
                        }
                    }
                    if f.done {
                        continue;
                    }
                    while f.producer.free_slots() > 0 {
                        let mut block = StreamBlock { source_frame: f.cursor, generation: f.seen_generation, frames: 0, left: [0.0; STREAM_BLOCK], right: [0.0; STREAM_BLOCK] };
                        let got: usize = f.reader.read_into(&mut block.left[..], &mut block.right[..]).unwrap_or_default();
                        if got == 0 {
                            f.done = true;
                            break;
                        }
                        block.frames = got as u16;
                        f.cursor += got as u64;
                        let _ = f.producer.push(block);
                        idle = false;
                    }
                }
                if idle {
                    std::thread::sleep(std::time::Duration::from_millis(2));
                }
            }
        })
        .map_err(|e| e.to_string())?;
    Ok((rts, StreamWorker { stop, handle: Some(handle) }))
}
