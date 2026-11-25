//! Time maps: tempo (us per quarter, constant segments) and meter.
//!
//! All conversions go through exact integer arithmetic. Each tempo segment
//! stores its cumulative start time in **tick-microseconds** (`ticks × us/qn`,
//! i.e. microseconds × PPQ) as `i128`, so tick → time is exact and time →
//! frame rounds once from the exact rational value, never accumulating
//! per-block error.

use rawdawg_types::units::{Meter, PPQ, SampleRate, Tick, UsPerQuarter};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MapError {
    #[error("map must have an entry at tick 0")]
    MissingOrigin,
    #[error("entries must be strictly increasing in tick (duplicate or unordered at tick {0})")]
    Unordered(i64),
    #[error("negative tick {0}")]
    NegativeTick(i64),
    #[error("invalid tempo {0} us/qn (allowed 150000..=3000000)")]
    InvalidTempo(u32),
    #[error("invalid meter {numerator}/{denominator}")]
    InvalidMeter { numerator: u8, denominator: u8 },
    #[error("meter change at tick {0} is not on a bar boundary")]
    MeterNotOnBar(i64),
    #[error("arithmetic overflow")]
    Overflow,
}

/// A tempo change at a tick (constant tempo until the next entry).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TempoEntry {
    pub tick: Tick,
    pub us_per_quarter: UsPerQuarter,
}

/// A meter change at a tick (must be on a bar boundary of the previous meter).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MeterEntry {
    pub tick: Tick,
    pub meter: Meter,
}

#[derive(Clone, Copy, Debug)]
struct TempoSegment {
    start_tick: i64,
    us_per_quarter: i128,
    /// Cumulative time at `start_tick`, in tick-microseconds (us × PPQ).
    start_tick_us: i128,
}

/// Validated, precomputed tempo map.
#[derive(Clone, Debug)]
pub struct TempoMap {
    entries: Vec<TempoEntry>,
    segments: Vec<TempoSegment>,
}

impl TempoMap {
    /// Constant 120 BPM.
    pub fn constant(us_per_quarter: UsPerQuarter) -> Self {
        Self::new(vec![TempoEntry { tick: Tick::ZERO, us_per_quarter }]).expect("constant map is valid")
    }

    pub fn new(mut entries: Vec<TempoEntry>) -> Result<Self, MapError> {
        if entries.is_empty() {
            return Err(MapError::MissingOrigin);
        }
        entries.sort_by_key(|e| e.tick);
        if entries[0].tick != Tick::ZERO {
            return Err(if entries[0].tick.0 < 0 { MapError::NegativeTick(entries[0].tick.0) } else { MapError::MissingOrigin });
        }
        for w in entries.windows(2) {
            if w[1].tick <= w[0].tick {
                return Err(MapError::Unordered(w[1].tick.0));
            }
        }
        for e in &entries {
            if !e.us_per_quarter.is_valid() {
                return Err(MapError::InvalidTempo(e.us_per_quarter.0));
            }
        }
        let mut segments = Vec::with_capacity(entries.len());
        let mut acc: i128 = 0;
        for (i, e) in entries.iter().enumerate() {
            segments.push(TempoSegment { start_tick: e.tick.0, us_per_quarter: i128::from(e.us_per_quarter.0), start_tick_us: acc });
            if let Some(next) = entries.get(i + 1) {
                let dt = i128::from(next.tick.0 - e.tick.0);
                acc = acc.checked_add(dt.checked_mul(i128::from(e.us_per_quarter.0)).ok_or(MapError::Overflow)?).ok_or(MapError::Overflow)?;
            }
        }
        Ok(Self { entries, segments })
    }

    pub fn entries(&self) -> &[TempoEntry] {
        &self.entries
    }

    fn segment_for_tick(&self, tick: i64) -> &TempoSegment {
        let idx = self.segments.partition_point(|s| s.start_tick <= tick);
        &self.segments[idx.saturating_sub(1).min(self.segments.len() - 1)]
    }

    /// Tempo in effect at `tick` (ticks before 0 use the first entry).
    pub fn tempo_at(&self, tick: Tick) -> UsPerQuarter {
        UsPerQuarter(self.segment_for_tick(tick.0).us_per_quarter as u32)
    }

    /// Exact elapsed time at `tick` in tick-microseconds (microseconds × PPQ).
    /// Negative ticks extrapolate with the first tempo (count-in).
    pub fn tick_to_tick_us(&self, tick: Tick) -> i128 {
        let seg = self.segment_for_tick(tick.0);
        seg.start_tick_us + i128::from(tick.0 - seg.start_tick) * seg.us_per_quarter
    }

    /// Elapsed microseconds at `tick`, rounded to nearest.
    pub fn tick_to_micros(&self, tick: Tick) -> i64 {
        div_round_nearest(self.tick_to_tick_us(tick), i128::from(PPQ)) as i64
    }

    /// Elapsed seconds at `tick` (f64, for display only).
    pub fn tick_to_seconds(&self, tick: Tick) -> f64 {
        self.tick_to_tick_us(tick) as f64 / (PPQ as f64 * 1_000_000.0)
    }

    /// Absolute frame index of `tick` at `rate`, rounding the exact rational
    /// value once (round half away from zero).
    pub fn tick_to_frame(&self, tick: Tick, rate: SampleRate) -> i64 {
        let tick_us = self.tick_to_tick_us(tick);
        // frames = tick_us * rate / (PPQ * 1e6)
        let num = tick_us * i128::from(rate.0);
        let den = i128::from(PPQ) * 1_000_000;
        div_round_nearest(num, den) as i64
    }

    /// Exact rational frame position of `tick`: returns (numerator, denominator) with
    /// `frame = num / den`. Used by tests as an oracle for rounding.
    pub fn tick_to_frame_rational(&self, tick: Tick, rate: SampleRate) -> (i128, i128) {
        (self.tick_to_tick_us(tick) * i128::from(rate.0), i128::from(PPQ) * 1_000_000)
    }

    /// Inverse of [`tick_to_tick_us`](Self::tick_to_tick_us): the (possibly fractional) tick at
    /// an exact tick-microsecond position, floored to an integer tick.
    pub fn tick_us_to_tick_floor(&self, tick_us: i128) -> Tick {
        let idx = self.segments.partition_point(|s| s.start_tick_us <= tick_us);
        let seg = &self.segments[idx.saturating_sub(1).min(self.segments.len() - 1)];
        let dt = (tick_us - seg.start_tick_us).div_euclid(seg.us_per_quarter);
        Tick(seg.start_tick + dt as i64)
    }

    /// Tick at (or immediately before) an absolute frame position.
    pub fn frame_to_tick_floor(&self, frame: i64, rate: SampleRate) -> Tick {
        // tick_us = frame * PPQ * 1e6 / rate  (floor)
        let num = i128::from(frame) * i128::from(PPQ) * 1_000_000;
        let tick_us = num.div_euclid(i128::from(rate.0));
        self.tick_us_to_tick_floor(tick_us)
    }

    /// Fractional tick position at a frame (for display / smooth playhead).
    pub fn frame_to_tick_f64(&self, frame: i64, rate: SampleRate) -> f64 {
        let tick_us = frame as f64 * (PPQ as f64) * 1_000_000.0 / rate.as_f64();
        let idx = self.segments.partition_point(|s| (s.start_tick_us as f64) <= tick_us);
        let seg = &self.segments[idx.saturating_sub(1).min(self.segments.len() - 1)];
        seg.start_tick as f64 + (tick_us - seg.start_tick_us as f64) / seg.us_per_quarter as f64
    }

    /// First tick `t` with `tick_to_frame(t) >= frame`, i.e. the first tick whose
    /// event lands in a block starting at `frame`. Equivalent to the half-open
    /// block rule: an event at tick `t` belongs to block `[start, end)` iff
    /// `start <= tick_to_frame(t) < end`.
    pub fn first_tick_at_or_after_frame(&self, frame: i64, rate: SampleRate) -> Tick {
        let mut t = self.frame_to_tick_floor(frame, rate);
        // floor can land one tick early due to rounding; advance until the frame rule holds.
        while self.tick_to_frame(t, rate) < frame {
            t = Tick(t.0 + 1);
        }
        // and back up while the previous tick still satisfies it (floor may overshoot by rounding).
        while t.0 > i64::MIN + 1 && self.tick_to_frame(Tick(t.0 - 1), rate) >= frame {
            t = Tick(t.0 - 1);
        }
        t
    }
}

impl Default for TempoMap {
    fn default() -> Self {
        Self::constant(UsPerQuarter::DEFAULT)
    }
}

fn div_round_nearest(num: i128, den: i128) -> i128 {
    debug_assert!(den > 0);
    if num >= 0 { (num + den / 2) / den } else { -((-num + den / 2) / den) }
}

/// Validated meter map.
#[derive(Clone, Debug)]
pub struct MeterMap {
    entries: Vec<MeterEntry>,
    /// Bar index at each entry's start.
    bar_starts: Vec<i64>,
}

impl MeterMap {
    pub fn constant(meter: Meter) -> Self {
        Self::new(vec![MeterEntry { tick: Tick::ZERO, meter }]).expect("constant meter map is valid")
    }

    pub fn new(mut entries: Vec<MeterEntry>) -> Result<Self, MapError> {
        if entries.is_empty() {
            return Err(MapError::MissingOrigin);
        }
        entries.sort_by_key(|e| e.tick);
        if entries[0].tick != Tick::ZERO {
            return Err(if entries[0].tick.0 < 0 { MapError::NegativeTick(entries[0].tick.0) } else { MapError::MissingOrigin });
        }
        for e in &entries {
            if !e.meter.is_valid() {
                return Err(MapError::InvalidMeter { numerator: e.meter.numerator, denominator: e.meter.denominator });
            }
        }
        let mut bar_starts = Vec::with_capacity(entries.len());
        let mut bar: i64 = 0;
        for (i, e) in entries.iter().enumerate() {
            bar_starts.push(bar);
            if let Some(next) = entries.get(i + 1) {
                if next.tick <= e.tick {
                    return Err(MapError::Unordered(next.tick.0));
                }
                let span = next.tick.0 - e.tick.0;
                let bar_ticks = e.meter.bar_ticks().0;
                if span % bar_ticks != 0 {
                    return Err(MapError::MeterNotOnBar(next.tick.0));
                }
                bar += span / bar_ticks;
            }
        }
        Ok(Self { entries, bar_starts })
    }

    pub fn entries(&self) -> &[MeterEntry] {
        &self.entries
    }

    fn index_for_tick(&self, tick: i64) -> usize {
        self.entries.partition_point(|e| e.tick.0 <= tick).saturating_sub(1).min(self.entries.len() - 1)
    }

    pub fn meter_at(&self, tick: Tick) -> Meter {
        self.entries[self.index_for_tick(tick.0)].meter
    }

    /// Musical position: (bar, beat, tick-within-beat), all zero-based. Negative
    /// ticks (count-in) yield negative bars using the first meter.
    pub fn position(&self, tick: Tick) -> BarBeatTick {
        let i = self.index_for_tick(tick.0);
        let e = &self.entries[i];
        let rel = tick.0 - e.tick.0;
        let bar_ticks = e.meter.bar_ticks().0;
        let beat_ticks = e.meter.beat_ticks().0;
        let bar = self.bar_starts[i] + rel.div_euclid(bar_ticks);
        let in_bar = rel.rem_euclid(bar_ticks);
        BarBeatTick { bar, beat: in_bar / beat_ticks, tick: in_bar % beat_ticks }
    }

    /// Tick at the start of a (zero-based) bar.
    pub fn bar_start_tick(&self, bar: i64) -> Tick {
        let i = self.bar_starts.partition_point(|b| *b <= bar).saturating_sub(1).min(self.entries.len() - 1);
        let e = &self.entries[i];
        Tick(e.tick.0 + (bar - self.bar_starts[i]) * e.meter.bar_ticks().0)
    }

    /// Start tick of the bar containing `tick`.
    pub fn bar_start_containing(&self, tick: Tick) -> Tick {
        self.bar_start_tick(self.position(tick).bar)
    }
}

impl Default for MeterMap {
    fn default() -> Self {
        Self::constant(Meter::FOUR_FOUR)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BarBeatTick {
    pub bar: i64,
    pub beat: i64,
    pub tick: i64,
}

impl std::fmt::Display for BarBeatTick {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // One-based display like FL Studio: bar:beat:tick
        write!(f, "{}:{}:{:03}", self.bar + 1, self.beat + 1, self.tick)
    }
}

/// Convert a frame count to a wall-clock duration for display.
pub fn frames_to_seconds(frames: i64, rate: SampleRate) -> f64 {
    frames as f64 / rate.as_f64()
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    const R48: SampleRate = SampleRate(48_000);

    #[test]
    fn beat_and_bar_frames_at_120bpm_48k() {
        let m = TempoMap::default();
        assert_eq!(m.tick_to_frame(Tick(960), R48), 24_000);
        assert_eq!(m.tick_to_frame(Tick(3840), R48), 96_000);
        assert_eq!(m.tick_to_frame(Tick(0), R48), 0);
        // 1 hour at 120 BPM = 7200 beats
        assert_eq!(m.tick_to_frame(Tick(960 * 7200), R48), 48_000 * 3600);
        assert_eq!(m.tick_to_micros(Tick(960)), 500_000);
    }

    #[test]
    fn single_tick_rounding_matches_rational_oracle() {
        let m = TempoMap::default();
        // one tick at 120 BPM = 500000/960 us = 520.8333 us → 25 frames at 48k exactly
        assert_eq!(m.tick_to_frame(Tick(1), R48), 25);
        let r = SampleRate(44_100);
        // 1 tick → 44100 * 500000 / (960 * 1e6) = 22.96875 → 23
        assert_eq!(m.tick_to_frame(Tick(1), r), 23);
        assert_eq!(m.tick_to_frame(Tick(3), r), 69); // 68.90625 → 69
        for t in 0..5000i64 {
            let (num, den) = m.tick_to_frame_rational(Tick(t), r);
            let exact = num as f64 / den as f64;
            let got = m.tick_to_frame(Tick(t), r) as f64;
            assert!((got - exact).abs() <= 0.5 + 1e-9, "t={t} exact={exact} got={got}");
        }
    }

    #[test]
    fn tempo_change_segments_are_cumulative_and_exact() {
        let m = TempoMap::new(vec![
            TempoEntry { tick: Tick(0), us_per_quarter: UsPerQuarter(500_000) },
            TempoEntry { tick: Tick(3840), us_per_quarter: UsPerQuarter(1_000_000) }, // 60 BPM from bar 2
            TempoEntry { tick: Tick(7680), us_per_quarter: UsPerQuarter::from_bpm(240.0).unwrap() },
        ])
        .unwrap();
        assert_eq!(m.tick_to_frame(Tick(3840), R48), 96_000);
        assert_eq!(m.tick_to_frame(Tick(3840 + 960), R48), 96_000 + 48_000);
        assert_eq!(m.tick_to_frame(Tick(7680), R48), 96_000 + 192_000);
        assert_eq!(m.tick_to_frame(Tick(7680 + 960), R48), 96_000 + 192_000 + 12_000);
        assert_eq!(m.tempo_at(Tick(3839)), UsPerQuarter(500_000));
        assert_eq!(m.tempo_at(Tick(3840)), UsPerQuarter(1_000_000));
        assert_eq!(m.tempo_at(Tick(-10)), UsPerQuarter(500_000));
    }

    #[test]
    fn negative_ticks_extrapolate_for_count_in() {
        let m = TempoMap::default();
        assert_eq!(m.tick_to_frame(Tick(-960), R48), -24_000);
        assert_eq!(m.tick_to_frame(Tick(-1), R48), -25);
    }

    #[test]
    fn frame_to_tick_inverse_consistency() {
        let m = TempoMap::new(vec![TempoEntry { tick: Tick(0), us_per_quarter: UsPerQuarter(500_000) }, TempoEntry { tick: Tick(1920), us_per_quarter: UsPerQuarter(400_000) }])
            .unwrap();
        for rate in SampleRate::SUPPORTED.map(SampleRate) {
            for t in [0i64, 1, 7, 959, 960, 1919, 1920, 1921, 5000, 123_456] {
                let f = m.tick_to_frame(Tick(t), rate);
                let back = m.frame_to_tick_floor(f, rate);
                // floor(tick at frame) is within one tick of the original
                assert!((back.0 - t).abs() <= 1, "rate={rate:?} t={t} f={f} back={back:?}");
                let first = m.first_tick_at_or_after_frame(f, rate);
                assert!(m.tick_to_frame(first, rate) >= f);
                assert!(m.tick_to_frame(Tick(first.0 - 1), rate) < f);
            }
        }
    }

    #[test]
    fn half_open_block_scheduling_has_no_duplicates_or_drops() {
        // Every tick in [0, N) must land in exactly one block for every block size.
        let m = TempoMap::new(vec![TempoEntry { tick: Tick(0), us_per_quarter: UsPerQuarter(500_000) }, TempoEntry { tick: Tick(2000), us_per_quarter: UsPerQuarter(333_333) }])
            .unwrap();
        let rate = SampleRate(44_100);
        let total_ticks = 6000i64;
        let end_frame = m.tick_to_frame(Tick(total_ticks), rate);
        for block in [32i64, 64, 127, 128, 256, 511, 1024, 2048] {
            let mut seen = vec![0u8; total_ticks as usize];
            let mut start = 0i64;
            while start < end_frame {
                let end = (start + block).min(end_frame);
                let mut t = m.first_tick_at_or_after_frame(start, rate);
                while t.0 < total_ticks && m.tick_to_frame(t, rate) < end {
                    seen[t.0 as usize] += 1;
                    t = Tick(t.0 + 1);
                }
                start = end;
            }
            assert!(seen.iter().all(|&c| c == 1), "block {block}: some tick scheduled {} times", seen.iter().filter(|&&c| c != 1).count());
        }
    }

    #[test]
    fn rejects_invalid_maps() {
        assert_eq!(TempoMap::new(vec![]).unwrap_err(), MapError::MissingOrigin);
        assert_eq!(TempoMap::new(vec![TempoEntry { tick: Tick(10), us_per_quarter: UsPerQuarter::DEFAULT }]).unwrap_err(), MapError::MissingOrigin);
        assert_eq!(TempoMap::new(vec![TempoEntry { tick: Tick(-1), us_per_quarter: UsPerQuarter::DEFAULT }]).unwrap_err(), MapError::NegativeTick(-1));
        assert_eq!(
            TempoMap::new(vec![TempoEntry { tick: Tick(0), us_per_quarter: UsPerQuarter::DEFAULT }, TempoEntry { tick: Tick(0), us_per_quarter: UsPerQuarter::DEFAULT }])
                .unwrap_err(),
            MapError::Unordered(0)
        );
        assert_eq!(TempoMap::new(vec![TempoEntry { tick: Tick(0), us_per_quarter: UsPerQuarter(10) }]).unwrap_err(), MapError::InvalidTempo(10));
    }

    #[test]
    fn meter_positions_and_changes() {
        let mm = MeterMap::new(vec![
            MeterEntry { tick: Tick(0), meter: Meter::FOUR_FOUR },
            MeterEntry { tick: Tick(3840 * 2), meter: Meter::new(3, 4).unwrap() },
            MeterEntry { tick: Tick(3840 * 2 + 2880 * 2), meter: Meter::new(7, 8).unwrap() },
        ])
        .unwrap();
        assert_eq!(mm.position(Tick(0)), BarBeatTick { bar: 0, beat: 0, tick: 0 });
        assert_eq!(mm.position(Tick(960 + 5)), BarBeatTick { bar: 0, beat: 1, tick: 5 });
        assert_eq!(mm.position(Tick(3840 * 2)), BarBeatTick { bar: 2, beat: 0, tick: 0 });
        assert_eq!(mm.position(Tick(3840 * 2 + 2880 + 960)), BarBeatTick { bar: 3, beat: 1, tick: 0 });
        assert_eq!(mm.position(Tick(3840 * 2 + 2880 * 2 + 480 * 6)), BarBeatTick { bar: 4, beat: 6, tick: 0 });
        assert_eq!(mm.bar_start_tick(4), Tick(3840 * 2 + 2880 * 2));
        assert_eq!(mm.bar_start_tick(5), Tick(3840 * 2 + 2880 * 2 + 3360));
        assert_eq!(mm.position(Tick(-1)), BarBeatTick { bar: -1, beat: 3, tick: 959 });
        assert_eq!(mm.position(Tick(960)).to_string(), "1:2:000");
        assert_eq!(mm.meter_at(Tick(3840 * 2 - 1)), Meter::FOUR_FOUR);
        // not on bar boundary
        assert_eq!(
            MeterMap::new(vec![MeterEntry { tick: Tick(0), meter: Meter::FOUR_FOUR }, MeterEntry { tick: Tick(100), meter: Meter::new(3, 4).unwrap() }]).unwrap_err(),
            MapError::MeterNotOnBar(100)
        );
    }

    proptest! {
        #[test]
        fn tick_to_frame_is_monotonic(a in 0i64..10_000_000, b in 0i64..10_000_000, rate_idx in 0usize..6, bpm in 20.0f64..400.0) {
            let m = TempoMap::constant(UsPerQuarter::from_bpm(bpm).unwrap());
            let rate = SampleRate(SampleRate::SUPPORTED[rate_idx]);
            let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
            prop_assert!(m.tick_to_frame(Tick(lo), rate) <= m.tick_to_frame(Tick(hi), rate));
        }

        #[test]
        fn frame_rounding_never_exceeds_half_frame(t in 0i64..100_000_000, rate_idx in 0usize..6, bpm in 20.0f64..400.0) {
            let m = TempoMap::constant(UsPerQuarter::from_bpm(bpm).unwrap());
            let rate = SampleRate(SampleRate::SUPPORTED[rate_idx]);
            let (num, den) = m.tick_to_frame_rational(Tick(t), rate);
            let got = m.tick_to_frame(Tick(t), rate);
            // |got*den - num| <= den/2
            let diff = (i128::from(got) * den - num).abs();
            prop_assert!(diff * 2 <= den + 1);
        }
    }
}
