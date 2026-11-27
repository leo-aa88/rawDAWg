//! Parameter descriptors: stable IDs, normalized ↔ physical mapping, display.
//!
//! Every processor exposes an immutable slice of descriptors. Normalized values
//! are `f32` in [0,1]; physical values are `f64` in the descriptor's unit.

use serde::{Deserialize, Serialize};
use std::fmt;

/// Stable numeric parameter identifier within a processor type. Never reused.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ParamId(pub u32);

impl fmt::Debug for ParamId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "P{}", self.0)
    }
}

/// Physical unit for display/parsing.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum ParamUnit {
    None,
    Percent,
    Decibel,
    Hertz,
    Seconds,
    Milliseconds,
    Semitones,
    Cents,
    Degrees,
    Ratio,
    Bpm,
}

/// Mapping between normalized [0,1] and physical values.
#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub enum ParamMapping {
    /// `phys = min + n*(max-min)`.
    Linear { min: f64, max: f64 },
    /// Exponential mapping, requires `0 < min < max`.
    Log { min: f64, max: f64 },
    /// Like `Log` but `n == 0` maps to an explicit zero/bypass value (e.g. "off").
    LogWithZero { min: f64, max: f64 },
    /// Integer steps `0..=steps-1`, with optional labels handled by the processor.
    Discrete { steps: u32 },
    /// Boolean switch.
    Toggle,
}

/// Behavioral flags.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct ParamFlags {
    pub automatable: bool,
    pub modulatable: bool,
    pub stepped: bool,
    pub hidden: bool,
    pub read_only: bool,
    /// Bypass semantics (host may map to its bypass control).
    pub is_bypass: bool,
}

/// Semantic category used by randomization locks.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum ParamCategory {
    Wave,
    Pitch,
    Envelope,
    Filter,
    Modulation,
    Effects,
    Topology,
    Mix,
    /// Safety-related (master protection); locked by default.
    Safety,
    /// Routing to the DAW; locked by default.
    Routing,
}

/// Immutable description of one parameter.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ParamDescriptor {
    pub id: ParamId,
    /// Stable semantic identifier, e.g. `amp.attack`.
    pub key: &'static str,
    /// Human-readable name.
    pub name: &'static str,
    pub unit: ParamUnit,
    pub mapping: ParamMapping,
    /// Default in normalized domain.
    pub default_normalized: f32,
    /// Smoothing time in milliseconds (0 = stepped, no smoothing).
    pub smoothing_ms: f32,
    pub flags: ParamFlags,
    pub category: ParamCategory,
    /// Display labels for a `Discrete` mapping, one per step (menus and readouts use them).
    #[serde(skip)]
    pub labels: Option<&'static [&'static str]>,
    /// Added to an unlabelled `Discrete` step for display and typing: step 48 of a −48…+48
    /// transpose reads "+0 st", step 31 of a 1…64 voice count reads "32". The parameter's
    /// value stays the step (saved states, automation and hosts are unaffected).
    #[serde(default)]
    pub display_offset: i32,
}

impl ParamDescriptor {
    /// Convenience constructor for automatable continuous parameters.
    pub const fn continuous(id: u32, key: &'static str, name: &'static str, unit: ParamUnit, mapping: ParamMapping, default_normalized: f32, category: ParamCategory) -> Self {
        Self {
            id: ParamId(id),
            key,
            name,
            unit,
            mapping,
            default_normalized,
            smoothing_ms: 10.0,
            flags: ParamFlags { automatable: true, modulatable: true, stepped: false, hidden: false, read_only: false, is_bypass: false },
            category,
            labels: None,
            display_offset: 0,
        }
    }

    /// Show (and accept) a discrete step as `step + offset`.
    pub const fn with_display_offset(mut self, offset: i32) -> Self {
        self.display_offset = offset;
        self
    }

    /// The number a discrete step stands for (its step plus the display offset).
    pub fn step_value(&self, normalized: f32) -> i64 {
        self.to_physical(normalized) as i64 + i64::from(self.display_offset)
    }

    /// Attach one display label per discrete step.
    pub const fn with_labels(mut self, labels: &'static [&'static str]) -> Self {
        self.labels = Some(labels);
        self
    }

    /// The label of the current step of a labelled discrete parameter.
    pub fn label_for(&self, normalized: f32) -> Option<&'static str> {
        let labels = self.labels?;
        labels.get(self.to_physical(normalized).max(0.0) as usize).copied()
    }

    pub const fn with_smoothing(mut self, ms: f32) -> Self {
        self.smoothing_ms = ms;
        self
    }

    pub const fn stepped(mut self) -> Self {
        self.flags.stepped = true;
        self.flags.modulatable = false;
        self.smoothing_ms = 0.0;
        self
    }

    pub const fn hidden(mut self) -> Self {
        self.flags.hidden = true;
        self
    }

    /// Not settable through the real-time path (e.g. structure changes that allocate).
    pub const fn read_only(mut self) -> Self {
        self.flags.read_only = true;
        self.flags.automatable = false;
        self.flags.modulatable = false;
        self
    }

    /// Map normalized → physical. Input is clamped to [0,1]; NaN maps to the default.
    pub fn to_physical(&self, normalized: f32) -> f64 {
        let n = if normalized.is_finite() { f64::from(normalized.clamp(0.0, 1.0)) } else { f64::from(self.default_normalized) };
        match self.mapping {
            ParamMapping::Linear { min, max } => min + n * (max - min),
            ParamMapping::Log { min, max } => min * (max / min).powf(n),
            ParamMapping::LogWithZero { min, max } => {
                if n <= 0.0 {
                    0.0
                } else {
                    min * (max / min).powf(n)
                }
            }
            ParamMapping::Discrete { steps } => {
                if steps <= 1 {
                    0.0
                } else {
                    (n * f64::from(steps - 1)).round()
                }
            }
            ParamMapping::Toggle => {
                if n >= 0.5 {
                    1.0
                } else {
                    0.0
                }
            }
        }
    }

    /// Map physical → normalized, clamped to [0,1]. Non-finite input maps to the default.
    pub fn to_normalized(&self, physical: f64) -> f32 {
        if !physical.is_finite() {
            return self.default_normalized;
        }
        let n = match self.mapping {
            ParamMapping::Linear { min, max } => {
                if max == min {
                    0.0
                } else {
                    (physical - min) / (max - min)
                }
            }
            ParamMapping::Log { min, max } => {
                if physical <= 0.0 {
                    0.0
                } else {
                    (physical / min).ln() / (max / min).ln()
                }
            }
            ParamMapping::LogWithZero { min, max } => {
                if physical <= 0.0 {
                    0.0
                } else {
                    (physical / min).ln() / (max / min).ln()
                }
            }
            ParamMapping::Discrete { steps } => {
                if steps <= 1 {
                    0.0
                } else {
                    physical / f64::from(steps - 1)
                }
            }
            ParamMapping::Toggle => {
                if physical >= 0.5 {
                    1.0
                } else {
                    0.0
                }
            }
        };
        (n.clamp(0.0, 1.0)) as f32
    }

    /// Default value in physical units.
    pub fn default_physical(&self) -> f64 {
        self.to_physical(self.default_normalized)
    }

    /// Validate the descriptor itself (used by tests over all processors).
    pub fn is_well_formed(&self) -> bool {
        let mapping_ok = match self.mapping {
            ParamMapping::Linear { min, max } => min.is_finite() && max.is_finite() && min < max,
            ParamMapping::Log { min, max } | ParamMapping::LogWithZero { min, max } => min.is_finite() && max.is_finite() && min > 0.0 && min < max,
            ParamMapping::Discrete { steps } => steps >= 2,
            ParamMapping::Toggle => true,
        };
        mapping_ok
            && self.default_normalized.is_finite()
            && (0.0..=1.0).contains(&self.default_normalized)
            && self.smoothing_ms.is_finite()
            && self.smoothing_ms >= 0.0
            && !self.key.is_empty()
            && !self.name.is_empty()
    }

    fn percent_range_is_unity(&self) -> bool {
        match self.mapping {
            ParamMapping::Linear { min, max } | ParamMapping::Log { min, max } | ParamMapping::LogWithZero { min, max } => (max - min).abs() <= 1.5,
            _ => true,
        }
    }

    /// Format a normalized value for display.
    pub fn format(&self, normalized: f32) -> String {
        let p = self.to_physical(normalized);
        match (self.mapping, self.unit) {
            (ParamMapping::Toggle, _) => (if p >= 0.5 { "On" } else { "Off" }).to_owned(),
            (ParamMapping::Discrete { .. }, unit) => self.label_for(normalized).map(str::to_owned).unwrap_or_else(|| {
                let v = self.step_value(normalized);
                match unit {
                    // a range that goes below zero shows its sign (+12 st, −12 st)
                    ParamUnit::Semitones if self.display_offset < 0 => format!("{v:+} st"),
                    ParamUnit::Semitones => format!("{v} st"),
                    _ => format!("{v}"),
                }
            }),
            (ParamMapping::LogWithZero { .. }, _) if p <= 0.0 => "Off".to_owned(),
            (_, ParamUnit::Percent) => {
                // physical ranges wider than unity are already expressed in percent
                let scale = if self.percent_range_is_unity() { 100.0 } else { 1.0 };
                format!("{:.0} %", p * scale)
            }
            (_, ParamUnit::Decibel) => {
                if p <= -144.0 {
                    "-inf dB".to_owned()
                } else {
                    format!("{p:.1} dB")
                }
            }
            (_, ParamUnit::Hertz) => {
                if p >= 1000.0 {
                    format!("{:.2} kHz", p / 1000.0)
                } else {
                    format!("{p:.1} Hz")
                }
            }
            (_, ParamUnit::Seconds) => {
                if p < 1.0 {
                    format!("{:.0} ms", p * 1000.0)
                } else {
                    format!("{p:.2} s")
                }
            }
            (_, ParamUnit::Milliseconds) => format!("{p:.1} ms"),
            (_, ParamUnit::Semitones) => format!("{p:+.0} st"),
            (_, ParamUnit::Cents) => format!("{p:+.0} ct"),
            (_, ParamUnit::Degrees) => format!("{p:.0}°"),
            (_, ParamUnit::Ratio) => format!("{p:.2}:1"),
            (_, ParamUnit::Bpm) => format!("{p:.1} BPM"),
            (_, ParamUnit::None) => format!("{p:.3}"),
        }
    }

    /// Parse user text in physical units into a normalized value.
    pub fn parse(&self, text: &str) -> Option<f32> {
        // a typographic minus (−12 st) reads as a minus
        let t = text.trim().replace('\u{2212}', "-").to_lowercase();
        if let ParamMapping::Discrete { steps } = self.mapping {
            let last = f64::from(steps.max(1) - 1);
            let number = |s: &str| -> Option<f64> {
                let num: String = s.chars().take_while(|c| c.is_ascii_digit() || matches!(c, '-' | '+' | '.')).collect();
                num.parse().ok()
            };
            if let Some(labels) = self.labels {
                let labels: Vec<String> = labels.iter().take(steps as usize).map(|l| l.replace('\u{2212}', "-").to_lowercase()).collect();
                // a step by its whole name
                if let Some(i) = labels.iter().position(|l| *l == t) {
                    return Some(self.to_normalized(i as f64));
                }
                // a number is the step whose name starts with it ("4" → "4 voices"), before a
                // step index: names hold the numbers people read
                if let Some(v) = t.parse::<f64>().ok().filter(|v| v.is_finite())
                    && let Some(i) = labels.iter().position(|l| l.split_whitespace().next().and_then(|w| w.parse::<f64>().ok()) == Some(v))
                {
                    return Some(self.to_normalized(i as f64));
                }
                // the start of exactly one name ("off" → "Off (ms)", "tri" → "Triangle")
                let starts: Vec<usize> = (0..labels.len()).filter(|i| !t.is_empty() && labels[*i].starts_with(t.as_str())).collect();
                if let [i] = starts[..] {
                    return Some(self.to_normalized(i as f64));
                }
            }
            // an unnamed step by the number it shows
            let v = number(&t)?;
            if !v.is_finite() {
                return None;
            }
            let step = (v.round() - f64::from(self.display_offset)).clamp(0.0, last);
            return Some(self.to_normalized(step));
        }
        if matches!(self.mapping, ParamMapping::Toggle) {
            return match t.as_str() {
                "on" | "1" | "true" | "yes" => Some(1.0),
                "off" | "0" | "false" | "no" => Some(0.0),
                _ => None,
            };
        }
        if t == "off" || t == "-inf" || t == "-inf db" {
            return Some(0.0);
        }
        let mut num = String::new();
        let mut suffix = String::new();
        for ch in t.chars() {
            if suffix.is_empty() && (ch.is_ascii_digit() || matches!(ch, '.' | '-' | '+' | 'e')) {
                num.push(ch);
            } else if !ch.is_whitespace() {
                suffix.push(ch);
            }
        }
        let mut v: f64 = num.parse().ok()?;
        match (self.unit, suffix.as_str()) {
            (ParamUnit::Percent, _) => v /= 100.0,
            (ParamUnit::Hertz, "khz" | "k") => v *= 1000.0,
            (ParamUnit::Seconds, "ms") => v /= 1000.0,
            (ParamUnit::Milliseconds, "s") => v *= 1000.0,
            _ => {}
        }
        if !v.is_finite() {
            return None;
        }
        Some(self.to_normalized(v))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn freq() -> ParamDescriptor {
        ParamDescriptor::continuous(1, "filter.cutoff", "Cutoff", ParamUnit::Hertz, ParamMapping::Log { min: 20.0, max: 20000.0 }, 1.0, ParamCategory::Filter)
    }

    #[test]
    fn log_mapping_round_trips_and_is_exponential() {
        let d = freq();
        assert!((d.to_physical(0.0) - 20.0).abs() < 1e-9);
        assert!((d.to_physical(1.0) - 20000.0).abs() < 1e-6);
        // geometric midpoint
        assert!((d.to_physical(0.5) - (20.0f64 * 20000.0).sqrt()).abs() < 1e-6);
        for n in [0.0f32, 0.1, 0.37, 0.5, 0.9, 1.0] {
            let p = d.to_physical(n);
            assert!((d.to_normalized(p) - n).abs() < 1e-6, "n={n}");
        }
        assert_eq!(d.to_normalized(f64::NAN), 1.0);
        assert_eq!(d.to_physical(f32::NAN), 20000.0);
        assert_eq!(d.format(1.0), "20.00 kHz");
        assert!((d.parse("1 kHz").unwrap() - d.to_normalized(1000.0)).abs() < 1e-6);
    }

    #[test]
    fn linear_percent_and_db_formatting() {
        let mix = ParamDescriptor::continuous(2, "mix", "Mix", ParamUnit::Percent, ParamMapping::Linear { min: 0.0, max: 1.0 }, 0.5, ParamCategory::Mix);
        assert_eq!(mix.format(0.25), "25 %");
        assert!((mix.parse("40%").unwrap() - 0.4).abs() < 1e-6);
        let gain = ParamDescriptor::continuous(3, "gain", "Gain", ParamUnit::Decibel, ParamMapping::Linear { min: -60.0, max: 12.0 }, 0.8333333, ParamCategory::Mix);
        assert_eq!(gain.format(1.0), "12.0 dB");
        assert!((gain.parse("-6 dB").unwrap() - gain.to_normalized(-6.0)).abs() < 1e-6);
        assert!(gain.parse("garbage").is_none());
    }

    #[test]
    fn log_with_zero_has_off_position() {
        let d = ParamDescriptor::continuous(4, "lfo.rate", "Rate", ParamUnit::Hertz, ParamMapping::LogWithZero { min: 0.01, max: 50.0 }, 0.0, ParamCategory::Modulation);
        assert_eq!(d.to_physical(0.0), 0.0);
        assert_eq!(d.format(0.0), "Off");
        assert!((d.to_physical(1.0) - 50.0).abs() < 1e-9);
        assert_eq!(d.parse("off"), Some(0.0));
    }

    #[test]
    fn discrete_and_toggle() {
        let mode = ParamDescriptor::continuous(5, "osc.mode", "Mode", ParamUnit::None, ParamMapping::Discrete { steps: 6 }, 0.0, ParamCategory::Topology).stepped();
        assert_eq!(mode.to_physical(1.0), 5.0);
        assert_eq!(mode.to_physical(0.5), 3.0); // round(2.5) = 3
        assert!((mode.to_normalized(2.0) - 0.4).abs() < 1e-6);
        assert!(mode.flags.stepped && !mode.flags.modulatable);
        let t = ParamDescriptor::continuous(6, "bypass", "Bypass", ParamUnit::None, ParamMapping::Toggle, 0.0, ParamCategory::Mix);
        assert_eq!(t.format(0.7), "On");
        assert_eq!(t.parse("off"), Some(0.0));
    }

    #[test]
    fn well_formedness() {
        assert!(freq().is_well_formed());
        let bad = ParamDescriptor::continuous(7, "x", "X", ParamUnit::None, ParamMapping::Log { min: 0.0, max: 1.0 }, 0.5, ParamCategory::Mix);
        assert!(!bad.is_well_formed());
        let bad2 = ParamDescriptor::continuous(8, "x", "X", ParamUnit::None, ParamMapping::Linear { min: 1.0, max: 1.0 }, 0.5, ParamCategory::Mix);
        assert!(!bad2.is_well_formed());
    }

    #[test]
    fn named_steps_take_their_names_a_unique_start_and_the_numbers_they_hold() {
        const LABELS: &[&str] = &["Off (ms)", "1/16", "2 voices", "4 voices", "Triangle", "Tri-saw", "\u{2212}12 st"];
        let d = ParamDescriptor::continuous(9, "x", "X", ParamUnit::None, ParamMapping::Discrete { steps: 7 }, 0.0, ParamCategory::Mix).stepped().with_labels(LABELS);
        let step = |t: &str| d.parse(t).map(|n| d.to_physical(n) as i64);
        assert_eq!(step("Off (ms)"), Some(0), "a whole name");
        assert_eq!(step("off"), Some(0), "a unique start: Off");
        assert_eq!(step("  OFF "), Some(0));
        assert_eq!(step("1/16"), Some(1));
        assert_eq!(step("4"), Some(3), "a number is the name that starts with it, not step 4");
        assert_eq!(step("2"), Some(2));
        assert_eq!(step("4.0"), Some(3), "the number a name starts with, however it is written");
        assert_eq!(step("tri"), None, "a start shared by two names chooses neither");
        assert_eq!(step("trian"), Some(4), "Triangle by a unique start");
        assert_eq!(step("tri-"), Some(5));
        assert_eq!(step("-12 st"), Some(6), "a typographic minus reads as a minus");
        assert_eq!(step("\u{2212}12 st"), Some(6));
        assert_eq!(step("6"), Some(6), "an unnamed number is the step it shows");
        assert_eq!(step("nonsense"), None);
        // an unnamed stepped value with a typographic minus
        let transpose = ParamDescriptor::continuous(1, "t", "Transpose", ParamUnit::Semitones, ParamMapping::Discrete { steps: 97 }, 0.5, ParamCategory::Pitch)
            .stepped()
            .with_display_offset(-48);
        assert_eq!(transpose.parse("\u{2212}12").map(|n| transpose.step_value(n)), Some(-12));
    }

    #[test]
    fn discrete_steps_show_and_accept_the_values_they_stand_for() {
        let transpose = ParamDescriptor::continuous(1, "t", "Transpose", ParamUnit::Semitones, ParamMapping::Discrete { steps: 97 }, 0.5, ParamCategory::Pitch)
            .stepped()
            .with_display_offset(-48);
        assert_eq!(transpose.format(0.5), "+0 st");
        assert_eq!(transpose.format(1.0), "+48 st");
        assert_eq!(transpose.format(0.0), "-48 st");
        let up = transpose.parse("12").unwrap();
        assert_eq!(transpose.to_physical(up), 60.0, "typing 12 is the step for +12, not 12 steps from −48");
        assert_eq!(transpose.format(up), "+12 st");
        assert_eq!(transpose.to_physical(transpose.parse("-12 st").unwrap()), 36.0);
        assert_eq!(transpose.to_physical(transpose.parse("99").unwrap()), 96.0, "clamped to the range");
        let voices =
            ParamDescriptor::continuous(2, "v", "Voices", ParamUnit::None, ParamMapping::Discrete { steps: 64 }, 31.0 / 63.0, ParamCategory::Topology).with_display_offset(1);
        assert_eq!(voices.format(31.0 / 63.0), "32");
        assert_eq!(voices.to_physical(voices.parse("8").unwrap()), 7.0);
        let bend = ParamDescriptor::continuous(3, "b", "Bend", ParamUnit::Semitones, ParamMapping::Discrete { steps: 49 }, 2.0 / 48.0, ParamCategory::Pitch);
        assert_eq!(bend.format(2.0 / 48.0), "2 st", "no sign for a range that starts at zero");
        let mode = ParamDescriptor::continuous(4, "m", "Mode", ParamUnit::None, ParamMapping::Discrete { steps: 3 }, 0.0, ParamCategory::Filter).with_labels(&[
            "Low-pass",
            "High-pass",
            "Notch",
        ]);
        assert_eq!(mode.format(0.5), "High-pass");
        assert_eq!(mode.to_physical(mode.parse("notch").unwrap()), 2.0, "a name is accepted");
        assert_eq!(mode.to_physical(mode.parse("1").unwrap()), 1.0, "and the step number");
    }
}
