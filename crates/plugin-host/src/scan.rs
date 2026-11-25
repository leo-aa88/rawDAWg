//! Module discovery and the isolated scanner (PLUGIN_API.md §3).
//!
//! Discovery walks the standard per-OS directories plus user roots with bounded
//! depth and symlink-loop protection. Each candidate is described in a **separate
//! process** (`rawdawg --scan-plugin <path>`) with a bounded timeout and bounded
//! output; crashes, hangs and unloadable files become quarantine records instead
//! of taking the app down. Isolation is crash containment, not a security sandbox.

use crate::catalog::{Catalog, Fingerprint, Format, QuarantineReason, QuarantineRecord, ScannedLibrary, now_unix};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Maximum directory depth below a search root.
pub const MAX_DEPTH: usize = 8;
/// Maximum candidate files per discovery run.
pub const MAX_CANDIDATES: usize = 10_000;
/// Maximum bytes accepted from a scanner child's stdout.
pub const MAX_OUTPUT_BYTES: usize = 1024 * 1024;
/// Maximum bytes kept from a scanner child's stderr (diagnostics only).
const MAX_STDERR_BYTES: usize = 16 * 1024;
/// Environment variable with extra search roots (`:`/`;` separated, per CLAP convention).
pub const CLAP_PATH_ENV: &str = "CLAP_PATH";
/// Test hook read by the scanner child (deterministic quarantine tests): `hang` or `crash`.
pub const TEST_BEHAVIOUR_ENV: &str = "RAWDAWG_SCAN_TEST_BEHAVIOUR";

/// Standard search roots for `format` on this OS (existing or not; discovery skips missing ones).
pub fn standard_paths(format: Format) -> Vec<PathBuf> {
    let mut out = Vec::new();
    match format {
        Format::Clap => {
            if cfg!(target_os = "windows") {
                if let Ok(p) = std::env::var("COMMONPROGRAMFILES") {
                    out.push(PathBuf::from(p).join("CLAP"));
                }
                if let Ok(p) = std::env::var("LOCALAPPDATA") {
                    out.push(PathBuf::from(p).join("Programs").join("Common").join("CLAP"));
                }
            } else {
                if let Ok(home) = std::env::var("HOME") {
                    out.push(PathBuf::from(home).join(".clap"));
                }
                out.push(PathBuf::from("/usr/lib/clap"));
                out.push(PathBuf::from("/usr/local/lib/clap"));
            }
            if let Ok(extra) = std::env::var(CLAP_PATH_ENV) {
                let sep = if cfg!(target_os = "windows") { ';' } else { ':' };
                out.extend(extra.split(sep).filter(|s| !s.is_empty()).map(PathBuf::from));
            }
        }
        Format::Vst3 => {
            if cfg!(target_os = "windows") {
                if let Ok(p) = std::env::var("COMMONPROGRAMFILES") {
                    out.push(PathBuf::from(p).join("VST3"));
                }
                if let Ok(p) = std::env::var("LOCALAPPDATA") {
                    out.push(PathBuf::from(p).join("Programs").join("Common").join("VST3"));
                }
            } else {
                if let Ok(home) = std::env::var("HOME") {
                    out.push(PathBuf::from(home).join(".vst3"));
                }
                out.push(PathBuf::from("/usr/lib/vst3"));
                out.push(PathBuf::from("/usr/local/lib/vst3"));
            }
        }
    }
    out
}

fn has_extension(path: &Path, ext: &str) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case(ext))
}

/// Recursively list candidate modules of `format` under `roots`: files with the format's
/// extension (and, for VST3 bundles, the bundle directory itself). Bounded depth and count;
/// symlinked directories are followed once (canonical path set).
pub fn enumerate(roots: &[PathBuf], format: Format) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut visited: HashSet<PathBuf> = HashSet::new();
    for root in roots {
        if !root.is_dir() {
            continue;
        }
        walk(root, 0, format, &mut visited, &mut out);
        if out.len() >= MAX_CANDIDATES {
            break;
        }
    }
    out.sort();
    out.dedup();
    out
}

fn walk(dir: &Path, depth: usize, format: Format, visited: &mut HashSet<PathBuf>, out: &mut Vec<PathBuf>) {
    if depth > MAX_DEPTH || out.len() >= MAX_CANDIDATES {
        return;
    }
    let Ok(canon) = std::fs::canonicalize(dir) else { return };
    if !visited.insert(canon) {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    entries.sort();
    for path in entries {
        if out.len() >= MAX_CANDIDATES {
            return;
        }
        let is_dir = path.is_dir();
        if has_extension(&path, format.extension()) {
            // CLAP modules are files; VST3 may be a bundle directory or a bare module file.
            if !is_dir || format == Format::Vst3 {
                out.push(path);
                continue;
            }
        }
        if is_dir {
            walk(&path, depth + 1, format, visited, out);
        }
    }
}

/// How the parent runs scanner children.
#[derive(Clone, Debug)]
pub struct ScanOptions {
    /// Executable implementing `--scan-plugin <path>` (normally the running `rawdawg` binary).
    pub scanner_exe: PathBuf,
    pub timeout: Duration,
    pub max_output_bytes: usize,
}

impl ScanOptions {
    pub fn new(scanner_exe: PathBuf, timeout_secs: u64) -> Self {
        Self { scanner_exe, timeout: Duration::from_secs(timeout_secs.max(1)), max_output_bytes: MAX_OUTPUT_BYTES }
    }
}

/// What the scanner child prints on stdout (exactly one JSON document).
#[derive(Serialize, Deserialize, Debug)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum ChildReport {
    Ok { library: ScannedLibrary },
    LoadFailed { message: String },
}

pub enum ScanOutcome {
    Scanned(ScannedLibrary),
    Quarantined(QuarantineRecord),
}

/// Scanner child entry: describe `path` in this process and print a [`ChildReport`].
/// Returns the process exit code. Must not open audio devices or the GUI.
pub fn run_scanner_child(path: &Path, format: Format) -> i32 {
    // Deterministic failure modes for the quarantine tests; never set in normal use.
    match std::env::var(TEST_BEHAVIOUR_ENV).as_deref() {
        Ok("hang") => loop {
            std::thread::sleep(Duration::from_secs(1));
        },
        Ok("crash") => std::process::abort(),
        _ => {}
    }
    let report = match format {
        // SAFETY: this is the isolated scanner process; executing the module is its purpose.
        Format::Clap => match unsafe { crate::describe::describe_clap_library(path) } {
            Ok(library) => ChildReport::Ok { library },
            Err(message) => ChildReport::LoadFailed { message },
        },
        Format::Vst3 => ChildReport::LoadFailed { message: "VST3 scanning is not implemented yet".to_owned() },
    };
    let code = if matches!(report, ChildReport::Ok { .. }) { 0 } else { 1 };
    match serde_json::to_string(&report) {
        Ok(json) => {
            println!("{json}");
            code
        }
        Err(_) => 3,
    }
}

fn read_bounded(mut r: impl Read, cap: usize) -> (Vec<u8>, bool) {
    let mut out = Vec::new();
    let mut buf = [0u8; 8192];
    let mut truncated = false;
    loop {
        match r.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                if out.len() + n > cap {
                    out.extend_from_slice(&buf[..cap.saturating_sub(out.len())]);
                    truncated = true;
                    // keep draining so the child never blocks on a full pipe
                    let mut sink = [0u8; 8192];
                    while matches!(r.read(&mut sink), Ok(k) if k > 0) {}
                    break;
                }
                out.extend_from_slice(&buf[..n]);
            }
        }
    }
    (out, truncated)
}

/// Describe one module in a child process with timeout, bounded output and quarantine.
pub fn scan_library(path: &Path, format: Format, opts: &ScanOptions) -> ScanOutcome {
    let fingerprint = Fingerprint::of(path);
    let quarantine = |reason: QuarantineReason| ScanOutcome::Quarantined(QuarantineRecord { path: path.to_path_buf(), fingerprint, reason, at_unix: now_unix() });
    if fingerprint.is_none() {
        return quarantine(QuarantineReason::LoadFailed { message: "file not found".to_owned() });
    }
    let mut child = match Command::new(&opts.scanner_exe)
        .arg("--scan-plugin")
        .arg(path)
        .arg("--scan-format")
        .arg(format.extension())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => return quarantine(QuarantineReason::InvalidOutput { message: format!("cannot start scanner {}: {e}", opts.scanner_exe.display()) }),
    };
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let cap = opts.max_output_bytes;
    let out_thread = std::thread::spawn(move || stdout.map(|s| read_bounded(s, cap)).unwrap_or_default());
    let err_thread = std::thread::spawn(move || stderr.map(|s| read_bounded(s, MAX_STDERR_BYTES).0).unwrap_or_default());
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {
                if started.elapsed() >= opts.timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    break None;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(_) => break None,
        }
    };
    let (stdout, truncated) = out_thread.join().unwrap_or_default();
    let stderr = String::from_utf8_lossy(&err_thread.join().unwrap_or_default()).into_owned();
    let Some(status) = status else {
        return quarantine(QuarantineReason::Timeout { secs: opts.timeout.as_secs() });
    };
    if truncated {
        return quarantine(QuarantineReason::InvalidOutput { message: format!("scanner output exceeded {} bytes", opts.max_output_bytes) });
    }
    match serde_json::from_slice::<ChildReport>(&stdout) {
        Ok(ChildReport::Ok { mut library }) => {
            library.path = path.to_path_buf();
            if let Some(fp) = fingerprint {
                library.fingerprint = fp;
            }
            ScanOutcome::Scanned(library)
        }
        Ok(ChildReport::LoadFailed { message }) => quarantine(QuarantineReason::LoadFailed { message }),
        Err(e) => {
            if status.success() {
                quarantine(QuarantineReason::InvalidOutput { message: format!("unparsable report: {e}") })
            } else {
                quarantine(QuarantineReason::Crash { exit_code: status.code(), stderr })
            }
        }
    }
}

/// Summary of a catalog refresh.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ScanReport {
    pub candidates: usize,
    pub scanned: usize,
    pub quarantined: usize,
    pub unchanged: usize,
    pub pruned: usize,
}

/// Refresh `catalog` for `format`: prune vanished modules, enumerate the standard and user
/// roots, scan new/changed candidates (quarantined ones only if their file changed or the
/// user cleared the record). `progress(path, index, total)` runs before each child.
pub fn refresh(catalog: &mut Catalog, format: Format, opts: &ScanOptions, mut progress: impl FnMut(&Path, usize, usize)) -> ScanReport {
    let mut report = ScanReport { pruned: catalog.prune_missing(), ..Default::default() };
    let mut roots = standard_paths(format);
    roots.extend(catalog.user_paths.iter().cloned());
    let candidates = enumerate(&roots, format);
    report.candidates = candidates.len();
    let to_scan: Vec<PathBuf> = candidates.iter().filter(|p| catalog.needs_scan(p)).cloned().collect();
    report.unchanged = candidates.len() - to_scan.len();
    let total = to_scan.len();
    for (i, path) in to_scan.iter().enumerate() {
        progress(path, i, total);
        match scan_library(path, format, opts) {
            ScanOutcome::Scanned(lib) => {
                report.scanned += 1;
                catalog.insert_library(lib);
            }
            ScanOutcome::Quarantined(rec) => {
                report.quarantined += 1;
                log::warn!("quarantined {}: {}", rec.path.display(), rec.reason.summary());
                catalog.insert_quarantine(rec);
            }
        }
    }
    catalog.last_scan_unix = now_unix();
    report
}
