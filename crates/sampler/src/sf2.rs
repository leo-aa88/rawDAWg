//! SoundFont 2 (RIFF `sfbk`) parser. Reads INFO, sample data and the preset/
//! instrument/zone/generator hierarchy into a validated in-memory bank.
//! Supported generators are listed in [`SUPPORTED_GENERATORS`]; any other
//! generator or modulator found is reported, never silently approximated.

use std::collections::BTreeSet;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Sf2Error {
    #[error("not a RIFF/sfbk file")]
    NotSoundFont,
    #[error("truncated or malformed chunk: {0}")]
    Malformed(String),
    #[error("file too large ({0} bytes)")]
    TooLarge(u64),
    #[error("unsupported SoundFont version {0}.{1}")]
    Version(u16, u16),
}

/// Maximum accepted file size (bounded allocation).
pub const MAX_SF2_BYTES: u64 = 2 * 1024 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Gen {
    StartAddrsOffset = 0,
    EndAddrsOffset = 1,
    StartloopAddrsOffset = 2,
    EndloopAddrsOffset = 3,
    StartAddrsCoarseOffset = 4,
    EndAddrsCoarseOffset = 12,
    InitialFilterFc = 8,
    InitialFilterQ = 9,
    Pan = 17,
    DelayVolEnv = 33,
    AttackVolEnv = 34,
    HoldVolEnv = 35,
    DecayVolEnv = 36,
    SustainVolEnv = 37,
    ReleaseVolEnv = 38,
    KeynumToVolEnvHold = 39,
    KeynumToVolEnvDecay = 40,
    Instrument = 41,
    KeyRange = 43,
    VelRange = 44,
    StartloopAddrsCoarseOffset = 45,
    Keynum = 46,
    Velocity = 47,
    InitialAttenuation = 48,
    EndloopAddrsCoarseOffset = 50,
    CoarseTune = 51,
    FineTune = 52,
    SampleId = 53,
    SampleModes = 54,
    ScaleTuning = 56,
    ExclusiveClass = 57,
    OverridingRootKey = 58,
}

/// Generators this player honors.
pub const SUPPORTED_GENERATORS: &[u16] = &[0, 1, 2, 3, 4, 8, 9, 12, 17, 33, 34, 35, 36, 37, 38, 41, 43, 44, 45, 46, 47, 48, 50, 51, 52, 53, 54, 56, 57, 58];

pub fn generator_name(id: u16) -> &'static str {
    match id {
        0 => "startAddrsOffset",
        1 => "endAddrsOffset",
        2 => "startloopAddrsOffset",
        3 => "endloopAddrsOffset",
        4 => "startAddrsCoarseOffset",
        5 => "modLfoToPitch",
        6 => "vibLfoToPitch",
        7 => "modEnvToPitch",
        8 => "initialFilterFc",
        9 => "initialFilterQ",
        10 => "modLfoToFilterFc",
        11 => "modEnvToFilterFc",
        12 => "endAddrsCoarseOffset",
        13 => "modLfoToVolume",
        15 => "chorusEffectsSend",
        16 => "reverbEffectsSend",
        17 => "pan",
        21 => "delayModLFO",
        22 => "freqModLFO",
        23 => "delayVibLFO",
        24 => "freqVibLFO",
        25 => "delayModEnv",
        26 => "attackModEnv",
        27 => "holdModEnv",
        28 => "decayModEnv",
        29 => "sustainModEnv",
        30 => "releaseModEnv",
        31 => "keynumToModEnvHold",
        32 => "keynumToModEnvDecay",
        33 => "delayVolEnv",
        34 => "attackVolEnv",
        35 => "holdVolEnv",
        36 => "decayVolEnv",
        37 => "sustainVolEnv",
        38 => "releaseVolEnv",
        39 => "keynumToVolEnvHold",
        40 => "keynumToVolEnvDecay",
        41 => "instrument",
        43 => "keyRange",
        44 => "velRange",
        45 => "startloopAddrsCoarseOffset",
        46 => "keynum",
        47 => "velocity",
        48 => "initialAttenuation",
        50 => "endloopAddrsCoarseOffset",
        51 => "coarseTune",
        52 => "fineTune",
        53 => "sampleID",
        54 => "sampleModes",
        56 => "scaleTuning",
        57 => "exclusiveClass",
        58 => "overridingRootKey",
        _ => "unknown",
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct SampleHeader {
    pub name: String,
    pub start: u32,
    pub end: u32,
    pub loop_start: u32,
    pub loop_end: u32,
    pub sample_rate: u32,
    pub original_pitch: u8,
    pub pitch_correction: i8,
    pub sample_link: u16,
    pub sample_type: u16,
}

/// A zone: a bag of generator values (SF2 generator id → amount).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Zone {
    pub generators: Vec<(u16, i16)>,
    pub modulator_count: usize,
}

impl Zone {
    pub fn get(&self, g: Gen) -> Option<i16> {
        self.generators.iter().find(|(id, _)| *id == g as u16).map(|(_, v)| *v)
    }
    pub fn key_range(&self) -> (u8, u8) {
        match self.get(Gen::KeyRange) {
            Some(v) => ((v as u16 & 0xFF) as u8, ((v as u16) >> 8) as u8),
            None => (0, 127),
        }
    }
    pub fn vel_range(&self) -> (u8, u8) {
        match self.get(Gen::VelRange) {
            Some(v) => ((v as u16 & 0xFF) as u8, ((v as u16) >> 8) as u8),
            None => (0, 127),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Instrument {
    pub name: String,
    pub global: Option<Zone>,
    pub zones: Vec<Zone>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Preset {
    pub name: String,
    pub preset: u16,
    pub bank: u16,
    pub global: Option<Zone>,
    /// Zones referencing instruments (generator 41 present).
    pub zones: Vec<Zone>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SoundFont {
    pub name: String,
    pub version: (u16, u16),
    pub presets: Vec<Preset>,
    pub instruments: Vec<Instrument>,
    pub samples: Vec<SampleHeader>,
    /// 16-bit sample data converted to f32 (-1..1).
    pub sample_data: Vec<f32>,
    /// Generator ids seen in the file that this player does not implement.
    pub unsupported_generators: Vec<u16>,
    pub modulator_count: usize,
}

impl SoundFont {
    /// Human-readable warnings about features that will not sound as the author intended.
    pub fn warnings(&self) -> Vec<String> {
        let mut w = Vec::new();
        for g in &self.unsupported_generators {
            w.push(format!("generator {} ({}) is not supported and is ignored", g, generator_name(*g)));
        }
        if self.modulator_count > 0 {
            w.push(format!("{} modulator(s) present; only the default velocity→attenuation and key→pitch behaviour is applied", self.modulator_count));
        }
        if self.samples.iter().any(|s| s.sample_type & 0x8000 != 0) {
            w.push("ROM samples are referenced and will be silent".into());
        }
        w
    }
    pub fn find_preset(&self, bank: u16, preset: u16) -> Option<&Preset> {
        self.presets.iter().find(|p| p.bank == bank && p.preset == preset)
    }
}

fn u16_at(b: &[u8], i: usize) -> u16 {
    u16::from_le_bytes([b[i], b[i + 1]])
}
fn u32_at(b: &[u8], i: usize) -> u32 {
    u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]])
}
fn name_at(b: &[u8], i: usize, len: usize) -> String {
    let raw = &b[i..i + len];
    let end = raw.iter().position(|c| *c == 0).unwrap_or(len);
    String::from_utf8_lossy(&raw[..end]).trim().to_owned()
}

/// Walk RIFF sub-chunks of `data`, calling `f(id, body)`.
fn for_chunks(data: &[u8], mut f: impl FnMut(&[u8; 4], &[u8]) -> Result<(), Sf2Error>) -> Result<(), Sf2Error> {
    let mut pos = 0usize;
    while pos + 8 <= data.len() {
        let id: [u8; 4] = data[pos..pos + 4].try_into().expect("4 bytes");
        let size = u32_at(data, pos + 4) as usize;
        let body_start = pos + 8;
        let body_end = body_start.checked_add(size).ok_or_else(|| Sf2Error::Malformed("chunk size overflow".into()))?;
        if body_end > data.len() {
            return Err(Sf2Error::Malformed(format!("chunk {} exceeds file", String::from_utf8_lossy(&id))));
        }
        f(&id, &data[body_start..body_end])?;
        pos = body_end + (size & 1);
    }
    Ok(())
}

/// Parse an SF2 file from memory.
pub fn parse(data: &[u8]) -> Result<SoundFont, Sf2Error> {
    if data.len() as u64 > MAX_SF2_BYTES {
        return Err(Sf2Error::TooLarge(data.len() as u64));
    }
    if data.len() < 12 || &data[0..4] != b"RIFF" || &data[8..12] != b"sfbk" {
        return Err(Sf2Error::NotSoundFont);
    }
    let riff_size = u32_at(data, 4) as usize;
    let body_end = (8 + riff_size).min(data.len());
    let body = &data[12..body_end];
    let mut name = String::new();
    let mut version = (0u16, 0u16);
    let mut smpl: Vec<u8> = Vec::new();
    let mut phdr = Vec::new();
    let mut pbag = Vec::new();
    let mut pmod_count = 0usize;
    let mut pgen = Vec::new();
    let mut inst = Vec::new();
    let mut ibag = Vec::new();
    let mut imod_count = 0usize;
    let mut igen = Vec::new();
    let mut shdr = Vec::new();
    for_chunks(body, |id, chunk| {
        if id != b"LIST" || chunk.len() < 4 {
            return Ok(());
        }
        let kind: [u8; 4] = chunk[0..4].try_into().expect("4 bytes");
        let inner = &chunk[4..];
        match &kind {
            b"INFO" => for_chunks(inner, |cid, c| {
                match cid {
                    b"INAM" => name = name_at(c, 0, c.len()),
                    b"ifil" if c.len() >= 4 => version = (u16_at(c, 0), u16_at(c, 2)),
                    _ => {}
                }
                Ok(())
            }),
            b"sdta" => for_chunks(inner, |cid, c| {
                if cid == b"smpl" {
                    smpl = c.to_vec();
                }
                Ok(())
            }),
            b"pdta" => for_chunks(inner, |cid, c| {
                match cid {
                    b"phdr" => {
                        if !c.len().is_multiple_of(38) {
                            return Err(Sf2Error::Malformed("phdr size".into()));
                        }
                        for i in (0..c.len()).step_by(38) {
                            phdr.push((name_at(c, i, 20), u16_at(c, i + 20), u16_at(c, i + 22), u16_at(c, i + 24)));
                        }
                    }
                    b"pbag" | b"ibag" => {
                        if !c.len().is_multiple_of(4) {
                            return Err(Sf2Error::Malformed("bag size".into()));
                        }
                        let v: Vec<(u16, u16)> = (0..c.len()).step_by(4).map(|i| (u16_at(c, i), u16_at(c, i + 2))).collect();
                        if cid == b"pbag" {
                            pbag = v;
                        } else {
                            ibag = v;
                        }
                    }
                    // the last record of each modulator list is the terminal record
                    b"pmod" => pmod_count = (c.len() / 10).saturating_sub(1),
                    b"imod" => imod_count = (c.len() / 10).saturating_sub(1),
                    b"pgen" | b"igen" => {
                        if !c.len().is_multiple_of(4) {
                            return Err(Sf2Error::Malformed("gen size".into()));
                        }
                        let v: Vec<(u16, i16)> = (0..c.len()).step_by(4).map(|i| (u16_at(c, i), u16_at(c, i + 2) as i16)).collect();
                        if cid == b"pgen" {
                            pgen = v;
                        } else {
                            igen = v;
                        }
                    }
                    b"inst" => {
                        if !c.len().is_multiple_of(22) {
                            return Err(Sf2Error::Malformed("inst size".into()));
                        }
                        for i in (0..c.len()).step_by(22) {
                            inst.push((name_at(c, i, 20), u16_at(c, i + 20)));
                        }
                    }
                    b"shdr" => {
                        if !c.len().is_multiple_of(46) {
                            return Err(Sf2Error::Malformed("shdr size".into()));
                        }
                        for i in (0..c.len()).step_by(46) {
                            shdr.push(SampleHeader {
                                name: name_at(c, i, 20),
                                start: u32_at(c, i + 20),
                                end: u32_at(c, i + 24),
                                loop_start: u32_at(c, i + 28),
                                loop_end: u32_at(c, i + 32),
                                sample_rate: u32_at(c, i + 36),
                                original_pitch: c[i + 40],
                                pitch_correction: c[i + 41] as i8,
                                sample_link: u16_at(c, i + 42),
                                sample_type: u16_at(c, i + 44),
                            });
                        }
                    }
                    _ => {}
                }
                Ok(())
            }),
            _ => Ok(()),
        }
    })?;
    if version.0 != 2 {
        return Err(Sf2Error::Version(version.0, version.1));
    }
    if phdr.len() < 2 || inst.len() < 2 || shdr.is_empty() {
        return Err(Sf2Error::Malformed("missing preset, instrument or sample headers".into()));
    }
    let sample_data: Vec<f32> = smpl.as_chunks::<2>().0.iter().map(|c| f32::from(i16::from_le_bytes(*c)) / 32768.0).collect();
    // drop terminal records
    let samples: Vec<SampleHeader> = shdr[..shdr.len() - 1].iter().filter(|s| s.name != "EOS").cloned().collect();
    let mut unsupported = BTreeSet::new();
    let build_zones = |bags: &[(u16, u16)], gens: &[(u16, i16)], from: u16, to: u16| -> Result<Vec<Zone>, Sf2Error> {
        let mut zones = Vec::new();
        for b in from..to {
            let (gstart, _) = *bags.get(b as usize).ok_or_else(|| Sf2Error::Malformed("bag index".into()))?;
            let gend = bags.get(b as usize + 1).map(|x| x.0).unwrap_or(gens.len() as u16);
            if gstart > gend || gend as usize > gens.len() {
                return Err(Sf2Error::Malformed("generator index".into()));
            }
            zones.push(Zone { generators: gens[gstart as usize..gend as usize].to_vec(), modulator_count: 0 });
        }
        Ok(zones)
    };
    let mut instruments = Vec::new();
    for i in 0..inst.len() - 1 {
        let zones = build_zones(&ibag, &igen, inst[i].1, inst[i + 1].1)?;
        let (global, zones) = split_global(zones, Gen::SampleId as u16);
        for z in global.iter().chain(zones.iter()) {
            for (g, _) in &z.generators {
                if !SUPPORTED_GENERATORS.contains(g) {
                    unsupported.insert(*g);
                }
            }
        }
        instruments.push(Instrument { name: inst[i].0.clone(), global, zones });
    }
    let mut presets = Vec::new();
    for i in 0..phdr.len() - 1 {
        let zones = build_zones(&pbag, &pgen, phdr[i].3, phdr[i + 1].3)?;
        let (global, zones) = split_global(zones, Gen::Instrument as u16);
        for z in global.iter().chain(zones.iter()) {
            for (g, _) in &z.generators {
                if !SUPPORTED_GENERATORS.contains(g) {
                    unsupported.insert(*g);
                }
            }
            if let Some(v) = z.get(Gen::Instrument)
                && v as usize >= instruments.len()
            {
                return Err(Sf2Error::Malformed("preset references missing instrument".into()));
            }
        }
        presets.push(Preset { name: phdr[i].0.clone(), preset: phdr[i].1, bank: phdr[i].2, global, zones });
    }
    for inst in &instruments {
        for z in &inst.zones {
            if let Some(s) = z.get(Gen::SampleId)
                && s as usize >= samples.len()
            {
                return Err(Sf2Error::Malformed("zone references missing sample".into()));
            }
        }
    }
    for s in &samples {
        if s.end as usize > sample_data.len() || s.start > s.end {
            return Err(Sf2Error::Malformed(format!("sample {} bounds exceed data", s.name)));
        }
    }
    Ok(SoundFont { name, version, presets, instruments, samples, sample_data, unsupported_generators: unsupported.into_iter().collect(), modulator_count: pmod_count + imod_count })
}

/// The first zone without the terminal generator is the global zone.
fn split_global(mut zones: Vec<Zone>, terminal: u16) -> (Option<Zone>, Vec<Zone>) {
    if let Some(first) = zones.first()
        && !first.generators.iter().any(|(g, _)| *g == terminal)
    {
        let g = zones.remove(0);
        return (Some(g), zones);
    }
    (None, zones)
}

/// Build a minimal valid SF2 file in memory (fixtures/tests): one preset,
/// one instrument with the given zones, and mono 16-bit samples.
pub struct FixtureSample {
    pub name: &'static str,
    pub data: Vec<i16>,
    pub rate: u32,
    pub root: u8,
    pub loop_start: u32,
    pub loop_end: u32,
}

pub struct FixtureZone {
    pub sample: u16,
    pub key_lo: u8,
    pub key_hi: u8,
    pub vel_lo: u8,
    pub vel_hi: u8,
    pub loop_mode: i16,
    pub extra: Vec<(u16, i16)>,
}

pub fn build_fixture(name: &str, bank: u16, preset: u16, samples: &[FixtureSample], zones: &[FixtureZone]) -> Vec<u8> {
    fn chunk(id: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut v = Vec::with_capacity(body.len() + 9);
        v.extend_from_slice(id);
        v.extend_from_slice(&(body.len() as u32).to_le_bytes());
        v.extend_from_slice(body);
        if body.len() % 2 == 1 {
            v.push(0);
        }
        v
    }
    fn list(kind: &[u8; 4], children: &[Vec<u8>]) -> Vec<u8> {
        let mut body = kind.to_vec();
        for c in children {
            body.extend_from_slice(c);
        }
        chunk(b"LIST", &body)
    }
    fn fixed(s: &str, n: usize) -> Vec<u8> {
        let mut v = s.as_bytes().to_vec();
        v.resize(n, 0);
        v
    }
    let mut smpl = Vec::new();
    let mut shdr = Vec::new();
    let mut offset = 0u32;
    for s in samples {
        let start = offset;
        for v in &s.data {
            smpl.extend_from_slice(&v.to_le_bytes());
        }
        offset += s.data.len() as u32;
        let end = offset;
        for _ in 0..46 {
            smpl.extend_from_slice(&0i16.to_le_bytes());
        }
        offset += 46;
        shdr.extend(fixed(s.name, 20));
        shdr.extend_from_slice(&start.to_le_bytes());
        shdr.extend_from_slice(&end.to_le_bytes());
        shdr.extend_from_slice(&(start + s.loop_start).to_le_bytes());
        shdr.extend_from_slice(&(start + s.loop_end).to_le_bytes());
        shdr.extend_from_slice(&s.rate.to_le_bytes());
        shdr.push(s.root);
        shdr.push(0);
        shdr.extend_from_slice(&0u16.to_le_bytes());
        shdr.extend_from_slice(&1u16.to_le_bytes());
    }
    shdr.extend(fixed("EOS", 20));
    shdr.extend_from_slice(&[0u8; 26]);
    // preset: one zone → instrument 0
    let mut phdr = fixed(name, 20);
    phdr.extend_from_slice(&preset.to_le_bytes());
    phdr.extend_from_slice(&bank.to_le_bytes());
    phdr.extend_from_slice(&0u16.to_le_bytes());
    phdr.extend_from_slice(&[0u8; 12]);
    phdr.extend(fixed("EOP", 20));
    phdr.extend_from_slice(&0u16.to_le_bytes());
    phdr.extend_from_slice(&0u16.to_le_bytes());
    phdr.extend_from_slice(&1u16.to_le_bytes());
    phdr.extend_from_slice(&[0u8; 12]);
    let mut pbag = Vec::new();
    pbag.extend_from_slice(&0u16.to_le_bytes());
    pbag.extend_from_slice(&0u16.to_le_bytes());
    pbag.extend_from_slice(&1u16.to_le_bytes());
    pbag.extend_from_slice(&0u16.to_le_bytes());
    let mut pgen = Vec::new();
    pgen.extend_from_slice(&(Gen::Instrument as u16).to_le_bytes());
    pgen.extend_from_slice(&0u16.to_le_bytes());
    let pmod: Vec<u8> = vec![0u8; 10];
    let mut inst = fixed(name, 20);
    inst.extend_from_slice(&0u16.to_le_bytes());
    inst.extend(fixed("EOI", 20));
    inst.extend_from_slice(&(zones.len() as u16).to_le_bytes());
    let mut ibag = Vec::new();
    let mut igen = Vec::new();
    let mut gi = 0u16;
    for z in zones {
        ibag.extend_from_slice(&gi.to_le_bytes());
        ibag.extend_from_slice(&0u16.to_le_bytes());
        let mut gens: Vec<(u16, i16)> =
            vec![(Gen::KeyRange as u16, (z.key_lo as i16) | ((z.key_hi as i16) << 8)), (Gen::VelRange as u16, (z.vel_lo as i16) | ((z.vel_hi as i16) << 8))];
        gens.extend(z.extra.iter().cloned());
        gens.push((Gen::SampleModes as u16, z.loop_mode));
        gens.push((Gen::SampleId as u16, z.sample as i16));
        for (g, v) in gens {
            igen.extend_from_slice(&g.to_le_bytes());
            igen.extend_from_slice(&v.to_le_bytes());
            gi += 1;
        }
    }
    ibag.extend_from_slice(&gi.to_le_bytes());
    ibag.extend_from_slice(&0u16.to_le_bytes());
    let imod: Vec<u8> = vec![0u8; 10];
    let mut ifil = Vec::new();
    ifil.extend_from_slice(&2u16.to_le_bytes());
    ifil.extend_from_slice(&1u16.to_le_bytes());
    let info = list(b"INFO", &[chunk(b"ifil", &ifil), chunk(b"isng", b"EMU8000\0"), chunk(b"INAM", &format!("{name}\0").into_bytes())]);
    let sdta = list(b"sdta", &[chunk(b"smpl", &smpl)]);
    let pdta = list(
        b"pdta",
        &[
            chunk(b"phdr", &phdr),
            chunk(b"pbag", &pbag),
            chunk(b"pmod", &pmod),
            chunk(b"pgen", &pgen),
            chunk(b"inst", &inst),
            chunk(b"ibag", &ibag),
            chunk(b"imod", &imod),
            chunk(b"igen", &igen),
            chunk(b"shdr", &shdr),
        ],
    );
    let mut body = b"sfbk".to_vec();
    body.extend_from_slice(&info);
    body.extend_from_slice(&sdta);
    body.extend_from_slice(&pdta);
    let mut out = b"RIFF".to_vec();
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(&body);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(n: usize, period: usize) -> Vec<i16> {
        (0..n).map(|i| ((i as f64 / period as f64 * std::f64::consts::TAU).sin() * 16000.0) as i16).collect()
    }

    #[test]
    fn fixture_round_trips_through_the_parser() {
        let bytes = build_fixture(
            "Test Bank",
            0,
            0,
            &[
                FixtureSample { name: "sine", data: sine(4800, 100), rate: 48_000, root: 60, loop_start: 100, loop_end: 4700 },
                FixtureSample { name: "hi", data: sine(2400, 50), rate: 48_000, root: 72, loop_start: 0, loop_end: 2400 },
            ],
            &[
                FixtureZone { sample: 0, key_lo: 0, key_hi: 66, vel_lo: 0, vel_hi: 127, loop_mode: 1, extra: vec![(Gen::InitialAttenuation as u16, 60)] },
                FixtureZone { sample: 1, key_lo: 67, key_hi: 127, vel_lo: 0, vel_hi: 127, loop_mode: 0, extra: vec![] },
            ],
        );
        let sf = parse(&bytes).unwrap();
        assert_eq!(sf.name, "Test Bank");
        assert_eq!(sf.version, (2, 1));
        assert_eq!(sf.presets.len(), 1);
        assert_eq!(sf.instruments.len(), 1);
        assert_eq!(sf.samples.len(), 2);
        assert_eq!(sf.samples[0].sample_rate, 48_000);
        assert_eq!(sf.samples[0].loop_end - sf.samples[0].loop_start, 4600);
        assert_eq!(sf.instruments[0].zones.len(), 2);
        assert_eq!(sf.instruments[0].zones[0].key_range(), (0, 66));
        assert_eq!(sf.instruments[0].zones[1].key_range(), (67, 127));
        assert_eq!(sf.instruments[0].zones[0].get(Gen::InitialAttenuation), Some(60));
        assert!(sf.unsupported_generators.is_empty());
        assert!(sf.warnings().is_empty());
        assert_eq!(sf.sample_data.len(), 4800 + 46 + 2400 + 46);
        assert!(sf.find_preset(0, 0).is_some());
    }

    #[test]
    fn unsupported_generators_are_reported() {
        let bytes = build_fixture(
            "X",
            0,
            1,
            &[FixtureSample { name: "s", data: sine(1000, 50), rate: 44_100, root: 60, loop_start: 0, loop_end: 1000 }],
            &[FixtureZone { sample: 0, key_lo: 0, key_hi: 127, vel_lo: 0, vel_hi: 127, loop_mode: 0, extra: vec![(5, 100), (16, 500)] }],
        );
        let sf = parse(&bytes).unwrap();
        assert_eq!(sf.unsupported_generators, vec![5, 16]);
        let w = sf.warnings();
        assert!(w.iter().any(|x| x.contains("modLfoToPitch")));
        assert!(w.iter().any(|x| x.contains("reverbEffectsSend")));
    }

    #[test]
    fn malformed_files_are_rejected() {
        assert_eq!(parse(b"RIFF\0\0\0\0WAVE"), Err(Sf2Error::NotSoundFont));
        assert_eq!(parse(b""), Err(Sf2Error::NotSoundFont));
        let good = build_fixture(
            "X",
            0,
            0,
            &[FixtureSample { name: "s", data: sine(1000, 50), rate: 44_100, root: 60, loop_start: 0, loop_end: 1000 }],
            &[FixtureZone { sample: 0, key_lo: 0, key_hi: 127, vel_lo: 0, vel_hi: 127, loop_mode: 0, extra: vec![] }],
        );
        // truncated
        assert!(matches!(parse(&good[..good.len() / 2]), Err(Sf2Error::Malformed(_))));
        // corrupt a chunk size
        let mut bad = good.clone();
        let pos = bad.windows(4).position(|w| w == b"shdr").unwrap();
        bad[pos + 4..pos + 8].copy_from_slice(&0xFFFF_FFF0u32.to_le_bytes());
        assert!(matches!(parse(&bad), Err(Sf2Error::Malformed(_))));
    }
}
