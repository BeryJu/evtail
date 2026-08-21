//! Channel discovery for `--list-channels`.

use eyre::{Result, WrapErr};
use windows::Win32::Foundation::{ERROR_INSUFFICIENT_BUFFER, ERROR_NO_MORE_ITEMS};
use windows::Win32::System::EventLog::{EvtNextChannelPath, EvtOpenChannelEnum};

use super::handle::EvtHandle;
use super::is_win32;

pub fn list() -> Result<Vec<String>> {
    let en = unsafe { EvtOpenChannelEnum(None, 0) }.wrap_err("EvtOpenChannelEnum failed")?;
    let en = unsafe { EvtHandle::new(en) };

    let mut names = Vec::new();
    let mut buf = vec![0u16; 512];
    loop {
        let mut used = 0u32;
        match unsafe { EvtNextChannelPath(en.raw(), Some(&mut buf), &mut used) } {
            Ok(()) => {}
            Err(e) if is_win32(&e, ERROR_NO_MORE_ITEMS) => break,
            Err(e) if is_win32(&e, ERROR_INSUFFICIENT_BUFFER) => {
                // The enumerator does not advance on failure, so growing the
                // buffer and asking again returns the same channel.
                buf.resize(used as usize, 0);
                continue;
            }
            Err(e) => return Err(e).wrap_err("EvtNextChannelPath failed"),
        }
        let chars = (used as usize).min(buf.len());
        let name = String::from_utf16_lossy(&buf[..chars]);
        names.push(name.trim_end_matches('\0').to_owned());
    }

    names.sort_by_key(|n| n.to_lowercase());
    Ok(names)
}
