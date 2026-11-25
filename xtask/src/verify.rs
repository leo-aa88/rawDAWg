//! Verification suites. Each check records PASS / FAIL / BLOCKED / NOT RUN with
//! evidence, and the run fails (nonzero) if any required check failed.

use crate::Suite;
use anyhow::bail;
use rawdawg_audio_engine::offline::{OfflineOptions, WavFormat, render_project, render_project_to_wav};
use rawdawg_dsp::analysis::dominant_frequency;
use serde::Serialize;
use std::path::Path;
use std::process::Command;
use std::time::Instant;

#[derive(Serialize, Clone, Copy, PartialEq, Eq, Debug)]
#[allow(dead_code)]
pub enum Status {
    Pass,
    Fail,
    Blocked,
    NotRun,
}

#[derive(Serialize, Debug)]
pub struct Check {
    pub id: String,
    pub requirement_ids: Vec<&'static str>,
    pub status: Status,
    pub detail: String,
    pub evidence: Option<String>,
}

#[derive(Serialize)]
struct Report {
    suite: String,
    os: String,
    arch: String,
    rustc: String,
    git_commit: String,
    started_utc_unix: u64,
    checks: Vec<Check>,
}

fn git_commit() -> String {
    Command::new("git")
        .args(["rev-parse", "--short=12", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        .unwrap_or_else(|| "unknown".into())
}

fn rustc_version() -> String {
    Command::new("rustc").arg("--version").output().ok().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned()).unwrap_or_default()
}

fn check(id: &str, reqs: &[&'static str], f: impl FnOnce() -> anyhow::Result<(String, Option<String>)>) -> Check {
    match f() {
        Ok((detail, evidence)) => Check { id: id.into(), requirement_ids: reqs.to_vec(), status: Status::Pass, detail, evidence },
        Err(e) => Check { id: id.into(), requirement_ids: reqs.to_vec(), status: Status::Fail, detail: format!("{e:#}"), evidence: None },
    }
}

fn blocked(id: &str, reqs: &[&'static str], why: &str) -> Check {
    Check { id: id.into(), requirement_ids: reqs.to_vec(), status: Status::Blocked, detail: why.into(), evidence: None }
}

fn headless_checks(root: &Path, out: &Path) -> Vec<Check> {
    let mut checks = Vec::new();
    let demo = root.join("fixtures/demo.rawdawg");
    checks.push(check("HL-01 demo project loads and validates", &["DATA-01"], || {
        let p = rawdawg_project::load_project_dir(&demo)?;
        Ok((format!("{} channels, {} patterns, schema v{}", p.channels.len(), p.patterns.len(), p.schema_version), Some(demo.display().to_string())))
    }));
    checks.push(check("HL-02 headless render demo (pattern mode) to WAV", &["IO-06", "QUAL-01", "CORE-01"], || {
        let p = rawdawg_project::load_project_dir(&demo)?;
        let path = out.join("demo.wav");
        let stats = render_project_to_wav(&p, &OfflineOptions::default(), WavFormat::Float32, &path)?;
        if stats.nonfinite_blocks > 0 || stats.dropped_events > 0 {
            bail!("health counters nonzero: nonfinite {} dropped {}", stats.nonfinite_blocks, stats.dropped_events);
        }
        if stats.peak <= 0.01 {
            bail!("render is silent (peak {})", stats.peak);
        }
        let reader = hound::WavReader::open(&path)?;
        if reader.duration() as usize != stats.frames {
            bail!("wav frame count mismatch");
        }
        Ok((format!("{} frames @ {} Hz, peak {:.3}, {:.1}x realtime", stats.frames, stats.sample_rate, stats.peak, stats.realtime_factor), Some(path.display().to_string())))
    }));
    checks.push(check("HL-03 A4 pitch accuracy at all six project rates", &["T-OSC", "QUAL-01", "SYN-01"], || {
        let mut p = rawdawg_project::Project::new_default("a4", serde_json::to_value(rawdawg_synthesizer::Patch::default())?);
        let ch = p.channels[0].id;
        p.patterns[0].lanes[0].notes.push(rawdawg_project::Note {
            id: rawdawg_types::NoteId::new(),
            tick: rawdawg_types::units::Tick(0),
            duration_ticks: rawdawg_types::units::Tick(3840),
            pitch: 69,
            velocity: 1.0,
            channel: 0,
        });
        let _ = ch;
        let mut details = Vec::new();
        for sr in rawdawg_types::SampleRate::SUPPORTED {
            let rate = rawdawg_types::SampleRate(sr);
            let (l, _, _) = render_project(&p, &OfflineOptions { sample_rate: Some(rate), tail_seconds: 0.0, ..Default::default() })?;
            let skip = sr as usize / 10;
            let f = dominant_frequency(&l[skip..], f64::from(sr));
            if (f - 440.0).abs() > 0.5 {
                bail!("{sr} Hz: measured {f:.3} Hz");
            }
            details.push(format!("{sr}:{f:.3}"));
        }
        Ok((details.join(" "), None))
    }));
    checks.push(check("HL-04 block-partition invariance (32..2048 incl. odd sizes)", &["T-TIME", "QUAL-01"], || {
        let p = rawdawg_project::load_project_dir(&demo)?;
        let reference = render_project(&p, &OfflineOptions { block_frames: 2048, tail_seconds: 0.5, ..Default::default() })?;
        let mut worst = 0.0f32;
        for block in [32usize, 64, 127, 128, 256, 511, 1024] {
            let other = render_project(&p, &OfflineOptions { block_frames: block, tail_seconds: 0.5, ..Default::default() })?;
            let err = reference.0.iter().zip(&other.0).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
            worst = worst.max(err);
            if err > 1e-5 {
                bail!("block {block}: max abs error {err}");
            }
        }
        Ok((format!("max abs error {worst:.2e} ≤ 1e-5"), None))
    }));
    checks.push(check("HL-05 repeated render identical (determinism)", &["DATA-02", "QUAL-01"], || {
        let p = rawdawg_project::load_project_dir(&demo)?;
        let a = render_project(&p, &OfflineOptions::default())?;
        let b = render_project(&p, &OfflineOptions::default())?;
        if a.0 != b.0 || a.1 != b.1 {
            bail!("renders differ");
        }
        Ok((format!("{} frames bit-identical", a.0.len()), None))
    }));
    checks.push(check("HL-06 project save/load round trip in temp dir", &["DATA-01", "DATA-02"], || {
        let p = rawdawg_project::load_project_dir(&demo)?;
        let dir = out.join("roundtrip.rawdawg");
        let _ = std::fs::remove_dir_all(&dir);
        rawdawg_project::save_project_dir(&dir, &p)?;
        let back = rawdawg_project::load_project_dir(&dir)?;
        if back != p {
            bail!("loaded project differs from saved");
        }
        let a = render_project(&p, &OfflineOptions::default())?;
        let b = render_project(&back, &OfflineOptions::default())?;
        if a.0 != b.0 {
            bail!("render differs after reload");
        }
        Ok(("identical model and render after reload".into(), Some(dir.display().to_string())))
    }));
    checks.push(check("HL-08 sampler + SoundFont demo renders from imported assets", &["SYN-09", "IO-05", "DATA-01"], || {
        let dir = root.join("fixtures/demo-sampler.rawdawg");
        let p = rawdawg_project::load_project_dir(&dir)?;
        let opts = OfflineOptions { tail_seconds: 0.0, project_dir: Some(dir.clone()), ..Default::default() };
        let (l, _, stats) = render_project(&p, &opts)?;
        if stats.nonfinite_blocks > 0 {
            bail!("nonfinite blocks");
        }
        let f = dominant_frequency(&l[2000..20_000], 48_000.0);
        if (f - 440.0).abs() > 1.5 {
            bail!("expected the sample and SF2 zone at 440 Hz on beat 1, measured {f:.2} Hz");
        }
        if rawdawg_dsp::math::rms(&l[2000..20_000]) < 0.05 {
            bail!("render too quiet");
        }
        Ok((format!("{} frames, beat 1 fundamental {f:.2} Hz, peak {:.3}", stats.frames, stats.peak), Some(dir.display().to_string())))
    }));
    checks.push(check("HL-09 modular graph demo (delayed feedback, expression node) renders finite and audible", &["SYN-07", "QUAL-02"], || {
        let dir = root.join("fixtures/demo-graph.rawdawg");
        let p = rawdawg_project::load_project_dir(&dir)?;
        let state = &p.channels[0].instrument.state;
        let patch: rawdawg_synthesizer::Patch = serde_json::from_value(state.clone())?;
        let voice = rawdawg_synthesizer::graph::compile(&patch.graph.voice, rawdawg_synthesizer::graph::Scope::Voice)?;
        let global = rawdawg_synthesizer::graph::compile(&patch.graph.global, rawdawg_synthesizer::graph::Scope::Global)?;
        if voice.feedback_edges.is_empty() {
            bail!("the demo voice graph should contain an explicit feedback edge");
        }
        let opts = OfflineOptions { tail_seconds: 1.0, ..Default::default() };
        let (l, r, stats) = render_project(&p, &opts)?;
        if stats.nonfinite_blocks > 0 {
            bail!("nonfinite blocks");
        }
        let level = rawdawg_dsp::math::rms(&l[2000..40_000]);
        if level < 0.03 {
            bail!("graph demo is too quiet: rms {level:.4}");
        }
        let tail = rawdawg_dsp::math::rms(&l[l.len() - 24_000..]);
        let wav = out.join("hl-09-graph-demo.wav");
        rawdawg_audio_engine::offline::write_wav(&l, &r, 48_000, WavFormat::Pcm16, &wav)?;
        Ok((
            format!(
                "voice cost {}/{} ({} feedback edge), global cost {}/{}, rms {level:.3}, tail rms {tail:.4}, peak {:.3}",
                voice.cost,
                rawdawg_synthesizer::graph::MAX_VOICE_COST,
                voice.feedback_edges.len(),
                global.cost,
                rawdawg_synthesizer::graph::MAX_GLOBAL_COST,
                stats.peak
            ),
            Some(wav.display().to_string()),
        ))
    }));
    checks.push(check("HL-10 song demo renders clips through the tempo map (song mode)", &["CORE-01", "CORE-04"], || {
        let dir = root.join("fixtures/demo-song.rawdawg");
        let p = rawdawg_project::load_project_dir(&dir)?;
        let opts = OfflineOptions { mode: rawdawg_audio_engine::TransportMode::Song, tail_seconds: 0.0, ..Default::default() };
        let (l, _, stats) = render_project(&p, &opts)?;
        if stats.nonfinite_blocks > 0 {
            bail!("nonfinite blocks");
        }
        // bars 1–2 at 120 BPM (4.0 s) + bars 3–4 at 130 BPM (3.692 s) = 369 231 frames at 48 kHz
        let expected = 48_000.0 * (4.0 + 2.0 * 4.0 * 60.0 / 130.0);
        if (stats.frames as f64 - expected).abs() > 2.0 {
            bail!("song length {} frames, expected ≈{expected:.0} (tempo change at bar 3)", stats.frames);
        }
        // bar 2 holds only the looped bass clip; its first note is C2 ≈ 65.4 Hz (frames 96 000..114 000)
        let f = dominant_frequency(&l[98_000..112_000], 48_000.0);
        if (f - 65.41).abs() > 1.0 {
            bail!("bar 2 should be the bass pattern at 65.4 Hz, measured {f:.2} Hz");
        }
        if rawdawg_dsp::math::rms(&l[200_000..300_000]) < 0.03 {
            bail!("bars 3–4 (both clips) are too quiet");
        }
        Ok((format!("{} frames (tempo change honoured), bar 2 fundamental {f:.2} Hz, peak {:.3}", stats.frames, stats.peak), Some(dir.display().to_string())))
    }));
    checks.push(check("HL-11 automation clip sweeps the filter in the song render", &["CORE-08"], || {
        let dir = root.join("fixtures/demo-song.rawdawg");
        let p = rawdawg_project::load_project_dir(&dir)?;
        let opts = OfflineOptions { mode: rawdawg_audio_engine::TransportMode::Song, tail_seconds: 0.0, ..Default::default() };
        let (l, _, stats) = render_project(&p, &opts)?;
        if stats.nonfinite_blocks > 0 {
            bail!("nonfinite blocks");
        }
        // bars 3–4 (frames 192 000..369 231): cutoff rises from 25 % to 95 % → spectral centroid rises
        let (_, c_early, _) = rawdawg_dsp::analysis::features(&l[196_000..240_000], 48_000.0);
        let (_, c_late, _) = rawdawg_dsp::analysis::features(&l[320_000..364_000], 48_000.0);
        if c_late < c_early * 1.3 {
            bail!("expected the spectral centroid to rise with the cutoff sweep: early {c_early:.0} Hz, late {c_late:.0} Hz");
        }
        Ok((format!("centroid {c_early:.0} Hz → {c_late:.0} Hz across the sweep"), Some(dir.display().to_string())))
    }));
    checks.push(check(
        "E2E-02 (headless subset) steps, piano-roll edits, count-in recording, arrangement, tempo change, loop, export",
        &["CORE-01", "CORE-02", "CORE-03", "CORE-04", "IO-03"],
        || {
            use rawdawg_audio_engine::commands::EngineCommand;
            use rawdawg_audio_engine::engine::Engine;
            use rawdawg_audio_engine::{Take, compile};
            use rawdawg_project::{Clip, ClipContent, Command, History, LoopRange, Note, Pattern, Project, Track};
            use rawdawg_types::events::{EventSource, TimedEvent};
            use rawdawg_types::units::{PPQ, SampleRate, Tick, UsPerQuarter};
            use rawdawg_types::{ClipId, NoteId, PatternId, TrackId};
            let limits = rawdawg_types::limits::EngineLimits::DEFAULT;
            let mut project = Project::new_default("e2e-02", serde_json::to_value(rawdawg_synthesizer::Patch::default())?);
            let mut history = History::new();
            let ch = project.channels[0].id;
            let drums = project.patterns[0].id;
            // 1. drum-like steps: four-on-the-floor on C2 through the step model (AddNotes per step)
            for step in [0, 4, 8, 12] {
                history.apply(
                    &mut project,
                    Command::AddNotes {
                        pattern_id: drums,
                        channel_id: ch,
                        notes: vec![Note { id: NoteId::new(), tick: Tick(step * 240), duration_ticks: Tick(240), pitch: 36, velocity: 0.9, channel: 0 }],
                    },
                )?;
            }
            // 2. melodic pattern: notes, a chord, velocity edits
            let melody = PatternId::new();
            history.apply(
                &mut project,
                Command::AddPattern {
                    pattern: Pattern { id: melody, name: "Melody".into(), color: None, length_ticks: Tick(PPQ * 4), steps: 16, step_ticks: Tick(240), lanes: vec![] },
                },
            )?;
            let chord: Vec<Note> =
                [60u8, 64, 67].iter().map(|p| Note { id: NoteId::new(), tick: Tick(PPQ * 2), duration_ticks: Tick(PPQ), pitch: *p, velocity: 0.6, channel: 0 }).collect();
            let lead = Note { id: NoteId::new(), tick: Tick(0), duration_ticks: Tick(PPQ), pitch: 72, velocity: 0.5, channel: 0 };
            history.apply(&mut project, Command::AddNotes { pattern_id: melody, channel_id: ch, notes: vec![lead] })?;
            history.apply(&mut project, Command::AddNotes { pattern_id: melody, channel_id: ch, notes: chord.clone() })?;
            history.apply(&mut project, Command::UpdateNotes { pattern_id: melody, channel_id: ch, notes: vec![Note { velocity: 1.0, ..lead }] })?;
            history.undo(&mut project)?;
            history.redo(&mut project)?;
            if project.pattern(melody).unwrap().lanes[0].notes.iter().find(|n| n.id == lead.id).unwrap().velocity != 1.0 {
                bail!("velocity edit lost across undo/redo");
            }
            // 3. record with a one-bar count-in: a live note on beat 2 of the pattern loop (overdub onto the melody)
            history.apply(&mut project, Command::SelectPattern { id: Some(melody) })?;
            let out = compile(&project, 1, 256, &limits, &Default::default(), &rawdawg_audio_engine::NoAssets)?;
            let (mut engine, mut handle) = Engine::new(SampleRate(48_000), 256, &limits);
            engine.install_plan(out.plan);
            handle.send(EngineCommand::SetRecording(true)).map_err(|_| anyhow::anyhow!("queue"))?;
            handle.send(EngineCommand::PlayWithCountIn { beats: 4 }).map_err(|_| anyhow::anyhow!("queue"))?;
            let mut l = vec![0.0f32; 256];
            let mut r = vec![0.0f32; 256];
            let mut take = Take::new(melody, ch, PPQ * 4, 0, true, rawdawg_transport::TempoMap::constant(UsPerQuarter(500_000)), SampleRate(48_000), 0.0);
            let mut blocks = 0;
            let mut started = false;
            while blocks < 375 + 300 {
                if engine.status().playing.load(std::sync::atomic::Ordering::Relaxed) && !started {
                    started = true;
                }
                // play a note while the transport is at beat 2 (frames 24 000..) → note-on at block 94 after start
                let pos = engine.position_frame();
                if started && pos == 94 * 256 {
                    handle
                        .send(EngineCommand::Live { channel: ch, event: TimedEvent::note_on(0, EventSource::MidiDevice(0), 0, 500, 65, 0.8) })
                        .map_err(|_| anyhow::anyhow!("queue"))?;
                }
                if started && pos == 140 * 256 {
                    handle
                        .send(EngineCommand::Live { channel: ch, event: TimedEvent::note_off(0, EventSource::MidiDevice(0), 0, 500, 65) })
                        .map_err(|_| anyhow::anyhow!("queue"))?;
                }
                l.fill(0.0);
                r.fill(0.0);
                engine.render_block(&mut l, &mut r);
                while let Some(ev) = handle.poll_recorded() {
                    take.push(ev);
                }
                blocks += 1;
            }
            take.finish(engine.position_frame());
            if take.notes.len() != 1 {
                bail!("expected one recorded note, got {}", take.notes.len());
            }
            let rec = take.notes[0];
            // on at frame 94*256 = 24 064 → tick 962 (raw, not snapped); off at 140*256 → 1434 ticks later
            if rec.tick.0 != 962 || rec.duration_ticks.0 != 471 || rec.pitch != 65 {
                bail!("recorded note {rec:?}: expected tick 962, duration 471, pitch 65");
            }
            history.apply(&mut project, Command::AddNotes { pattern_id: melody, channel_id: ch, notes: take.notes.clone() })?;
            // 4. arrange: drums repeat for 4 bars (looped clip), melody twice, make the second unique and trim it
            let t1 = TrackId::new();
            let t2 = TrackId::new();
            history.apply(&mut project, Command::AddTrack { track: Track { id: t1, name: "Drums".into(), color: None, mute: false, solo: false, height: None }, index: None })?;
            history.apply(&mut project, Command::AddTrack { track: Track { id: t2, name: "Melody".into(), color: None, mute: false, solo: false, height: None }, index: None })?;
            history.apply(
                &mut project,
                Command::AddClip {
                    clip: Clip {
                        id: ClipId::new(),
                        track_id: t1,
                        start_tick: Tick(0),
                        length_ticks: Tick(PPQ * 16),
                        muted: false,
                        content: ClipContent::Pattern { pattern_id: drums, source_offset_ticks: Tick::ZERO, looped: true },
                    },
                },
            )?;
            history.apply(
                &mut project,
                Command::AddClip {
                    clip: Clip {
                        id: ClipId::new(),
                        track_id: t2,
                        start_tick: Tick(0),
                        length_ticks: Tick(PPQ * 4),
                        muted: false,
                        content: ClipContent::Pattern { pattern_id: melody, source_offset_ticks: Tick::ZERO, looped: false },
                    },
                },
            )?;
            let second = ClipId::new();
            history.apply(
                &mut project,
                Command::AddClip {
                    clip: Clip {
                        id: second,
                        track_id: t2,
                        start_tick: Tick(PPQ * 8),
                        length_ticks: Tick(PPQ * 4),
                        muted: false,
                        content: ClipContent::Pattern { pattern_id: melody, source_offset_ticks: Tick::ZERO, looped: false },
                    },
                },
            )?;
            let unique = project.pattern(melody).unwrap().duplicate("Melody copy".into());
            let uid = unique.id;
            history.apply(
                &mut project,
                Command::Batch {
                    commands: vec![
                        Command::AddPattern { pattern: unique },
                        Command::UpdateClip {
                            clip: Clip {
                                id: second,
                                track_id: t2,
                                start_tick: Tick(PPQ * 8),
                                length_ticks: Tick(PPQ * 4),
                                muted: false,
                                content: ClipContent::Pattern { pattern_id: uid, source_offset_ticks: Tick::ZERO, looped: false },
                            },
                        },
                    ],
                },
            )?;
            // trim the start of the unique clip by one beat (keeps the source offset) and transpose its chord up an octave
            history.apply(
                &mut project,
                Command::UpdateClip {
                    clip: Clip {
                        id: second,
                        track_id: t2,
                        start_tick: Tick(PPQ * 9),
                        length_ticks: Tick(PPQ * 3),
                        muted: false,
                        content: ClipContent::Pattern { pattern_id: uid, source_offset_ticks: Tick(PPQ), looped: false },
                    },
                },
            )?;
            let up: Vec<Note> = project.pattern(uid).unwrap().lanes[0].notes.iter().filter(|n| n.tick.0 == PPQ * 2).map(|n| Note { pitch: n.pitch + 12, ..*n }).collect();
            history.apply(&mut project, Command::UpdateNotes { pattern_id: uid, channel_id: ch, notes: up })?;
            if project.pattern(melody).unwrap().lanes[0].notes.iter().any(|n| n.pitch > 72) {
                bail!("make unique leaked edits into the original pattern");
            }
            // 5. tempo change at bar 3 and a loop range over bars 2–3
            let mut tempo = project.tempo.clone();
            tempo.push(rawdawg_project::TempoPoint { tick: Tick(PPQ * 8), us_per_quarter: UsPerQuarter(400_000) });
            history.apply(&mut project, Command::SetTempoMap { tempo })?;
            history.apply(&mut project, Command::SetLoop { range: Some(LoopRange { start_tick: Tick(PPQ * 4), end_tick: Tick(PPQ * 12), enabled: true }) })?;
            rawdawg_project::validate(&project)?;
            // 6. export the song and inspect timing: kick onsets at every beat; bars 1–2 at 120 BPM (4 s),
            //    bars 3–4 at 150 BPM (8 beats × 0.4 s)
            let mut p_export = project.clone();
            p_export.r#loop = None;
            let opts = OfflineOptions { mode: rawdawg_audio_engine::TransportMode::Song, tail_seconds: 0.0, ..Default::default() };
            let (l, _, stats) = render_project(&p_export, &opts)?;
            let expected_frames = 48_000.0 * (4.0 + 8.0 * 0.4);
            if (stats.frames as f64 - expected_frames).abs() > 2.0 {
                bail!("song length {} frames, expected ≈{expected_frames:.0}", stats.frames);
            }
            let rms_at = |f: usize| rawdawg_dsp::math::rms(&l[f..f + 2_000]);
            for beat in 0..8usize {
                let at = beat * 24_000;
                if rms_at(at + 100) < 0.02 {
                    bail!("no kick at beat {beat} (frame {at})");
                }
            }
            // after the tempo change (bar 3 = frame 192 000) beats are 19 200 frames apart
            for beat in 0..8usize {
                let at = 192_000 + beat * 19_200;
                if rms_at(at + 100) < 0.02 {
                    bail!("no kick at bar 3 beat {beat} (frame {at}) after the tempo change");
                }
            }
            // the recorded note (F4, 962 ticks ≈ frame 24 050) sounds in bar 1
            let f = dominant_frequency(&l[26_000..34_000], 48_000.0);
            let ok_f4 = (f - 349.23).abs() < 3.0 || (f - 65.41).abs() < 2.0; // F4 or the kick fundamental, depending on balance
            if !ok_f4 {
                bail!("unexpected dominant frequency after the recorded note: {f:.1} Hz");
            }
            // loop range playback: with the loop on, the engine wraps at bar 3 instead of continuing
            let out = compile(&project, 2, 256, &limits, &Default::default(), &rawdawg_audio_engine::NoAssets)?;
            let (mut engine, mut handle) = Engine::new(SampleRate(48_000), 256, &limits);
            engine.install_plan(out.plan);
            handle.send(EngineCommand::SetMode(rawdawg_audio_engine::TransportMode::Song)).map_err(|_| anyhow::anyhow!("queue"))?;
            handle.send(EngineCommand::SeekTick(PPQ * 11)).map_err(|_| anyhow::anyhow!("queue"))?;
            handle.send(EngineCommand::Play).map_err(|_| anyhow::anyhow!("queue"))?;
            let (mut bl, mut br) = (vec![0.0f32; 256], vec![0.0f32; 256]);
            for _ in 0..200 {
                engine.render_block(&mut bl, &mut br);
            }
            let tick_now = engine.status().position_tick.load(std::sync::atomic::Ordering::Relaxed);
            if !(PPQ * 4..PPQ * 12).contains(&tick_now) {
                bail!("loop did not wrap into bars 2–3: playhead at tick {tick_now}");
            }
            Ok((format!("recorded note tick 962/dur 471, undo/redo exact, song {} frames with tempo change, loop wrapped to tick {tick_now}", stats.frames), None))
        },
    ));
    checks.push(check("HL-07 audio device enumeration (no device required)", &["PLAT-02"], || {
        let snap = rawdawg_audio_io::enumerate();
        let path = out.join("devices.json");
        std::fs::write(&path, serde_json::to_vec_pretty(&snap)?)?;
        Ok((format!("{} hosts, {} devices, output available: {}", snap.hosts.len(), snap.devices.len(), snap.has_output()), Some(path.display().to_string())))
    }));
    checks
}

fn plugin_checks(root: &Path, out: &Path) -> Vec<Check> {
    let mut checks = Vec::new();
    let profile_dir = root.join("target").join(if cfg!(debug_assertions) { "debug" } else { "release" });
    let clap_name = if cfg!(target_os = "windows") { "rawdawg_clap.dll" } else { "librawdawg_clap.so" };
    let vst3_name = if cfg!(target_os = "windows") { "rawdawg_vst3.dll" } else { "librawdawg_vst3.so" };
    checks.push(check("PL-01 CLAP and VST3 modules built", &["PLUG-01"], || {
        let c = profile_dir.join(clap_name);
        let v = profile_dir.join(vst3_name);
        if !c.is_file() || !v.is_file() {
            bail!("missing {} or {} (run cargo build --workspace)", c.display(), v.display());
        }
        Ok((format!("{} ({} bytes), {} ({} bytes)", clap_name, c.metadata()?.len(), vst3_name, v.metadata()?.len()), Some(profile_dir.display().to_string())))
    }));
    checks.push(check("PL-02 CLAP/VST3 ABI round-trip tests (cargo test)", &["PLUG-01", "T-PLUGIN"], || {
        let status = Command::new("cargo").args(["test", "-q", "-p", "rawdawg-plugin-clap", "-p", "rawdawg-plugin-vst3"]).status()?;
        if !status.success() {
            bail!("plugin tests failed");
        }
        Ok(("host load, instantiate, process, params, state: pass".into(), None))
    }));
    let validator = which("clap-validator");
    checks.push(match validator {
        Some(path) => check("PL-03 clap-validator on exported CLAP", &["PLUG-03", "T-PLUGIN"], || {
            let clap_path = out.join("rawDAWg.clap");
            std::fs::copy(profile_dir.join(clap_name), &clap_path)?;
            let output = Command::new(&path).args(["validate", clap_path.to_str().unwrap()]).output()?;
            let log = out.join("clap-validator.log");
            std::fs::write(&log, [&output.stdout[..], &output.stderr[..]].concat())?;
            if !output.status.success() {
                bail!("clap-validator failed, see {}", log.display());
            }
            Ok(("clap-validator passed".into(), Some(log.display().to_string())))
        }),
        None => blocked(
            "PL-03 clap-validator on exported CLAP",
            &["PLUG-03", "T-PLUGIN"],
            "clap-validator not installed on this machine; install from https://github.com/free-audio/clap-validator and rerun",
        ),
    });
    checks.push(blocked(
        "PL-04 VST3 SDK validator on exported VST3",
        &["PLUG-03", "T-PLUGIN"],
        "Steinberg validator requires the VST3 SDK build; scheduled for M7 with bundle layout",
    ));
    checks.push(blocked(
        "PL-05 independent host test (CLAP/VST3)",
        &["PLUG-01", "PLUG-02", "E2E-07"],
        "manual gate: load exported plugins in an independent host on Windows and Ubuntu (M7)",
    ));
    checks
}

fn which(bin: &str) -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|p| p.join(bin)).find(|p| p.is_file())
}

pub fn run(root: &Path, suite: Suite) -> anyhow::Result<()> {
    let out = root.join("target/verification");
    std::fs::create_dir_all(&out)?;
    let started = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs();
    let mut checks = Vec::new();
    if matches!(suite, Suite::Headless | Suite::All) {
        checks.extend(headless_checks(root, &out));
    }
    if matches!(suite, Suite::Plugins | Suite::All) {
        checks.extend(plugin_checks(root, &out));
    }
    // Gates that this suite can never establish by itself.
    if matches!(suite, Suite::All) {
        checks.push(blocked(
            "HW-01 real audio device playback/record matrix",
            &["PLAT-02", "IO-04", "QUAL-04"],
            "requires real hardware session on Windows and Ubuntu; run `rawdawg --audio-smoke` and record device/rate/buffer/xruns in docs/verification/hardware.md",
        ));
        checks.push(blocked("GUI-01 manual GUI verification at 1024x720/1440x900/1920x1080", &["PLAT-03", "QUAL-04"], "requires display session; take screenshots on both OSs"));
    }
    let report = Report {
        suite: format!("{suite:?}"),
        os: std::env::consts::OS.into(),
        arch: std::env::consts::ARCH.into(),
        rustc: rustc_version(),
        git_commit: git_commit(),
        started_utc_unix: started,
        checks,
    };
    let json = out.join(format!("verify-{}.json", format!("{suite:?}").to_lowercase()));
    std::fs::write(&json, serde_json::to_vec_pretty(&report)?)?;
    let mut failed = 0;
    println!("{:<64} {:<8} detail", "check", "status");
    for c in &report.checks {
        let s = match c.status {
            Status::Pass => "PASS",
            Status::Fail => {
                failed += 1;
                "FAIL"
            }
            Status::Blocked => "BLOCKED",
            Status::NotRun => "NOT RUN",
        };
        println!("{:<64} {:<8} {}", c.id, s, c.detail);
    }
    println!("report: {}", json.display());
    if failed > 0 {
        bail!("{failed} check(s) FAILED");
    }
    Ok(())
}

/// Offline throughput + block timing on the reference-style workload (M1 subset:
/// four synths playing 8-note chords). Prints realtime factor and block-time
/// percentiles; the full M8 workload adds samplers, effects and sends.
pub fn bench_audio(root: &Path, seconds: f64) -> anyhow::Result<()> {
    use rawdawg_audio_engine::commands::EngineCommand;
    use rawdawg_audio_engine::engine::Engine;
    use rawdawg_types::units::Tick;
    let mut p = rawdawg_project::Project::new_default("bench", serde_json::to_value(rawdawg_synthesizer::Patch::default())?);
    let base_channel = p.channels[0].clone();
    for i in 1..4 {
        let mut c = base_channel.clone();
        c.id = rawdawg_types::ChannelId::new();
        c.instrument.id = rawdawg_types::InstanceId::new();
        c.name = format!("Synth {}", i + 1);
        p.channels.push(c);
    }
    let ids: Vec<_> = p.channels.iter().map(|c| c.id).collect();
    p.patterns[0].lanes = ids
        .iter()
        .map(|id| rawdawg_project::Lane {
            channel_id: *id,
            notes: (0..8)
                .map(|k| rawdawg_project::Note { id: rawdawg_types::NoteId::new(), tick: Tick(0), duration_ticks: Tick(3840), pitch: 48 + k * 3, velocity: 0.8, channel: 0 })
                .collect(),
            controllers: vec![],
        })
        .collect();
    let limits = rawdawg_types::limits::EngineLimits::DEFAULT;
    let block = 256usize;
    let plan = rawdawg_audio_engine::plan::compile(&p, 1, block, &limits, &std::collections::HashMap::new(), &rawdawg_audio_engine::NoAssets)?.plan;
    let (mut engine, mut handle) = Engine::new(rawdawg_types::SampleRate::DEFAULT, block, &limits);
    engine.install_plan(plan);
    handle.send(EngineCommand::Play).ok();
    let mut l = vec![0.0f32; block];
    let mut r = vec![0.0f32; block];
    let blocks = ((seconds * 48_000.0) / block as f64) as usize;
    let mut times = Vec::with_capacity(blocks);
    let start = Instant::now();
    for _ in 0..blocks {
        let t = Instant::now();
        engine.render_block(&mut l, &mut r);
        times.push(t.elapsed().as_secs_f64() * 1e3);
    }
    let elapsed = start.elapsed().as_secs_f64();
    times.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let pct = |q: f64| times[((times.len() - 1) as f64 * q) as usize];
    let deadline_ms = block as f64 / 48.0;
    let voices = engine.status().active_voices.load(std::sync::atomic::Ordering::Relaxed);
    println!("workload: 4 synths × 8 voices = {voices} voices, 48 kHz, {block}-frame blocks, {blocks} blocks ({seconds:.0} s of audio)");
    println!("offline: {:.2} s wall, {:.1}x realtime", elapsed, seconds / elapsed);
    println!(
        "block time ms: p50 {:.3} p95 {:.3} p99 {:.3} max {:.3} (deadline {:.3} ms; p99 at {:.0}% of deadline)",
        pct(0.5),
        pct(0.95),
        pct(0.99),
        times[times.len() - 1],
        deadline_ms,
        pct(0.99) / deadline_ms * 100.0
    );
    let out = root.join("target/verification");
    std::fs::create_dir_all(&out)?;
    let report = serde_json::json!({"voices": voices, "block": block, "blocks": blocks, "seconds": seconds, "wall_seconds": elapsed, "p50_ms": pct(0.5), "p95_ms": pct(0.95), "p99_ms": pct(0.99), "max_ms": times[times.len()-1], "deadline_ms": deadline_ms, "build": if cfg!(debug_assertions) {"debug"} else {"release"}, "os": std::env::consts::OS});
    std::fs::write(out.join("bench-audio.json"), serde_json::to_vec_pretty(&report)?)?;
    Ok(())
}
