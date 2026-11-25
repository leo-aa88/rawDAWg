//! A cell that makes a main-thread-only value storable inside `Send` containers
//! while refusing access from any other thread.
//!
//! rawDAWg keeps every main-thread processor "mirror" in one map of
//! `Box<dyn Processor>` (a `Send` trait object). A CLAP plugin instance is
//! `!Send` by construction (the ABI binds its main-thread functions to the thread
//! that created it). The cell carries the creating thread's id and hands the value
//! out only on that thread; elsewhere `get()` returns `None`, so a misuse fails
//! loudly instead of calling the plugin from the wrong thread. Dropping the cell on
//! another thread leaks the value (logged) rather than destroying it off-thread.

use std::mem::ManuallyDrop;
use std::thread::ThreadId;

pub struct MainThreadCell<T> {
    value: ManuallyDrop<T>,
    owner: ThreadId,
}

// SAFETY: `value` is only reachable through `get`/`get_mut`/`into_inner`, which verify that
// the calling thread is the thread that created the cell, and `Drop` destroys the value only
// on that thread (otherwise it is leaked). A foreign thread can therefore never run the
// value's code.
unsafe impl<T> Send for MainThreadCell<T> {}

impl<T> MainThreadCell<T> {
    pub fn new(value: T) -> Self {
        Self { value: ManuallyDrop::new(value), owner: std::thread::current().id() }
    }

    pub fn on_owner_thread(&self) -> bool {
        std::thread::current().id() == self.owner
    }

    pub fn get(&self) -> Option<&T> {
        self.on_owner_thread().then_some(&*self.value)
    }

    pub fn get_mut(&mut self) -> Option<&mut T> {
        self.on_owner_thread().then_some(&mut *self.value)
    }

    /// Take the value out (owner thread only; elsewhere the cell is returned untouched).
    pub fn into_inner(mut self) -> Result<T, Self> {
        if self.on_owner_thread() {
            // SAFETY: `value` is taken exactly once; `self` is forgotten right after so
            // `Drop` never sees the moved-out field.
            let v = unsafe { ManuallyDrop::take(&mut self.value) };
            std::mem::forget(self);
            Ok(v)
        } else {
            Err(self)
        }
    }
}

impl<T> Drop for MainThreadCell<T> {
    fn drop(&mut self) {
        if self.on_owner_thread() {
            // SAFETY: dropped exactly once, on the owner thread.
            unsafe { ManuallyDrop::drop(&mut self.value) }
        } else {
            log::warn!("main-thread value dropped on another thread; leaking it instead of destroying it off-thread");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_creating_thread_sees_the_value() {
        let mut cell = MainThreadCell::new(String::from("x"));
        assert_eq!(cell.get().map(String::as_str), Some("x"));
        cell.get_mut().unwrap().push('y');
        let cell = std::thread::spawn(move || {
            assert!(cell.get().is_none(), "foreign thread gets nothing");
            cell
        })
        .join()
        .unwrap();
        assert_eq!(cell.into_inner().ok().as_deref(), Some("xy"));
        // a cell moved to another thread and taken there stays sealed
        let cell = MainThreadCell::new(1u8);
        let back = std::thread::spawn(move || cell.into_inner().expect_err("sealed off-thread")).join().unwrap();
        assert_eq!(back.into_inner().ok(), Some(1));
    }
}
