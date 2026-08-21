//! Reading the events that already exist — the `-n` half of the job.

use std::thread::sleep;
use std::time::Duration;

use eyre::{Report, Result, WrapErr, eyre};
use windows::Win32::Foundation::{
    ERROR_ACCESS_DENIED, ERROR_EVT_CHANNEL_NOT_FOUND, ERROR_EVT_INVALID_CHANNEL_PATH,
    ERROR_EVT_INVALID_QUERY, ERROR_FILE_NOT_FOUND, ERROR_NO_MORE_ITEMS,
};
use windows::Win32::System::EventLog::{
    EVT_HANDLE, EvtCreateBookmark, EvtNext, EvtQuery, EvtQueryChannelPath, EvtQueryFilePath,
    EvtQueryForwardDirection, EvtQueryReverseDirection, EvtUpdateBookmark,
};
use windows::core::{Error, PCWSTR};

use super::handle::EvtHandle;
use super::render::Renderer;
use super::{is_win32, wide};
use crate::cli::Start;
use crate::event::Event;

/// How many event handles to pull out of a result set per `EvtNext`.
const BATCH: usize = 64;

/// A thing to tail: a channel, or an exported log file.
#[derive(Debug, Clone)]
pub struct Target {
    pub name: String,
    pub is_file: bool,
}

impl Target {
    /// Channel names legitimately contain `/`
    /// (`Microsoft-Windows-WinRM/Operational`), so only a `.evtx` extension or
    /// something that is unmistakably a path counts as a file.
    pub fn parse(name: &str) -> Self {
        let is_file = name.to_ascii_lowercase().ends_with(".evtx")
            || name.contains('\\')
            || name.contains(':');
        Self {
            name: name.to_owned(),
            is_file,
        }
    }
}

#[derive(Default)]
pub struct History {
    pub events: Vec<Event>,
    /// Marks the newest event read, so following can resume from exactly here.
    pub bookmark: Option<EvtHandle>,
}

/// Read the events selected by `start`, oldest first.
pub fn read(
    target: &Target,
    xpath: Option<&str>,
    start: Start,
    min_record: Option<u64>,
    retry: bool,
    renderer: &mut Renderer,
) -> Result<History> {
    let mut history = History::default();
    let (reverse, limit) = match start {
        // `-n 0`: no history, but following still has to start somewhere.
        Start::Last(0) => return Ok(history),
        Start::Last(n) => (true, Some(n)),
        Start::FromRecord(_) => (false, None),
    };

    let query = open_query(target, xpath, reverse, retry)?;
    let mut handles = [0isize; BATCH];

    'outer: loop {
        let mut returned = 0u32;
        let want = match limit {
            Some(n) => n
                .saturating_sub(history.events.len() as u64)
                .min(BATCH as u64) as usize,
            None => BATCH,
        };
        if want == 0 {
            break;
        }
        // Read until ERROR_NO_MORE_ITEMS rather than until a short batch:
        // EvtNext is free to hand back fewer events than asked for.
        let slice = &mut handles[..want];
        match unsafe { EvtNext(query.raw(), slice, 0, 0, &mut returned) } {
            Ok(()) => {}
            Err(e) if is_win32(&e, ERROR_NO_MORE_ITEMS) => break,
            Err(e) => return Err(friendly(e, target)),
        }

        for raw in slice.iter().take(returned as usize) {
            let evt = unsafe { EvtHandle::new(EVT_HANDLE(*raw)) };
            let event = renderer.render(evt.raw())?;

            // A raw --xpath leaves no room for the record filter in the query.
            if min_record.is_some_and(|min| event.record_id.is_some_and(|id| id < min)) {
                continue;
            }

            // In reverse the first event is the newest; going forward it is the
            // last one, so keep updating.
            if reverse {
                if history.bookmark.is_none() {
                    history.bookmark = Some(bookmark(evt.raw())?);
                }
            } else {
                match &history.bookmark {
                    Some(bm) => unsafe { EvtUpdateBookmark(bm.raw(), evt.raw()) }
                        .wrap_err("EvtUpdateBookmark failed")?,
                    None => history.bookmark = Some(bookmark(evt.raw())?),
                }
            }

            history.events.push(event);
            if limit.is_some_and(|n| history.events.len() as u64 >= n) {
                break 'outer;
            }
        }

        if returned == 0 {
            break;
        }
    }

    // A reverse query hands back newest-first; tail prints oldest-first.
    if reverse {
        history.events.reverse();
    }
    Ok(history)
}

/// A bookmark positioned at `evt`.
pub fn bookmark(evt: EVT_HANDLE) -> Result<EvtHandle> {
    let bm = unsafe { EvtCreateBookmark(PCWSTR::null()) }.wrap_err("EvtCreateBookmark failed")?;
    let bm = unsafe { EvtHandle::new(bm) };
    unsafe { EvtUpdateBookmark(bm.raw(), evt) }.wrap_err("EvtUpdateBookmark failed")?;
    Ok(bm)
}

fn open_query(
    target: &Target,
    xpath: Option<&str>,
    reverse: bool,
    retry: bool,
) -> Result<EvtHandle> {
    let path = wide(&target.name);
    let query = xpath.map(wide);
    let flags = if target.is_file {
        EvtQueryFilePath.0
    } else {
        EvtQueryChannelPath.0
    } | if reverse {
        EvtQueryReverseDirection.0
    } else {
        EvtQueryForwardDirection.0
    };

    let mut announced = false;
    loop {
        let handle = unsafe {
            EvtQuery(
                None,
                PCWSTR::from_raw(path.as_ptr()),
                query
                    .as_ref()
                    .map_or(PCWSTR::null(), |q| PCWSTR::from_raw(q.as_ptr())),
                flags,
            )
        };
        match handle {
            Ok(h) => return Ok(unsafe { EvtHandle::new(h) }),
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

/// Failures that `--retry` should sit and wait out.
pub fn retryable(e: &Error) -> bool {
    is_win32(e, ERROR_EVT_CHANNEL_NOT_FOUND) || is_win32(e, ERROR_FILE_NOT_FOUND)
}

/// Win32 status codes that deserve an explanation rather than an HRESULT.
pub fn friendly(e: Error, target: &Target) -> Report {
    if is_win32(&e, ERROR_EVT_CHANNEL_NOT_FOUND) || is_win32(&e, ERROR_EVT_INVALID_CHANNEL_PATH) {
        eyre!(
            "no channel named `{}` (try `evtail --list-channels`)",
            target.name
        )
    } else if is_win32(&e, ERROR_FILE_NOT_FOUND) {
        eyre!("no such file: `{}`", target.name)
    } else if is_win32(&e, ERROR_ACCESS_DENIED) {
        eyre!(
            "access denied reading `{}`; channels like Security need an elevated prompt",
            target.name
        )
    } else if is_win32(&e, ERROR_EVT_INVALID_QUERY) {
        eyre!("the event log service rejected the query: {}", e.message())
    } else {
        Report::new(e).wrap_err(format!("could not read `{}`", target.name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_paths_are_not_mistaken_for_files() {
        assert!(!Target::parse("System").is_file);
        assert!(!Target::parse("Microsoft-Windows-WinRM/Operational").is_file);
        assert!(Target::parse("export.evtx").is_file);
        assert!(Target::parse("EXPORT.EVTX").is_file);
        assert!(Target::parse(r"C:\logs\export.evtx").is_file);
        assert!(Target::parse(r".\export.evtx").is_file);
    }
}
