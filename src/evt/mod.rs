//! Safe wrappers over the Windows Event Log API (`wevtapi.dll`).
//!
//! All of the crate's `unsafe` lives under this module, and every raw handle
//! leaves it wrapped in [`handle::EvtHandle`] so `EvtClose` is never called by
//! hand.

pub mod channels;
pub mod follow;
pub mod handle;
pub mod reader;
pub mod render;

use windows::Win32::Foundation::WIN32_ERROR;
use windows::core::{Error, HRESULT};

/// NUL-terminated UTF-16, ready to be handed out as a `PCWSTR`.
pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Whether a `windows` error is a particular Win32 status.
pub fn is_win32(e: &Error, code: WIN32_ERROR) -> bool {
    e.code() == HRESULT::from_win32(code.0)
}
