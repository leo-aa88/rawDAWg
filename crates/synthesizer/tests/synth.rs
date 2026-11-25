//! T-VOICE / T-OSC / T-RT checks for `native.synth`.

use rawdawg_dsp::analysis::{dominant_frequency, features};
use rawdawg_dsp::math::{peak, rms};
use rawdawg_dsp::processor::{PrepareSpec, ProcessContext, Processor, TransportView};
use rawdawg_dsp::{AudioBlock, BusLayout};
use rawdawg_rt::alloc_guard::{GuardAllocator, measure_rt};
use rawdawg_synthesizer::params;
use rawdawg_synthesizer::patch::{Generator, Layer, Patch, VoiceMode};
use rawdawg_synthesizer::{Synth, TYPE_ID};
use rawdawg_types::ParamId;
use rawdawg_types::events::{EventKind, EventSource, TimedEvent};

#[global_allocator]
static GLOBAL: GuardAllocator = GuardAllocator;

const SR: f64 = 48_000.0;

struct Harness {
    synth: Synth,
    l: Vec<f32>,
    r: Vec<f32>,
    transport: TransportView,
}

impl Harness {
    fn new(patch: Patch, sr: f64, max_frames: usize) -> Self {
        let mut synth = Synth::new(patch);
        synth.prepare(&PrepareSpec::new(sr, max_frames, BusLayout::Stereo)).unwrap();
        Self { synth, l: vec![0.0; max_frames], r: vec![0.0; max_frames], transport: TransportView::stopped(sr) }
    }

    /// Process one block; returns (left, right) copies.
    fn block(&mut self, frames: usize, events: &[TimedEvent]) -> (Vec<f32>, Vec<f32>) {
        let mut outs: [&mut [f32]; 2] = [&mut self.l[..frames], &mut self.r[..frames]];
        let mut ctx = ProcessContext { audio: AudioBlock::output_only(frames, &mut outs), events, transport: &self.transport };
        self.synth.process(&mut ctx);
        (self.l[..frames].to_vec(), self.r[..frames].to_vec())
    }

    /// Render `total` frames in blocks of `block`, applying events at absolute frames.
    fn render(&mut self, total: usize, block: usize, events: &[(usize, TimedEvent)]) -> Vec<f32> {
        let mut out = Vec::with_capacity(total);
        let mut pos = 0;
        let mut evbuf: Vec<TimedEvent> = Vec::new();
        while pos < total {
            let n = block.min(total - pos);
            evbuf.clear();
            for (at, ev) in events {
                if *at >= pos && *at < pos + n {
                    let mut e = *ev;
                    e.frame = (*at - pos) as u32;
                    evbuf.push(e);
                }
            }
            rawdawg_types::events::sort_events(&mut evbuf);
            let (l, _) = self.block(n, &evbuf);
            out.extend_from_slice(&l);
            pos += n;
        }
        out
    }
}

fn on(id: u32, pitch: u8, vel: f32) -> TimedEvent {
    TimedEvent::note_on(0, EventSource::TypingKeyboard, 0, id, pitch, vel)
}
fn off(id: u32, pitch: u8) -> TimedEvent {
    TimedEvent::note_off(0, EventSource::TypingKeyboard, 0, id, pitch)
}

fn sine_patch() -> Patch {
    Patch { layers: vec![Layer::basic(rawdawg_dsp::oscillator::BasicWave::Sine)], ..Patch::default() }
}

#[test]
fn type_id_and_descriptors() {
    let s = Synth::default();
    assert_eq!(s.type_id(), TYPE_ID);
    assert_eq!(s.type_id(), "native.synth");
    assert!(s.descriptors().len() > 20);
}

#[test]
fn a4_renders_440hz_within_half_hz_at_all_rates() {
    for sr in [44_100.0, 48_000.0, 88_200.0, 96_000.0, 176_400.0, 192_000.0] {
        for patch in [Patch::default(), sine_patch()] {
            let mut h = Harness::new(patch, sr, 512);
            let total = (2.0 * sr) as usize;
            let buf = h.render(total, 512, &[(0, on(1, 69, 1.0))]);
            // skip attack
            let f = dominant_frequency(&buf[(sr as usize) / 10..], sr);
            assert!((f - 440.0).abs() < 0.5, "sr={sr}: {f}");
        }
    }
}

#[test]
fn default_patch_is_audible_at_moderate_level_and_silent_before_notes() {
    let mut h = Harness::new(Patch::default(), SR, 256);
    let silent = h.render(4800, 256, &[]);
    assert!(peak(&silent) == 0.0);
    let buf = h.render(48_000, 256, &[(0, on(1, 60, 0.8))]);
    let p = peak(&buf[2400..]);
    assert!(p > 0.05 && p < 0.7, "peak {p}");
    assert!(buf.iter().all(|s| s.is_finite()));
}

#[test]
fn release_leads_to_silence_with_no_hanging_notes() {
    let mut h = Harness::new(Patch::default(), SR, 256);
    // chord on, off at 0.5 s, render 1.5 s
    let evs = vec![(0, on(1, 60, 1.0)), (0, on(2, 64, 1.0)), (0, on(3, 67, 1.0)), (24_000, off(1, 60)), (24_000, off(2, 64)), (24_000, off(3, 67))];
    let buf = h.render(72_000, 256, &evs);
    assert!(rms(&buf[12_000..24_000]) > 0.02);
    // release 180 ms → by 0.5 s + 0.25 s everything must be silent
    assert_eq!(peak(&buf[36_000..]), 0.0, "sound persisted after release");
    assert_eq!(h.synth.stats().active_voices, 0);
}

#[test]
fn polyphony_sums_and_voice_count_tracks_notes() {
    let mut h = Harness::new(sine_patch(), SR, 256);
    let evs: Vec<(usize, TimedEvent)> = (0..8).map(|i| (0, on(i, 48 + i as u8 * 3, 0.9))).collect();
    let buf = h.render(9600, 256, &evs);
    assert_eq!(h.synth.stats().active_voices, 8);
    let one = Harness::new(sine_patch(), SR, 256).render(9600, 256, &[(0, on(0, 48, 0.9))]);
    assert!(rms(&buf[4800..]) > rms(&one[4800..]) * 1.5);
}

#[test]
fn voice_stealing_is_bounded_and_click_free() {
    let mut p = sine_patch();
    p.max_voices = 4;
    let mut h = Harness::new(p, SR, 64);
    let evs: Vec<(usize, TimedEvent)> = (0..12).map(|i| (i * 64, on(i as u32, 40 + i as u8, 0.9))).collect();
    let buf = h.render(64 * 40, 64, &evs);
    let st = h.synth.stats();
    assert!(st.active_voices <= 4 + rawdawg_synthesizer::synth::SPARE_VOICES as u32);
    assert!(st.stolen_voices >= 8, "stolen {}", st.stolen_voices);
    // after fades complete only 4 logical voices remain
    h.render(4800, 64, &[]);
    assert_eq!(h.synth.stats().active_voices, 4);
    // no hard discontinuities: max sample-to-sample jump bounded (voices are soft sines ~0.3 peak)
    let max_jump = buf.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0f32, f32::max);
    assert!(max_jump < 0.15, "click detected: jump {max_jump}");
}

#[test]
fn sustain_pedal_defers_release_per_source() {
    let mut h = Harness::new(sine_patch(), SR, 256);
    let pedal_down = TimedEvent::new(0, EventSource::TypingKeyboard, 0, EventKind::Control { cc: 64, value: 1.0 });
    let pedal_up = TimedEvent::new(0, EventSource::TypingKeyboard, 0, EventKind::Control { cc: 64, value: 0.0 });
    let other_pedal_up = TimedEvent::new(0, EventSource::MidiDevice(0), 0, EventKind::Control { cc: 64, value: 0.0 });
    let evs = vec![(0, pedal_down), (0, on(1, 60, 1.0)), (4800, off(1, 60)), (9600, other_pedal_up), (24_000, pedal_up)];
    let buf = h.render(48_000, 256, &evs);
    // held by pedal between 0.1 s and 0.5 s (other source's pedal-up must not release it)
    assert!(rms(&buf[12_000..24_000]) > 0.02);
    assert!(rms(&buf[14_400..16_800]) > 0.02);
    // released after pedal up (180 ms release)
    assert_eq!(peak(&buf[40_000..]), 0.0);
}

#[test]
fn note_off_from_other_source_does_not_release() {
    let mut h = Harness::new(sine_patch(), SR, 256);
    let foreign_off = TimedEvent::note_off(0, EventSource::MidiDevice(3), 0, 1, 60);
    let buf = h.render(24_000, 256, &[(0, on(1, 60, 1.0)), (2400, foreign_off)]);
    assert!(rms(&buf[12_000..]) > 0.02, "note was released by a foreign source");
    assert_eq!(h.synth.stats().active_voices, 1);
}

#[test]
fn all_notes_off_for_source_only_affects_that_source() {
    let mut h = Harness::new(sine_patch(), SR, 256);
    let midi_on = TimedEvent::note_on(0, EventSource::MidiDevice(0), 0, 7, 72, 1.0);
    let typing_panic = TimedEvent::new(0, EventSource::TypingKeyboard, 0, EventKind::AllNotesOff { source: Some(EventSource::TypingKeyboard) });
    h.render(2400, 256, &[(0, on(1, 60, 1.0)), (0, midi_on), (1000, typing_panic)]);
    h.render(24_000, 256, &[]);
    assert_eq!(h.synth.stats().active_voices, 1);
    let global = TimedEvent::new(0, EventSource::Transport, 0, EventKind::AllNotesOff { source: None });
    h.render(24_000, 256, &[(0, global)]);
    assert_eq!(h.synth.stats().active_voices, 0);
}

#[test]
fn mono_legato_glides_and_returns_to_held_note() {
    let mut p = sine_patch();
    p.voice_mode = VoiceMode::MonoLegato;
    p.portamento_s = 0.05;
    let mut h = Harness::new(p, SR, 256);
    let evs = vec![(0, on(1, 60, 1.0)), (24_000, on(2, 72, 1.0)), (48_000, off(2, 72)), (72_000, off(1, 60))];
    let buf = h.render(96_000, 256, &evs);
    assert_eq!(h.synth.stats().active_voices, 0);
    let f1 = dominant_frequency(&buf[12_000..24_000], SR);
    let f2 = dominant_frequency(&buf[36_000..48_000], SR);
    let f3 = dominant_frequency(&buf[60_000..72_000], SR);
    assert!((f1 - 261.63).abs() < 1.0, "{f1}");
    assert!((f2 - 523.25).abs() < 1.0, "{f2}");
    assert!((f3 - 261.63).abs() < 1.0, "returned to held note: {f3}");
    // glide: frequency measured right after the second note-on lies between
    let glide = dominant_frequency(&buf[24_000..24_000 + 1024], SR);
    assert!(glide > 270.0 && glide < 520.0, "glide {glide}");
    // envelope never retriggered: no silence gap around the transition
    assert!(rms(&buf[24_000..25_000]) > 0.05);
}

#[test]
fn pitch_bend_moves_pitch_by_range() {
    let mut h = Harness::new(sine_patch(), SR, 256);
    let bend = TimedEvent::new(0, EventSource::TypingKeyboard, 0, EventKind::PitchBend { value: 1.0 });
    let buf = h.render(48_000, 256, &[(0, on(1, 69, 1.0)), (24_000, bend)]);
    let f_before = dominant_frequency(&buf[8000..24_000], SR);
    let f_after = dominant_frequency(&buf[28_000..48_000], SR);
    assert!((f_before - 440.0).abs() < 0.5);
    // +2 semitones = 493.88 Hz
    assert!((f_after - 493.88).abs() < 1.0, "{f_after}");
}

#[test]
fn block_partition_invariance() {
    let evs = vec![(0, on(1, 60, 0.9)), (777, on(2, 67, 0.7)), (5000, off(1, 60)), (9001, off(2, 67))];
    let reference = Harness::new(Patch::default(), SR, 2048).render(24_000, 2048, &evs);
    for block in [32usize, 64, 127, 128, 256, 511, 1024] {
        let other = Harness::new(Patch::default(), SR, 2048).render(24_000, block, &evs);
        let max_err = reference.iter().zip(&other).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
        assert!(max_err <= 1e-5, "block {block}: max error {max_err}");
    }
}

#[test]
fn repeated_render_is_deterministic() {
    let evs = vec![(0, on(1, 60, 0.9)), (100, on(2, 64, 0.5))];
    let a = Harness::new(Patch::default(), SR, 256).render(12_000, 256, &evs);
    let b = Harness::new(Patch::default(), SR, 256).render(12_000, 256, &evs);
    assert_eq!(a, b);
}

#[test]
fn parameters_round_trip_and_change_sound() {
    let mut h = Harness::new(Patch::default(), SR, 256);
    let cutoff = ParamId(params::FILTER_CUTOFF);
    let d = h.synth.descriptors().iter().find(|d| d.id == cutoff).unwrap();
    assert!(h.synth.set_param(cutoff, d.to_normalized(300.0)));
    assert!((d.to_physical(h.synth.param(cutoff).unwrap()) - 300.0).abs() < 0.5);
    let dark = h.render(24_000, 256, &[(0, on(1, 48, 1.0))]);
    let mut h2 = Harness::new(Patch::default(), SR, 256);
    let bright = h2.render(24_000, 256, &[(0, on(1, 48, 1.0))]);
    let (_, c_dark, _) = features(&dark[4800..], SR);
    let (_, c_bright, _) = features(&bright[4800..], SR);
    assert!(c_dark < c_bright * 0.5, "centroid dark {c_dark} bright {c_bright}");
    // every descriptor round-trips its default
    for d in h.synth.descriptors() {
        if let Some(n) = h.synth.param(d.id) {
            assert!((0.0..=1.0).contains(&n), "{}", d.key);
        }
    }
    assert!(!h.synth.set_param(ParamId(99_999), 0.5));
    assert!(!h.synth.set_param(cutoff, f32::NAN));
}

#[test]
fn param_event_in_block_applies_at_offset() {
    let mut h = Harness::new(sine_patch(), SR, 256);
    let gain_desc = h.synth.descriptors().iter().find(|d| d.id.0 == params::MASTER_GAIN).unwrap();
    let mute = TimedEvent::new(0, EventSource::Transport, 0, EventKind::Param { id: ParamId(params::MASTER_GAIN), value: gain_desc.to_normalized(-60.0) });
    let buf = h.render(24_000, 256, &[(0, on(1, 60, 1.0)), (12_000, mute)]);
    assert!(rms(&buf[6000..12_000]) > 0.02);
    assert!(rms(&buf[18_000..]) < 0.002);
}

#[test]
fn state_save_load_round_trip_and_rejects_garbage() {
    let mut s = Synth::default();
    s.prepare(&PrepareSpec::new(SR, 256, BusLayout::Stereo)).unwrap();
    s.set_param(ParamId(params::AMP_RELEASE), 0.9);
    let bytes = s.save_state();
    let mut t = Synth::default();
    t.prepare(&PrepareSpec::new(SR, 256, BusLayout::Stereo)).unwrap();
    t.load_state(&bytes).unwrap();
    assert_eq!(s.patch(), t.patch());
    assert!(t.load_state(b"{\"version\": 5}").is_err());
    assert_eq!(s.patch(), t.patch(), "failed load must not alter state");
    let mut bad = Patch::default();
    bad.layers[0].generator = Generator::Basic { wave: rawdawg_dsp::oscillator::BasicWave::Saw, pulse_width: 3.0 };
    assert!(t.load_state(&bad.to_json()).is_err());
}

#[test]
fn process_path_is_allocation_free() {
    let mut p = Patch::default();
    p.layers.push(Layer::basic(rawdawg_dsp::oscillator::BasicWave::Saw));
    p.layers.push(Layer::basic(rawdawg_dsp::oscillator::BasicWave::Pulse));
    p.max_voices = 16;
    let mut h = Harness::new(p, SR, 2048);
    // warm up
    h.render(4096, 256, &[(0, on(1, 60, 1.0))]);
    let pedal = TimedEvent::new(0, EventSource::TypingKeyboard, 0, EventKind::Control { cc: 64, value: 1.0 });
    let bend = TimedEvent::new(0, EventSource::TypingKeyboard, 0, EventKind::PitchBend { value: 0.3 });
    let param = TimedEvent::new(0, EventSource::Transport, 0, EventKind::Param { id: ParamId(params::FILTER_CUTOFF), value: 0.4 });
    let mut events: Vec<TimedEvent> = Vec::new();
    for i in 0..40u32 {
        events.push(on(100 + i, 36 + (i % 40) as u8, 0.8));
    }
    events.push(pedal);
    events.push(bend);
    events.push(param);
    for i in 0..10u32 {
        events.push(off(100 + i, 36 + i as u8));
    }
    events.push(TimedEvent::new(0, EventSource::Transport, 0, EventKind::AllNotesOff { source: None }));
    let mut frames_list = [32usize, 64, 127, 256, 511, 1024, 2048].iter().cycle();
    let (_, allocs) = measure_rt(|| {
        for _ in 0..64 {
            let n = *frames_list.next().unwrap();
            for ev in events.iter_mut() {
                ev.frame = 0;
            }
            rawdawg_types::events::sort_events(&mut events);
            let mut outs: [&mut [f32]; 2] = [&mut h.l[..n], &mut h.r[..n]];
            let mut ctx = ProcessContext { audio: AudioBlock::output_only(n, &mut outs), events: &events, transport: &h.transport };
            h.synth.process(&mut ctx);
            h.synth.reset();
        }
    });
    assert_eq!(allocs, 0, "allocations observed on the process path");
}

#[test]
fn multi_layer_patch_mixes_layers_with_pan() {
    let mut p = sine_patch();
    let mut right = Layer::basic(rawdawg_dsp::oscillator::BasicWave::Triangle);
    right.pan = 1.0;
    right.coarse_semitones = 12;
    p.layers[0].pan = -1.0;
    p.layers.push(right);
    let mut h = Harness::new(p, SR, 8192);
    h.render(2400, 256, &[(0, on(1, 57, 1.0))]);
    let (l, r) = h.block(8192, &[]);
    let fl = dominant_frequency(&l, SR);
    let fr = dominant_frequency(&r, SR);
    assert!((fl - 220.0).abs() < 1.0, "{fl}");
    assert!((fr - 440.0).abs() < 1.0, "{fr}");
}
