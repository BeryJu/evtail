//! Turning an `EVT_HANDLE` into an [`Event`].
//!
//! Two calls per event in the common case: `EvtRender` for the System section
//! and `EvtFormatMessage` for the description. Publisher metadata is cached,
//! including the misses — re-resolving a publisher that has no manifest on
//! every single event is the obvious way to make this slow.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use eyre::{Result, WrapErr};
use windows::Win32::Foundation::{ERROR_EVT_UNRESOLVED_VALUE_INSERT, ERROR_INSUFFICIENT_BUFFER};
use windows::Win32::System::EventLog::{
    EVT_HANDLE, EVT_SYSTEM_PROPERTY_ID, EVT_VARIANT, EVT_VARIANT_TYPE, EVT_VARIANT_TYPE_MASK,
    EvtCreateRenderContext, EvtFormatMessage, EvtFormatMessageEvent, EvtOpenPublisherMetadata,
    EvtRender, EvtRenderContextSystem, EvtRenderEventValues, EvtRenderEventXml, EvtSystemChannel,
    EvtSystemComputer, EvtSystemEventID, EvtSystemEventRecordId, EvtSystemLevel,
    EvtSystemProviderName, EvtSystemTimeCreated, EvtVarTypeByte, EvtVarTypeFileTime,
    EvtVarTypeHexInt32, EvtVarTypeHexInt64, EvtVarTypeInt16, EvtVarTypeInt32, EvtVarTypeInt64,
    EvtVarTypeSByte, EvtVarTypeString, EvtVarTypeUInt16, EvtVarTypeUInt32, EvtVarTypeUInt64,
};
use windows::core::PCWSTR;

use super::handle::EvtHandle;
use super::{is_win32, wide};
use crate::event::Event;

/// 100ns ticks between the FILETIME epoch (1601-01-01) and the Unix epoch.
const FILETIME_TO_UNIX: u64 = 116_444_736_000_000_000;

pub struct Renderer {
    ctx: EvtHandle,
    publishers: HashMap<String, Option<EvtHandle>>,
    /// Scratch for `EvtRender`'s `EVT_VARIANT` array. `u64` rather than `u8`
    /// because the buffer has to be 8-byte aligned.
    values: Vec<u64>,
    text: Vec<u16>,
    want_xml: bool,
}

impl Renderer {
    pub fn new(want_xml: bool) -> Result<Self> {
        let ctx = unsafe { EvtCreateRenderContext(None, EvtRenderContextSystem.0) }
            .wrap_err("EvtCreateRenderContext failed")?;
        Ok(Self {
            ctx: unsafe { EvtHandle::new(ctx) },
            publishers: HashMap::new(),
            values: vec![0; 128],
            text: vec![0; 1024],
            want_xml,
        })
    }

    pub fn render(&mut self, evt: EVT_HANDLE) -> Result<Event> {
        let sys = self.system(evt)?;
        let xml = if self.want_xml {
            Some(self.xml(evt)?)
        } else {
            None
        };

        let meta = self.publisher(&sys.provider);
        let message = match self.format_message(evt, meta) {
            Some(m) => m,
            None => {
                // No manifest, or no message for this id: show the event's own
                // data rather than nothing at all.
                let raw = match &xml {
                    Some(x) => x.clone(),
                    None => self.xml(evt).unwrap_or_default(),
                };
                data_values(&raw).join(" ")
            }
        };

        Ok(Event {
            time: sys
                .filetime
                .and_then(filetime_to_utc)
                .unwrap_or(DateTime::UNIX_EPOCH),
            record_id: sys.record_id,
            level: sys.level,
            event_id: sys.event_id,
            provider: sys.provider,
            channel: sys.channel,
            computer: sys.computer,
            message,
            xml,
        })
    }

    /// The System section, copied out into owned values.
    fn system(&mut self, evt: EVT_HANDLE) -> Result<SystemFields> {
        let mut used = 0u32;
        let mut count = 0u32;
        let flags = EvtRenderEventValues.0;

        unsafe {
            let first = EvtRender(
                Some(self.ctx.raw()),
                evt,
                flags,
                byte_len(&self.values),
                Some(self.values.as_mut_ptr().cast()),
                &mut used,
                &mut count,
            );
            if let Err(e) = first {
                if !is_win32(&e, ERROR_INSUFFICIENT_BUFFER) {
                    return Err(e).wrap_err("EvtRender failed for the System section");
                }
                self.values.resize((used as usize).div_ceil(8).max(1), 0);
                EvtRender(
                    Some(self.ctx.raw()),
                    evt,
                    flags,
                    byte_len(&self.values),
                    Some(self.values.as_mut_ptr().cast()),
                    &mut used,
                    &mut count,
                )
                .wrap_err("EvtRender failed for the System section")?;
            }

            let variants = std::slice::from_raw_parts(
                self.values.as_ptr().cast::<EVT_VARIANT>(),
                count as usize,
            );
            Ok(SystemFields {
                provider: pick(variants, EvtSystemProviderName)
                    .and_then(as_string)
                    .unwrap_or_default(),
                event_id: pick(variants, EvtSystemEventID)
                    .and_then(as_u64)
                    .unwrap_or_default() as u32,
                level: pick(variants, EvtSystemLevel)
                    .and_then(as_u64)
                    .unwrap_or_default() as u8,
                filetime: pick(variants, EvtSystemTimeCreated).and_then(as_filetime),
                record_id: pick(variants, EvtSystemEventRecordId).and_then(as_u64),
                channel: pick(variants, EvtSystemChannel)
                    .and_then(as_string)
                    .unwrap_or_default(),
                computer: pick(variants, EvtSystemComputer)
                    .and_then(as_string)
                    .unwrap_or_default(),
            })
        }
    }

    fn xml(&mut self, evt: EVT_HANDLE) -> Result<String> {
        let mut used = 0u32;
        let mut count = 0u32;
        let flags = EvtRenderEventXml.0;

        unsafe {
            let first = EvtRender(
                None,
                evt,
                flags,
                char_len(&self.text),
                Some(self.text.as_mut_ptr().cast()),
                &mut used,
                &mut count,
            );
            if let Err(e) = first {
                if !is_win32(&e, ERROR_INSUFFICIENT_BUFFER) {
                    return Err(e).wrap_err("EvtRender failed for the event XML");
                }
                self.text.resize((used as usize).div_ceil(2).max(1), 0);
                EvtRender(
                    None,
                    evt,
                    flags,
                    char_len(&self.text),
                    Some(self.text.as_mut_ptr().cast()),
                    &mut used,
                    &mut count,
                )
                .wrap_err("EvtRender failed for the event XML")?;
            }
            // `used` is a byte count even though the buffer holds u16s.
            let chars = (used as usize / 2).min(self.text.len());
            Ok(from_utf16(&self.text[..chars]))
        }
    }

    /// The publisher metadata handle for a provider, opening and caching it on
    /// first use. `None` means the publisher is not registered on this machine.
    fn publisher(&mut self, provider: &str) -> Option<EVT_HANDLE> {
        if !self.publishers.contains_key(provider) {
            let name = wide(provider);
            let opened = unsafe {
                EvtOpenPublisherMetadata(
                    None,
                    PCWSTR::from_raw(name.as_ptr()),
                    PCWSTR::null(),
                    0,
                    0,
                )
            };
            let entry = opened.ok().map(|h| unsafe { EvtHandle::new(h) });
            self.publishers.insert(provider.to_owned(), entry);
        }
        self.publishers.get(provider)?.as_ref().map(EvtHandle::raw)
    }

    fn format_message(&mut self, evt: EVT_HANDLE, meta: Option<EVT_HANDLE>) -> Option<String> {
        meta?;
        let mut used = 0u32;
        let flags = EvtFormatMessageEvent.0;

        unsafe {
            if let Err(e) = EvtFormatMessage(meta, Some(evt), 0, None, flags, None, &mut used) {
                // UNRESOLVED_VALUE_INSERT reports failure but still fills the
                // buffer with everything it could resolve, which is exactly
                // what Event Viewer shows in the same situation.
                let expected = is_win32(&e, ERROR_INSUFFICIENT_BUFFER)
                    || is_win32(&e, ERROR_EVT_UNRESOLVED_VALUE_INSERT);
                if !expected {
                    return None;
                }
            }
            if used == 0 {
                return None;
            }
            self.text.resize(used as usize, 0);
            let second = EvtFormatMessage(
                meta,
                Some(evt),
                0,
                None,
                flags,
                Some(&mut self.text),
                &mut used,
            );
            if let Err(e) = second
                && !is_win32(&e, ERROR_EVT_UNRESOLVED_VALUE_INSERT)
            {
                return None;
            }
            let chars = (used as usize).min(self.text.len());
            let text = from_utf16(&self.text[..chars]);
            (!text.trim().is_empty()).then_some(text)
        }
    }
}

struct SystemFields {
    provider: String,
    event_id: u32,
    level: u8,
    filetime: Option<u64>,
    record_id: Option<u64>,
    channel: String,
    computer: String,
}

fn byte_len(buf: &[u64]) -> u32 {
    u32::try_from(buf.len().saturating_mul(8)).unwrap_or(u32::MAX)
}

fn char_len(buf: &[u16]) -> u32 {
    u32::try_from(buf.len().saturating_mul(2)).unwrap_or(u32::MAX)
}

fn from_utf16(buf: &[u16]) -> String {
    String::from_utf16_lossy(buf)
        .trim_end_matches('\0')
        .to_owned()
}

fn filetime_to_utc(ticks: u64) -> Option<DateTime<Utc>> {
    let since_epoch = ticks.checked_sub(FILETIME_TO_UNIX)?;
    let secs = i64::try_from(since_epoch / 10_000_000).ok()?;
    let nanos = u32::try_from(since_epoch % 10_000_000).ok()? * 100;
    DateTime::from_timestamp(secs, nanos)
}

fn pick(variants: &[EVT_VARIANT], id: EVT_SYSTEM_PROPERTY_ID) -> Option<&EVT_VARIANT> {
    let v = variants.get(usize::try_from(id.0).ok()?)?;
    (variant_type(v) != EVT_VARIANT_TYPE(0)).then_some(v)
}

fn variant_type(v: &EVT_VARIANT) -> EVT_VARIANT_TYPE {
    // The array bit is not interesting here; the System section is scalars.
    EVT_VARIANT_TYPE((v.Type & EVT_VARIANT_TYPE_MASK) as i32)
}

fn as_string(v: &EVT_VARIANT) -> Option<String> {
    if variant_type(v) != EvtVarTypeString {
        return None;
    }
    unsafe {
        let s = v.Anonymous.StringVal;
        if s.is_null() {
            None
        } else {
            s.to_string().ok()
        }
    }
}

fn as_u64(v: &EVT_VARIANT) -> Option<u64> {
    unsafe {
        let a = &v.Anonymous;
        let t = variant_type(v);
        Some(match t {
            t if t == EvtVarTypeByte => u64::from(a.ByteVal),
            t if t == EvtVarTypeUInt16 => u64::from(a.UInt16Val),
            t if t == EvtVarTypeUInt32 || t == EvtVarTypeHexInt32 => u64::from(a.UInt32Val),
            t if t == EvtVarTypeUInt64 || t == EvtVarTypeHexInt64 => a.UInt64Val,
            t if t == EvtVarTypeSByte => u64::try_from(a.SByteVal).ok()?,
            t if t == EvtVarTypeInt16 => u64::try_from(a.Int16Val).ok()?,
            t if t == EvtVarTypeInt32 => u64::try_from(a.Int32Val).ok()?,
            t if t == EvtVarTypeInt64 => u64::try_from(a.Int64Val).ok()?,
            _ => return None,
        })
    }
}

fn as_filetime(v: &EVT_VARIANT) -> Option<u64> {
    if variant_type(v) != EvtVarTypeFileTime {
        return None;
    }
    unsafe { Some(v.Anonymous.FileTimeVal) }
}

/// Pull the `<Data>` values out of event XML, for events whose publisher has no
/// message table. This is the "the description for event id ... cannot be
/// found" case, where the raw inserts are all the information there is.
fn data_values(xml: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = xml;
    while let Some(start) = rest.find("<Data") {
        rest = &rest[start + "<Data".len()..];
        let Some(close) = rest.find('>') else { break };
        let self_closing = rest[..close].ends_with('/');
        rest = &rest[close + 1..];
        if self_closing {
            continue;
        }
        let Some(end) = rest.find("</Data>") else {
            break;
        };
        let text = unescape(&rest[..end]);
        if !text.trim().is_empty() {
            out.push(text);
        }
        rest = &rest[end + "</Data>".len()..];
    }
    out
}

fn unescape(s: &str) -> String {
    if !s.contains('&') {
        return s.to_owned();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        rest = &rest[amp..];
        match rest.find(';').map(|i| (&rest[..=i], i)) {
            Some((entity, i)) => {
                match entity {
                    "&lt;" => out.push('<'),
                    "&gt;" => out.push('>'),
                    "&amp;" => out.push('&'),
                    "&quot;" => out.push('"'),
                    "&apos;" => out.push('\''),
                    _ => match numeric_entity(entity) {
                        Some(c) => out.push(c),
                        None => out.push_str(entity),
                    },
                }
                rest = &rest[i + 1..];
            }
            None => {
                out.push_str(rest);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

fn numeric_entity(entity: &str) -> Option<char> {
    let body = entity.strip_prefix("&#")?.strip_suffix(';')?;
    let code = match body.strip_prefix(['x', 'X']) {
        Some(hex) => u32::from_str_radix(hex, 16).ok()?,
        None => body.parse().ok()?,
    };
    char::from_u32(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filetime_converts_to_utc() {
        // 2026-08-21T00:00:00Z as a FILETIME.
        let ticks = FILETIME_TO_UNIX + 1_787_270_400 * 10_000_000;
        let dt = filetime_to_utc(ticks);
        assert_eq!(
            dt.map(|d| d.to_rfc3339()),
            Some("2026-08-21T00:00:00+00:00".to_owned())
        );
        // Pre-1970 timestamps must not wrap around.
        assert!(filetime_to_utc(0).is_none());
    }

    #[test]
    fn data_values_are_pulled_out_of_event_xml() {
        let xml = "<Event><EventData>\
            <Data Name='param1'>The Spooler service</Data>\
            <Data Name='param2'>%%1053</Data>\
            <Data Name='empty'/>\
            <Data Name='blank'>  </Data>\
            </EventData></Event>";
        assert_eq!(data_values(xml), vec!["The Spooler service", "%%1053"]);
        assert!(data_values("<Event/>").is_empty());
    }

    #[test]
    fn data_values_are_unescaped() {
        let xml = "<Event><Data>a &lt;b&gt; &amp; &quot;c&quot; &#65;&#x42;</Data></Event>";
        assert_eq!(data_values(xml), vec!["a <b> & \"c\" AB"]);
    }

    #[test]
    fn unescape_leaves_unknown_entities_alone() {
        assert_eq!(unescape("100% &nbsp; done"), "100% &nbsp; done");
        assert_eq!(unescape("no entities"), "no entities");
        assert_eq!(unescape("dangling & amp"), "dangling & amp");
    }
}
