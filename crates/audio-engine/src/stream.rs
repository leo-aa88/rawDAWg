//! Clip streaming (ARCHITECTURE §6): long audio assets are not decoded whole. A worker
//! thread decodes ahead into bounded SPSC queues; the audio thread keeps a fixed ring per
//! clip and reads from it, requesting a refill (via atomics) after seeks/loop wraps and
//! emitting silence — counted as a dropout — while data for the new position is missing.
//! Offline renders wait outside the audio path until every stream is ready.

use rawdawg_rt::{ControlQueue, QueueConsumer, QueueProducer};
use rawdawg_sampler::stream::{StreamInfo, StreamReader};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// Frames per streamed block.
pub const STREAM_BLOCK: usize = 1024;
/// Blocks a stream queue holds (≈ 1.4 s at 48 kHz) and the RT ring length in frames.
pub const STREAM_QUEUE_BLOCKS: usize = 64;
pub const RING_FRAMES: usize = STREAM_BLOCK * 128;
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
}

impl ClipStream {
    fn new(asset_frames: u64, queue: QueueConsumer<StreamBlock>, control: Arc<StreamControl>) -> Self {
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
        }
    }

    /// A stream with no worker yet (replaced once the worker starts); reads always miss.
    pub fn placeholder(asset_frames: u64) -> Self {
        let control = Arc::new(StreamControl { want: AtomicU64::new(NO_WANT), generation: AtomicU64::new(0), stop: AtomicBool::new(false) });
        Self { asset_frames, queue: None, control, last_request: NO_WANT, ring_l: Vec::new(), ring_r: Vec::new(), ring_start: 0, ring_fill: 0, ring_head: 0, generation: 0 }
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

/// Open readers for `infos` and start the worker. Returns the RT streams in order.
pub fn start_streams(infos: &[(StreamInfo, u64)]) -> Result<(Vec<ClipStream>, StreamWorker), String> {
    let mut rts = Vec::with_capacity(infos.len());
    let mut feeds = Vec::with_capacity(infos.len());
    for (info, first_frame) in infos {
        let mut reader = StreamReader::open(&info.path).map_err(|e| e.to_string())?;
        reader.seek(*first_frame).map_err(|e| e.to_string())?;
        let (producer, consumer) = ControlQueue::create::<StreamBlock>(STREAM_QUEUE_BLOCKS);
        let control = Arc::new(StreamControl { want: AtomicU64::new(NO_WANT), generation: AtomicU64::new(0), stop: AtomicBool::new(false) });
        rts.push(ClipStream::new(info.frames, consumer, control.clone()));
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
