//! Structural validation of a loaded or edited project.

use crate::model::*;
use rawdawg_types::units::{Meter, PPQ, UsPerQuarter};
use std::collections::HashSet;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ValidationError {
    #[error("unsupported schema version {found} (this build reads up to {supported})")]
    UnsupportedSchema { found: u32, supported: u32 },
    #[error("ppq must be {expected}, found {found}")]
    BadPpq { expected: i64, found: i64 },
    #[error("sample rate {0} Hz is not a supported project rate")]
    BadSampleRate(u32),
    #[error("tempo map invalid: {0}")]
    Tempo(String),
    #[error("meter map invalid: {0}")]
    Meter(String),
    #[error("duplicate id {0}")]
    DuplicateId(String),
    #[error("{what} references missing {target}")]
    MissingReference { what: String, target: String },
    #[error("invalid value in {0}")]
    InvalidValue(String),
    #[error("mixer master id does not point at a mixer channel")]
    MissingMaster,
    #[error("mixer routing cycle through {0}")]
    MixerCycle(String),
    #[error("resource limit exceeded: {0}")]
    LimitExceeded(String),
    #[error("asset path is not a safe relative path: {0}")]
    UnsafePath(String),
}

/// Default caps enforced before any allocation driven by the file.
/// Largest timeline position or length accepted in a project: 2^40 ticks (at PPQ 960 and
/// 120 BPM about 18 years). Bounding every tick-valued field keeps sums such as
/// `tick + duration` and tick→frame conversions far from i64 overflow.
pub const MAX_TICK: i64 = 1 << 40;
/// Largest audio clip source offset/length/fade in frames (about 6 days at 192 kHz).
pub const MAX_CLIP_FRAMES: u64 = 1 << 37;

fn tick_ok(t: rawdawg_types::units::Tick) -> bool {
    (0..=MAX_TICK).contains(&t.0)
}

pub struct Limits {
    pub max_channels: usize,
    pub max_patterns: usize,
    pub max_notes_per_lane: usize,
    pub max_clips: usize,
    pub max_assets: usize,
    pub max_mixer_channels: usize,
    pub max_inserts: usize,
    pub max_automation_points: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_channels: 512,
            max_patterns: 4096,
            max_notes_per_lane: 200_000,
            max_clips: 100_000,
            max_assets: 10_000,
            max_mixer_channels: 128,
            max_inserts: 32,
            max_automation_points: 1_000_000,
        }
    }
}

fn dup<T: std::fmt::Display + std::hash::Hash + Eq + Clone>(seen: &mut HashSet<T>, id: T) -> Result<(), ValidationError> {
    if !seen.insert(id.clone()) {
        return Err(ValidationError::DuplicateId(id.to_string()));
    }
    Ok(())
}

/// Validate a relative portable asset path (no traversal, no absolute, `/` only).
pub fn is_safe_relative_path(p: &str) -> bool {
    !p.is_empty()
        && !p.starts_with('/')
        && !p.contains('\\')
        && !p.contains(':')
        && !p.contains("//")
        && p.split('/').all(|seg| !seg.is_empty() && seg != "." && seg != ".." && !seg.chars().any(|c| c.is_control()))
}

pub fn validate(p: &Project) -> Result<(), ValidationError> {
    validate_with(p, &Limits::default())
}

pub fn validate_with(p: &Project, lim: &Limits) -> Result<(), ValidationError> {
    if p.schema_version == 0 || p.schema_version > crate::SCHEMA_VERSION {
        return Err(ValidationError::UnsupportedSchema { found: p.schema_version, supported: crate::SCHEMA_VERSION });
    }
    if p.ppq != PPQ {
        return Err(ValidationError::BadPpq { expected: PPQ, found: p.ppq });
    }
    if !p.sample_rate_hz.is_supported_project_rate() {
        return Err(ValidationError::BadSampleRate(p.sample_rate_hz.0));
    }
    if p.channels.len() > lim.max_channels {
        return Err(ValidationError::LimitExceeded("channels".into()));
    }
    if p.patterns.len() > lim.max_patterns {
        return Err(ValidationError::LimitExceeded("patterns".into()));
    }
    if p.clips.len() > lim.max_clips {
        return Err(ValidationError::LimitExceeded("clips".into()));
    }
    if p.assets.len() > lim.max_assets {
        return Err(ValidationError::LimitExceeded("assets".into()));
    }
    if p.mixer.channels.len() > lim.max_mixer_channels {
        return Err(ValidationError::LimitExceeded("mixer channels".into()));
    }
    for t in &p.tempo {
        if !tick_ok(t.tick) {
            return Err(ValidationError::Tempo(format!("tempo point at tick {} out of range", t.tick.0)));
        }
        if !t.us_per_quarter.is_valid() {
            return Err(ValidationError::Tempo(format!("tempo {} us/qn out of range", t.us_per_quarter.0)));
        }
    }
    p.tempo_map().map_err(|e| ValidationError::Tempo(e.to_string()))?;
    for m in &p.meter {
        if !tick_ok(m.tick) {
            return Err(ValidationError::Meter(format!("meter point at tick {} out of range", m.tick.0)));
        }
        if !m.meter().is_valid() {
            return Err(ValidationError::Meter(format!("{}/{}", m.numerator, m.denominator)));
        }
    }
    p.meter_map().map_err(|e| ValidationError::Meter(e.to_string()))?;
    let _ = UsPerQuarter::DEFAULT;
    let _ = Meter::FOUR_FOUR;

    // Assets
    let mut asset_ids = HashSet::new();
    for a in &p.assets {
        if !a.id.is_valid() {
            return Err(ValidationError::InvalidValue(format!("asset id {}", a.id)));
        }
        dup(&mut asset_ids, a.id.clone())?;
        if !is_safe_relative_path(&a.path) {
            return Err(ValidationError::UnsafePath(a.path.clone()));
        }
    }

    // Mixer
    let mut mixer_ids = HashSet::new();
    let mut instance_ids = HashSet::new();
    for m in &p.mixer.channels {
        dup(&mut mixer_ids, m.id)?;
        if !m.gain_db.is_valid() || !m.pan.is_valid() {
            return Err(ValidationError::InvalidValue(format!("mixer channel {}", m.name)));
        }
        if m.inserts.len() > lim.max_inserts {
            return Err(ValidationError::LimitExceeded(format!("inserts on {}", m.name)));
        }
        for ins in &m.inserts {
            dup(&mut instance_ids, ins.id)?;
            validate_instance(ins)?;
        }
        for s in &m.sends {
            if !s.level_db.is_valid() {
                return Err(ValidationError::InvalidValue(format!("send level on {}", m.name)));
            }
        }
    }
    if !mixer_ids.contains(&p.mixer.master_id) {
        return Err(ValidationError::MissingMaster);
    }
    for m in &p.mixer.channels {
        if let Some(out) = m.output_id
            && !mixer_ids.contains(&out)
        {
            return Err(ValidationError::MissingReference { what: format!("mixer channel {}", m.name), target: out.to_string() });
        }
        for s in &m.sends {
            if !mixer_ids.contains(&s.target_id) {
                return Err(ValidationError::MissingReference { what: format!("send from {}", m.name), target: s.target_id.to_string() });
            }
        }
    }
    check_mixer_cycles(p)?;

    // Channels
    let mut channel_ids = HashSet::new();
    for c in &p.channels {
        dup(&mut channel_ids, c.id)?;
        dup(&mut instance_ids, c.instrument.id)?;
        validate_instance(&c.instrument)?;
        if !c.gain_db.is_valid() || !c.pan.is_valid() {
            return Err(ValidationError::InvalidValue(format!("channel {}", c.name)));
        }
        if !mixer_ids.contains(&c.mixer_id) {
            return Err(ValidationError::MissingReference { what: format!("channel {}", c.name), target: c.mixer_id.to_string() });
        }
    }

    // Patterns
    let mut pattern_ids = HashSet::new();
    let mut note_ids = HashSet::new();
    for pat in &p.patterns {
        dup(&mut pattern_ids, pat.id)?;
        if pat.length_ticks.0 <= 0 || !tick_ok(pat.length_ticks) || pat.steps == 0 || pat.step_ticks.0 <= 0 || !tick_ok(pat.step_ticks) {
            return Err(ValidationError::InvalidValue(format!("pattern {}", pat.name)));
        }
        let mut lane_channels = HashSet::new();
        for lane in &pat.lanes {
            if !channel_ids.contains(&lane.channel_id) {
                return Err(ValidationError::MissingReference { what: format!("lane in pattern {}", pat.name), target: lane.channel_id.to_string() });
            }
            if !lane_channels.insert(lane.channel_id) {
                return Err(ValidationError::DuplicateId(format!("lane for channel {} in pattern {}", lane.channel_id, pat.name)));
            }
            if lane.notes.len() > lim.max_notes_per_lane {
                return Err(ValidationError::LimitExceeded("notes".into()));
            }
            for n in &lane.notes {
                dup(&mut note_ids, n.id)?;
                if !tick_ok(n.tick)
                    || n.duration_ticks.0 <= 0
                    || !tick_ok(n.duration_ticks)
                    || n.pitch > 127
                    || !n.velocity.is_finite()
                    || n.velocity <= 0.0
                    || n.velocity > 1.0
                    || n.channel > 15
                {
                    return Err(ValidationError::InvalidValue(format!("note {} in pattern {}", n.id, pat.name)));
                }
                if n.end_tick().0 > pat.length_ticks.0 {
                    return Err(ValidationError::InvalidValue(format!("note {} extends past pattern {} end", n.id, pat.name)));
                }
            }
            for c in &lane.controllers {
                if !tick_ok(c.tick) || c.channel > 15 {
                    return Err(ValidationError::InvalidValue(format!("controller event in pattern {}", pat.name)));
                }
                let ok = match c.kind {
                    ControllerKind::Cc { cc, value } => cc <= 127 && value.is_finite() && (0.0..=1.0).contains(&value),
                    ControllerKind::PitchBend { value } => value.is_finite() && (-1.0..=1.0).contains(&value),
                    ControllerKind::ChannelPressure { value } => value.is_finite() && (0.0..=1.0).contains(&value),
                };
                if !ok {
                    return Err(ValidationError::InvalidValue(format!("controller event in pattern {}", pat.name)));
                }
            }
        }
    }
    if let Some(sel) = p.selected_pattern
        && !pattern_ids.contains(&sel)
    {
        return Err(ValidationError::MissingReference { what: "selected_pattern".into(), target: sel.to_string() });
    }

    // Tracks and clips
    let mut track_ids = HashSet::new();
    for t in &p.tracks {
        dup(&mut track_ids, t.id)?;
        if let Some(m) = t.mixer_id
            && !p.mixer.channels.iter().any(|c| c.id == m)
        {
            return Err(ValidationError::MissingReference { what: format!("track {}", t.id), target: m.to_string() });
        }
    }
    let mut curve_ids = HashSet::new();
    let mut point_ids = HashSet::new();
    let mut total_points = 0usize;
    for c in &p.automation {
        dup(&mut curve_ids, c.id)?;
        total_points += c.points.len();
        if total_points > lim.max_automation_points {
            return Err(ValidationError::LimitExceeded("automation points".into()));
        }
        let mut last = i64::MIN;
        for pt in &c.points {
            dup(&mut point_ids, pt.id)?;
            if !tick_ok(pt.tick) || pt.tick.0 < last || !pt.value.is_finite() || !(0.0..=1.0).contains(&pt.value) {
                return Err(ValidationError::InvalidValue(format!("automation curve {}", c.name)));
            }
            last = pt.tick.0;
        }
        if !c.disabled && !instance_ids.contains(&c.target.instance_id) {
            return Err(ValidationError::MissingReference { what: format!("automation curve {}", c.name), target: c.target.instance_id.to_string() });
        }
    }
    let mut clip_ids = HashSet::new();
    for c in &p.clips {
        dup(&mut clip_ids, c.id)?;
        if !track_ids.contains(&c.track_id) {
            return Err(ValidationError::MissingReference { what: format!("clip {}", c.id), target: c.track_id.to_string() });
        }
        if !tick_ok(c.start_tick) || c.length_ticks.0 <= 0 || !tick_ok(c.length_ticks) {
            return Err(ValidationError::InvalidValue(format!("clip {}", c.id)));
        }
        match &c.content {
            ClipContent::Pattern { pattern_id, source_offset_ticks, .. } => {
                if !pattern_ids.contains(pattern_id) {
                    return Err(ValidationError::MissingReference { what: format!("clip {}", c.id), target: pattern_id.to_string() });
                }
                if !tick_ok(*source_offset_ticks) {
                    return Err(ValidationError::InvalidValue(format!("clip {} offset", c.id)));
                }
            }
            ClipContent::Audio { asset_id, source_offset_frames, source_length_frames, gain_db, playback_rate, fade_in_frames, fade_out_frames } => {
                if [*source_offset_frames, *source_length_frames, *fade_in_frames, *fade_out_frames].iter().any(|f| *f > MAX_CLIP_FRAMES) {
                    return Err(ValidationError::InvalidValue(format!("audio clip {} frame range", c.id)));
                }
                // Missing assets are allowed (placeholder) but the id must be well-formed.
                if !asset_id.is_valid() {
                    return Err(ValidationError::InvalidValue(format!("clip {} asset id", c.id)));
                }
                if *source_length_frames == 0 || !gain_db.is_valid() || !playback_rate.is_finite() || *playback_rate <= 0.0 || *playback_rate > 16.0 {
                    return Err(ValidationError::InvalidValue(format!("audio clip {}", c.id)));
                }
            }
            ClipContent::Automation { curve_id, .. } => {
                if !curve_ids.contains(curve_id) {
                    return Err(ValidationError::MissingReference { what: format!("clip {}", c.id), target: curve_id.to_string() });
                }
            }
        }
    }
    if let Some(l) = &p.r#loop
        && (!tick_ok(l.start_tick) || !tick_ok(l.end_tick) || l.end_tick <= l.start_tick)
    {
        return Err(ValidationError::InvalidValue("loop range".into()));
    }
    let mut mapping_ids = HashSet::new();
    for m in &p.midi_mappings {
        dup(&mut mapping_ids, m.id)?;
        if m.cc > 127 || m.channel.is_some_and(|c| c > 15) || !m.min.is_finite() || !m.max.is_finite() {
            return Err(ValidationError::InvalidValue(format!("midi mapping {}", m.id)));
        }
    }
    Ok(())
}

fn validate_instance(i: &Instance) -> Result<(), ValidationError> {
    if i.type_id.as_str().is_empty() || i.state_version == 0 || !i.mix.is_finite() || !(0.0..=1.0).contains(&i.mix) {
        return Err(ValidationError::InvalidValue(format!("instance {}", i.id)));
    }
    if let Some(p) = &i.opaque_state_path
        && !is_safe_relative_path(p)
    {
        return Err(ValidationError::UnsafePath(p.clone()));
    }
    Ok(())
}

/// Reject cycles in main output routing (sends are also instantaneous edges).
fn check_mixer_cycles(p: &Project) -> Result<(), ValidationError> {
    use std::collections::HashMap;
    let idx: HashMap<_, _> = p.mixer.channels.iter().enumerate().map(|(i, c)| (c.id, i)).collect();
    let n = p.mixer.channels.len();
    let mut state = vec![0u8; n]; // 0 = new, 1 = visiting, 2 = done
    fn visit(i: usize, p: &Project, idx: &HashMap<rawdawg_types::MixerChannelId, usize>, state: &mut [u8]) -> Result<(), ValidationError> {
        if state[i] == 1 {
            return Err(ValidationError::MixerCycle(p.mixer.channels[i].name.clone()));
        }
        if state[i] == 2 {
            return Ok(());
        }
        state[i] = 1;
        let ch = &p.mixer.channels[i];
        let mut targets: Vec<rawdawg_types::MixerChannelId> = ch.sends.iter().map(|s| s.target_id).collect();
        if let Some(o) = ch.output_id {
            targets.push(o);
        }
        for t in targets {
            if let Some(&j) = idx.get(&t) {
                visit(j, p, idx, state)?;
            }
        }
        state[i] = 2;
        Ok(())
    }
    for i in 0..n {
        visit(i, p, &idx, &mut state)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rawdawg_types::ids::*;
    use rawdawg_types::units::*;

    fn base() -> Project {
        Project::new_default("t", serde_json::json!({}))
    }

    #[test]
    fn default_project_validates() {
        validate(&base()).unwrap();
    }

    #[test]
    fn rejects_newer_schema_and_bad_ppq() {
        let mut p = base();
        p.schema_version = 2;
        assert!(matches!(validate(&p), Err(ValidationError::UnsupportedSchema { found: 2, .. })));
        let mut p = base();
        p.ppq = 480;
        assert!(matches!(validate(&p), Err(ValidationError::BadPpq { .. })));
        let mut p = base();
        p.sample_rate_hz = SampleRate(22_050);
        assert!(matches!(validate(&p), Err(ValidationError::BadSampleRate(22_050))));
    }

    #[test]
    fn rejects_duplicates_and_dangling_references() {
        let mut p = base();
        let c = p.channels[0].clone();
        p.channels.push(c);
        assert!(matches!(validate(&p), Err(ValidationError::DuplicateId(_))));
        let mut p = base();
        p.channels[0].mixer_id = MixerChannelId::new();
        assert!(matches!(validate(&p), Err(ValidationError::MissingReference { .. })));
        let mut p = base();
        p.mixer.master_id = MixerChannelId::new();
        assert_eq!(validate(&p), Err(ValidationError::MissingMaster));
        let mut p = base();
        p.selected_pattern = Some(PatternId::new());
        assert!(matches!(validate(&p), Err(ValidationError::MissingReference { .. })));
    }

    #[test]
    fn rejects_bad_notes() {
        let mut p = base();
        let pat = &mut p.patterns[0];
        pat.lanes[0].notes.push(Note { id: NoteId::new(), tick: Tick(0), duration_ticks: Tick(0), pitch: 60, velocity: 0.5, channel: 0 });
        assert!(matches!(validate(&p), Err(ValidationError::InvalidValue(_))));
        let mut p = base();
        p.patterns[0].lanes[0].notes.push(Note { id: NoteId::new(), tick: Tick(3800), duration_ticks: Tick(100), pitch: 60, velocity: 0.5, channel: 0 });
        assert!(matches!(validate(&p), Err(ValidationError::InvalidValue(_))));
        let mut p = base();
        p.patterns[0].lanes[0].notes.push(Note { id: NoteId::new(), tick: Tick(0), duration_ticks: Tick(100), pitch: 60, velocity: 0.0, channel: 0 });
        assert!(validate(&p).is_err());
        let mut p = base();
        let ch = p.channels[0].id;
        p.patterns[0].lanes.push(Lane { channel_id: ch, notes: vec![], controllers: vec![] });
        assert!(matches!(validate(&p), Err(ValidationError::DuplicateId(_))));
    }

    #[test]
    fn rejects_mixer_cycles_and_unsafe_paths() {
        let mut p = base();
        let master = p.mixer.master_id;
        let insert = p.mixer.channels[1].id;
        p.mixer_channel_mut(master).unwrap().sends.push(Send { target_id: insert, level_db: Db(0.0), tap: SendTap::PostFader, sidechain: false });
        assert!(matches!(validate(&p), Err(ValidationError::MixerCycle(_))));
        let mut p = base();
        p.assets.push(Asset {
            id: AssetId::from_hex(&"a".repeat(64)).unwrap(),
            path: "../etc/passwd".into(),
            kind: AssetKind::Audio,
            source_name: "x".into(),
            original_rate_hz: None,
            channels: None,
            frames: None,
            bytes: 1,
        });
        assert!(matches!(validate(&p), Err(ValidationError::UnsafePath(_))));
        assert!(is_safe_relative_path("assets/sha256/abc.wav"));
        assert!(!is_safe_relative_path("/abs"));
        assert!(!is_safe_relative_path("a\\b"));
        assert!(!is_safe_relative_path("C:/x"));
        assert!(!is_safe_relative_path("a//b"));
        assert!(!is_safe_relative_path("a/./b"));
    }

    #[test]
    fn limits_are_enforced() {
        let mut p = base();
        let lim = Limits { max_channels: 1, ..Limits::default() };
        validate_with(&p, &lim).unwrap();
        let mut c = p.channels[0].clone();
        c.id = ChannelId::new();
        c.instrument.id = InstanceId::new();
        p.channels.push(c);
        assert!(matches!(validate_with(&p, &lim), Err(ValidationError::LimitExceeded(_))));
    }
}
