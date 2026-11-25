//! The clack host handlers rawDAWg presents to plugins. Shared by the scanner
//! (description only) and the in-process bridge (M7 slice 3).

use clack_host::prelude::*;
use std::sync::atomic::{AtomicBool, Ordering};

pub const HOST_NAME: &str = "rawDAWg";
pub const HOST_VENDOR: &str = "rawDAWg";
pub const HOST_URL: &str = "https://github.com/leonardo-aa/rawDAWg";

pub fn host_info() -> HostInfo {
    HostInfo::new(HOST_NAME, HOST_VENDOR, HOST_URL, rawdawg_types::VERSION).expect("static host info is valid")
}

/// Thread-safe host data: plugin requests are recorded as flags the owner polls.
#[derive(Default)]
pub struct Shared {
    pub restart_requested: AtomicBool,
    pub process_requested: AtomicBool,
    pub callback_requested: AtomicBool,
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

#[derive(Default)]
pub struct MainThread;

impl<'a> MainThreadHandler<'a> for MainThread {}

pub struct MinimalHost;

impl HostHandlers for MinimalHost {
    type Shared<'a> = Shared;
    type MainThread<'a> = MainThread;
    type AudioProcessor<'a> = ();
}
