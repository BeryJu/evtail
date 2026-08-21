//! RAII for the two kinds of handle this program owns.

use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::EventLog::{EVT_HANDLE, EvtClose};

/// An `EVT_HANDLE` (query, subscription, bookmark, render context, publisher
/// metadata) that closes itself.
#[derive(Debug)]
pub struct EvtHandle(EVT_HANDLE);

impl EvtHandle {
    /// # Safety
    /// `handle` must be a valid, owned `EVT_HANDLE`; it is closed on drop.
    pub unsafe fn new(handle: EVT_HANDLE) -> Self {
        Self(handle)
    }

    pub fn raw(&self) -> EVT_HANDLE {
        self.0
    }
}

impl Drop for EvtHandle {
    fn drop(&mut self) {
        // Nothing useful to do if this fails, and it must not panic in a drop.
        let _ = unsafe { EvtClose(self.0) };
    }
}

/// A kernel object handle; here, always an event used to wake up a subscriber.
#[derive(Debug)]
pub struct OwnedHandle(HANDLE);

impl OwnedHandle {
    /// # Safety
    /// `handle` must be a valid, owned kernel handle; it is closed on drop.
    pub unsafe fn new(handle: HANDLE) -> Self {
        Self(handle)
    }

    pub fn raw(&self) -> HANDLE {
        self.0
    }
}

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        let _ = unsafe { CloseHandle(self.0) };
    }
}
