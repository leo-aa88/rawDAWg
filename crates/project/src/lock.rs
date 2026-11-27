//! Advisory single-writer project lock (`recovery/lock.json`): pid, host and time. A
//! second opener sees the holder and opens read-only unless it takes the lock over; a
//! stale lock (dead or reused pid on this host, written before the last boot, or older than
//! `STALE_AFTER`) is reclaimed.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const LOCK_FILE: &str = "recovery/lock.json";
pub const STALE_AFTER: Duration = Duration::from_secs(6 * 3600);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockInfo {
    pub pid: u32,
    pub host: String,
    pub acquired_unix_ms: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum LockError {
    #[error("project is open elsewhere (pid {} on {} since {} ms)", .0.pid, .0.host, .0.acquired_unix_ms)]
    Held(LockInfo),
    #[error("could not write the lock: {0}")]
    Io(String),
}

pub struct ProjectLock {
    path: PathBuf,
    pub info: LockInfo,
}

fn hostname() -> String {
    std::env::var("HOSTNAME").or_else(|_| std::env::var("COMPUTERNAME")).unwrap_or_else(|_| "local".into())
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|t| t.as_millis() as u64).unwrap_or(0)
}

/// How far process start and boot times may lag a lock's own time stamp (coarse clocks, small
/// wall-clock adjustments) before a pid counts as reused.
const CLOCK_MARGIN_MS: u64 = 30_000;

/// Whether `info` still plausibly guards a live process.
pub fn is_live(info: &LockInfo) -> bool {
    let age = now_ms().saturating_sub(info.acquired_unix_ms);
    if age > STALE_AFTER.as_millis() as u64 {
        return false;
    }
    if info.host != hostname() {
        return true; // another machine: nothing to check here, the age rule decides
    }
    if info.pid == std::process::id() {
        return false; // our own earlier lock (crash in this process id is not possible)
    }
    // written before this machine last started: whatever runs under that pid now is another process
    if boot_unix_ms().is_some_and(|boot| info.acquired_unix_ms.saturating_add(CLOCK_MARGIN_MS) < boot) {
        return false;
    }
    match process_start_unix_ms(info.pid) {
        ProcessState::Gone => false,
        // started after the lock was written: the pid was reused by another program
        ProcessState::StartedAt(ms) => ms <= info.acquired_unix_ms.saturating_add(CLOCK_MARGIN_MS),
        ProcessState::Unknown => true, // running, start unknown (or not ours to query): the safe side
    }
}

#[cfg_attr(not(any(target_os = "linux", target_os = "windows")), allow(dead_code))]
enum ProcessState {
    Gone,
    StartedAt(u64),
    Unknown,
}

/// When this machine started (unix ms), where the OS tells.
#[cfg(target_os = "linux")]
fn boot_unix_ms() -> Option<u64> {
    let stat = std::fs::read_to_string("/proc/stat").ok()?;
    let secs: u64 = stat.lines().find_map(|l| l.strip_prefix("btime "))?.trim().parse().ok()?;
    Some(secs * 1000)
}

/// Linux: `/proc/<pid>/stat` field 22 is the start time in clock ticks after boot (USER_HZ,
/// 100 on every architecture rawDAWg targets).
#[cfg(target_os = "linux")]
fn process_start_unix_ms(pid: u32) -> ProcessState {
    const USER_HZ: u64 = 100;
    let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
        return if Path::new(&format!("/proc/{pid}")).exists() { ProcessState::Unknown } else { ProcessState::Gone };
    };
    // the command name (field 2) is in parentheses and may hold spaces: count after the last ')'
    let ticks = stat.rfind(')').and_then(|i| stat[i + 1..].split_whitespace().nth(22 - 3)).and_then(|t| t.parse::<u64>().ok());
    match (ticks, boot_unix_ms()) {
        (Some(t), Some(boot)) => ProcessState::StartedAt(boot + t * 1000 / USER_HZ),
        _ => ProcessState::Unknown,
    }
}

#[cfg(target_os = "windows")]
fn boot_unix_ms() -> Option<u64> {
    // SAFETY: GetTickCount64 takes no arguments and only reads the system tick count.
    let up = unsafe { windows_sys::Win32::System::SystemInformation::GetTickCount64() };
    now_ms().checked_sub(up)
}

/// Windows: whether `pid` still runs and when it was created. A process we may not query counts
/// as running: the safe side for a lock.
#[cfg(target_os = "windows")]
fn process_start_unix_ms(pid: u32) -> ProcessState {
    use windows_sys::Win32::Foundation::{CloseHandle, ERROR_ACCESS_DENIED, FILETIME, GetLastError, STILL_ACTIVE};
    use windows_sys::Win32::System::Threading::{GetExitCodeProcess, GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    // SAFETY: OpenProcess takes plain values and returns a process handle or null; a non-null
    // handle is only passed to GetExitCodeProcess and GetProcessTimes (which write into locals we
    // own and outlive the calls) and then closed exactly once before returning. GetLastError reads
    // this thread's error code.
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return if GetLastError() == ERROR_ACCESS_DENIED { ProcessState::Unknown } else { ProcessState::Gone };
        }
        let mut code = 0u32;
        let exited = GetExitCodeProcess(handle, &mut code) != 0 && code != STILL_ACTIVE as u32;
        let zero = FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
        let (mut created, mut exit, mut kernel, mut user) = (zero, zero, zero, zero);
        let timed = GetProcessTimes(handle, &mut created, &mut exit, &mut kernel, &mut user) != 0;
        CloseHandle(handle);
        if exited {
            return ProcessState::Gone;
        }
        // FILETIME: 100 ns intervals since 1601-01-01
        let since_1601 = (u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime);
        match since_1601.checked_sub(116_444_736_000_000_000) {
            Some(t) if timed => ProcessState::StartedAt(t / 10_000),
            _ => ProcessState::Unknown,
        }
    }
}

#[cfg(not(any(target_os = "linux", target_os = "windows")))]
fn boot_unix_ms() -> Option<u64> {
    None
}

#[cfg(not(any(target_os = "linux", target_os = "windows")))]
fn process_start_unix_ms(_pid: u32) -> ProcessState {
    ProcessState::Unknown
}

impl ProjectLock {
    pub fn read(dir: &Path) -> Option<LockInfo> {
        let bytes = std::fs::read(dir.join(LOCK_FILE)).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    /// Take the lock; `force` takes over a live holder (the user's explicit choice).
    pub fn acquire(dir: &Path, force: bool) -> Result<Self, LockError> {
        let path = dir.join(LOCK_FILE);
        if let Some(existing) = Self::read(dir)
            && is_live(&existing)
            && !force
        {
            return Err(LockError::Held(existing));
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| LockError::Io(e.to_string()))?;
        }
        let info = LockInfo { pid: std::process::id(), host: hostname(), acquired_unix_ms: now_ms() };
        std::fs::write(&path, serde_json::to_vec_pretty(&info).map_err(|e| LockError::Io(e.to_string()))?).map_err(|e| LockError::Io(e.to_string()))?;
        Ok(Self { path, info })
    }
}

impl Drop for ProjectLock {
    fn drop(&mut self) {
        if Self::read(self.path.parent().and_then(|p| p.parent()).unwrap_or(Path::new("."))).as_ref() == Some(&self.info) {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A child process that runs until killed.
    fn sleeper() -> std::process::Child {
        #[cfg(windows)]
        let mut cmd = {
            let mut c = std::process::Command::new("ping");
            c.args(["-n", "60", "127.0.0.1"]);
            c
        };
        #[cfg(not(windows))]
        let mut cmd = {
            let mut c = std::process::Command::new("sleep");
            c.arg("60");
            c
        };
        cmd.stdout(std::process::Stdio::null()).spawn().expect("start a sleeping child process")
    }

    #[test]
    fn a_running_process_holds_the_lock_and_a_dead_or_reused_pid_does_not() {
        let mut child = sleeper();
        let lock = LockInfo { pid: child.id(), host: hostname(), acquired_unix_ms: now_ms() };
        assert!(is_live(&lock), "a running process on this machine holds its lock");
        // the same pid stamped an hour before that process started: the pid was reused
        let reused = LockInfo { acquired_unix_ms: now_ms() - 3_600_000, ..lock.clone() };
        #[cfg(any(target_os = "linux", target_os = "windows"))]
        assert!(!is_live(&reused), "a process started after the lock is not its holder");
        // another machine's lock is honoured until it is stale
        assert!(is_live(&LockInfo { host: format!("{}-elsewhere", hostname()), ..reused.clone() }));
        let old = LockInfo { acquired_unix_ms: now_ms() - STALE_AFTER.as_millis() as u64 - 1000, ..lock.clone() };
        assert!(!is_live(&old), "older than STALE_AFTER");
        child.kill().unwrap();
        child.wait().unwrap();
        assert!(!is_live(&lock), "the process is gone");
    }

    #[cfg(any(target_os = "linux", target_os = "windows"))]
    #[test]
    fn a_lock_written_before_the_machine_started_is_stale() {
        let boot = boot_unix_ms().expect("boot time");
        assert!(boot < now_ms() && boot > now_ms() - 400 * 86_400_000, "a plausible boot time: {boot}");
        let mut child = sleeper();
        // our running child's pid, but stamped before the boot: not the same process
        let lock = LockInfo { pid: child.id(), host: hostname(), acquired_unix_ms: boot.saturating_sub(CLOCK_MARGIN_MS + 60_000) };
        assert!(!is_live(&lock));
        child.kill().unwrap();
        child.wait().unwrap();
    }

    #[test]
    fn a_crashed_holder_leaves_a_lock_that_the_next_open_takes() {
        let dir = std::env::temp_dir().join(format!("rawdawg-lock-crash-{}", now_ms()));
        std::fs::create_dir_all(dir.join("recovery")).unwrap();
        let mut child = sleeper();
        let info = LockInfo { pid: child.id(), host: hostname(), acquired_unix_ms: now_ms() };
        std::fs::write(dir.join(LOCK_FILE), serde_json::to_vec(&info).unwrap()).unwrap();
        assert!(matches!(ProjectLock::acquire(&dir, false), Err(LockError::Held(_))), "held while that process runs");
        child.kill().unwrap();
        child.wait().unwrap();
        let taken = ProjectLock::acquire(&dir, false).expect("a dead holder's lock is reclaimed without asking");
        assert_eq!(ProjectLock::read(&dir), Some(taken.info.clone()));
        drop(taken);
        assert!(ProjectLock::read(&dir).is_none(), "released on drop");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
