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

#[test]
fn live_table_update_changes_sound_without_retrigger_and_retires_old_table() {
    use rawdawg_dsp::wavetable::{TableBuilder, cycles};
    use rawdawg_synthesizer::table_channel;
    let (mut sender, inbox) = table_channel();
    let mut synth = Synth::new(Patch::default());
    synth.set_table_inbox(inbox);
    synth.prepare(&PrepareSpec::new(SR, 256, BusLayout::Stereo)).unwrap();
    let mut h = Harness { synth, l: vec![0.0; 256], r: vec![0.0; 256], transport: TransportView::stopped(SR) };
    let before = h.render(24_000, 256, &[(0, on(1, 48, 1.0))]);
    let (_, c_before, _) = features(&before[12_000..], SR);
    // send a square table while the note is held
    let mut b = TableBuilder::new();
    let table = Box::new(b.build(&cycles::square(), true));
    assert!(sender.send(0, 0, table).is_ok(), "queued");
    let during = h.render(2048, 256, &[]);
    assert!(during.iter().all(|s| s.is_finite()));
    // crossfade completes within ~8 ms (384 samples): no discontinuity larger than the
    // signal's own slew (soft saw→square at C3 with 0.3 peak)
    let max_jump = during.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0f32, f32::max);
    assert!(max_jump < 0.25, "click during table swap: {max_jump}");
    assert_eq!(h.synth.layer_xfade(0), 1.0);
    let after = h.render(24_000, 256, &[]);
    let (_, c_after, _) = features(&after[12_000..], SR);
    assert!(rms(&after[12_000..]) > 0.02, "note still sounding (no retrigger/cut)");
    assert!((c_after - c_before).abs() > c_before * 0.15, "spectrum changed: {c_before} -> {c_after}");
    assert_eq!(h.synth.stats().active_voices, 1);
    // the previous table came back for deallocation off the audio thread
    assert_eq!(sender.collect_retired(), 1);
    // stale generations are ignored: queue two, only the newest applies
    let t1 = Box::new(b.build(&cycles::sine(), true));
    let t2 = Box::new(b.build(&cycles::saw(), true));
    assert!(sender.send(0, 0, t1).is_ok());
    assert!(sender.send(0, 0, t2).is_ok());
    h.render(4096, 256, &[]);
    let (_, c_final, _) = features(&h.render(24_000, 256, &[])[12_000..], SR);
    assert!(c_final > c_before * 0.8, "saw (newest) should be bright, got centroid {c_final}");
    assert!(sender.collect_retired() >= 2);
}

#[test]
fn migrate_from_carries_voices_and_tables_into_a_new_instance() {
    use rawdawg_synthesizer::table_channel;
    let (mut sender, inbox) = table_channel();
    let mut old = Synth::new(Patch::default());
    old.set_table_inbox(inbox);
    old.prepare(&PrepareSpec::new(SR, 256, BusLayout::Stereo)).unwrap();
    let mut h = Harness { synth: old, l: vec![0.0; 256], r: vec![0.0; 256], transport: TransportView::stopped(SR) };
    h.render(12_000, 256, &[(0, on(1, 60, 1.0)), (0, on(2, 64, 0.8))]);
    assert_eq!(h.synth.stats().active_voices, 2);
    let mut new = Synth::new(Patch::default());
    new.prepare(&PrepareSpec::new(SR, 256, BusLayout::Stereo)).unwrap();
    new.migrate_from(&mut h.synth);
    assert_eq!(h.synth.stats().active_voices, 0, "old instance is now silent");
    let mut h2 = Harness { synth: new, l: vec![0.0; 256], r: vec![0.0; 256], transport: TransportView::stopped(SR) };
    assert_eq!(h2.synth.stats().active_voices, 2);
    let cont = h2.render(2048, 256, &[]);
    assert!(rms(&cont) > 0.02, "voices keep sounding after migration");
    assert!(cont.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0f32, f32::max) < 0.25);
    // the table inbox moved with the voices: a live table update still reaches them
    let mut b = rawdawg_dsp::wavetable::TableBuilder::new();
    assert!(sender.send(0, 0, Box::new(b.build(&rawdawg_dsp::wavetable::cycles::square(), true))).is_ok());
    h2.render(4096, 256, &[]);
    assert_eq!(sender.collect_retired(), 1);
    // releasing through the new instance works (note ids survived)
    h2.render(48_000, 256, &[(0, off(1, 60)), (0, off(2, 64))]);
    assert_eq!(h2.synth.stats().active_voices, 0);
}

fn layer_patch(generator: rawdawg_synthesizer::patch::Generator) -> Patch {
    Patch { layers: vec![Layer::with_generator(generator)], ..Patch::default() }
}

#[test]
fn every_generator_plays_in_tune_and_finite() {
    use rawdawg_dsp::analysis::fundamental_frequency;
    use rawdawg_synthesizer::patch::*;
    let gens = [
        ("fm", Generator::Fm(FmSettings::default())),
        ("additive", Generator::Additive(AdditiveSettings::default())),
        ("additive-detuned", Generator::Additive(AdditiveSettings { detuned: true, ..AdditiveSettings::default() })),
        ("granular", Generator::Granular { settings: GranularSettings { jitter: 0.0, ..GranularSettings::default() }, source: rawdawg_dsp::wavetable::cycles::saw() }),
        ("pluck", Generator::Pluck(PluckSettings::default())),
        ("basic-pulse", Generator::Basic { wave: rawdawg_dsp::oscillator::BasicWave::Pulse, pulse_width: 0.3 }),
    ];
    for (name, g) in gens {
        let mut h = Harness::new(layer_patch(g), SR, 256);
        let buf = h.render(48_000, 256, &[(0, on(1, 57, 1.0))]);
        assert!(buf.iter().all(|s| s.is_finite()), "{name}");
        assert!(rms(&buf[4800..12_000]) > 0.01, "{name} is audible");
        let f = fundamental_frequency(&buf[4800..30_000], SR, 0.15);
        assert!((f - 220.0).abs() < 2.5, "{name}: fundamental {f}");
        assert_eq!(h.synth.stats().active_voices, 1, "{name}");
    }
}

#[test]
fn unison_widens_stereo_image_and_normalizes_gain() {
    let mono = sine_patch();
    let mut wide = sine_patch();
    wide.layers[0].unison = 6;
    wide.layers[0].unison_detune_cents = 30.0;
    wide.layers[0].unison_spread = 1.0;
    let mut h1 = Harness::new(mono, SR, 4096);
    let mut h2 = Harness::new(wide, SR, 4096);
    h1.render(2400, 256, &[(0, on(1, 60, 1.0))]);
    h2.render(2400, 256, &[(0, on(1, 60, 1.0))]);
    let (l1, r1) = h1.block(4096, &[]);
    let (l2, r2) = h2.block(4096, &[]);
    let diff1: f32 = l1.iter().zip(&r1).map(|(a, b)| (a - b).abs()).sum::<f32>() / 4096.0;
    let diff2: f32 = l2.iter().zip(&r2).map(|(a, b)| (a - b).abs()).sum::<f32>() / 4096.0;
    assert!(diff1 < 1e-6, "mono is identical on both sides");
    assert!(diff2 > 0.01, "unison spread differs between channels: {diff2}");
    let loud1 = rms(&l1).max(rms(&r1));
    let loud2 = rms(&l2).max(rms(&r2));
    assert!(loud2 < loud1 * 2.0, "unison does not multiply loudness: {loud1} vs {loud2}");
}

#[test]
fn lfo_vibrato_and_mod_wheel_routes_change_the_sound() {
    use rawdawg_synthesizer::patch::*;
    let mut p = sine_patch();
    p.lfos[0] = Lfo { shape: LfoShape::Sine, rate_hz: 6.0, sync_beats: 0.0, depth: 1.0, per_voice: false };
    p.mod_routes.push(ModRoute { source: ModSource::Lfo1, target: ModTarget::Pitch, depth: 1.0 / 24.0, enabled: true }); // ±1 semitone
    let mut h = Harness::new(p, SR, 256);
    let buf = h.render(48_000, 256, &[(0, on(1, 69, 1.0))]);
    // frequency measured over short windows must swing around 440 Hz
    let mut fs = Vec::new();
    for w in (8000..40_000).step_by(2000) {
        fs.push(dominant_frequency(&buf[w..w + 2048], SR));
    }
    let (lo, hi) = fs.iter().fold((f64::MAX, f64::MIN), |(lo, hi), f| (lo.min(*f), hi.max(*f)));
    assert!(hi - lo > 20.0 && lo > 400.0 && hi < 480.0, "vibrato range {lo}..{hi}");
    // mod wheel → cutoff on a bright saw
    let mut p2 = Patch { layers: vec![Layer::basic(rawdawg_dsp::oscillator::BasicWave::Saw)], ..Patch::default() };
    p2.filter.cutoff_hz = 300.0;
    p2.mod_routes.push(ModRoute { source: ModSource::ModWheel, target: ModTarget::Cutoff, depth: 1.0, enabled: true }); // +4 octaves at full wheel
    let mut h2 = Harness::new(p2, SR, 256);
    let dark = h2.render(24_000, 256, &[(0, on(1, 48, 1.0))]);
    let wheel = TimedEvent::new(0, EventSource::TypingKeyboard, 0, EventKind::Control { cc: 1, value: 1.0 });
    let bright = h2.render(24_000, 256, &[(0, wheel)]);
    let (_, c_dark, _) = features(&dark[6000..], SR);
    let (_, c_bright, _) = features(&bright[6000..], SR);
    assert!(c_bright > c_dark * 1.5, "mod wheel opened the filter: {c_dark} -> {c_bright}");
}

#[test]
fn all_generators_process_path_is_allocation_free() {
    use rawdawg_synthesizer::patch::*;
    let layers = vec![
        Layer { unison: 4, ..Layer::default_soft_saw() },
        Layer::with_generator(Generator::Fm(FmSettings::default())),
        Layer::with_generator(Generator::Granular { settings: GranularSettings::default(), source: rawdawg_dsp::wavetable::cycles::saw() }),
        Layer::with_generator(Generator::Pluck(PluckSettings::default())),
    ];
    let mut p = Patch { layers, ..Patch::default() };
    p.lfos[0].depth = 1.0;
    p.lfos[1] = Lfo { per_voice: true, depth: 1.0, shape: LfoShape::SampleHold, ..Lfo::default() };
    p.mod_routes.push(ModRoute { source: ModSource::Lfo1, target: ModTarget::Cutoff, depth: 0.5, enabled: true });
    p.mod_routes.push(ModRoute { source: ModSource::Lfo2, target: ModTarget::Pan, depth: 0.5, enabled: true });
    p.max_voices = 8;
    let mut h = Harness::new(p, SR, 1024);
    h.render(4096, 256, &[(0, on(1, 50, 1.0))]);
    let mut events: Vec<TimedEvent> = (0..12u32).map(|i| on(10 + i, 40 + i as u8 * 2, 0.8)).collect();
    events.push(TimedEvent::new(0, EventSource::TypingKeyboard, 0, EventKind::Control { cc: 1, value: 0.7 }));
    events.push(TimedEvent::new(0, EventSource::TypingKeyboard, 0, EventKind::ChannelPressure { value: 0.5 }));
    events.push(TimedEvent::new(0, EventSource::TypingKeyboard, 0, EventKind::NotePressure { note_id: 11, value: 0.7 }));
    events.push(TimedEvent::new(0, EventSource::TypingKeyboard, 0, EventKind::Control { cc: 74, value: 0.9 }));
    for (i, x) in rawdawg_types::events::NoteExpression::ALL.into_iter().enumerate() {
        events.push(TimedEvent::new(0, EventSource::TypingKeyboard, 0, EventKind::NoteExpression { note_id: 12 + i as u32, expression: x, value: 0.6 }));
    }
    events.push(TimedEvent::new(0, EventSource::Transport, 0, EventKind::Param { id: ParamId(params::gen_param(1, params::G_FM_LEVEL2)), value: 0.8 }));
    events.push(TimedEvent::new(0, EventSource::Transport, 0, EventKind::Param { id: ParamId(params::mod_param(5, params::MOD_ENABLED)), value: 1.0 }));
    let (_, allocs) = measure_rt(|| {
        for i in 0..60 {
            let n = [32usize, 127, 256, 1024][i % 4];
            rawdawg_types::events::sort_events(&mut events);
            let mut outs: [&mut [f32]; 2] = [&mut h.l[..n], &mut h.r[..n]];
            let mut ctx = ProcessContext { audio: AudioBlock::output_only(n, &mut outs), events: &events, transport: &h.transport };
            h.synth.process(&mut ctx);
        }
    });
    assert_eq!(allocs, 0, "allocations with all generators active");
    assert!(h.synth.stats().nonfinite_samples == 0);
}
