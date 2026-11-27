//! Advisory single-writer project lock (`recovery/lock.json`): pid, host and time. A
//! second opener sees the holder and opens read-only unless it takes the lock over; a
//! stale lock (dead pid on this host, or older than `STALE_AFTER`) is reclaimed.

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

/// Whether `info` still plausibly guards a live process.
pub fn is_live(info: &LockInfo) -> bool {
    let age = now_ms().saturating_sub(info.acquired_unix_ms);
    if age > STALE_AFTER.as_millis() as u64 {
        return false;
    }
    if info.host == hostname() {
        if info.pid == std::process::id() {
            return false; // our own earlier lock (crash in this process id is not possible)
        }
        #[cfg(target_os = "linux")]
        {
            return Path::new(&format!("/proc/{}", info.pid)).exists();
        }
        #[cfg(target_os = "windows")]
        {
            return process_alive(info.pid);
        }
    }
    true
}

/// Whether process `pid` is still running (Windows). A process we may not query counts as
/// running: the safe side for a lock.
#[cfg(target_os = "windows")]
fn process_alive(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, ERROR_ACCESS_DENIED, GetLastError, STILL_ACTIVE};
    use windows_sys::Win32::System::Threading::{GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    // SAFETY: OpenProcess takes plain values and returns a process handle or null; a non-null
    // handle is only passed to GetExitCodeProcess (which writes one u32 into a local we own) and
    // then closed exactly once before returning. GetLastError reads this thread's error code.
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return GetLastError() == ERROR_ACCESS_DENIED;
        }
        let mut code = 0u32;
        let ok = GetExitCodeProcess(handle, &mut code) != 0;
        CloseHandle(handle);
        !ok || code == STILL_ACTIVE as u32
    }
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
