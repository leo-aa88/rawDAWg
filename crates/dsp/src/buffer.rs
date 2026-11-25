//! Planar f32 audio buffers.

use serde::{Deserialize, Serialize};

/// Channel layout of a bus.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum BusLayout {
    Mono,
    Stereo,
}

impl BusLayout {
    pub fn channels(self) -> usize {
        match self {
            BusLayout::Mono => 1,
            BusLayout::Stereo => 2,
        }
    }
    pub fn from_channels(n: usize) -> Option<Self> {
        match n {
            1 => Some(BusLayout::Mono),
            2 => Some(BusLayout::Stereo),
            _ => None,
        }
    }
}

/// Owned, preallocated planar buffer. Allocated off the RT thread; `process`
/// works on the first `frames` samples of each channel.
#[derive(Clone, Debug)]
pub struct AudioBuffer {
    channels: Vec<Vec<f32>>,
    capacity: usize,
}

impl AudioBuffer {
    pub fn new(channels: usize, capacity: usize) -> Self {
        Self { channels: (0..channels).map(|_| vec![0.0; capacity]).collect(), capacity }
    }
    pub fn channel_count(&self) -> usize {
        self.channels.len()
    }
    pub fn capacity(&self) -> usize {
        self.capacity
    }
    /// Zero the first `frames` samples of every channel.
    pub fn clear(&mut self, frames: usize) {
        let n = frames.min(self.capacity);
        for ch in &mut self.channels {
            ch[..n].fill(0.0);
        }
    }
    pub fn channel(&self, i: usize) -> &[f32] {
        &self.channels[i]
    }
    pub fn channel_mut(&mut self, i: usize) -> &mut [f32] {
        &mut self.channels[i]
    }
    /// Borrow the first `frames` samples of two channels mutably.
    pub fn stereo_mut(&mut self, frames: usize) -> (&mut [f32], &mut [f32]) {
        let n = frames.min(self.capacity);
        let (a, b) = self.channels.split_at_mut(1);
        (&mut a[0][..n], &mut b[0][..n])
    }
    /// Mutable slices for all channels truncated to `frames`.
    pub fn channels_mut(&mut self, frames: usize) -> impl Iterator<Item = &mut [f32]> {
        let n = frames.min(self.capacity);
        self.channels.iter_mut().map(move |c| &mut c[..n])
    }
    pub fn channels(&self, frames: usize) -> impl Iterator<Item = &[f32]> {
        let n = frames.min(self.capacity);
        self.channels.iter().map(move |c| &c[..n])
    }
    /// Interleave the first `frames` into `out` (length must be `frames * channels`).
    pub fn interleave_into(&self, frames: usize, out: &mut [f32]) {
        let nch = self.channels.len();
        for (i, frame) in out.chunks_exact_mut(nch).take(frames).enumerate() {
            for (c, s) in frame.iter_mut().enumerate() {
                *s = self.channels[c][i];
            }
        }
    }
    /// Deinterleave `input` (frames * channels) into this buffer.
    pub fn deinterleave_from(&mut self, input: &[f32], frames: usize) {
        let nch = self.channels.len();
        for (i, frame) in input.chunks_exact(nch).take(frames.min(self.capacity)).enumerate() {
            for (c, s) in frame.iter().enumerate() {
                self.channels[c][i] = *s;
            }
        }
    }
}

/// Borrowed planar block for one `process` call: optional inputs and outputs
/// on the main bus plus optional sidechain input. All slices have `frames` length.
pub struct AudioBlock<'a> {
    pub frames: usize,
    pub inputs: &'a [&'a [f32]],
    pub outputs: &'a mut [&'a mut [f32]],
    pub sidechain: &'a [&'a [f32]],
}

impl<'a> AudioBlock<'a> {
    pub fn output_only(frames: usize, outputs: &'a mut [&'a mut [f32]]) -> Self {
        Self { frames, inputs: &[], outputs, sidechain: &[] }
    }
    pub fn clear_outputs(&mut self) {
        for o in self.outputs.iter_mut() {
            o[..self.frames].fill(0.0);
        }
    }
    /// Copy inputs to outputs (true dry path) for matching channel counts; extra
    /// outputs are zeroed.
    pub fn copy_input_to_output(&mut self) {
        for (i, o) in self.outputs.iter_mut().enumerate() {
            match self.inputs.get(i) {
                Some(inp) => o[..self.frames].copy_from_slice(&inp[..self.frames]),
                None => o[..self.frames].fill(0.0),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interleave_round_trip() {
        let mut b = AudioBuffer::new(2, 8);
        let il = [1.0, 2.0, 3.0, 4.0, 5.0, 6.0];
        b.deinterleave_from(&il, 3);
        assert_eq!(&b.channel(0)[..3], &[1.0, 3.0, 5.0]);
        assert_eq!(&b.channel(1)[..3], &[2.0, 4.0, 6.0]);
        let mut out = [0.0; 6];
        b.interleave_into(3, &mut out);
        assert_eq!(out, il);
        b.clear(8);
        assert!(b.channel(0).iter().all(|s| *s == 0.0));
    }
}
