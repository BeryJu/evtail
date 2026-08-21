//! Writing events out, in each of the four formats.

use std::io::{self, Write};

use chrono::{Local, SecondsFormat};
use serde::Serialize;

use crate::cli::Format;
use crate::event::{Event, squash, tidy};

const RESET: &str = "\x1b[0m";
const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";
const CYAN: &str = "\x1b[36m";

fn level_color(level: u8) -> &'static str {
    match level {
        1 => "\x1b[1;31m",
        2 => "\x1b[31m",
        3 => "\x1b[33m",
        5 => "\x1b[2m",
        _ => "\x1b[32m",
    }
}

pub struct Printer<W: Write> {
    out: W,
    format: Format,
    color: bool,
    utc: bool,
    headers: bool,
    /// Follow mode flushes per event; a bulk read flushes once at the end.
    flush_each: bool,
    last_header: Option<String>,
    wrote_any: bool,
}

impl<W: Write> Printer<W> {
    pub fn new(out: W, format: Format, color: bool, utc: bool, headers: bool) -> Self {
        Self {
            out,
            format,
            color,
            utc,
            headers,
            flush_each: false,
            last_header: None,
            wrote_any: false,
        }
    }

    pub fn flush_each(mut self, yes: bool) -> Self {
        self.flush_each = yes;
        self
    }

    pub fn print(&mut self, target: &str, ev: &Event) -> io::Result<()> {
        self.header(target)?;
        match self.format {
            Format::Line => self.line(ev)?,
            Format::Full => self.full(ev)?,
            Format::Json => self.json(target, ev)?,
            Format::Xml => writeln!(self.out, "{}", ev.xml.as_deref().unwrap_or_default())?,
        }
        self.wrote_any = true;
        if self.flush_each {
            self.out.flush()?;
        }
        Ok(())
    }

    pub fn flush(&mut self) -> io::Result<()> {
        self.out.flush()
    }

    /// `==> System <==`, printed like tail does: once per switch of target,
    /// preceded by a blank line after the first block.
    fn header(&mut self, target: &str) -> io::Result<()> {
        if !self.headers || self.last_header.as_deref() == Some(target) {
            return Ok(());
        }
        if self.wrote_any {
            writeln!(self.out)?;
        }
        let (a, b) = self.paint(BOLD);
        writeln!(self.out, "{a}==> {target} <=={b}")?;
        self.last_header = Some(target.to_owned());
        Ok(())
    }

    fn line(&mut self, ev: &Event) -> io::Result<()> {
        let (dim, dim_end) = self.paint(DIM);
        let (lvl, lvl_end) = self.paint(level_color(ev.level));
        let (cyan, cyan_end) = self.paint(CYAN);
        writeln!(
            self.out,
            "{dim}{time}{dim_end} {lvl}{level:<7}{lvl_end} {cyan}{provider}[{id}]{cyan_end} {msg}",
            time = self.timestamp(ev),
            level = ev.level_name(),
            provider = ev.provider,
            id = ev.event_id,
            msg = squash(&ev.message),
        )
    }

    fn full(&mut self, ev: &Event) -> io::Result<()> {
        let (dim, dim_end) = self.paint(DIM);
        let (lvl, lvl_end) = self.paint(level_color(ev.level));
        let (cyan, cyan_end) = self.paint(CYAN);
        let record = match ev.record_id {
            Some(id) => format!(" record {id}"),
            None => String::new(),
        };
        writeln!(
            self.out,
            "{dim}{time}{dim_end} {lvl}{level:<7}{lvl_end} {cyan}{provider}[{id}]{cyan_end}\
             {dim}{record} on {channel} ({computer}){dim_end}",
            time = self.timestamp(ev),
            level = ev.level_name(),
            provider = ev.provider,
            id = ev.event_id,
            channel = ev.channel,
            computer = ev.computer,
        )?;
        let body = tidy(&ev.message);
        if !body.is_empty() {
            writeln!(self.out, "{body}")?;
        }
        writeln!(self.out)
    }

    fn json(&mut self, target: &str, ev: &Event) -> io::Result<()> {
        let time = if self.utc {
            ev.time.to_rfc3339_opts(SecondsFormat::Millis, true)
        } else {
            ev.time
                .with_timezone(&Local)
                .to_rfc3339_opts(SecondsFormat::Millis, false)
        };
        let record = JsonEvent {
            time,
            target,
            level: ev.level,
            level_name: ev.level_name(),
            provider: &ev.provider,
            event_id: ev.event_id,
            record_id: ev.record_id,
            channel: &ev.channel,
            computer: &ev.computer,
            message: &ev.message,
        };
        match serde_json::to_string(&record) {
            Ok(json) => writeln!(self.out, "{json}"),
            Err(e) => Err(io::Error::other(e)),
        }
    }

    fn timestamp(&self, ev: &Event) -> String {
        if self.utc {
            ev.time.format("%Y-%m-%d %H:%M:%S").to_string()
        } else {
            ev.time
                .with_timezone(&Local)
                .format("%Y-%m-%d %H:%M:%S")
                .to_string()
        }
    }

    /// The (prefix, suffix) pair for one styled span, empty when not coloring.
    fn paint(&self, code: &'static str) -> (&'static str, &'static str) {
        if self.color { (code, RESET) } else { ("", "") }
    }
}

#[derive(Serialize)]
struct JsonEvent<'a> {
    time: String,
    target: &'a str,
    level: u8,
    level_name: &'a str,
    provider: &'a str,
    event_id: u32,
    record_id: Option<u64>,
    channel: &'a str,
    computer: &'a str,
    message: &'a str,
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use chrono::DateTime;

    use super::*;

    fn event(message: &str) -> Event {
        Event {
            time: DateTime::from_timestamp(1_755_000_000, 0).unwrap(),
            record_id: Some(447_273),
            level: 3,
            event_id: 7000,
            provider: "Service Control Manager".to_owned(),
            channel: "System".to_owned(),
            computer: "DESK".to_owned(),
            message: message.to_owned(),
            xml: None,
        }
    }

    fn render(format: Format, headers: bool, events: &[(&str, Event)]) -> String {
        let mut buf = Vec::new();
        {
            let mut p = Printer::new(&mut buf, format, false, true, headers);
            for (target, ev) in events {
                p.print(target, ev).unwrap();
            }
        }
        String::from_utf8(buf).unwrap()
    }

    #[test]
    fn line_format_is_one_line_per_event() {
        let out = render(
            Format::Line,
            false,
            &[("System", event("failed to\r\n  start"))],
        );
        assert_eq!(
            out,
            "2025-08-12 12:00:00 WARN    Service Control Manager[7000] failed to start\n"
        );
    }

    #[test]
    fn headers_print_once_per_switch() {
        let out = render(
            Format::Line,
            true,
            &[
                ("System", event("a")),
                ("System", event("b")),
                ("Application", event("c")),
            ],
        );
        let headers: Vec<&str> = out.lines().filter(|l| l.starts_with("==>")).collect();
        assert_eq!(headers, vec!["==> System <==", "==> Application <=="]);
        // tail separates blocks with a blank line, but not before the first.
        assert!(out.starts_with("==> System <=="));
        assert!(out.contains("\n\n==> Application <=="));
    }

    #[test]
    fn full_format_keeps_the_message_intact() {
        let out = render(
            Format::Full,
            false,
            &[("System", event("line one\nline two\n\n"))],
        );
        let mut lines = out.lines();
        assert_eq!(
            lines.next(),
            Some(
                "2025-08-12 12:00:00 WARN    Service Control Manager[7000] record 447273 on System (DESK)"
            )
        );
        assert_eq!(lines.next(), Some("line one"));
        assert_eq!(lines.next(), Some("line two"));
        assert_eq!(lines.next(), Some(""));
    }

    #[test]
    fn json_format_is_one_object_per_line() {
        let out = render(Format::Json, false, &[("System", event("boom"))]);
        let v: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
        assert_eq!(v["time"], "2025-08-12T12:00:00.000Z");
        assert_eq!(v["event_id"], 7000);
        assert_eq!(v["level_name"], "WARN");
        assert_eq!(v["provider"], "Service Control Manager");
        assert_eq!(v["record_id"], 447_273);
        assert_eq!(v["message"], "boom");
    }

    #[test]
    fn color_wraps_only_when_asked() {
        let mut buf = Vec::new();
        let mut p = Printer::new(&mut buf, Format::Line, true, true, false);
        p.print("System", &event("boom")).unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(out.contains("\x1b[33mWARN"));
        assert!(!render(Format::Line, false, &[("System", event("boom"))]).contains('\x1b'));
    }
}
