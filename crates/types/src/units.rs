//! Musical and audio units with checked arithmetic.
//!
//! * Project time is integer ticks with PPQ = 960.
//! * Tempo is a positive integer number of microseconds per quarter note.
//! * Frames are signed 64-bit so that negative offsets (count-in, latency
//!   compensation) can be represented; project positions are nonnegative.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::ops::{Add, Sub};

/// Pulses per quarter note (fixed timebase).
pub const PPQ: i64 = 960;

/// Error raised by checked unit arithmetic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum UnitError {
    #[error("integer overflow in time arithmetic")]
    Overflow,
    #[error("value out of range: {0}")]
    OutOfRange(&'static str),
}

/// A position or duration in ticks (PPQ 960).
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Tick(pub i64);

impl Tick {
    pub const ZERO: Tick = Tick(0);
    pub const QUARTER: Tick = Tick(PPQ);

    pub const fn new(v: i64) -> Self {
        Tick(v)
    }
    pub fn checked_add(self, rhs: Tick) -> Result<Tick, UnitError> {
        self.0.checked_add(rhs.0).map(Tick).ok_or(UnitError::Overflow)
    }
    pub fn checked_sub(self, rhs: Tick) -> Result<Tick, UnitError> {
        self.0.checked_sub(rhs.0).map(Tick).ok_or(UnitError::Overflow)
    }
    pub fn checked_mul(self, k: i64) -> Result<Tick, UnitError> {
        self.0.checked_mul(k).map(Tick).ok_or(UnitError::Overflow)
    }
    pub fn is_nonnegative(self) -> bool {
        self.0 >= 0
    }
    /// Ticks for a note value expressed as a fraction of a whole note
    /// (e.g. `(1, 16)` for a sixteenth). Whole note = 4 quarters.
    pub fn from_note_value(numer: i64, denom: i64) -> Result<Tick, UnitError> {
        if denom <= 0 || numer < 0 {
            return Err(UnitError::OutOfRange("note value"));
        }
        let whole = PPQ.checked_mul(4).ok_or(UnitError::Overflow)?;
        let n = whole.checked_mul(numer).ok_or(UnitError::Overflow)?;
        if n % denom != 0 {
            return Err(UnitError::OutOfRange("note value not representable in PPQ 960"));
        }
        Ok(Tick(n / denom))
    }
}

impl Add for Tick {
    type Output = Tick;
    fn add(self, rhs: Tick) -> Tick {
        Tick(self.0 + rhs.0)
    }
}
impl Sub for Tick {
    type Output = Tick;
    fn sub(self, rhs: Tick) -> Tick {
        Tick(self.0 - rhs.0)
    }
}
impl fmt::Debug for Tick {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}t", self.0)
    }
}
impl fmt::Display for Tick {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A position or duration in audio frames (one frame = one sample per channel).
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Frames(pub i64);

impl Frames {
    pub const ZERO: Frames = Frames(0);
    pub const fn new(v: i64) -> Self {
        Frames(v)
    }
    pub fn checked_add(self, rhs: Frames) -> Result<Frames, UnitError> {
        self.0.checked_add(rhs.0).map(Frames).ok_or(UnitError::Overflow)
    }
    pub fn checked_sub(self, rhs: Frames) -> Result<Frames, UnitError> {
        self.0.checked_sub(rhs.0).map(Frames).ok_or(UnitError::Overflow)
    }
    /// Convert a nonnegative frame count into `usize` (for buffer indexing).
    pub fn to_usize(self) -> Option<usize> {
        usize::try_from(self.0).ok()
    }
}
impl Add for Frames {
    type Output = Frames;
    fn add(self, rhs: Frames) -> Frames {
        Frames(self.0 + rhs.0)
    }
}
impl Sub for Frames {
    type Output = Frames;
    fn sub(self, rhs: Frames) -> Frames {
        Frames(self.0 - rhs.0)
    }
}
impl fmt::Debug for Frames {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}f", self.0)
    }
}

/// Sample rate in Hz. Only the six supported rates are constructible via [`SampleRate::supported`].
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SampleRate(pub u32);

impl SampleRate {
    pub const SUPPORTED: [u32; 6] = [44_100, 48_000, 88_200, 96_000, 176_400, 192_000];
    pub const DEFAULT: SampleRate = SampleRate(48_000);

    /// Accept any positive rate (devices may report unusual values); use
    /// [`SampleRate::is_supported_project_rate`] to check project support.
    pub fn new(hz: u32) -> Result<Self, UnitError> {
        if hz == 0 { Err(UnitError::OutOfRange("sample rate must be positive")) } else { Ok(SampleRate(hz)) }
    }
    pub fn supported(hz: u32) -> Option<Self> {
        Self::SUPPORTED.contains(&hz).then_some(SampleRate(hz))
    }
    pub fn is_supported_project_rate(self) -> bool {
        Self::SUPPORTED.contains(&self.0)
    }
    pub fn hz(self) -> u32 {
        self.0
    }
    pub fn as_f64(self) -> f64 {
        f64::from(self.0)
    }
}
impl Default for SampleRate {
    fn default() -> Self {
        Self::DEFAULT
    }
}
impl fmt::Debug for SampleRate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}Hz", self.0)
    }
}
impl fmt::Display for SampleRate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0.is_multiple_of(1000) { write!(f, "{} kHz", self.0 / 1000) } else { write!(f, "{:.1} kHz", self.0 as f64 / 1000.0) }
    }
}

/// Tempo as integer microseconds per quarter note (MIDI semantics).
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct UsPerQuarter(pub u32);

impl UsPerQuarter {
    /// 120 BPM.
    pub const DEFAULT: UsPerQuarter = UsPerQuarter(500_000);
    /// 400 BPM.
    pub const MIN: UsPerQuarter = UsPerQuarter(150_000);
    /// 20 BPM.
    pub const MAX: UsPerQuarter = UsPerQuarter(3_000_000);

    pub fn new(us: u32) -> Result<Self, UnitError> {
        if (Self::MIN.0..=Self::MAX.0).contains(&us) { Ok(UsPerQuarter(us)) } else { Err(UnitError::OutOfRange("tempo outside 20..=400 BPM")) }
    }
    pub fn from_bpm(bpm: f64) -> Result<Self, UnitError> {
        if !bpm.is_finite() || !(20.0..=400.0).contains(&bpm) {
            return Err(UnitError::OutOfRange("tempo outside 20..=400 BPM"));
        }
        let us = (60_000_000.0 / bpm).round();
        Self::new(us as u32)
    }
    pub fn bpm(self) -> Bpm {
        Bpm(60_000_000.0 / f64::from(self.0))
    }
    pub fn micros(self) -> u32 {
        self.0
    }
    pub fn is_valid(self) -> bool {
        (Self::MIN.0..=Self::MAX.0).contains(&self.0)
    }
}
impl Default for UsPerQuarter {
    fn default() -> Self {
        Self::DEFAULT
    }
}
impl fmt::Debug for UsPerQuarter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}us/qn", self.0)
    }
}

/// Beats per minute (display/UI only; the authoritative value is [`UsPerQuarter`]).
#[derive(Clone, Copy, PartialEq, PartialOrd, Debug)]
pub struct Bpm(pub f64);

impl fmt::Display for Bpm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:.2}", self.0)
    }
}

/// Gain in decibels. `-inf` is represented by [`Db::SILENCE`].
#[derive(Clone, Copy, PartialEq, PartialOrd, Debug, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Db(pub f64);

impl Db {
    pub const UNITY: Db = Db(0.0);
    /// Threshold below which gain is treated as zero (silence).
    pub const SILENCE: Db = Db(-144.0);

    pub fn to_linear(self) -> f64 {
        if self.0 <= Self::SILENCE.0 || !self.0.is_finite() { 0.0 } else { 10f64.powf(self.0 / 20.0) }
    }
    pub fn from_linear(lin: f64) -> Db {
        if lin <= 0.0 || !lin.is_finite() { Self::SILENCE } else { Db((20.0 * lin.log10()).max(Self::SILENCE.0)) }
    }
    pub fn is_valid(self) -> bool {
        self.0.is_finite()
    }
}

/// Pan in [-1, 1], -1 = hard left, 0 = center, 1 = hard right.
#[derive(Clone, Copy, PartialEq, PartialOrd, Debug, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Pan(pub f64);

impl Pan {
    pub const CENTER: Pan = Pan(0.0);
    pub fn new(v: f64) -> Result<Self, UnitError> {
        if v.is_finite() && (-1.0..=1.0).contains(&v) { Ok(Pan(v)) } else { Err(UnitError::OutOfRange("pan outside [-1, 1]")) }
    }
    pub fn is_valid(self) -> bool {
        self.0.is_finite() && (-1.0..=1.0).contains(&self.0)
    }
    /// Equal-power mono pan law: returns (left, right) gains with l²+r² = 1.
    pub fn equal_power_gains(self) -> (f32, f32) {
        let theta = (self.0.clamp(-1.0, 1.0) + 1.0) * std::f64::consts::FRAC_PI_4;
        (theta.cos() as f32, theta.sin() as f32)
    }
    /// Stereo balance law: attenuates the opposite side linearly, never boosts.
    pub fn balance_gains(self) -> (f32, f32) {
        let p = self.0.clamp(-1.0, 1.0);
        if p <= 0.0 { (1.0, (1.0 + p) as f32) } else { ((1.0 - p) as f32, 1.0) }
    }
}

/// MIDI pitch 0..=127.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Pitch(pub u8);

impl Pitch {
    pub const A4: Pitch = Pitch(69);
    pub const C4: Pitch = Pitch(60);
    pub fn new(v: u8) -> Result<Self, UnitError> {
        if v <= 127 { Ok(Pitch(v)) } else { Err(UnitError::OutOfRange("pitch outside 0..=127")) }
    }
    pub fn is_valid(self) -> bool {
        self.0 <= 127
    }
    /// Equal-tempered frequency with the given A4 reference (default 440 Hz),
    /// including fractional semitone offset (bend/detune).
    pub fn frequency_hz(self, a4_hz: f64, semitone_offset: f64) -> f64 {
        a4_hz * 2f64.powf((f64::from(self.0) - 69.0 + semitone_offset) / 12.0)
    }
    pub fn name(self) -> String {
        const NAMES: [&str; 12] = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];
        let octave = i32::from(self.0) / 12 - 1;
        format!("{}{}", NAMES[usize::from(self.0 % 12)], octave)
    }
}

/// Normalized velocity in (0, 1]. Zero velocity is a note-off and is not a valid `Velocity`.
#[derive(Clone, Copy, PartialEq, PartialOrd, Debug, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Velocity(pub f32);

impl Velocity {
    pub const DEFAULT: Velocity = Velocity(0.8);
    pub fn new(v: f32) -> Result<Self, UnitError> {
        if v.is_finite() && v > 0.0 && v <= 1.0 { Ok(Velocity(v)) } else { Err(UnitError::OutOfRange("velocity outside (0, 1]")) }
    }
    pub fn is_valid(self) -> bool {
        self.0.is_finite() && self.0 > 0.0 && self.0 <= 1.0
    }
    /// From MIDI 1..=127. Returns `None` for 0 (note-off).
    pub fn from_midi(v: u8) -> Option<Self> {
        (1..=127).contains(&v).then(|| Velocity(f32::from(v) / 127.0))
    }
    pub fn to_midi(self) -> u8 {
        ((self.0 * 127.0).round() as u8).clamp(1, 127)
    }
}

/// Time signature with power-of-two denominator; meter changes land on bar boundaries.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct Meter {
    pub numerator: u8,
    pub denominator: u8,
}

impl Meter {
    pub const FOUR_FOUR: Meter = Meter { numerator: 4, denominator: 4 };
    pub fn new(numerator: u8, denominator: u8) -> Result<Self, UnitError> {
        let m = Meter { numerator, denominator };
        if m.is_valid() { Ok(m) } else { Err(UnitError::OutOfRange("meter numerator 1..=32, denominator in {1,2,4,8,16,32}")) }
    }
    pub fn is_valid(self) -> bool {
        (1..=32).contains(&self.numerator) && matches!(self.denominator, 1 | 2 | 4 | 8 | 16 | 32)
    }
    /// Ticks per beat (one `1/denominator` note).
    pub fn beat_ticks(self) -> Tick {
        Tick(PPQ * 4 / i64::from(self.denominator))
    }
    /// Ticks per bar.
    pub fn bar_ticks(self) -> Tick {
        Tick(self.beat_ticks().0 * i64::from(self.numerator))
    }
}
impl Default for Meter {
    fn default() -> Self {
        Self::FOUR_FOUR
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn note_values_in_ticks() {
        assert_eq!(Tick::from_note_value(1, 4).unwrap(), Tick(960));
        assert_eq!(Tick::from_note_value(1, 16).unwrap(), Tick(240));
        assert_eq!(Tick::from_note_value(1, 32).unwrap(), Tick(120));
        // triplet eighth = 1/12 whole
        assert_eq!(Tick::from_note_value(1, 12).unwrap(), Tick(320));
        assert_eq!(Tick::from_note_value(1, 1).unwrap(), Tick(3840));
        assert!(Tick::from_note_value(1, 0).is_err());
        assert!(Tick::from_note_value(1, 7).is_err());
    }

    #[test]
    fn checked_tick_arithmetic_detects_overflow() {
        assert_eq!(Tick(i64::MAX).checked_add(Tick(1)), Err(UnitError::Overflow));
        assert_eq!(Tick(5).checked_sub(Tick(7)).unwrap(), Tick(-2));
    }

    #[test]
    fn tempo_bounds_and_bpm_conversion() {
        assert_eq!(UsPerQuarter::from_bpm(120.0).unwrap(), UsPerQuarter(500_000));
        assert!((UsPerQuarter(500_000).bpm().0 - 120.0).abs() < 1e-9);
        assert!(UsPerQuarter::from_bpm(19.9).is_err());
        assert!(UsPerQuarter::from_bpm(400.0).is_ok());
        assert!(UsPerQuarter::from_bpm(400.1).is_err());
        assert!(UsPerQuarter::new(149_999).is_err());
        assert!(UsPerQuarter::new(3_000_001).is_err());
    }

    #[test]
    fn db_linear_round_trip() {
        assert!((Db(0.0).to_linear() - 1.0).abs() < 1e-12);
        assert!((Db(-6.020599913).to_linear() - 0.5).abs() < 1e-9);
        assert!((Db::from_linear(2.0).0 - 6.020599913).abs() < 1e-6);
        assert_eq!(Db::from_linear(0.0), Db::SILENCE);
        assert_eq!(Db(-200.0).to_linear(), 0.0);
        assert_eq!(Db(f64::NAN).to_linear(), 0.0);
    }

    #[test]
    fn equal_power_pan_law() {
        let (l, r) = Pan::CENTER.equal_power_gains();
        assert!((l - r).abs() < 1e-6);
        assert!((l * l + r * r - 1.0).abs() < 1e-6);
        assert!((l - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-6);
        let (l, r) = Pan(-1.0).equal_power_gains();
        assert!((l - 1.0).abs() < 1e-6 && r.abs() < 1e-6);
        let (l, r) = Pan(1.0).equal_power_gains();
        assert!(l.abs() < 1e-6 && (r - 1.0).abs() < 1e-6);
        assert!(Pan::new(1.5).is_err());
        assert!(Pan::new(f64::NAN).is_err());
    }

    #[test]
    fn balance_law_never_boosts() {
        assert_eq!(Pan(0.0).balance_gains(), (1.0, 1.0));
        assert_eq!(Pan(-1.0).balance_gains(), (1.0, 0.0));
        let (l, r) = Pan(0.5).balance_gains();
        assert!((l - 0.5).abs() < 1e-6 && (r - 1.0).abs() < 1e-6);
    }

    #[test]
    fn pitch_frequency_and_names() {
        assert!((Pitch::A4.frequency_hz(440.0, 0.0) - 440.0).abs() < 1e-9);
        assert!((Pitch(57).frequency_hz(440.0, 0.0) - 220.0).abs() < 1e-9);
        assert!((Pitch(60).frequency_hz(440.0, 0.0) - 261.6255653).abs() < 1e-6);
        assert!((Pitch(69).frequency_hz(440.0, 12.0) - 880.0).abs() < 1e-9);
        assert_eq!(Pitch(60).name(), "C4");
        assert_eq!(Pitch(69).name(), "A4");
        assert_eq!(Pitch(0).name(), "C-1");
        assert!(Pitch::new(128).is_err());
    }

    #[test]
    fn velocity_midi_semantics() {
        assert!(Velocity::from_midi(0).is_none());
        assert_eq!(Velocity::from_midi(127).unwrap().to_midi(), 127);
        assert_eq!(Velocity::from_midi(64).unwrap().to_midi(), 64);
        assert!(Velocity::new(0.0).is_err());
        assert!(Velocity::new(1.0).is_ok());
    }

    #[test]
    fn meter_ticks() {
        assert_eq!(Meter::FOUR_FOUR.beat_ticks(), Tick(960));
        assert_eq!(Meter::FOUR_FOUR.bar_ticks(), Tick(3840));
        let m = Meter::new(7, 8).unwrap();
        assert_eq!(m.beat_ticks(), Tick(480));
        assert_eq!(m.bar_ticks(), Tick(3360));
        assert!(Meter::new(0, 4).is_err());
        assert!(Meter::new(4, 3).is_err());
        assert!(Meter::new(33, 4).is_err());
    }

    #[test]
    fn sample_rates() {
        assert!(SampleRate::supported(48_000).is_some());
        assert!(SampleRate::supported(22_050).is_none());
        assert_eq!(SampleRate(48_000).to_string(), "48 kHz");
        assert_eq!(SampleRate(44_100).to_string(), "44.1 kHz");
        assert!(SampleRate::new(0).is_err());
    }
}
