//! T-SAMPLER: one-shot/loop bounds and interpolation, pitch/octave duration,
//! envelope, sample-rate conversion, SF2 zones, bank/program, loop,
//! unsupported-feature warning, malformed RIFF rejection, allocation-free process.

use rawdawg_dsp::analysis::{dominant_frequency, fundamental_frequency};
use rawdawg_dsp::math::{peak, rms};
use rawdawg_dsp::processor::{PrepareSpec, ProcessContext, Processor, TransportView};
use rawdawg_dsp::{AudioBlock, BusLayout};
use rawdawg_rt::alloc_guard::{GuardAllocator, measure_rt};
use rawdawg_sampler::sf2::{self, FixtureSample, FixtureZone, Gen};
use rawdawg_sampler::sf2_player::{Sf2Patch, Sf2Player};
use rawdawg_sampler::{SampleAsset, Sampler, SamplerPatch};
use rawdawg_types::events::{EventKind, EventSource, TimedEvent};
use std::sync::Arc;

#[global_allocator]
static GLOBAL: GuardAllocator = GuardAllocator;

const SR: f64 = 48_000.0;

fn run(proc_: &mut dyn Processor, total: usize, block: usize, events: &[(usize, TimedEvent)]) -> Vec<f32> {
    let mut l = vec![0.0f32; block];
    let mut r = vec![0.0f32; block];
    let transport = TransportView::stopped(SR);
    let mut out = Vec::with_capacity(total);
    let mut pos = 0;
    while pos < total {
        let n = block.min(total - pos);
        let mut evs: Vec<TimedEvent> = events.iter().filter(|(at, _)| *at >= pos && *at < pos + n).map(|(at, e)| TimedEvent { frame: (*at - pos) as u32, ..*e }).collect();
        rawdawg_types::events::sort_events(&mut evs);
        let mut outs: [&mut [f32]; 2] = [&mut l[..n], &mut r[..n]];
        let mut ctx = ProcessContext { audio: AudioBlock::output_only(n, &mut outs), events: &evs, transport: &transport };
        proc_.process(&mut ctx);
        out.extend_from_slice(&l[..n]);
        pos += n;
    }
    out
}

fn on(id: u32, pitch: u8) -> TimedEvent {
    TimedEvent::note_on(0, EventSource::TypingKeyboard, 0, id, pitch, 1.0)
}
fn off(id: u32, pitch: u8) -> TimedEvent {
    TimedEvent::note_off(0, EventSource::TypingKeyboard, 0, id, pitch)
}

/// 0.5 s sine at 440 Hz, 44.1 kHz (to also exercise rate conversion).
fn sine_asset() -> Arc<SampleAsset> {
    let rate = 44_100u32;
    let n = rate as usize / 2;
    SampleAsset::mono("sine", rate, (0..n).map(|i| (std::f64::consts::TAU * 440.0 * i as f64 / f64::from(rate)).sin() as f32 * 0.5).collect())
}

fn sampler(patch: SamplerPatch, asset: Arc<SampleAsset>) -> Sampler {
    let mut s = Sampler::new(patch, Some(asset));
    s.prepare(&PrepareSpec::new(SR, 256, BusLayout::Stereo)).unwrap();
    s
}

#[test]
fn root_pitch_plays_back_at_original_pitch_across_sample_rates() {
    let patch = SamplerPatch { root_pitch: 69, env: rawdawg_dsp::envelope::AdsrParams { attack_s: 0.0, decay_s: 0.0, sustain: 1.0, release_s: 0.01 }, ..SamplerPatch::default() };
    let mut s = sampler(patch, sine_asset());
    let buf = run(&mut s, 20_000, 256, &[(0, on(1, 69))]);
    let f = dominant_frequency(&buf[1000..20_000], SR);
    assert!((f - 440.0).abs() < 1.0, "{f}");
    assert!(peak(&buf) <= 0.6);
    // one octave up: 880 Hz and half the duration (one-shot plays to the end)
    let patch = SamplerPatch {
        root_pitch: 69,
        one_shot: true,
        env: rawdawg_dsp::envelope::AdsrParams { attack_s: 0.0, decay_s: 0.0, sustain: 1.0, release_s: 0.001 },
        ..SamplerPatch::default()
    };
    let mut s2 = sampler(patch, sine_asset());
    let buf2 = run(&mut s2, 48_000, 256, &[(0, on(1, 81)), (100, off(1, 81))]);
    assert!((dominant_frequency(&buf2[500..10_000], SR) - 880.0).abs() < 2.0);
    // 0.5 s at 44.1k played 2× faster at 48k: 0.5*48000/2 ≈ 12000 frames of sound
    let last = buf2.iter().rposition(|s| s.abs() > 1e-4).unwrap();
    assert!((11_000..13_500).contains(&last), "one-shot length {last}");
}

#[test]
fn gated_note_releases_and_loop_keeps_sounding() {
    let gated = SamplerPatch { root_pitch: 69, env: rawdawg_dsp::envelope::AdsrParams { attack_s: 0.0, decay_s: 0.0, sustain: 1.0, release_s: 0.02 }, ..SamplerPatch::default() };
    let mut s = sampler(gated, sine_asset());
    let buf = run(&mut s, 24_000, 256, &[(0, on(1, 69)), (4800, off(1, 69))]);
    assert!(rms(&buf[1000..4000]) > 0.1);
    assert_eq!(peak(&buf[8000..]), 0.0, "released note is silent");
    // looped: well beyond the 0.5 s sample the note still sounds, in tune, without clicks
    let looped = SamplerPatch {
        root_pitch: 69,
        loop_mode: rawdawg_sampler::sampler::LoopMode::Forward,
        loop_start: 0.1,
        loop_end: 0.9,
        loop_crossfade_ms: 4.0,
        env: rawdawg_dsp::envelope::AdsrParams { attack_s: 0.0, decay_s: 0.0, sustain: 1.0, release_s: 0.02 },
        ..SamplerPatch::default()
    };
    let mut s2 = sampler(looped, sine_asset());
    let buf2 = run(&mut s2, 96_000, 256, &[(0, on(1, 69))]);
    assert!(rms(&buf2[80_000..96_000]) > 0.1, "loop sustains");
    assert!((dominant_frequency(&buf2[60_000..96_000], SR) - 440.0).abs() < 1.5);
    let max_jump = buf2[20_000..].windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0f32, f32::max);
    assert!(max_jump < 0.08, "loop click {max_jump}");
}

#[test]
fn start_end_bounds_and_envelope_apply() {
    // start at 50%: a 0.5 s sample at 44.1k gives ~0.25 s of material → ~12000 frames at 48k... plus rate ratio
    let patch = SamplerPatch {
        root_pitch: 69,
        one_shot: true,
        start: 0.5,
        end: 1.0,
        env: rawdawg_dsp::envelope::AdsrParams { attack_s: 0.0, decay_s: 0.0, sustain: 1.0, release_s: 0.001 },
        ..SamplerPatch::default()
    };
    let mut s = sampler(patch, sine_asset());
    let buf = run(&mut s, 48_000, 256, &[(0, on(1, 69)), (10, off(1, 69))]);
    let last = buf.iter().rposition(|s| s.abs() > 1e-4).unwrap();
    assert!((11_000..13_500).contains(&last), "half the sample: {last}");
    let slow = SamplerPatch { root_pitch: 69, env: rawdawg_dsp::envelope::AdsrParams { attack_s: 0.2, decay_s: 0.0, sustain: 1.0, release_s: 0.05 }, ..SamplerPatch::default() };
    let mut s2 = sampler(slow, sine_asset());
    let buf2 = run(&mut s2, 24_000, 256, &[(0, on(1, 69))]);
    assert!(peak(&buf2[..2000]) < peak(&buf2[9000..11_000]) * 0.6, "attack ramps");
}

#[test]
fn sampler_without_asset_is_silent_and_state_round_trips() {
    let mut s = Sampler::new(SamplerPatch::default(), None);
    s.prepare(&PrepareSpec::new(SR, 256, BusLayout::Stereo)).unwrap();
    let buf = run(&mut s, 4096, 256, &[(0, on(1, 60))]);
    assert_eq!(peak(&buf), 0.0);
    let bytes = s.save_state();
    let mut t = Sampler::new(SamplerPatch::default(), None);
    t.load_state(&bytes).unwrap();
    assert_eq!(t.patch(), s.patch());
    assert!(t.load_state(b"{\"version\": 9}").is_err());
    assert!(s.set_param(rawdawg_types::ParamId(rawdawg_sampler::sampler::P_ROOT), 0.5));
    assert_eq!(s.patch().root_pitch, 64);
}

#[test]
fn sampler_process_is_allocation_free() {
    let mut s = sampler(SamplerPatch { loop_mode: rawdawg_sampler::sampler::LoopMode::Forward, ..SamplerPatch::default() }, sine_asset());
    run(&mut s, 1024, 256, &[(0, on(1, 60))]);
    let mut l = vec![0.0f32; 512];
    let mut r = vec![0.0f32; 512];
    let transport = TransportView::stopped(SR);
    let events = vec![on(2, 64), on(3, 67), TimedEvent::new(0, EventSource::TypingKeyboard, 0, EventKind::PitchBend { value: 0.5 }), off(1, 60)];
    let (_, allocs) = measure_rt(|| {
        for _ in 0..50 {
            let mut outs: [&mut [f32]; 2] = [&mut l[..512], &mut r[..512]];
            let mut ctx = ProcessContext { audio: AudioBlock::output_only(512, &mut outs), events: &events, transport: &transport };
            s.process(&mut ctx);
        }
    });
    assert_eq!(allocs, 0);
}

fn sine_i16(n: usize, period: usize) -> Vec<i16> {
    (0..n).map(|i| ((i as f64 / period as f64 * std::f64::consts::TAU).sin() * 16000.0) as i16).collect()
}

fn fixture_font() -> Arc<sf2::SoundFont> {
    // sample 0: 440 Hz at 44.1k root A4 looped; sample 1: 880 Hz for high keys; velocity split on sample 2 (quiet)
    let bytes = sf2::build_fixture(
        "Fixture",
        0,
        0,
        &[
            FixtureSample { name: "a4", data: sine_i16(44_100, 100), rate: 44_100, root: 69, loop_start: 100, loop_end: 44_000 },
            FixtureSample { name: "a5", data: sine_i16(22_050, 50), rate: 44_100, root: 81, loop_start: 50, loop_end: 22_000 },
            FixtureSample { name: "soft", data: sine_i16(44_100, 200), rate: 44_100, root: 57, loop_start: 200, loop_end: 44_000 },
        ],
        &[
            FixtureZone { sample: 0, key_lo: 0, key_hi: 75, vel_lo: 64, vel_hi: 127, loop_mode: 1, extra: vec![] },
            FixtureZone { sample: 1, key_lo: 76, key_hi: 127, vel_lo: 64, vel_hi: 127, loop_mode: 1, extra: vec![] },
            FixtureZone { sample: 2, key_lo: 0, key_hi: 127, vel_lo: 0, vel_hi: 63, loop_mode: 1, extra: vec![(Gen::InitialAttenuation as u16, 120)] },
        ],
    );
    Arc::new(sf2::parse(&bytes).unwrap())
}

#[test]
fn sf2_key_and_velocity_zones_select_samples_and_pitch_tracks() {
    let mut p = Sf2Player::new(Sf2Patch::default(), Some(fixture_font()));
    p.prepare(&PrepareSpec::new(SR, 256, BusLayout::Stereo)).unwrap();
    assert_eq!(p.presets().len(), 1);
    // A4 at full velocity: zone 0, 440 Hz
    let buf = run(&mut p, 24_000, 256, &[(0, on(1, 69))]);
    assert!((dominant_frequency(&buf[2000..24_000], SR) - 440.0).abs() < 1.5);
    assert!(rms(&buf[4000..]) > 0.1);
    // C6 (84) → zone 1 (880 Hz sample rooted at A5): C6 = 1046.5 Hz
    let mut p2 = Sf2Player::new(Sf2Patch::default(), Some(fixture_font()));
    p2.prepare(&PrepareSpec::new(SR, 256, BusLayout::Stereo)).unwrap();
    let buf2 = run(&mut p2, 24_000, 256, &[(0, on(1, 84))]);
    assert!((fundamental_frequency(&buf2[2000..24_000], SR, 0.2) - 1046.5).abs() < 3.0);
    // low velocity → quiet zone (initialAttenuation 12 dB + velocity curve)
    let mut p3 = Sf2Player::new(Sf2Patch::default(), Some(fixture_font()));
    p3.prepare(&PrepareSpec::new(SR, 256, BusLayout::Stereo)).unwrap();
    let soft = TimedEvent::note_on(0, EventSource::TypingKeyboard, 0, 1, 69, 0.3);
    let buf3 = run(&mut p3, 24_000, 256, &[(0, soft)]);
    assert!(rms(&buf3[4000..]) < rms(&buf[4000..]) * 0.5, "velocity zone is quieter");
    // the soft sample is a 220.5 Hz tone rooted at A3, so A4 plays it one octave up
    assert!((dominant_frequency(&buf3[2000..24_000], SR) - 441.0).abs() < 3.0, "soft zone pitch");
}

#[test]
fn sf2_loops_sustain_and_release_works_and_unknown_program_falls_back() {
    let mut p = Sf2Player::new(Sf2Patch { bank: 5, preset: 77, ..Sf2Patch::default() }, Some(fixture_font()));
    p.prepare(&PrepareSpec::new(SR, 256, BusLayout::Stereo)).unwrap();
    let buf = run(&mut p, 96_000, 256, &[(0, on(1, 69)), (72_000, off(1, 69))]);
    assert!(rms(&buf[60_000..70_000]) > 0.1, "looped sample sustains past its length (fallback preset)");
    assert!(rms(&buf[90_000..]) < 0.01, "released");
    let mut q = Sf2Player::new(Sf2Patch::default(), None);
    q.prepare(&PrepareSpec::new(SR, 256, BusLayout::Stereo)).unwrap();
    assert_eq!(peak(&run(&mut q, 2048, 256, &[(0, on(1, 60))])), 0.0);
}

#[test]
fn sf2_unsupported_generators_and_malformed_files_are_surfaced() {
    let bytes = sf2::build_fixture(
        "X",
        0,
        0,
        &[FixtureSample { name: "s", data: sine_i16(1000, 50), rate: 44_100, root: 60, loop_start: 0, loop_end: 1000 }],
        &[FixtureZone { sample: 0, key_lo: 0, key_hi: 127, vel_lo: 0, vel_hi: 127, loop_mode: 0, extra: vec![(6, 50)] }],
    );
    let sf = sf2::parse(&bytes).unwrap();
    assert_eq!(sf.unsupported_generators, vec![6]);
    assert!(sf.warnings()[0].contains("vibLfoToPitch"));
    assert!(sf2::parse(b"RIFF\x04\0\0\0junk").is_err());
}
