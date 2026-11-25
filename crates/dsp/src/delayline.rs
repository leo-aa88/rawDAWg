//! Fixed-capacity delay lines (integer and fractional) for compensation, dry-path
//! alignment and modulation effects. Allocated at prepare time, RT-safe afterwards.

#[derive(Clone, Debug)]
pub struct DelayLine {
    buf: Vec<f32>,
    write: usize,
    mask: usize,
}

impl DelayLine {
    /// Capacity is rounded up to a power of two ≥ `max_delay + 1`.
    pub fn new(max_delay: usize) -> Self {
        let cap = (max_delay + 1).next_power_of_two().max(2);
        Self { buf: vec![0.0; cap], write: 0, mask: cap - 1 }
    }
    pub fn capacity(&self) -> usize {
        self.buf.len()
    }
    pub fn reset(&mut self) {
        self.buf.fill(0.0);
        self.write = 0;
    }
    #[inline]
    pub fn push(&mut self, x: f32) {
        self.buf[self.write] = x;
        self.write = (self.write + 1) & self.mask;
    }
    /// Sample written `delay` pushes ago (0 = the most recent push).
    #[inline]
    pub fn read(&self, delay: usize) -> f32 {
        self.buf[(self.write + self.buf.len() - 1 - (delay & self.mask)) & self.mask]
    }
    /// Linear-interpolated read at a fractional delay (≥ 0).
    #[inline]
    pub fn read_frac(&self, delay: f32) -> f32 {
        let d = delay.max(0.0);
        let i = d.floor() as usize;
        let f = d - i as f32;
        let a = self.read(i);
        let b = self.read(i + 1);
        a + (b - a) * f
    }
    /// Delay a block in place by `delay` frames (compensation).
    pub fn delay_block(&mut self, buf: &mut [f32], delay: usize) {
        if delay == 0 {
            return;
        }
        for s in buf.iter_mut() {
            self.push(*s);
            *s = self.read(delay);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integer_and_fractional_reads() {
        let mut d = DelayLine::new(10);
        for i in 0..8 {
            d.push(i as f32);
        }
        assert_eq!(d.read(0), 7.0);
        assert_eq!(d.read(3), 4.0);
        assert!((d.read_frac(2.5) - 4.5).abs() < 1e-6);
        let mut block: Vec<f32> = (0..6).map(|i| (i + 10) as f32).collect();
        let mut dl = DelayLine::new(4);
        dl.delay_block(&mut block, 2);
        assert_eq!(block, vec![0.0, 0.0, 10.0, 11.0, 12.0, 13.0]);
    }
}
