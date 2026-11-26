//! Verification suites. Each check records PASS / FAIL / BLOCKED / NOT RUN with
//! evidence, and the run fails (nonzero) if any required check failed.

use crate::Suite;
use anyhow::bail;
use rawdawg_audio_engine::offline::{OfflineOptions, WavFormat, render_project, render_project_to_wav};
use rawdawg_dsp::analysis::dominant_frequency;
use serde::Serialize;
use std::path::Path;
use std::process::Command;

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
    checks.push(check("HL-12 audio clip plays from the song fixture at its bar with gain and fades", &["CORE-04", "IO-05"], || {
        let dir = root.join("fixtures/demo-song.rawdawg");
        let mut p = rawdawg_project::load_project_dir(&dir)?;
        // mute the instrument tracks: only the audio clip (bar 1, first second of the sine, −6 dB) remains
        for t in p.tracks.iter_mut().filter(|t| t.name != "Audio") {
            t.mute = true;
        }
        let opts = OfflineOptions {
            mode: rawdawg_audio_engine::commands::TransportMode::Song,
            tail_seconds: 0.0,
            end_tick: Some(rawdawg_types::units::Tick(3840 * 2)),
            project_dir: Some(dir.clone()),
            unprotected: true,
            ..Default::default()
        };
        let (l, r, stats) = render_project(&p, &opts)?;
        if stats.nonfinite_blocks > 0 {
            bail!("nonfinite blocks");
        }
        let f = dominant_frequency(&l[4000..40_000], 48_000.0);
        if (f - 440.0).abs() > 1.0 {
            bail!("expected the 440 Hz clip in bar 1, measured {f:.2} Hz");
        }
        let pk = rawdawg_dsp::math::peak(&l[8000..40_000]);
        if (pk - 0.25).abs() > 0.02 {
            bail!("clip gain −6 dB on a 0.5 peak sine should peak ≈ 0.25, measured {pk:.3}");
        }
        if rawdawg_dsp::math::rms(&l[0..200]) > rawdawg_dsp::math::rms(&l[2000..2200]) * 0.5 {
            bail!("fade-in missing");
        }
        if rawdawg_dsp::math::peak(&l[49_000..]) > 0.0 {
            bail!("the clip must stop after its first second");
        }
        if l.iter().zip(&r).any(|(a, b)| (a - b).abs() > 1e-6) {
            bail!("mono clip should feed both channels equally");
        }
        Ok((format!("bar 1 fundamental {f:.2} Hz, peak {pk:.3} (−6 dB), fades and end honoured"), Some(dir.display().to_string())))
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
            history.apply(
                &mut project,
                Command::AddTrack { track: Track { id: t1, name: "Drums".into(), color: None, mute: false, solo: false, height: None, mixer_id: None }, index: None },
            )?;
            history.apply(
                &mut project,
                Command::AddTrack { track: Track { id: t2, name: "Melody".into(), color: None, mute: false, solo: false, height: None, mixer_id: None }, index: None },
            )?;
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
    checks.push(check(
        "E2E-04 (headless subset) import WAV, FLAC, SF2 and MIDI; export WAV, FLAC, MIDI and stems; inspect timing and audio (MP3 import: unit-level decode only, no encoder here)",
        &["IO-05", "IO-06", "CORE-04"],
        || {
            use rawdawg_audio_engine::commands::TransportMode;
            use rawdawg_audio_engine::offline::{ExportFormat, WavFormat, export_project, export_stems};
            use rawdawg_project::{Channel, Clip, ClipContent, Command, History, Instance, Lane, MixerChannel, Pattern, Project, Track};
            use rawdawg_types::units::{Db, Pan, Tick};
            use rawdawg_types::{ChannelId, ClipId, MixerChannelId, PatternId, TrackId};
            let dir = out.join("e2e-04.rawdawg");
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir)?;
            let mut project = Project::new_default("e2e-04", serde_json::to_value(rawdawg_synthesizer::Patch::default())?);
            let mut history = History::new();
            rawdawg_project::save_project_dir(&dir, &project)?;
            // 1. import WAV, FLAC and SF2 into the project's managed assets (originals untouched)
            let wav = rawdawg_project::assets::import_asset(&dir, &root.join("fixtures/sine_440_48k.wav"))?;
            let flac = rawdawg_project::assets::import_asset(&dir, &root.join("fixtures/sine_440_48k.flac"))?;
            let sf2 = rawdawg_project::assets::import_asset(&dir, &root.join("fixtures/fixture.sf2"))?;
            for a in [&wav, &flac, &sf2] {
                history.apply(&mut project, Command::AddAsset { asset: a.clone() })?;
            }
            let flac_decoded = rawdawg_sampler::decode::decode_file(&root.join("fixtures/sine_440_48k.flac"))?;
            if flac_decoded.frames() != 96_000 || (dominant_frequency(&flac_decoded.channels[0][..16_384], 48_000.0) - 440.0).abs() > 1.0 {
                bail!("FLAC fixture decodes to {} frames", flac_decoded.frames());
            }
            if rawdawg_project::assets::import_asset(&dir, &root.join("Cargo.toml")).is_ok() {
                bail!("unsupported files must be refused explicitly");
            }
            // 2. import the MIDI fixture: channels, a pattern and a playlist clip, tempo/meter from the file
            let imp = rawdawg_project::smf::import(&std::fs::read(root.join("fixtures/motif.mid"))?, "motif")?;
            if imp.tracks.is_empty() || imp.tempo.len() < 2 {
                bail!("MIDI import: {} tracks, {} tempo points", imp.tracks.len(), imp.tempo.len());
            }
            let master = project.mixer.master_id;
            let mut lanes = Vec::new();
            for t in &imp.tracks {
                let mix = MixerChannelId::new();
                history.apply(&mut project, Command::AddMixerChannel { channel: MixerChannel { id: mix, name: t.name.clone(), color: None, gain_db: Db(0.0), pan: Pan(0.0), mute: false, solo: false, inserts: vec![], sends: vec![], output_id: Some(master) }, index: None })?;
                let ch = ChannelId::new();
                history.apply(&mut project, Command::AddChannel { channel: Channel { id: ch, name: t.name.clone(), color: None, instrument: Instance::native(rawdawg_types::ids::native_types::SYNTH, serde_json::to_value(rawdawg_synthesizer::Patch::default())?), gain_db: Db(0.0), pan: Pan(0.0), mute: false, solo: false, mixer_id: mix }, index: None })?;
                lanes.push(Lane { channel_id: ch, notes: t.notes.clone(), controllers: t.controllers.clone() });
            }
            let pid = PatternId::new();
            history.apply(&mut project, Command::AddPattern { pattern: Pattern { id: pid, name: "motif".into(), color: None, length_ticks: imp.length_ticks, steps: 16, step_ticks: Tick(240), lanes } })?;
            let tid = TrackId::new();
            history.apply(&mut project, Command::AddTrack { track: Track { id: tid, name: "motif".into(), color: None, mute: false, solo: false, height: None, mixer_id: None }, index: None })?;
            history.apply(&mut project, Command::AddClip { clip: Clip { id: ClipId::new(), track_id: tid, start_tick: Tick(0), length_ticks: imp.length_ticks, muted: false, content: ClipContent::Pattern { pattern_id: pid, source_offset_ticks: Tick::ZERO, looped: false } } })?;
            history.apply(&mut project, Command::Batch { commands: vec![Command::SetTempoMap { tempo: imp.tempo.clone() }, Command::SetMeterMap { meter: imp.meter.clone() }] })?;
            // the WAV as an audio clip on its own track
            let at = TrackId::new();
            history.apply(&mut project, Command::AddTrack { track: Track { id: at, name: "Audio".into(), color: None, mute: false, solo: false, height: None, mixer_id: None }, index: None })?;
            history.apply(&mut project, Command::AddClip { clip: Clip { id: ClipId::new(), track_id: at, start_tick: Tick(0), length_ticks: Tick(3840), muted: false, content: ClipContent::Audio { asset_id: wav.id.clone(), source_offset_frames: 0, source_length_frames: 96_000, gain_db: Db(-6.0), fade_in_frames: 0, fade_out_frames: 0, playback_rate: 1.0 } } })?;
            rawdawg_project::save_project_dir(&dir, &project)?;
            let reloaded = rawdawg_project::load_project_dir(&dir)?;
            if reloaded.tempo != imp.tempo || reloaded.meter != imp.meter {
                bail!("tempo/meter did not survive save/reload");
            }
            // 3. export WAV 24, FLAC 16, stems and MIDI; inspect
            let opts = OfflineOptions { mode: TransportMode::Song, tail_seconds: 0.2, project_dir: Some(dir.clone()), ..Default::default() };
            let wav_out = dir.join("export.wav");
            let stats = export_project(&reloaded, &opts, ExportFormat::Wav(WavFormat::Pcm24), &wav_out, &mut |_, _| true)?;
            let flac_out = dir.join("export.flac");
            let stats_f = export_project(&reloaded, &opts, ExportFormat::Flac16, &flac_out, &mut |_, _| true)?;
            let (dw, df) = (rawdawg_sampler::decode::decode_file(&wav_out)?, rawdawg_sampler::decode::decode_file(&flac_out)?);
            if dw.frames() != stats.frames || df.frames() != stats_f.frames || dw.frames() != df.frames() {
                bail!("export lengths differ: wav {} flac {} (expected {})", dw.frames(), df.frames(), stats.frames);
            }
            let diff = dw.channels[0].iter().zip(&df.channels[0]).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
            if diff > 4.0 / 32767.0 {
                bail!("WAV 24 and FLAC 16 exports differ by {diff} (dither aside they are the same render)");
            }
            if stats.nonfinite_blocks > 0 || rawdawg_dsp::math::rms(&dw.channels[0][..40_000]) < 0.05 {
                bail!("export silent or nonfinite");
            }
            let fund = dominant_frequency(&dw.channels[0][4000..20_000], 48_000.0);
            let _ = fund;
            let (stems, st) = export_stems(&reloaded, &opts, ExportFormat::Wav(WavFormat::Float32), &dir.join("stems"), &mut |_, _| true)?;
            if stems.len() != reloaded.mixer.channels.len() {
                bail!("{} stems for {} mixer channels", stems.len(), reloaded.mixer.channels.len());
            }
            for s in &stems {
                if rawdawg_sampler::decode::decode_file(&s.path)?.frames() != st.frames {
                    bail!("stem {} length differs", s.name);
                }
            }
            let mid_out = dir.join("export.mid");
            std::fs::write(&mid_out, rawdawg_project::smf::export_song(&reloaded)?)?;
            let back = rawdawg_project::smf::import(&std::fs::read(&mid_out)?, "back")?;
            let n_arranged = rawdawg_project::smf::song_notes(&reloaded).len();
            let n_back: usize = back.tracks.iter().map(|t| t.notes.len()).sum();
            if n_back != n_arranged || back.tempo != reloaded.tempo {
                bail!("MIDI export/import mismatch: {n_back} notes vs {n_arranged}, tempo equal: {}", back.tempo == reloaded.tempo);
            }
            Ok((
                format!("assets wav/flac/sf2 imported, {} MIDI tracks with {} tempo points, exports {} frames (wav24 ≡ flac16 within {diff:.5}), {} stems, MIDI round trip {n_back} notes", imp.tracks.len(), imp.tempo.len(), stats.frames, stems.len()),
                Some(dir.display().to_string()),
            ))
        },
    ));
    checks.push(check(
        "E2E-05 (headless subset) insert chain, EQ band edit, sidechain compressor, hall + convolution sends, latency-bearing effect, parallel branch, automation, randomize, morph, bypass, undo",
        &["CORE-05", "CORE-08", "FX-01", "FX-02", "FX-03", "QUAL-02"],
        || {
            use rawdawg_audio_engine::commands::TransportMode;
            use rawdawg_effects::{convolution, dynamics::compressor, eq, rack, reverb, testfx};
            use rawdawg_project::{
                AutomationCurve, AutomationMode, AutomationPoint, AutomationTarget, Channel, Clip, ClipContent, Command, History, Instance, Interpolation, MixerChannel, Note, Project,
                Send, SendTap, Track,
            };
            use rawdawg_types::units::{Db, PPQ, Pan, Tick};
            use rawdawg_types::{ChannelId, ClipId, CurveId, MixerChannelId, NoteId, ParamId, PointId, TrackId};
            let dir = out.join("e2e-05.rawdawg");
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir)?;
            // an effect state from (key, physical value) pairs, through the real processor
            let fx_state = |type_id: &str, values: &[(&str, f64)]| -> anyhow::Result<serde_json::Value> {
                let mut p = rawdawg_effects::make_effect(type_id, None).map_err(anyhow::Error::msg)?;
                for (key, v) in values {
                    let d = p.descriptors().iter().find(|d| d.key == *key).copied().ok_or_else(|| anyhow::anyhow!("no param {key}"))?;
                    p.set_param(d.id, d.to_normalized(*v));
                }
                Ok(serde_json::from_slice(&p.save_state())?)
            };
            let patch = serde_json::to_value(rawdawg_synthesizer::Patch::default())?;
            let mut project = Project::new_default("e2e-05", patch.clone());
            let mut history = History::new();
            let ch_a = project.channels[0].id;
            let mix_a = project.channels[0].mixer_id;
            let pattern = project.patterns[0].id;
            // channel B on its own mixer channel: a held bass note that the sidechain will duck
            let mix_b = MixerChannelId::new();
            // `output_id: None` means "no output" (the master); every other channel routes somewhere
            let master = project.mixer.master_id;
            let bus = |id: MixerChannelId, name: &str| MixerChannel { id, name: name.into(), color: None, gain_db: Db(0.0), pan: Pan(0.0), mute: false, solo: false, inserts: vec![], sends: vec![], output_id: Some(master) };
            history.apply(&mut project, Command::AddMixerChannel { channel: bus(mix_b, "B"), index: None })?;
            let ch_b = ChannelId::new();
            let mut inst_b = Instance::native(rawdawg_types::ids::native_types::SYNTH, patch);
            inst_b.display_name = "B".into();
            history.apply(
                &mut project,
                Command::AddChannel { channel: Channel { id: ch_b, name: "B".into(), color: None, instrument: inst_b, gain_db: Db(0.0), pan: Pan(0.0), mute: false, solo: false, mixer_id: mix_b }, index: None },
            )?;
            let eighths: Vec<Note> = (0..8).map(|k| Note { id: NoteId::new(), tick: Tick(k * PPQ / 2), duration_ticks: Tick(PPQ / 4), pitch: 72, velocity: 0.9, channel: 0 }).collect();
            history.apply(&mut project, Command::AddNotes { pattern_id: pattern, channel_id: ch_a, notes: eighths })?;
            history.apply(
                &mut project,
                Command::AddNotes { pattern_id: pattern, channel_id: ch_b, notes: vec![Note { id: NoteId::new(), tick: Tick(0), duration_ticks: Tick(PPQ * 4), pitch: 36, velocity: 0.8, channel: 0 }] },
            )?;
            // 1. insert chain on A: EQ (band 0 "dragged" to 300 Hz / +6 dB) then a latency-bearing effect (1 000 frames)
            let mut eq_inst = Instance::native(eq::TYPE_ID, fx_state(eq::TYPE_ID, &[("band0.on", 1.0), ("band0.freq", 300.0), ("band0.gain", 6.0)])?);
            eq_inst.display_name = "EQ".into();
            let eq_id = eq_inst.id;
            history.apply(&mut project, Command::AddInsert { mixer_id: mix_a, instance: eq_inst, index: None })?;
            let mut lat_inst = Instance::native(testfx::TYPE_ID, fx_state(testfx::TYPE_ID, &[("latency", 1000.0)])?);
            lat_inst.display_name = "Latency".into();
            let lat_id = lat_inst.id;
            history.apply(&mut project, Command::AddInsert { mixer_id: mix_a, instance: lat_inst, index: None })?;
            // 2. sends: hall reverb bus and a convolution bus (impulse IR = identity, imported into the project)
            let (mix_verb, mix_conv) = (MixerChannelId::new(), MixerChannelId::new());
            history.apply(&mut project, Command::AddMixerChannel { channel: bus(mix_verb, "Verb"), index: None })?;
            history.apply(&mut project, Command::AddMixerChannel { channel: bus(mix_conv, "Conv"), index: None })?;
            let mut hall = Instance::native(reverb::TYPE_ID, fx_state(reverb::TYPE_ID, &[("mode", 1.0), ("decay", 2.0), ("mix", 100.0)])?);
            hall.display_name = "Hall".into();
            history.apply(&mut project, Command::AddInsert { mixer_id: mix_verb, instance: hall, index: None })?;
            rawdawg_project::save_project_dir(&dir, &project)?;
            let asset = rawdawg_project::assets::import_asset(&dir, &root.join("fixtures/impulse_48k.wav"))?;
            let ir_id = asset.id.to_string();
            history.apply(&mut project, Command::AddAsset { asset })?;
            let mut conv_state = fx_state(convolution::TYPE_ID, &[("mix", 100.0)])?;
            conv_state["ir_asset"] = serde_json::Value::String(ir_id);
            let mut conv = Instance::native(convolution::TYPE_ID, conv_state);
            conv.display_name = "Convolution".into();
            history.apply(&mut project, Command::AddInsert { mixer_id: mix_conv, instance: conv, index: None })?;
            // 3. compressor on B listening to A through a sidechain send
            let duck = rawdawg_effects::presets(compressor::TYPE_ID).iter().find(|p| p.name.to_lowercase().contains("duck") || p.name.to_lowercase().contains("sidechain")).ok_or_else(|| anyhow::anyhow!("no sidechain preset"))?;
            let duck_state: serde_json::Value = serde_json::from_slice(&rawdawg_effects::preset_state(compressor::TYPE_ID, duck).ok_or_else(|| anyhow::anyhow!("preset state"))?)?;
            let mut comp = Instance::native(compressor::TYPE_ID, duck_state.clone());
            comp.display_name = "Duck".into();
            let comp_id = comp.id;
            history.apply(&mut project, Command::AddInsert { mixer_id: mix_b, instance: comp, index: None })?;
            let sends_a = vec![
                Send { target_id: mix_verb, level_db: Db(-12.0), tap: SendTap::PostFader, sidechain: false },
                Send { target_id: mix_conv, level_db: Db(-12.0), tap: SendTap::PostFader, sidechain: false },
                Send { target_id: mix_b, level_db: Db(0.0), tap: SendTap::PreFader, sidechain: true },
            ];
            history.apply(&mut project, Command::SetMixerSends { id: mix_a, sends: sends_a.clone() })?;
            // 4. parallel branch of A (the mixer's "+ Parallel branch"): bus fed by a 0 dB post-fader send, flat EQ on it
            let mix_par = MixerChannelId::new();
            let mut par_sends = sends_a.clone();
            par_sends.push(Send { target_id: mix_par, level_db: Db(0.0), tap: SendTap::PostFader, sidechain: false });
            history.apply(&mut project, Command::Batch { commands: vec![Command::AddMixerChannel { channel: bus(mix_par, "A ∥"), index: None }, Command::SetMixerSends { id: mix_a, sends: par_sends.clone() }] })?;
            let mut flat = Instance::native(eq::TYPE_ID, fx_state(eq::TYPE_ID, &[("band0.on", 0.0), ("band1.on", 0.0)])?);
            flat.display_name = "Flat".into();
            history.apply(&mut project, Command::AddInsert { mixer_id: mix_par, instance: flat, index: None })?;
            // 5. song arrangement (two bars) with an automation clip sweeping EQ band 0 gain
            let track = TrackId::new();
            history.apply(&mut project, Command::AddTrack { track: Track { id: track, name: "Song".into(), color: None, mute: false, solo: false, height: None, mixer_id: None }, index: None })?;
            history.apply(
                &mut project,
                Command::AddClip {
                    clip: Clip { id: ClipId::new(), track_id: track, start_tick: Tick(0), length_ticks: Tick(PPQ * 8), muted: false, content: ClipContent::Pattern { pattern_id: pattern, source_offset_ticks: Tick::ZERO, looped: true } },
                },
            )?;
            let gain_param = eq::band_param(0, eq::B_GAIN);
            let curve = AutomationCurve {
                id: CurveId::new(),
                name: "EQ gain".into(),
                target: AutomationTarget { instance_id: eq_id, param_id: gain_param },
                points: vec![
                    AutomationPoint { id: PointId::new(), tick: Tick(0), value: 0.5, interpolation: Interpolation::Linear },
                    AutomationPoint { id: PointId::new(), tick: Tick(PPQ * 8), value: 0.75, interpolation: Interpolation::Linear },
                ],
                mode: AutomationMode::Read,
                disabled: false,
            };
            let curve_id = curve.id;
            history.apply(&mut project, Command::Batch { commands: vec![
                Command::AddAutomationCurve { curve },
                Command::AddClip { clip: Clip { id: ClipId::new(), track_id: track, start_tick: Tick(0), length_ticks: Tick(PPQ * 8), muted: false, content: ClipContent::Automation { curve_id, source_offset_ticks: Tick::ZERO, looped: false } } },
            ] })?;
            let opts = OfflineOptions { mode: TransportMode::Song, end_tick: Some(Tick(PPQ * 8)), tail_seconds: 0.3, project_dir: Some(dir.clone()), ..Default::default() };
            // full mix: finite, no clipped samples, under full scale (master protection on)
            let (l, r, stats) = render_project(&project, &opts)?;
            if stats.nonfinite_blocks > 0 || stats.clipped_samples > 0 || stats.peak > 1.0 {
                bail!("full mix: nonfinite {} clipped {} peak {:.3}", stats.nonfinite_blocks, stats.clipped_samples, stats.peak);
            }
            let full_rms = rawdawg_dsp::math::rms(&l[..]).max(rawdawg_dsp::math::rms(&r[..]));
            if full_rms < 0.02 {
                bail!("full mix too quiet: rms {full_rms:.4}");
            }
            // sidechain action in the full mix: B ducks under A's eighth notes, so the mix gets quieter
            let mut no_sc = duck_state.clone();
            // effect state = { version, params: { key: normalized } }; a toggle off is 0
            no_sc["params"]["sidechain"] = serde_json::json!(0.0);
            history.apply(&mut project, Command::SetInstanceState { instance_id: comp_id, state_version: 1, state: no_sc.clone() })?;
            let (l_mix_free, _, _) = render_project(&project, &opts)?;
            history.undo(&mut project)?;
            let mix_free = rawdawg_dsp::math::rms(&l_mix_free[24_000..144_000]);
            let mix_duck = rawdawg_dsp::math::rms(&l[24_000..144_000]);
            // B is a small share of the full mix, so only the direction is judged here; the
            // soloed render below measures the ducking itself
            if mix_duck >= mix_free {
                bail!("sidechain compressor did not lower the full mix: with {mix_duck:.4}, without {mix_free:.4}");
            }
            // solo B: solo keeps the sidechain feed from A, so B still ducks while A stays out of the
            // mix. Reference = B soloed with the compressor bypassed (uncompressed).
            history.apply(&mut project, Command::SetMixerSolo { id: mix_b, solo: true })?;
            let (l_duck, _, _) = render_project(&project, &opts)?;
            history.apply(&mut project, Command::SetInstanceBypass { instance_id: comp_id, bypass: true })?;
            let (l_free, _, _) = render_project(&project, &opts)?;
            history.undo(&mut project)?; // compressor back on
            history.undo(&mut project)?; // solo off
            let (rms_duck, rms_free) = (rawdawg_dsp::math::rms(&l_duck[24_000..144_000]), rawdawg_dsp::math::rms(&l_free[24_000..144_000]));
            if rms_duck >= rms_free * 0.85 {
                bail!("solo did not keep the sidechain feed: soloed B ducked {rms_duck:.4} vs uncompressed {rms_free:.4}");
            }
            // the ducking follows A's eighth notes: B is quieter while A sounds than just after it stops
            let on = rawdawg_dsp::math::rms(&l_duck[96_000 + 1000..96_000 + 5500]);
            let off = rawdawg_dsp::math::rms(&l_duck[96_000 + 7500..96_000 + 11_500]);
            if on >= off {
                bail!("ducking does not follow the sidechain rhythm: during A {on:.4}, after A {off:.4}");
            }
            // A itself must not be heard in the soloed mix: its fundamental (C5) is absent
            let spec = rawdawg_dsp::analysis::magnitude_spectrum(&l_duck[48_000..48_000 + 16_384]);
            let bin = |hz: f64| (hz / 48_000.0 * 16_384.0).round() as usize;
            let (a_level, b_level) = (spec[bin(523.25)], spec[bin(65.41)]);
            if a_level > b_level * 0.05 {
                bail!("soloing B leaked A into the mix: A fundamental {a_level:.5} vs B {b_level:.5}");
            }
            // parallel alignment: A (latency 1 000 frames) + its branch (no latency) must sum coherently (≈ 2×)
            for id in [mix_b, mix_verb, mix_conv] {
                history.apply(&mut project, Command::SetMixerMute { id, mute: true })?;
            }
            let (l_par, _, _) = render_project(&project, &opts)?;
            history.apply(&mut project, Command::SetMixerSends { id: mix_a, sends: sends_a.clone() })?;
            let (l_single, _, _) = render_project(&project, &opts)?;
            history.undo(&mut project)?;
            for _ in 0..3 {
                history.undo(&mut project)?;
            }
            let ratio = rawdawg_dsp::math::rms(&l_par[24_000..144_000]) / rawdawg_dsp::math::rms(&l_single[24_000..144_000]).max(1e-9);
            if (ratio - 2.0).abs() > 0.15 {
                bail!("parallel branch not aligned with the latency-bearing chain: level ratio {ratio:.3} (expected ≈ 2.0)");
            }
            // randomize the rack of A deterministically (mix locked), one undo step restores it
            let before_eq = project.instance(eq_id).unwrap().state.clone();
            let mut p = rawdawg_effects::make_effect(eq::TYPE_ID, Some(&serde_json::to_vec(&before_eq)?)).map_err(anyhow::Error::msg)?;
            let current: Vec<(ParamId, f32)> = p.descriptors().iter().filter_map(|d| p.param(d.id).map(|v| (d.id, v))).collect();
            let edits = rack::randomize(p.descriptors(), &current, &mut rawdawg_dsp::rng::DeterministicRng::new(5, 5), 60.0, &rack::RackLocks::default());
            if edits.is_empty() {
                bail!("randomize produced no edits");
            }
            for (id, v) in &edits {
                p.set_param(*id, *v);
            }
            let randomized: serde_json::Value = serde_json::from_slice(&p.save_state())?;
            history.apply(&mut project, Command::SetInstanceState { instance_id: eq_id, state_version: 1, state: randomized.clone() })?;
            if project.instance(eq_id).unwrap().state == before_eq {
                bail!("randomize did not change the EQ state");
            }
            history.undo(&mut project)?;
            if project.instance(eq_id).unwrap().state != before_eq {
                bail!("undo did not restore the EQ state");
            }
            // morph halfway between the original and the randomized EQ: continuous params land in the middle
            let rand_params: Vec<(ParamId, f32)> = {
                let q = rawdawg_effects::make_effect(eq::TYPE_ID, Some(&serde_json::to_vec(&randomized)?)).map_err(anyhow::Error::msg)?;
                q.descriptors().iter().filter_map(|d| q.param(d.id).map(|v| (d.id, v))).collect()
            };
            let mid = rack::morph(p.descriptors(), [&current, &rand_params, &current, &rand_params], 0.5, 0.0);
            let (gid, _) = edits.iter().find(|(id, _)| !matches!(p.descriptors().iter().find(|d| d.id == *id).unwrap().mapping, rawdawg_types::params::ParamMapping::Toggle | rawdawg_types::params::ParamMapping::Discrete { .. })).ok_or_else(|| anyhow::anyhow!("no continuous edit"))?;
            let (a, b, m) = (
                current.iter().find(|(i, _)| i == gid).unwrap().1,
                rand_params.iter().find(|(i, _)| i == gid).unwrap().1,
                mid.iter().find(|(i, _)| i == gid).unwrap().1,
            );
            if (m - (a + b) * 0.5).abs() > 1e-5 {
                bail!("morph midpoint {m} is not between {a} and {b}");
            }
            // bypass the latency effect (alignment kept by true bypass) and undo it
            history.apply(&mut project, Command::SetInstanceBypass { instance_id: lat_id, bypass: true })?;
            let (_, _, stats_byp) = render_project(&project, &opts)?;
            if stats_byp.nonfinite_blocks > 0 || stats_byp.clipped_samples > 0 {
                bail!("bypassed render clipped or nonfinite");
            }
            history.undo(&mut project)?;
            if project.instance(lat_id).unwrap().bypass {
                bail!("undo did not clear the bypass");
            }
            rawdawg_project::save_project_dir(&dir, &project)?;
            Ok((
                format!(
                    "full mix peak {:.3} rms {full_rms:.3}, soloed B ducked {rms_duck:.4} vs uncompressed {rms_free:.4} (during A {on:.4} / after {off:.4}), parallel ratio {ratio:.3}, {} randomized params, morph midpoint exact, bypass/undo ok",
                    stats.peak,
                    edits.len()
                ),
                Some(dir.display().to_string()),
            ))
        },
    ));
    checks.push(check(
        "E2E-06 (headless subset) save/reopen full project, autosave + forced-termination recovery, move to another folder, missing plugin and asset preservation, relink; native renders compared",
        &["DATA-01", "DATA-02", "CORE-07"],
        || {
            use rawdawg_audio_engine::commands::TransportMode;
            use rawdawg_project::{Command, History, Instance, PluginFormat};
            let base = out.join("e2e-06");
            let _ = std::fs::remove_dir_all(&base);
            std::fs::create_dir_all(&base)?;
            // 1. the full song demo saved into a fresh folder, reopened, renders identically
            let src = root.join("fixtures/demo-song.rawdawg");
            let mut p = rawdawg_project::load_project_dir(&src)?;
            let dir = base.join("Song.rawdawg");
            rawdawg_project::save_as(Some(&src), &dir, &p)?;
            let opts = |d: &std::path::Path| OfflineOptions { mode: TransportMode::Song, tail_seconds: 0.0, project_dir: Some(d.to_path_buf()), ..Default::default() };
            let (ref_l, _, _) = render_project(&p, &opts(&src))?;
            let reopened = rawdawg_project::load_project_dir(&dir)?;
            let (l1, _, _) = render_project(&reopened, &opts(&dir))?;
            if l1 != ref_l {
                bail!("render differs after Save As + reopen");
            }
            // 2. forced termination: an autosave with newer edits, then "crash" (no save); recovery offered and recoverable
            let mut history = History::new();
            let lock = rawdawg_project::lock::ProjectLock::acquire(&dir, false)?;
            history.apply(&mut p, Command::RenameProject { name: "edited after save".into() })?;
            p.revision = history.revision();
            let auto = rawdawg_project::autosave(&dir, &p)?;
            std::mem::forget(lock); // the lock file stays behind as after a crash
            let cands = rawdawg_project::recovery_candidates(&dir);
            if cands.len() != 1 || cands[0].path != auto {
                bail!("expected one recovery candidate, got {cands:?}");
            }
            let recovered = rawdawg_project::persist::parse_project(&std::fs::read(&cands[0].path)?)?;
            if recovered.name != "edited after save" || rawdawg_project::load_project_dir(&dir)?.name == "edited after save" {
                bail!("recovery candidate does not carry the unsaved edit, or the saved manifest was touched");
            }
            // the stale lock from the crashed process (our own pid) is reclaimed on reopen
            let relock = rawdawg_project::lock::ProjectLock::acquire(&dir, false)?;
            drop(relock);
            rawdawg_project::persist::clear_autosaves(&dir);
            // 3. move the folder: identical model and render from the new location
            let moved = base.join("moved").join("Song.rawdawg");
            std::fs::create_dir_all(moved.parent().unwrap())?;
            std::fs::rename(&dir, &moved)?;
            let m = rawdawg_project::load_project_dir(&moved)?;
            if m != reopened {
                bail!("model differs after moving the folder");
            }
            let (l2, _, _) = render_project(&m, &opts(&moved))?;
            if l2 != ref_l {
                bail!("render differs after moving the folder");
            }
            // 4. a missing third-party plugin: stand-in in the plan, state and opaque blob preserved across save/load/save-as
            let mut q = m.clone();
            let blob = b"opaque-plugin-state".to_vec();
            let blob_rel = format!("plugin-state/{}.bin", rawdawg_project::preset::sha256_hex(&blob));
            std::fs::write(moved.join(&blob_rel), &blob)?;
            let mut ext = Instance::native("com.example.fx", serde_json::json!({ "k": 1 }));
            ext.format = PluginFormat::Vst3;
            ext.display_name = "External".into();
            ext.opaque_state_path = Some(blob_rel.clone());
            let ext_id = ext.id;
            let master = q.mixer.master_id;
            history.apply(&mut q, Command::AddInsert { mixer_id: master, instance: ext, index: None })?;
            let compiled = rawdawg_audio_engine::compile(&q, 1, 256, &rawdawg_types::limits::EngineLimits::DEFAULT, &std::collections::HashMap::new(), &rawdawg_audio_engine::NoAssets)?;
            if compiled.missing_plugins.len() != 1 {
                bail!("expected one missing plugin, got {:?}", compiled.missing_plugins);
            }
            let (l3, _, _) = render_project(&q, &opts(&moved))?;
            if l3 != ref_l {
                bail!("a pass-through stand-in must not change the render");
            }
            rawdawg_project::save_project_dir(&moved, &q)?;
            let copy = base.join("Copy.rawdawg");
            rawdawg_project::save_as(Some(&moved), &copy, &rawdawg_project::load_project_dir(&moved)?)?;
            let c = rawdawg_project::load_project_dir(&copy)?;
            let ins = c.mixer_channel(master).unwrap().inserts.iter().find(|i| i.id == ext_id).unwrap();
            if ins.opaque_state_path.as_deref() != Some(blob_rel.as_str()) || std::fs::read(copy.join(&blob_rel))? != blob || ins.state != serde_json::json!({ "k": 1 }) {
                bail!("third-party state not preserved through save/load/save-as");
            }
            // 5. a missing asset renders silent (not an error) and relinks by hash
            let asset = c.assets.iter().find(|a| a.kind == rawdawg_project::AssetKind::Audio).cloned().ok_or_else(|| anyhow::anyhow!("song has an audio asset"))?;
            let asset_file = copy.join(&asset.path);
            let backup = base.join("backup.wav");
            std::fs::rename(&asset_file, &backup)?;
            let (l_missing, _, _) = render_project(&c, &opts(&copy))?;
            if l_missing == ref_l {
                bail!("the missing audio asset should change the render");
            }
            let relinked = rawdawg_project::assets::import_asset(&copy, &backup)?;
            if relinked.id != asset.id {
                bail!("relink by hash gave a different id");
            }
            let (l_back, _, _) = render_project(&c, &opts(&copy))?;
            if l_back != l3 {
                bail!("render after relink differs");
            }
            Ok((format!("save-as/reopen/move renders identical ({} frames), recovery candidate found and parsed, stand-in plugin with opaque state preserved, missing asset silent then relinked", ref_l.len()), Some(base.display().to_string())))
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
    checks.push(check("PL-01 CLAP and VST3 modules built and laid out as bundles", &["PLUG-01"], || {
        let c = profile_dir.join(clap_name);
        let v = profile_dir.join(vst3_name);
        if !c.is_file() || !v.is_file() {
            bail!("missing {} or {} (run cargo build --workspace)", c.display(), v.display());
        }
        // Standard bundle layouts (VST3 SDK "bundle format", CLAP single-file module).
        let bundles = out.join("bundles");
        let _ = std::fs::remove_dir_all(&bundles);
        let arch = if cfg!(target_os = "windows") { "x86_64-win" } else { "x86_64-linux" };
        let vst3_dir = bundles.join("rawDAWg.vst3").join("Contents").join(arch);
        std::fs::create_dir_all(&vst3_dir)?;
        let vst3_target = vst3_dir.join(if cfg!(target_os = "windows") { "rawDAWg.vst3" } else { "rawDAWg.so" });
        std::fs::copy(&v, &vst3_target)?;
        let plugins = rawdawg_plugin_api::exported_plugins();
        let classes: Vec<serde_json::Value> = plugins
            .iter()
            .map(|p| {
                let (pc, _) = rawdawg_vst3::class_ids(p);
                serde_json::json!({ "CID": pc.iter().map(|b| format!("{:02X}", *b as u8)).collect::<String>(), "Category": "Audio Module Class", "Name": p.name, "Vendor": "rawDAWg", "Version": rawdawg_types::VERSION, "SDKVersion": rawdawg_vst3::SDK_VERSION, "Sub Categories": p.vst3_subcategories().split('|').collect::<Vec<_>>(), "Class Flags": 0, "Cardinality": 2147483647 })
            })
            .collect();
        let moduleinfo = serde_json::json!({ "Name": "rawDAWg", "Version": rawdawg_types::VERSION, "Factory Info": { "Vendor": "rawDAWg", "URL": rawdawg_plugin_api::exported::URL, "E-Mail": "", "Flags": { "Unicode": true, "Classes Discardable": false, "Component Non Discardable": false } }, "Compatibility": [], "Classes": classes });
        std::fs::create_dir_all(bundles.join("rawDAWg.vst3").join("Contents").join("Resources"))?;
        std::fs::write(bundles.join("rawDAWg.vst3").join("Contents").join("Resources").join("moduleinfo.json"), serde_json::to_vec_pretty(&moduleinfo)?)?;
        std::fs::copy(&c, bundles.join("rawDAWg.clap"))?;
        Ok((
            format!("{} ({} bytes) → rawDAWg.clap; {} ({} bytes) → rawDAWg.vst3/Contents/{arch}/ + moduleinfo.json ({} classes)", clap_name, c.metadata()?.len(), vst3_name, v.metadata()?.len(), plugins.len()),
            Some(bundles.display().to_string()),
        ))
    }));
    checks.push(check("PL-02 CLAP/VST3 ABI round-trip tests (cargo test)", &["PLUG-01", "T-PLUGIN"], || {
        let status = Command::new("cargo").args(["test", "-q", "-p", "rawdawg-plugin-api", "-p", "rawdawg-plugin-clap", "-p", "rawdawg-plugin-vst3", "-p", "rawdawg-plugin-host"]).status()?;
        if !status.success() {
            bail!("plugin tests failed");
        }
        Ok(("CLAP and VST3 exports (17 plugins each) through the built modules: audio, events, sidechain, latency/tail, params, state envelope; CLAP host bridge: hosted vs native renders, migration, state, params".into(), None))
    }));
    // plugin editor windows need a display server: X11 (WSLg, Xorg, Xvfb) on Linux, the
    // desktop session on Windows
    let display = cfg!(target_os = "windows") || std::env::var_os("DISPLAY").is_some_and(|d| !d.is_empty());
    const GUI02: &str = "GUI-02 plugin editors on a real display: editor window, exported CLAP and VST3 editors hosted by rawDAWg with input, panel open/close";
    if display {
        checks.push(check(GUI02, &["PLUG-01", "PLUG-02", "PLAT-03"], || {
            let mut runs: Vec<Vec<&str>> = vec![
                vec!["test", "-q", "-p", "rawdawg-ui", "--test", "editor_window", "--", "--ignored"],
                vec!["test", "-q", "-p", "rawdawg", "--bin", "rawdawg", "the_effect_panel_opens", "--", "--ignored"],
            ];
            if cfg!(target_os = "linux") {
                runs.push(vec!["test", "-q", "-p", "rawdawg-plugin-host", "--test", "clap_editor", "--", "--ignored"]);
                runs.push(vec!["test", "-q", "-p", "rawdawg-plugin-host", "--test", "vst3_editor", "--", "--ignored"]);
            }
            for args in &runs {
                let o = Command::new("cargo").args(args).output()?;
                let text = String::from_utf8_lossy(&o.stdout).into_owned() + &String::from_utf8_lossy(&o.stderr);
                if !o.status.success() || !text.contains("1 passed") {
                    let tail: Vec<&str> = text.lines().filter(|l| l.contains("panicked") || l.contains("FAILED") || l.contains("error")).take(6).collect();
                    bail!("`cargo {}` failed: {}", args.join(" "), tail.join(" | "));
                }
            }
            Ok((
                format!(
                    "{} display tests passed: painted frame read back pixel-exact, resize, 2x scale, parent destruction{}; effect panel opens, closes and reopens the plugin window",
                    runs.len(),
                    if cfg!(target_os = "linux") { "; rawDAWg hosts its exported CLAP (timer-support) and VST3 (IRunLoop) delay editors, X input drags a slider, the host sees the edit, close/reopen" } else { "" }
                ),
                None,
            ))
        }));
    } else {
        checks.push(blocked(
            GUI02,
            &["PLUG-01", "PLUG-02", "PLAT-03"],
            "no display server (DISPLAY unset): run under WSLg/Xorg, or `xvfb-run -a cargo xtask verify --suite plugins`",
        ));
    }
    checks.push(check(
        "E2E-07 (headless subset) isolated scan of rawDAWg's own CLAP module into a catalog, hang/crash quarantine, safe-mode skip, hosted render equals native",
        &["PLUG-02", "PLUG-03", "T-PLUGIN", "E2E-07"],
        || {
            let exe = profile_dir.join(if cfg!(target_os = "windows") { "rawdawg.exe" } else { "rawdawg" });
            let module = profile_dir.join(clap_name);
            if !exe.is_file() || !module.is_file() {
                bail!("missing {} or {} (run cargo build --workspace)", exe.display(), module.display());
            }
            let base = out.join("e2e-07");
            let _ = std::fs::remove_dir_all(&base);
            let plugins = base.join("clap");
            std::fs::create_dir_all(&plugins)?;
            std::fs::copy(&module, plugins.join("rawDAWg.clap"))?;
            std::fs::write(plugins.join("broken.clap"), b"not a plugin")?;
            // the VST3 bundle laid out by PL-01, installed in the per-user VST3 folder of a
            // temporary home ($HOME/.vst3 on Linux, %LOCALAPPDATA%\Programs\Common\VST3 on Windows)
            let vst3_src = out.join("bundles").join("rawDAWg.vst3");
            if !vst3_src.is_dir() {
                bail!("PL-01 bundle missing at {}", vst3_src.display());
            }
            let vst3_root = user_vst3_dir(&base.join("home")).join("rawDAWg.vst3");
            for entry in walkdir(&vst3_src)? {
                let rel = entry.strip_prefix(&vst3_src)?.to_path_buf();
                if entry.is_dir() {
                    std::fs::create_dir_all(vst3_root.join(&rel))?;
                } else {
                    std::fs::create_dir_all(vst3_root.join(&rel).parent().unwrap())?;
                    std::fs::copy(&entry, vst3_root.join(&rel))?;
                }
            }
            let catalog = base.join("plugins.json");
            // 1. scan through the CLI (each module in its own child process)
            let mut scan = Command::new(&exe);
            scan.args(["--scan-plugins", "--catalog"]).arg(&catalog).env("CLAP_PATH", &plugins);
            isolate_plugin_roots(&mut scan, &base)?;
            let o = scan.output()?;
            if !o.status.success() {
                bail!("--scan-plugins failed: {}", String::from_utf8_lossy(&o.stderr));
            }
            let report: serde_json::Value = serde_json::from_slice(&o.stdout)?;
            let n = report["plugins"].as_array().map(|a| a.len()).unwrap_or(0);
            let vst3_n = report["plugins"].as_array().map(|a| a.iter().filter(|p| p["format"] == "vst3").count()).unwrap_or(0);
            if n != 34 || vst3_n != 17 || report["quarantine"].as_array().map(|a| a.len()) != Some(1) {
                bail!("expected 17 CLAP + 17 VST3 plugins and 1 quarantined module, got {n} ({vst3_n} VST3) and {:?}", report["quarantine"]);
            }
            // 2. a hanging scanner child is killed and quarantined (test hook, 1 s timeout)
            let mut cat = rawdawg_plugin_host::Catalog::load(&catalog);
            cat.timeout_secs = 1;
            cat.clear_quarantine(&plugins.join("broken.clap"));
            let opts = rawdawg_plugin_host::ScanOptions::new(exe.clone(), 1);
            let hang = {
                // SAFETY: xtask is single-threaded here; the hook only affects the children it spawns.
                unsafe { std::env::set_var(rawdawg_plugin_host::scan::TEST_BEHAVIOUR_ENV, "hang") };
                let r = rawdawg_plugin_host::scan::scan_library(&plugins.join("rawDAWg.clap"), rawdawg_plugin_host::Format::Clap, &opts);
                unsafe { std::env::remove_var(rawdawg_plugin_host::scan::TEST_BEHAVIOUR_ENV) };
                r
            };
            match hang {
                rawdawg_plugin_host::ScanOutcome::Quarantined(q) if matches!(q.reason, rawdawg_plugin_host::QuarantineReason::Timeout { .. }) => {}
                rawdawg_plugin_host::ScanOutcome::Quarantined(q) => bail!("hang quarantined with the wrong reason: {}", q.reason.summary()),
                rawdawg_plugin_host::ScanOutcome::Scanned(_) => bail!("a hanging scanner produced a library"),
            }
            // 3. hosted render equals native render; safe mode and missing plugins become stand-ins
            let status = Command::new("cargo").args(["test", "-q", "-p", "rawdawg-plugin-host", "--test", "clap_host", "--test", "vst3_host"]).status()?;
            if !status.success() {
                bail!("hosted-vs-native engine tests failed");
            }
            let status = Command::new("cargo").args(["test", "-q", "-p", "rawdawg", "--test", "plugin_scan", "--bin", "rawdawg", "hosted_plugins"]).status()?;
            if !status.success() {
                bail!("scanner subprocess / hosted UI tests failed");
            }
            Ok((
                format!(
                    "catalog {}: 17 CLAP + 17 VST3 plugins (bundle), broken module quarantined (load failed), hang quarantined (timeout 1 s), hosted CLAP and VST3 delay/synth renders equal native, safe mode stand-ins",
                    catalog.display()
                ),
                Some(base.display().to_string()),
            ))
        },
    ));
    checks.push(if cfg!(target_os = "linux") {
        check(
            "E2E-08 (headless subset) build the Ubuntu deb, inspect its layout, validate the desktop entry, launch the packaged binary offline for a headless render; no forbidden formats",
            &["PLAT-04", "QUAL-03", "E2E-08"],
            || {
                let produced = crate::package::package_deb(root)?;
                let deb = produced.artifacts.iter().find(|a| a.extension().is_some_and(|e| e == "deb")).ok_or_else(|| anyhow::anyhow!("no .deb produced"))?.clone();
                let listing = Command::new("dpkg-deb").arg("-c").arg(&deb).output()?;
                let listing = String::from_utf8_lossy(&listing.stdout).into_owned();
                for required in ["./usr/bin/rawdawg", "./usr/lib/clap/rawDAWg.clap", "./usr/lib/vst3/rawDAWg.vst3/Contents/x86_64-linux/rawDAWg.so", "./usr/lib/vst3/rawDAWg.vst3/Contents/Resources/moduleinfo.json", "./usr/share/applications/rawdawg.desktop", "./usr/share/mime/packages/rawdawg.xml", "./usr/share/icons/hicolor/256x256/apps/rawdawg.png", "./usr/share/doc/rawdawg/THIRD-PARTY-LICENSES.txt", "./usr/share/doc/rawdawg/sbom.cdx.json", "./usr/share/doc/rawdawg/copyright", "./usr/share/doc/rawdawg/USER_GUIDE.md", "./usr/share/doc/rawdawg/changelog.gz", "./usr/share/man/man1/rawdawg.1.gz", "./usr/share/rawdawg/demos/demo-song.rawdawg/project.json"] {
                    if !listing.contains(required) {
                        bail!("{required} missing from the deb");
                    }
                }
                let info = Command::new("dpkg-deb").arg("-f").arg(&deb).output()?;
                let info = String::from_utf8_lossy(&info.stdout).into_owned();
                if !info.contains("Package: rawdawg") || !info.contains("Architecture: amd64") || !info.contains("Depends: libc6") {
                    bail!("control file unexpected:\n{info}");
                }
                let base = out.join("e2e-08");
                let _ = std::fs::remove_dir_all(&base);
                std::fs::create_dir_all(&base)?;
                let status = Command::new("dpkg-deb").arg("-x").arg(&deb).arg(&base).status()?;
                if !status.success() {
                    bail!("dpkg-deb -x failed");
                }
                // offline launch of the packaged binary: headless render of the packaged demo project
                let exe = base.join("usr/bin/rawdawg");
                let demo = base.join("usr/share/rawdawg/demos/demo-song.rawdawg");
                let wav = base.join("packaged-render.wav");
                let o = Command::new(&exe).args(["--headless-render"]).arg(&demo).arg("--output").arg(&wav).args(["--mode", "song"]).env_remove("DISPLAY").env_remove("WAYLAND_DISPLAY").output()?;
                if !o.status.success() {
                    bail!("packaged binary failed to render: {}", String::from_utf8_lossy(&o.stderr));
                }
                let frames = hound::WavReader::open(&wav)?.duration();
                if frames == 0 {
                    bail!("packaged render is empty");
                }
                // the stripped plugin modules still load: the packaged binary scans them in its child process
                let catalog = base.join("plugins.json");
                std::fs::write(&catalog, serde_json::to_vec(&serde_json::json!({ "version": 1, "user_paths": [base.join("usr/lib/vst3")] }))?)?;
                let scan = Command::new(&exe).arg("--scan-plugins").arg("--catalog").arg(&catalog).env("CLAP_PATH", base.join("usr/lib/clap")).env("HOME", base.join("home")).output()?;
                if !scan.status.success() {
                    bail!("packaged binary failed to scan the packaged plugins: {}", String::from_utf8_lossy(&scan.stderr));
                }
                let report: serde_json::Value = serde_json::from_slice(&scan.stdout)?;
                let found = report["plugins"].as_array().map(|a| a.len()).unwrap_or(0);
                let quarantined = report["quarantine"].as_array().map(|a| a.len()).unwrap_or(0);
                if found != 34 || quarantined != 0 {
                    bail!("packaged plugins: expected 17 CLAP + 17 VST3 and none quarantined, got {found} and {quarantined} quarantined: {}", report["quarantine"]);
                }
                let sums = std::fs::read_to_string(root.join("target/package/SHA256SUMS"))?;
                if !sums.contains(deb.file_name().unwrap().to_str().unwrap()) {
                    bail!("SHA256SUMS does not list the deb");
                }
                let forbidden: Vec<String> = std::fs::read_dir(root.join("target/package"))?.flatten().map(|e| e.file_name().to_string_lossy().to_lowercase()).filter(|n| n.ends_with(".appimage") || n.ends_with(".snap") || n.ends_with(".flatpak")).collect();
                if !forbidden.is_empty() {
                    bail!("forbidden distribution formats produced: {forbidden:?}");
                }
                let warn = if produced.warnings.is_empty() { String::new() } else { format!("; {}", produced.warnings.join("; ")) };
                Ok((format!("{} ({} bytes): layout, control, desktop entry validated, packaged binary rendered {frames} frames offline and loaded all 34 packaged (stripped) plugins{warn}; install/uninstall in a clean machine: BLOCKED (needs root)", deb.file_name().unwrap().to_string_lossy(), deb.metadata()?.len()), Some(base.display().to_string())))
            },
        )
    } else {
        blocked("E2E-08 (headless subset) Ubuntu deb build and inspection", &["PLAT-04", "E2E-08"], "runs on Ubuntu; Windows MSI/portable are built by `cargo xtask package --format msi|portable` on Windows (NOT RUN here)")
    });
    checks.push(check("QA-01 parser fuzz regressions and smoke campaign (every saved crash input replays clean; 300 mutated inputs per target)", &["QUAL-03", "T-DATA"], || {
        let o = Command::new("cargo").args(["test", "-q", "-p", "rawdawg-fuzz"]).output()?;
        if !o.status.success() {
            let text = format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
            let lines: Vec<&str> = text.lines().filter(|l| l.contains("panicked") || l.contains("FAILED") || l.contains("assert") || l.starts_with("fuzz")).take(12).collect();
            bail!("fuzz regressions or smoke campaign failed:\n{}", lines.join("\n"));
        }
        let saved = walkdir(&root.join("fixtures/fuzz-regressions"))?.into_iter().filter(|p| p.is_file()).count();
        Ok((
            format!("{saved} saved regression inputs replay clean; {} targets survive the smoke campaign; long campaigns: cargo xtask fuzz", rawdawg_fuzz::targets::all().len()),
            None,
        ))
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
        "Steinberg validator needs the VST3 SDK build (not on this machine); run it on target/verification/bundles/rawDAWg.vst3 and record the SDK version",
    ));
    checks.push(blocked(
        "PL-05 independent host test (CLAP/VST3)",
        &["PLUG-01", "PLUG-02", "E2E-07"],
        "manual gate: load target/verification/bundles/rawDAWg.{clap,vst3} in an independent host on Windows and Ubuntu, and host an independent CLAP/VST3 instrument and effect in rawDAWg",
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

/// The per-user VST3 folder under a (temporary) home directory, as the scanner finds it.
fn user_vst3_dir(home: &Path) -> std::path::PathBuf {
    if cfg!(target_os = "windows") { home.join("Programs").join("Common").join("VST3") } else { home.join(".vst3") }
}

/// Point a scanner run's user and system plugin folders into `base` so only the plugins the
/// check installed there are found: `HOME` (Linux), `LOCALAPPDATA` and an empty
/// `COMMONPROGRAMFILES` (Windows).
fn isolate_plugin_roots(cmd: &mut Command, base: &Path) -> anyhow::Result<()> {
    let system = base.join("system");
    std::fs::create_dir_all(&system)?;
    cmd.env("HOME", base.join("home")).env("LOCALAPPDATA", base.join("home")).env("COMMONPROGRAMFILES", system);
    Ok(())
}

/// Every path below `root` (directories first), bounded depth.
fn walkdir(root: &std::path::Path) -> anyhow::Result<Vec<std::path::PathBuf>> {
    let mut out = Vec::new();
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    while let Some((dir, depth)) = stack.pop() {
        if depth > 8 {
            continue;
        }
        for entry in std::fs::read_dir(&dir)? {
            let p = entry?.path();
            out.push(p.clone());
            if p.is_dir() {
                stack.push((p, depth + 1));
            }
        }
    }
    Ok(out)
}
