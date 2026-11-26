//! Loopback latency calibration (IO-04): record the metronome through a cable (or a microphone
//! near a speaker), then measure where the recorded clicks land relative to the beats they were
//! played on. The median deviation is what the recording offset still has to absorb.

/// Where a take's clicks land.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LoopbackMeasurement {
    /// Median deviation of the clicks from their beats (positive: the clicks land late, so the
    /// recording offset should grow by this much).
    pub late_ms: f64,
    /// Clicks found (one per beat at most).
    pub clicks: usize,
    /// Spread of the deviations (10th to 90th percentile): large values mean an unreliable
    /// measurement (noise, a missed click, or not the metronome).
    pub spread_ms: f64,
}

/// A spread above this makes the measurement suspect.
pub const MAX_RELIABLE_SPREAD_MS: f64 = 2.0;
const MIN_CLICKS: usize = 4;

/// Measure the clicks in `audio` (one channel, `sample_rate`) against `beats`: the frame, inside
/// `audio`, at which each beat lands as the take is placed now. A click is the first sample
/// within half a beat of its beat that reaches a quarter of the take's peak.
pub fn measure_loopback(audio: &[f32], sample_rate: u32, beats: &[i64]) -> Result<LoopbackMeasurement, String> {
    let peak = audio.iter().fold(0.0f32, |m, s| if s.is_finite() { m.max(s.abs()) } else { m });
    if peak < 1e-3 {
        return Err("the take is silent: no click reached the input (check the cable or the microphone)".into());
    }
    let threshold = peak * 0.25;
    let spacing = beats.windows(2).map(|w| w[1] - w[0]).filter(|d| *d > 0).min().unwrap_or(i64::from(sample_rate) / 2);
    let window = (spacing / 2).clamp(1, i64::from(sample_rate) / 4);
    let n = audio.len() as i64;
    let mut deviations: Vec<i64> = Vec::with_capacity(beats.len());
    for &b in beats {
        let (lo, hi) = ((b - window).max(0), (b + window).min(n));
        if lo >= hi {
            continue;
        }
        if let Some(i) = (lo..hi).find(|&i| audio[i as usize].abs() >= threshold) {
            deviations.push(i - b);
        }
    }
    if deviations.len() < MIN_CLICKS {
        return Err(format!("found {} click(s); record at least {MIN_CLICKS} beats with the metronome on", deviations.len()));
    }
    deviations.sort_unstable();
    let to_ms = |frames: i64| frames as f64 * 1000.0 / f64::from(sample_rate);
    let median = deviations[deviations.len() / 2];
    let (p10, p90) = (deviations[deviations.len() / 10], deviations[(deviations.len() * 9) / 10]);
    Ok(LoopbackMeasurement { late_ms: to_ms(median), clicks: deviations.len(), spread_ms: to_ms(p90 - p10) })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: u32 = 48_000;

    /// A metronome-like click train (1 kHz bursts with an instant attack), delayed by `delay`
    /// frames, with some noise.
    fn take(beats: &[i64], delay: i64, len: usize, noise: f32) -> Vec<f32> {
        let mut x: Vec<f32> = (0..len).map(|i| noise * (((i * 7919) % 1000) as f32 / 500.0 - 1.0)).collect();
        for &b in beats {
            let start = b + delay;
            for k in 0..2400i64 {
                let i = start + k;
                if (0..len as i64).contains(&i) {
                    let env = 0.5 * (-(k as f32) / 300.0).exp();
                    x[i as usize] += env * (std::f32::consts::TAU * 1000.0 * k as f32 / SR as f32 + 0.3).sin();
                }
            }
        }
        x
    }

    #[test]
    fn finds_the_delay_of_late_and_early_clicks() {
        let beats: Vec<i64> = (0..8).map(|b| 2400 + b * 24_000).collect();
        for delay in [0i64, 48, 576, -96] {
            let x = take(&beats, delay, 8 * 24_000 + 4800, 0.002);
            let m = measure_loopback(&x, SR, &beats).unwrap();
            let expected = delay as f64 * 1000.0 / f64::from(SR);
            assert!((m.late_ms - expected).abs() < 0.1, "delay {delay}: measured {} ms, expected {expected} ms", m.late_ms);
            assert_eq!(m.clicks, 8);
            assert!(m.spread_ms < 0.1, "{}", m.spread_ms);
        }
    }

    #[test]
    fn silence_and_too_few_clicks_are_reported() {
        let beats: Vec<i64> = (0..8).map(|b| b * 24_000).collect();
        assert!(measure_loopback(&vec![0.0; 200_000], SR, &beats).unwrap_err().contains("silent"));
        let x = take(&beats[..2], 0, 200_000, 0.0);
        assert!(measure_loopback(&x, SR, &beats).unwrap_err().contains("found 2"));
    }

    #[test]
    fn an_irregular_take_has_a_large_spread() {
        let beats: Vec<i64> = (0..10).map(|b| 2400 + b * 24_000).collect();
        // each click lands somewhere else: not a loopback of the metronome
        let jittered: Vec<i64> = beats.iter().enumerate().map(|(i, b)| b + (i as i64 * 911) % 2000).collect();
        let mut x = vec![0.0f32; 10 * 24_000 + 4800];
        for (b, j) in beats.iter().zip(&jittered) {
            let one = take(&[*j], 0, x.len(), 0.0);
            for (a, o) in x.iter_mut().zip(one) {
                *a += o;
            }
            let _ = b;
        }
        let m = measure_loopback(&x, SR, &beats).unwrap();
        assert!(m.spread_ms > MAX_RELIABLE_SPREAD_MS, "{m:?}");
    }
}
