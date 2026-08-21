//! The `-f` half: `EvtSubscribe` plus a wait loop.
//!
//! The subscription is created with a signal event rather than a callback, so
//! every `EvtNext` stays on the main thread — no `extern "system"` callback, no
//! raw context pointer to make `Send`, no re-entrancy. It also means several
//! channels can be followed at once by waiting on all of their signal events
//! together.

use std::ffi::c_void;
use std::sync::atomic::{AtomicIsize, Ordering};
use std::thread::sleep;
use std::time::Duration;

use eyre::{Result, WrapErr, bail};
use windows::Win32::Foundation::{
    ERROR_EVT_QUERY_RESULT_STALE, ERROR_NO_MORE_ITEMS, HANDLE, WAIT_FAILED, WAIT_OBJECT_0,
};
use windows::Win32::System::Console::SetConsoleCtrlHandler;
use windows::Win32::System::EventLog::{
    EVT_HANDLE, EvtNext, EvtSubscribe, EvtSubscribeStartAfterBookmark, EvtSubscribeToFutureEvents,
};
use windows::Win32::System::Threading::{
    CreateEventW, INFINITE, ResetEvent, SetEvent, WaitForMultipleObjects,
};
use windows::core::{BOOL, PCWSTR};

use super::handle::{EvtHandle, OwnedHandle};
use super::reader::{Target, friendly, retryable};
use super::render::Renderer;
use super::{is_win32, wide};
use crate::event::Event;

/// `WaitForMultipleObjects` tops out at 64 handles, one of which is our stop
/// event.
pub const MAX_FOLLOWED: usize = 63;

const BATCH: usize = 64;

/// Set by the console control handler, read by nothing else.
static STOP_EVENT: AtomicIsize = AtomicIsize::new(0);

/// One followed target.
pub struct Subscription {
    target: Target,
    sub: EvtHandle,
    signal: OwnedHandle,
}

/// Create the event that Ctrl-C signals, and install the handler that sets it.
///
/// Without this, Ctrl-C kills the process mid-write and the last line can be
/// left half-flushed.
pub fn install_stop_handler() -> Result<OwnedHandle> {
    let handle = unsafe { CreateEventW(None, true, false, PCWSTR::null()) }
        .wrap_err("CreateEventW failed for the stop event")?;
    let stop = unsafe { OwnedHandle::new(handle) };
    STOP_EVENT.store(handle.0 as isize, Ordering::SeqCst);
    unsafe { SetConsoleCtrlHandler(Some(ctrl_handler), true) }
        .wrap_err("SetConsoleCtrlHandler failed")?;
    Ok(stop)
}

unsafe extern "system" fn ctrl_handler(_ctrl_type: u32) -> BOOL {
    let raw = STOP_EVENT.load(Ordering::SeqCst);
    if raw != 0 {
        let _ = unsafe { SetEvent(HANDLE(raw as *mut c_void)) };
    }
    // TRUE: handled, so the process is not torn down and the loop can exit.
    BOOL(1)
}

/// Subscribe to a channel, resuming immediately after `bookmark` when one was
/// produced by the historical read. Without that handoff, events written
/// between the read and the subscription would be missed.
pub fn subscribe(
    target: &Target,
    xpath: Option<&str>,
    bookmark: Option<&EvtHandle>,
    retry: bool,
) -> Result<Subscription> {
    if target.is_file {
        bail!(
            "--follow needs a channel; `{}` is a file, which cannot grow",
            target.name
        );
    }

    // Manual reset, and created *already signalled*: the service only arms the
    // notification once a subscriber has read through to ERROR_NO_MORE_ITEMS,
    // so the loop has to make that first draining pass unprompted. Starting
    // unsignalled means a quiet channel never wakes us at all.
    let signal = unsafe { CreateEventW(None, true, true, PCWSTR::null()) }
        .wrap_err("CreateEventW failed for a subscription")?;
    let signal = unsafe { OwnedHandle::new(signal) };

    let path = wide(&target.name);
    let query = xpath.map(wide);
    let flags = if bookmark.is_some() {
        EvtSubscribeStartAfterBookmark.0
    } else {
        EvtSubscribeToFutureEvents.0
    };

    let mut announced = false;
    loop {
        let handle = unsafe {
            EvtSubscribe(
                None,
                Some(signal.raw()),
                PCWSTR::from_raw(path.as_ptr()),
                query
                    .as_ref()
                    .map_or(PCWSTR::null(), |q| PCWSTR::from_raw(q.as_ptr())),
                bookmark.map(EvtHandle::raw),
                None,
                None,
                flags,
            )
        };
        match handle {
            Ok(sub) => {
                return Ok(Subscription {
                    target: target.clone(),
                    sub: unsafe { EvtHandle::new(sub) },
                    signal,
                });
            }
            Err(e) if retry && retryable(&e) => {
                if !announced {
                    eprintln!("evtail: waiting for `{}` to appear", target.name);
                    announced = true;
                }
                sleep(Duration::from_secs(1));
            }
            Err(e) => return Err(friendly(e, target)),
        }
    }
}

/// Wait for new events until Ctrl-C. `on_event` is called with the index of the
/// subscription that produced the event, so the caller can print tail's headers.
pub fn run(
    subs: &[Subscription],
    stop: &OwnedHandle,
    renderer: &mut Renderer,
    mut on_event: impl FnMut(&Target, &Event) -> Result<()>,
) -> Result<()> {
    let mut waits: Vec<HANDLE> = Vec::with_capacity(subs.len() + 1);
    waits.push(stop.raw());
    waits.extend(subs.iter().map(|s| s.signal.raw()));

    loop {
        let signalled = unsafe { WaitForMultipleObjects(&waits, false, INFINITE) };
        if signalled == WAIT_FAILED {
            return Err(windows::core::Error::from_thread())
                .wrap_err("WaitForMultipleObjects failed");
        }
        let index = (signalled.0 - WAIT_OBJECT_0.0) as usize;
        if index == 0 {
            return Ok(()); // Ctrl-C.
        }
        let Some(sub) = subs.get(index - 1) else {
            return Ok(());
        };

        // Reset before draining, not after: if the service signals while we
        // are still reading, resetting afterwards would swallow that wakeup
        // and the events behind it would sit unread until the next one.
        unsafe { ResetEvent(sub.signal.raw()) }.wrap_err("ResetEvent failed")?;
        drain(sub, renderer, &mut on_event)?;
    }
}

fn drain(
    sub: &Subscription,
    renderer: &mut Renderer,
    on_event: &mut impl FnMut(&Target, &Event) -> Result<()>,
) -> Result<()> {
    let mut handles = [0isize; BATCH];
    // Drain until the service says there is nothing left. Stopping early on a
    // short batch looks reasonable and is wrong: the service only re-signals
    // the event once a subscriber has read through to ERROR_NO_MORE_ITEMS, so
    // an early return means the next events arrive silently and sit there.
    loop {
        let mut returned = 0u32;
        match unsafe { EvtNext(sub.sub.raw(), &mut handles, 0, 0, &mut returned) } {
            Ok(()) => {}
            Err(e) if is_win32(&e, ERROR_NO_MORE_ITEMS) => return Ok(()),
            Err(e) if is_win32(&e, ERROR_EVT_QUERY_RESULT_STALE) => {
                // The channel wrapped past our position, tail's "file
                // truncated" moment. Newer events keep arriving.
                eprintln!(
                    "evtail: `{}` overwrote events before we read them",
                    sub.target.name
                );
                return Ok(());
            }
            Err(e) => return Err(friendly(e, &sub.target)),
        }

        if returned == 0 {
            return Ok(());
        }
        for raw in handles.iter().take(returned as usize) {
            let evt = unsafe { EvtHandle::new(EVT_HANDLE(*raw)) };
            let event = renderer.render(evt.raw())?;
            on_event(&sub.target, &event)?;
        }
    }
}
