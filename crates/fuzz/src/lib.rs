//! Deterministic, seeded mutation fuzzing of rawDAWg's untrusted inputs (TESTING.md:
//! "fuzz file parsers/graph/ABI validation with bounded fixtures"; PROJECT_FORMAT.md:
//! "never use unwrap on untrusted file").
//!
//! Every [`Target`] has seed inputs (generated fixtures, saved projects, presets, states),
//! a mutation mode, and a `run` function that must not panic and must uphold its stated
//! properties (it panics with a message when a property is violated). A campaign runs a
//! target for a number of iterations from a fixed seed, so a failure reproduces from
//! `(target, seed, iteration)` alone and its input is saved under `target/fuzz/`.
//!
//! No coverage instrumentation (that would need a nightly toolchain and cargo-fuzz); the
//! mutators are structure-aware where the format is JSON.

pub mod mutate;
pub mod targets;

use mutate::{Rng, mutate_bytes, mutate_json};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Raw bytes (binary formats, text).
    Bytes,
    /// JSON documents: mostly structure-aware mutations, some byte-level.
    Json,
}

pub struct Target {
    pub name: &'static str,
    pub mode: Mode,
    pub max_len: usize,
    pub seeds: Vec<Vec<u8>>,
    pub run: fn(&[u8]),
}

#[derive(Debug, Clone)]
pub struct Finding {
    pub target: String,
    pub iteration: u64,
    pub message: String,
    pub input: PathBuf,
}

#[derive(Debug, Default)]
pub struct Report {
    pub target: String,
    pub executed: u64,
    /// Inputs (seeds included) that parsed and went on to the deeper checks.
    pub parsed: u64,
    pub accepted_seeds: usize,
    pub findings: Vec<Finding>,
    pub slowest_ms: f64,
    pub elapsed: Duration,
}

/// Inputs that got past the target's parser in the current campaign (see [`mark_accepted`]).
static ACCEPTED: AtomicU64 = AtomicU64::new(0);

/// Called by a target once its input parsed, so reports show how deep mutations reach.
pub fn mark_accepted() {
    ACCEPTED.fetch_add(1, Ordering::Relaxed);
}

/// Campaigns change the global panic hook: run them one at a time.
static CAMPAIGN_LOCK: Mutex<()> = Mutex::new(());
static LAST_PANIC: Mutex<Option<String>> = Mutex::new(None);

fn save_input(dir: &Path, target: &str, kind: &str, input: &[u8]) -> PathBuf {
    let hash = rawdawg_project::preset::sha256_hex(input);
    let d = dir.join(target);
    let _ = std::fs::create_dir_all(&d);
    let p = d.join(format!("{kind}-{}.bin", &hash[..16]));
    let _ = std::fs::write(&p, input);
    p
}

/// Run `target` for `iterations` mutated inputs (plus every seed once) from `seed`.
/// Stops early after `max_findings` distinct failures or when `budget` elapses. A single
/// input running longer than `hang_after` is saved and the process is aborted (a hang in
/// a parser is a bug; the saved file reproduces it).
pub fn run_campaign(target: &Target, iterations: u64, seed: u64, budget: Duration, out_dir: &Path, max_findings: usize, hang_after: Duration) -> Report {
    let _guard = CAMPAIGN_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|info| {
        let msg = match (info.payload().downcast_ref::<&str>(), info.payload().downcast_ref::<String>()) {
            (Some(s), _) => (*s).to_owned(),
            (_, Some(s)) => s.clone(),
            _ => "panic".to_owned(),
        };
        let at = info.location().map(|l| format!(" at {}:{}", l.file(), l.line())).unwrap_or_default();
        *LAST_PANIC.lock().unwrap_or_else(|p| p.into_inner()) = Some(format!("{msg}{at}"));
    }));

    // hang watchdog: the current input and when it started
    let current: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let started_ms = Arc::new(AtomicU64::new(u64::MAX));
    let done = Arc::new(AtomicBool::new(false));
    let t0 = Instant::now();
    let watchdog = {
        let (current, started_ms, done) = (current.clone(), started_ms.clone(), done.clone());
        let (dir, name) = (out_dir.to_path_buf(), target.name);
        std::thread::spawn(move || {
            while !done.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(200));
                let s = started_ms.load(Ordering::Relaxed);
                if s != u64::MAX && (t0.elapsed().as_millis() as u64).saturating_sub(s) > hang_after.as_millis() as u64 {
                    let input = current.lock().map(|c| c.clone()).unwrap_or_default();
                    let p = save_input(&dir, name, "hang", &input);
                    eprintln!("fuzz {name}: input ran longer than {hang_after:?}; saved {} and aborting", p.display());
                    std::process::abort();
                }
            }
        })
    };

    let mut report = Report { target: target.name.to_owned(), ..Default::default() };
    ACCEPTED.store(0, Ordering::Relaxed);
    let mut rng = Rng::new(seed ^ fxhash(target.name));
    // seeds that parse as JSON, for the structure-aware mutator
    let json_seeds: Vec<serde_json::Value> = if target.mode == Mode::Json { target.seeds.iter().filter_map(|s| serde_json::from_slice(s).ok()).collect() } else { Vec::new() };
    let mut seen_messages: Vec<String> = Vec::new();
    let total = target.seeds.len() as u64 + iterations;
    for i in 0..total {
        if t0.elapsed() > budget || report.findings.len() >= max_findings {
            break;
        }
        let input: Vec<u8> = if (i as usize) < target.seeds.len() {
            target.seeds[i as usize].clone()
        } else if target.mode == Mode::Json && !json_seeds.is_empty() && rng.chance(4, 5) {
            let pick = rng.below(json_seeds.len());
            let doc = mutate_json(&mut rng, &json_seeds[pick]);
            let mut bytes = serde_json::to_vec(&doc).unwrap_or_default();
            bytes.truncate(target.max_len);
            bytes
        } else if target.seeds.is_empty() {
            mutate_bytes(&mut rng, &[], &[], target.max_len)
        } else {
            let base = &target.seeds[rng.below(target.seeds.len())];
            mutate_bytes(&mut rng, base, &target.seeds, target.max_len)
        };
        if let Ok(mut c) = current.lock() {
            c.clear();
            c.extend_from_slice(&input);
        }
        let s = Instant::now();
        // panics contained inside the target (decoder containment) must not be blamed later
        LAST_PANIC.lock().unwrap_or_else(|p| p.into_inner()).take();
        started_ms.store(t0.elapsed().as_millis() as u64, Ordering::Relaxed);
        let result = std::panic::catch_unwind(|| (target.run)(&input));
        started_ms.store(u64::MAX, Ordering::Relaxed);
        report.slowest_ms = report.slowest_ms.max(s.elapsed().as_secs_f64() * 1e3);
        report.executed += 1;
        if result.is_err() {
            let message = LAST_PANIC.lock().unwrap_or_else(|p| p.into_inner()).take().unwrap_or_else(|| "panic".into());
            // one reproducer per distinct panic site
            let site = message.rsplit(" at ").next().unwrap_or(&message).to_owned();
            if !seen_messages.contains(&site) {
                seen_messages.push(site);
                let path = save_input(out_dir, target.name, "crash", &input);
                report.findings.push(Finding { target: target.name.to_owned(), iteration: i, message, input: path });
            }
        } else if (i as usize) < target.seeds.len() {
            report.accepted_seeds += 1;
        }
    }
    report.elapsed = t0.elapsed();
    report.parsed = ACCEPTED.load(Ordering::Relaxed);
    done.store(true, Ordering::Relaxed);
    let _ = watchdog.join();
    std::panic::set_hook(previous_hook);
    report
}

/// Re-run one saved input against a target (returns the panic message, if any).
pub fn replay(target: &Target, input: &[u8]) -> Option<String> {
    let _guard = CAMPAIGN_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|info| {
        let msg = info.payload().downcast_ref::<String>().cloned().or_else(|| info.payload().downcast_ref::<&str>().map(|s| (*s).to_owned())).unwrap_or_else(|| "panic".into());
        let at = info.location().map(|l| format!(" at {}:{}", l.file(), l.line())).unwrap_or_default();
        *LAST_PANIC.lock().unwrap_or_else(|p| p.into_inner()) = Some(format!("{msg}{at}"));
    }));
    let r = std::panic::catch_unwind(|| (target.run)(input));
    std::panic::set_hook(previous_hook);
    r.err().map(|_| LAST_PANIC.lock().unwrap_or_else(|p| p.into_inner()).take().unwrap_or_else(|| "panic".into()))
}

fn fxhash(s: &str) -> u64 {
    s.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3))
}
