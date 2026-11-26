//! The clack host handlers rawDAWg presents to plugins. Shared by the scanner
//! (description only) and the in-process bridge (M7 slice 3).
//!
//! For plugin editors (ADR 0006) the in-process host also offers `timer-support`,
//! `posix-fd-support` (Unix), `gui` (resize / show / hide / closed requests) and `params`
//! (`request_flush`). Requests are recorded here; the owner of the instance services them on
//! the main thread ([`crate::clap::ClapInstance::editor_idle`]).

use clack_extensions::gui::{GuiSize, HostGui, HostGuiImpl};
use clack_extensions::params::{HostParams, HostParamsImplMainThread, HostParamsImplShared, ParamClearFlags, ParamRescanFlags};
#[cfg(unix)]
use clack_extensions::posix_fd::{FdFlags, HostPosixFd, HostPosixFdImpl};
use clack_extensions::timer::{HostTimer, HostTimerImpl, TimerId};
use clack_host::prelude::*;
use std::cell::{Cell, RefCell};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

pub const HOST_NAME: &str = "rawDAWg";
pub const HOST_VENDOR: &str = "rawDAWg";
pub const HOST_URL: &str = "https://github.com/leo-aa88/rawDAWg";

/// Shortest timer period granted to plugins (they may ask for less).
pub const MIN_TIMER_MS: u32 = 15;
/// Upper bound on timers and fds one plugin may register.
pub const MAX_TIMERS: usize = 32;

pub fn host_info() -> HostInfo {
    HostInfo::new(HOST_NAME, HOST_VENDOR, HOST_URL, rawdawg_types::VERSION).expect("static host info is valid")
}

/// Thread-safe host data: plugin requests are recorded as flags the owner polls.
#[derive(Default)]
pub struct Shared {
    pub restart_requested: AtomicBool,
    pub process_requested: AtomicBool,
    pub callback_requested: AtomicBool,
    /// `params.request_flush` (an editor changed parameters while the plugin was idle).
    pub flush_requested: AtomicBool,
    /// Editor requests: a new size (physical pixels), show / hide, and closed.
    pub gui_resize: Mutex<Option<GuiSize>>,
    pub gui_closed: AtomicBool,
}

impl<'a> SharedHandler<'a> for Shared {
    fn request_restart(&self) {
        self.restart_requested.store(true, Ordering::Relaxed);
    }
    fn request_process(&self) {
        self.process_requested.store(true, Ordering::Relaxed);
    }
    fn request_callback(&self) {
        self.callback_requested.store(true, Ordering::Relaxed);
    }
}

impl HostGuiImpl for Shared {
    fn resize_hints_changed(&self) {}
    fn request_resize(&self, new_size: GuiSize) -> Result<(), HostError> {
        if new_size.width == 0 || new_size.height == 0 || new_size.width > 16_384 || new_size.height > 16_384 {
            return Err(HostError::Message("implausible editor size"));
        }
        *self.gui_resize.lock().map_err(|_| HostError::Message("poisoned"))? = Some(new_size);
        Ok(())
    }
    fn request_show(&self) -> Result<(), HostError> {
        Ok(())
    }
    fn request_hide(&self) -> Result<(), HostError> {
        Ok(())
    }
    fn closed(&self, _was_destroyed: bool) {
        self.gui_closed.store(true, Ordering::Relaxed);
    }
}

impl HostParamsImplShared for Shared {
    fn request_flush(&self) {
        self.flush_requested.store(true, Ordering::Relaxed);
    }
}

/// A plugin timer.
#[derive(Clone, Copy, Debug)]
pub struct Timer {
    pub id: TimerId,
    pub period: Duration,
    pub next: Instant,
}

/// Main-thread host data: timers and file descriptors plugins registered.
#[derive(Default)]
pub struct MainThread {
    pub timers: RefCell<Vec<Timer>>,
    next_timer: Cell<u32>,
    #[cfg(unix)]
    pub fds: RefCell<Vec<(std::os::fd::RawFd, FdFlags)>>,
    /// `params.rescan` was called (parameter list or values changed).
    pub rescan_requested: Cell<bool>,
}

impl MainThread {
    /// Timers due now (their next deadline is advanced).
    pub fn due_timers(&self, now: Instant) -> Vec<TimerId> {
        let mut out = Vec::new();
        for t in self.timers.borrow_mut().iter_mut() {
            if now >= t.next {
                out.push(t.id);
                // skip missed ticks instead of bursting
                t.next = now + t.period;
            }
        }
        out
    }
}

impl<'a> MainThreadHandler<'a> for MainThread {}

impl HostTimerImpl for MainThread {
    fn register_timer(&self, period_ms: u32) -> Result<TimerId, HostError> {
        let mut timers = self.timers.borrow_mut();
        if timers.len() >= MAX_TIMERS {
            return Err(HostError::Message("too many timers"));
        }
        let id = TimerId(self.next_timer.get());
        self.next_timer.set(self.next_timer.get().wrapping_add(1));
        let period = Duration::from_millis(u64::from(period_ms.max(MIN_TIMER_MS)));
        timers.push(Timer { id, period, next: Instant::now() + period });
        Ok(id)
    }
    fn unregister_timer(&self, timer_id: TimerId) -> Result<(), HostError> {
        let mut timers = self.timers.borrow_mut();
        let before = timers.len();
        timers.retain(|t| t.id != timer_id);
        if timers.len() == before { Err(HostError::Message("unknown timer")) } else { Ok(()) }
    }
}

#[cfg(unix)]
impl HostPosixFdImpl for MainThread {
    fn register_fd(&self, fd: std::os::fd::RawFd, flags: FdFlags) -> Result<(), HostError> {
        let mut fds = self.fds.borrow_mut();
        if fd < 0 || fds.len() >= MAX_TIMERS || fds.iter().any(|(f, _)| *f == fd) {
            return Err(HostError::Message("fd refused"));
        }
        fds.push((fd, flags));
        Ok(())
    }
    fn modify_fd(&self, fd: std::os::fd::RawFd, flags: FdFlags) -> Result<(), HostError> {
        match self.fds.borrow_mut().iter_mut().find(|(f, _)| *f == fd) {
            Some(e) => {
                e.1 = flags;
                Ok(())
            }
            None => Err(HostError::Message("unknown fd")),
        }
    }
    fn unregister_fd(&self, fd: std::os::fd::RawFd) -> Result<(), HostError> {
        let mut fds = self.fds.borrow_mut();
        let before = fds.len();
        fds.retain(|(f, _)| *f != fd);
        if fds.len() == before { Err(HostError::Message("unknown fd")) } else { Ok(()) }
    }
}

impl HostParamsImplMainThread for MainThread {
    fn rescan(&self, _flags: ParamRescanFlags) {
        self.rescan_requested.set(true);
    }
    fn clear(&self, _param_id: ClapId, _flags: ParamClearFlags) {}
}

/// Host handlers for scanning: no editor support needed.
pub struct MinimalHost;

impl HostHandlers for MinimalHost {
    type Shared<'a> = Shared;
    type MainThread<'a> = MainThread;
    type AudioProcessor<'a> = ();
}

/// Register the editor-related host extensions (live instances).
pub fn declare_editor_extensions<H>(builder: &mut HostExtensions<H>)
where
    H: HostHandlers + for<'a> HostHandlers<Shared<'a> = Shared, MainThread<'a> = MainThread>,
{
    builder.register::<HostTimer>().register::<HostGui>().register::<HostParams>();
    #[cfg(unix)]
    builder.register::<HostPosixFd>();
}
