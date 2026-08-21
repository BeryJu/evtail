//! The one record type everything else passes around.

use chrono::{DateTime, Utc};

/// Output shapes live in `output`; this stays a plain record.
#[derive(Debug, Clone)]
pub struct Event {
    pub time: DateTime<Utc>,
    pub record_id: Option<u64>,
    pub level: u8,
    pub event_id: u32,
    pub provider: String,
    pub channel: String,
    pub computer: String,
    /// The rendered description, or a best-effort reconstruction of it when the
    /// publisher's message table is missing.
    pub message: String,
    /// Only populated for `--format xml`; rendering it costs a second API call.
    pub xml: Option<String>,
}

impl Event {
    pub fn level_name(&self) -> &'static str {
        level_name(self.level)
    }
}

/// Names are hard-coded rather than read back with `EvtFormatMessageLevel` so
/// that output does not change with the system locale.
pub fn level_name(level: u8) -> &'static str {
    match level {
        1 => "CRIT",
        2 => "ERROR",
        3 => "WARN",
        // 0 is LogAlways, which Event Viewer displays as Information.
        0 | 4 => "INFO",
        5 => "VERBOSE",
        _ => "LEVEL?",
    }
}

/// Collapse an event description onto one line so the default output stays
/// grep- and cut-friendly the way real `tail` output is.
pub fn squash(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut pending_space = false;
    for c in s.chars() {
        if c.is_whitespace() {
            pending_space = !out.is_empty();
        } else {
            if pending_space {
                out.push(' ');
                pending_space = false;
            }
            out.push(c);
        }
    }
    out
}

/// Event descriptions habitually end in several blank lines; drop those and any
/// trailing whitespace per line, but keep the internal structure.
pub fn tidy(s: &str) -> String {
    let mut lines: Vec<&str> = s.lines().map(|l| l.trim_end()).collect();
    while lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn level_zero_reads_as_information() {
        assert_eq!(level_name(0), "INFO");
        assert_eq!(level_name(4), "INFO");
        assert_eq!(level_name(2), "ERROR");
        assert_eq!(level_name(9), "LEVEL?");
    }

    #[test]
    fn squash_flattens_whitespace_runs() {
        assert_eq!(
            squash("  the X service\r\n\r\n  failed.  "),
            "the X service failed."
        );
        assert_eq!(squash("\t"), "");
        assert_eq!(squash("one"), "one");
    }

    #[test]
    fn tidy_keeps_shape_but_drops_trailing_blanks() {
        assert_eq!(tidy("a  \r\n\r\nb\r\n\r\n\r\n"), "a\n\nb");
        assert_eq!(tidy(""), "");
    }
}
