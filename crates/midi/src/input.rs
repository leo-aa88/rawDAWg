//! midir-backed input manager. Callback threads push raw messages into a
//! bounded queue; `poll` (coordinator thread) parses and returns engine events.

use crate::parser::MidiParser;
use midir::{Ignore, MidiInput, MidiInputConnection, MidiInputPort};
use rawdawg_rt::queue::{ControlQueue, QueueConsumer, QueueProducer};
use rawdawg_types::events::{EventKind, EventSource, TimedEvent};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Raw message as received from the device thread.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MidiMessage {
    /// Device timestamp in microseconds (monotonic per device, origin unspecified).
    pub timestamp_us: u64,
    /// When the message reached rawDAWg, on the process clock (`rawdawg_rt::clock`).
    pub arrival_us: u64,
    pub len: u8,
    pub bytes: [u8; 3],
}

/// When a MIDI message happened: the device's timestamp and its arrival on the process clock.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct MidiTime {
    pub device_us: u64,
    pub arrival_us: u64,
}

impl MidiTime {
    /// Events rawDAWg makes itself (the releases after a disconnect): play them at once.
    pub const UNTIMED: MidiTime = MidiTime { device_us: 0, arrival_us: 0 };

    pub fn is_untimed(&self) -> bool {
        *self == Self::UNTIMED
    }
}

/// Maps one device's timestamps onto the process clock. Delivery only ever delays a message,
/// so the smallest `arrival − stamp` seen is the clocks' offset (plus the fastest delivery);
/// it may relax by 100 µs per second to follow drift, and a jump of more than a second (the
/// device clock restarted) starts the estimate again.
#[derive(Clone, Copy, Debug, Default)]
pub struct MidiClock {
    offset: Option<i64>,
    last_arrival_us: u64,
}

impl MidiClock {
    /// Process-clock time at which the message stamped `t.device_us` was sent (never later
    /// than its arrival).
    pub fn wall_time(&mut self, t: MidiTime) -> u64 {
        let observed = t.arrival_us as i64 - t.device_us as i64;
        let offset = match self.offset {
            Some(o) if (observed - o).abs() <= 1_000_000 => {
                let relaxed = o + (t.arrival_us.saturating_sub(self.last_arrival_us) / 10_000) as i64;
                relaxed.min(observed)
            }
            _ => observed,
        };
        self.offset = Some(offset);
        self.last_arrival_us = t.arrival_us;
        ((t.device_us as i64 + offset).max(0) as u64).min(t.arrival_us)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MidiDeviceInfo {
    /// Opaque stable id from midir.
    pub id: String,
    pub name: String,
    pub connected: bool,
    pub enabled: bool,
    /// Small index used as `EventSource::MidiDevice(index)`.
    pub source_index: u16,
    pub messages: u64,
    pub dropped: u64,
}

struct Connection {
    _conn: MidiInputConnection<QueueProducer<MidiMessage>>,
    consumer: QueueConsumer<MidiMessage>,
    parser: MidiParser,
    info: MidiDeviceInfo,
}

/// Owns midir connections. All methods run on the coordinator/UI thread.
pub struct MidiInputManager {
    connections: HashMap<String, Connection>,
    /// Devices present at the last scan.
    present: Vec<(String, String)>,
    next_source: u16,
    source_by_id: HashMap<String, u16>,
    available: bool,
    last_error: Option<String>,
    queue_capacity: usize,
}

impl Default for MidiInputManager {
    fn default() -> Self {
        Self::new()
    }
}

impl MidiInputManager {
    pub fn new() -> Self {
        // the clock the arrival stamps use, fixed before any device thread runs
        rawdawg_rt::clock::init();
        let mut m =
            Self { connections: HashMap::new(), present: Vec::new(), next_source: 0, source_by_id: HashMap::new(), available: false, last_error: None, queue_capacity: 1024 };
        m.rescan();
        m
    }

    fn client() -> Result<MidiInput, String> {
        let mut input = MidiInput::new("rawDAWg").map_err(|e| e.to_string())?;
        input.ignore(Ignore::ActiveSense | Ignore::Time);
        Ok(input)
    }

    /// Whether a MIDI backend could be initialized at all (false on headless systems).
    pub fn is_available(&self) -> bool {
        self.available
    }
    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    /// Enumerate ports; detects disconnects (connected flag cleared, notes released by `poll`).
    pub fn rescan(&mut self) {
        match Self::client() {
            Ok(input) => {
                self.available = true;
                self.last_error = None;
                self.present = input.ports().iter().filter_map(|p| Some((p.id(), input.port_name(p).ok()?))).collect();
            }
            Err(e) => {
                self.available = false;
                self.last_error = Some(e);
                self.present.clear();
            }
        }
        let present_ids: Vec<String> = self.present.iter().map(|(id, _)| id.clone()).collect();
        for (id, c) in self.connections.iter_mut() {
            c.info.connected = present_ids.contains(id);
        }
    }

    fn source_for(&mut self, id: &str) -> u16 {
        if let Some(s) = self.source_by_id.get(id) {
            return *s;
        }
        let s = self.next_source;
        self.next_source = self.next_source.wrapping_add(1);
        self.source_by_id.insert(id.to_owned(), s);
        s
    }

    /// Devices known (present now or previously connected).
    pub fn devices(&self) -> Vec<MidiDeviceInfo> {
        let mut out: Vec<MidiDeviceInfo> = Vec::new();
        for (id, name) in &self.present {
            if let Some(c) = self.connections.get(id) {
                out.push(c.info.clone());
            } else {
                out.push(MidiDeviceInfo {
                    id: id.clone(),
                    name: name.clone(),
                    connected: true,
                    enabled: false,
                    source_index: *self.source_by_id.get(id).unwrap_or(&u16::MAX),
                    messages: 0,
                    dropped: 0,
                });
            }
        }
        for (id, c) in &self.connections {
            if !self.present.iter().any(|(pid, _)| pid == id) {
                out.push(c.info.clone());
            }
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    /// Enable (connect) a device by id.
    pub fn enable(&mut self, id: &str) -> Result<(), String> {
        if self.connections.contains_key(id) {
            return Ok(());
        }
        let input = Self::client()?;
        let port: MidiInputPort = input.ports().into_iter().find(|p| p.id() == id).ok_or_else(|| format!("MIDI device {id} is not connected"))?;
        let name = input.port_name(&port).unwrap_or_else(|_| id.to_owned());
        let (producer, consumer) = ControlQueue::create::<MidiMessage>(self.queue_capacity);
        let conn = input
            .connect(
                &port,
                "rawDAWg input",
                move |timestamp_us, bytes, producer: &mut QueueProducer<MidiMessage>| {
                    // Device thread: copy bytes only. Long messages (SysEx) are split into
                    // 3-byte chunks; the parser skips SysEx content anyway.
                    let arrival_us = rawdawg_rt::clock::now_us();
                    for chunk in bytes.chunks(3) {
                        let mut b = [0u8; 3];
                        b[..chunk.len()].copy_from_slice(chunk);
                        let _ = producer.push(MidiMessage { timestamp_us, arrival_us, len: chunk.len() as u8, bytes: b });
                    }
                },
                producer,
            )
            .map_err(|e| e.to_string())?;
        let source_index = self.source_for(id);
        self.connections.insert(
            id.to_owned(),
            Connection {
                _conn: conn,
                consumer,
                parser: MidiParser::new(),
                info: MidiDeviceInfo { id: id.to_owned(), name, connected: true, enabled: true, source_index, messages: 0, dropped: 0 },
            },
        );
        Ok(())
    }

    /// Disable a device; returns note-off events for notes it still holds.
    pub fn disable(&mut self, id: &str) -> Vec<TimedEvent> {
        let Some(mut c) = self.connections.remove(id) else { return vec![] };
        let src = EventSource::MidiDevice(c.info.source_index);
        c.parser.release_all().into_iter().map(|(ch, k)| TimedEvent::new(0, src, ch, k)).collect()
    }

    /// Drain all queues, parse, and return engine-ready events with their source and their
    /// time: the device's own timestamp and the arrival on the process clock. Disconnected
    /// devices release notes.
    pub fn poll(&mut self) -> Vec<(TimedEvent, MidiTime)> {
        let mut out = Vec::new();
        let mut to_release = Vec::new();
        for (id, c) in self.connections.iter_mut() {
            let src = EventSource::MidiDevice(c.info.source_index);
            while let Some(m) = c.consumer.pop() {
                c.info.messages += 1;
                for (ch, kind) in c.parser.feed(&m.bytes[..usize::from(m.len)]) {
                    out.push((TimedEvent::new(0, src, ch, kind), MidiTime { device_us: m.timestamp_us, arrival_us: m.arrival_us }));
                }
            }
            c.info.dropped = c.consumer.stats().overflowed();
            if !c.info.connected && c.parser.registry_mut().active_count() > 0 {
                to_release.push(id.clone());
            }
        }
        for id in to_release {
            if let Some(c) = self.connections.get_mut(&id) {
                let src = EventSource::MidiDevice(c.info.source_index);
                for (ch, k) in c.parser.release_all() {
                    out.push((TimedEvent::new(0, src, ch, k), MidiTime::UNTIMED));
                }
                // also emit a source-scoped all-notes-off as a safety net
                out.push((TimedEvent::new(0, src, 0, EventKind::AllNotesOff { source: Some(src) }), MidiTime::UNTIMED));
            }
        }
        out
    }

    /// Ids of enabled devices (for preferences).
    pub fn enabled_ids(&self) -> Vec<String> {
        self.connections.keys().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manager_degrades_gracefully_without_midi_backend() {
        // On machines without a sequencer, `MidiInput::new` fails or lists no ports;
        // either way the manager must not panic and must report state honestly.
        let mut m = MidiInputManager::new();
        let devices = m.devices();
        if m.is_available() {
            for d in &devices {
                assert!(!d.id.is_empty());
            }
        } else {
            assert!(m.last_error().is_some());
            assert!(devices.is_empty());
        }
        assert!(m.enable("no-such-device").is_err());
        assert!(m.poll().is_empty());
        assert!(m.disable("no-such-device").is_empty());
    }
}

#[cfg(test)]
mod clock_tests {
    use super::{MidiClock, MidiTime};

    /// A device whose clock is `offset` µs ahead of the process clock (`ppm` fast), sending a
    /// message every 10 ms that arrives 0.4–6 ms later (delivery jitter, like a USB driver and
    /// a polling thread).
    fn run(offset: i64, ppm: f64, seconds: u64) -> Vec<(u64, u64)> {
        let mut clock = MidiClock::default();
        let mut out = Vec::new();
        let mut jitter: u64 = 12345;
        for k in 0..seconds * 100 {
            let sent = 5_000_000 + k * 10_000;
            jitter = jitter.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let delay = 400 + (jitter >> 33) % 5600;
            let device = ((sent as f64) * (1.0 + ppm * 1e-6)) as i64 + offset;
            let t = MidiTime { device_us: device.max(0) as u64, arrival_us: sent + delay };
            out.push((sent, clock.wall_time(t)));
        }
        out
    }

    #[test]
    fn device_timestamps_recover_the_send_times_despite_delivery_jitter() {
        let res = run(3_000_000_000, 0.0, 20);
        // after a short warm-up the estimate is within the fastest delivery (≈ 0.4 ms) plus a
        // little relaxation, while the delivery itself jittered by up to 6 ms
        for (sent, wall) in &res[100..] {
            let err = *wall as i64 - *sent as i64;
            assert!((0..=1_000).contains(&err), "sent {sent} placed at {wall} (error {err} µs)");
        }
        let spread = |r: &[(u64, u64)]| {
            let e: Vec<i64> = r.iter().map(|(s, w)| *w as i64 - *s as i64).collect();
            e.iter().max().unwrap() - e.iter().min().unwrap()
        };
        assert!(spread(&res[100..]) < 800, "timing jitter removed: {} µs", spread(&res[100..]));
    }

    #[test]
    fn the_estimate_follows_clock_drift_and_a_device_restart() {
        // 50 ppm: the device clock gains 3 ms a minute
        let res = run(-2_000_000, 50.0, 60);
        for (sent, wall) in &res[200..] {
            let err = *wall as i64 - *sent as i64;
            assert!(err.abs() <= 1_500, "drift followed: error {err} µs at {sent}");
        }
        // a restarted device clock (stamps back near zero) is picked up at once
        let mut clock = MidiClock::default();
        let a = clock.wall_time(MidiTime { device_us: 900_000_000, arrival_us: 10_000_000 });
        let b = clock.wall_time(MidiTime { device_us: 1_000, arrival_us: 10_010_500 });
        assert_eq!(a, 10_000_000);
        assert_eq!(b, 10_010_500, "the first message after a restart is placed at its arrival");
        // and a stamp can never be placed after its own arrival
        let c = clock.wall_time(MidiTime { device_us: 50_000, arrival_us: 10_020_000 });
        assert!(c <= 10_020_000);
    }
}
