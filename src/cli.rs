//! Command line surface. Kept deliberately close to `tail(1)`: the flags that
//! have an obvious event-log meaning keep their tail spelling, and everything
//! event-log specific is a long option.

use std::str::FromStr;

use chrono::{DateTime, Duration, Local, NaiveDate, NaiveDateTime, TimeZone, Utc};
use clap::{ArgAction, Parser, ValueEnum};

use crate::query::Filters;

/// What `tail` reads from stdin, we read from the System channel.
pub const DEFAULT_TARGET: &str = "System";

#[derive(Debug, Parser)]
#[command(
    name = "evtail",
    version,
    about = "tail(1) for the Windows Event Log",
    after_help = "EXAMPLES:
    evtail                                 last 10 events from System
    evtail -f Application                  follow the Application channel
    evtail -n 50 --level err System        last 50 events, error or worse
    evtail -f --provider \"Service Control Manager\" System
    evtail --format json -n 5 System | jq .
    evtail -n 100 C:\\logs\\export.evtx      read an exported .evtx file
    evtail --list-channels | findstr WinRM"
)]
pub struct Cli {
    /// Channel names (System, Application, Microsoft-Windows-WinRM/Operational)
    /// or paths to .evtx files.
    #[arg(value_name = "TARGET")]
    pub targets: Vec<String>,

    /// Output the last NUM events; with a leading '+', start at record id NUM.
    #[arg(
        short = 'n',
        long = "lines",
        value_name = "[+]NUM",
        default_value = "10"
    )]
    pub lines: Start,

    /// Print new events as they are written.
    #[arg(short = 'f', long)]
    pub follow: bool,

    /// Same as --follow --retry.
    #[arg(short = 'F', action = ArgAction::SetTrue)]
    pub follow_retry: bool,

    /// Keep trying to open a channel that is missing or unavailable.
    #[arg(long)]
    pub retry: bool,

    /// Never print the '==> TARGET <==' headers.
    #[arg(short = 'q', long, conflicts_with = "verbose")]
    pub quiet: bool,

    /// Always print the '==> TARGET <==' headers.
    #[arg(short = 'v', long)]
    pub verbose: bool,

    /// Show this severity and anything more severe.
    #[arg(long, value_name = "LEVEL")]
    pub level: Option<Level>,

    /// Only events from this provider (repeatable).
    #[arg(long = "provider", value_name = "NAME")]
    pub providers: Vec<String>,

    /// Only these event ids: NUM or NUM-NUM (repeatable).
    #[arg(long = "id", value_name = "ID[-ID]")]
    pub ids: Vec<IdSpec>,

    /// Only events newer than a duration (30s, 5m, 2h, 3d, 1w) or a timestamp.
    #[arg(long, value_name = "WHEN")]
    pub since: Option<Since>,

    /// Raw XPath query, used instead of the other filter flags.
    #[arg(
        long,
        value_name = "QUERY",
        conflicts_with_all = ["level", "providers", "ids", "since"]
    )]
    pub xpath: Option<String>,

    /// Output format.
    #[arg(long, value_enum, default_value_t = Format::Line, conflicts_with = "full")]
    pub format: Format,

    /// Print the complete message; same as --format full.
    #[arg(long)]
    pub full: bool,

    /// Colorize the output.
    #[arg(long, value_enum, default_value_t = ColorChoice::Auto, value_name = "WHEN")]
    pub color: ColorChoice,

    /// Print timestamps in UTC instead of local time.
    #[arg(long)]
    pub utc: bool,

    /// List the channels this machine knows about and exit.
    #[arg(long, exclusive = true)]
    pub list_channels: bool,
}

impl Cli {
    pub fn follow(&self) -> bool {
        self.follow || self.follow_retry
    }

    pub fn retry(&self) -> bool {
        self.retry || self.follow_retry
    }

    pub fn format(&self) -> Format {
        if self.full { Format::Full } else { self.format }
    }

    /// tail prints headers only when it was given more than one file, unless
    /// -q/-v say otherwise.
    pub fn show_headers(&self, target_count: usize) -> bool {
        !self.quiet && (self.verbose || target_count > 1)
    }

    pub fn target_names(&self) -> Vec<String> {
        if self.targets.is_empty() {
            vec![DEFAULT_TARGET.to_owned()]
        } else {
            self.targets.clone()
        }
    }

    pub fn filters(&self) -> Filters<'_> {
        Filters {
            level: self.level,
            providers: &self.providers,
            ids: &self.ids,
            since: self.since.as_ref(),
            min_record: match self.lines {
                Start::FromRecord(n) => Some(n),
                Start::Last(_) => None,
            },
            xpath: self.xpath.as_deref(),
        }
    }
}

/// Where to begin reading, mirroring tail's `-n NUM` / `-n +NUM` split.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Start {
    /// `-n NUM`: the last NUM events.
    Last(u64),
    /// `-n +NUM`: everything from record id NUM onwards.
    FromRecord(u64),
}

impl FromStr for Start {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        let parse = |digits: &str| -> Result<u64, String> {
            digits
                .parse::<u64>()
                .map_err(|_| format!("expected a number, got `{s}`"))
        };
        match s.strip_prefix('+') {
            Some(rest) => Ok(Start::FromRecord(parse(rest)?)),
            // tail also accepts `-n -10` as a synonym for `-n 10`.
            None => Ok(Start::Last(parse(s.strip_prefix('-').unwrap_or(s))?)),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Level {
    #[value(alias = "critical", alias = "1")]
    Crit,
    #[value(alias = "error", alias = "2")]
    Err,
    #[value(alias = "warning", alias = "3")]
    Warn,
    #[value(alias = "information", alias = "informational", alias = "4")]
    Info,
    #[value(alias = "debug", alias = "5")]
    Verbose,
}

impl Level {
    /// The numeric level as it appears in `System/Level`.
    pub fn num(self) -> u8 {
        match self {
            Level::Crit => 1,
            Level::Err => 2,
            Level::Warn => 3,
            Level::Info => 4,
            Level::Verbose => 5,
        }
    }
}

/// A single event id or an inclusive range of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdSpec {
    One(u32),
    Range(u32, u32),
}

impl FromStr for IdSpec {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        let num = |v: &str| -> Result<u32, String> {
            v.trim()
                .parse::<u32>()
                .map_err(|_| format!("`{v}` is not an event id"))
        };
        match s.split_once('-') {
            Some((lo, hi)) => {
                let (lo, hi) = (num(lo)?, num(hi)?);
                if lo > hi {
                    return Err(format!("range `{s}` runs backwards"));
                }
                Ok(IdSpec::Range(lo, hi))
            }
            None => Ok(IdSpec::One(num(s)?)),
        }
    }
}

/// `--since`, either a relative age or a fixed point in time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Since {
    /// Milliseconds of age, handed to XPath's `timediff()`.
    Ago(i64),
    At(DateTime<Utc>),
}

impl FromStr for Since {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        if let Some(d) = parse_duration(s) {
            return Ok(Since::Ago(d.num_milliseconds()));
        }
        if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
            return Ok(Since::At(dt.with_timezone(&Utc)));
        }
        // Anything without a zone is read as local time, which is what someone
        // typing `--since "2026-08-21 09:00"` means.
        let formats = [
            "%Y-%m-%d %H:%M:%S",
            "%Y-%m-%dT%H:%M:%S",
            "%Y-%m-%d %H:%M",
            "%Y-%m-%dT%H:%M",
        ];
        for fmt in formats {
            if let Ok(naive) = NaiveDateTime::parse_from_str(s, fmt) {
                return local_to_utc(naive).map(Since::At);
            }
        }
        if let Ok(date) = NaiveDate::parse_from_str(s, "%Y-%m-%d") {
            let midnight = date.and_hms_opt(0, 0, 0).ok_or("invalid date")?;
            return local_to_utc(midnight).map(Since::At);
        }
        Err(format!(
            "`{s}` is not a duration (30s, 5m, 2h, 3d, 1w) or a timestamp"
        ))
    }
}

fn local_to_utc(naive: NaiveDateTime) -> Result<DateTime<Utc>, String> {
    Local
        .from_local_datetime(&naive)
        .single()
        .map(|dt| dt.with_timezone(&Utc))
        .ok_or_else(|| format!("`{naive}` is ambiguous or skipped in the local time zone"))
}

/// `90s`, `5m`, `2h`, `3d`, `1w`; a bare number is seconds.
fn parse_duration(s: &str) -> Option<Duration> {
    let (digits, unit) = match s.char_indices().find(|(_, c)| !c.is_ascii_digit()) {
        Some((i, _)) => s.split_at(i),
        None => (s, "s"),
    };
    let n: i64 = digits.parse().ok()?;
    match unit {
        "s" | "sec" | "secs" => Duration::try_seconds(n),
        "m" | "min" | "mins" => Duration::try_minutes(n),
        "h" | "hr" | "hrs" => Duration::try_hours(n),
        "d" | "day" | "days" => Duration::try_days(n),
        "w" | "wk" | "weeks" => Duration::try_weeks(n),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Format {
    /// One squashed line per event.
    Line,
    /// Header line plus the complete message.
    Full,
    /// One JSON object per line.
    Json,
    /// The raw event XML.
    Xml,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ColorChoice {
    Auto,
    Always,
    Never,
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn cli(args: &[&str]) -> Cli {
        Cli::try_parse_from(std::iter::once("evtail").chain(args.iter().copied())).unwrap()
    }

    #[test]
    fn start_parses_both_tail_forms() {
        assert_eq!("10".parse(), Ok(Start::Last(10)));
        assert_eq!("-10".parse(), Ok(Start::Last(10)));
        assert_eq!("+447273".parse(), Ok(Start::FromRecord(447_273)));
        assert_eq!("0".parse(), Ok(Start::Last(0)));
        assert!("abc".parse::<Start>().is_err());
        assert!("+".parse::<Start>().is_err());
    }

    #[test]
    fn id_specs_cover_singles_and_ranges() {
        assert_eq!("7000".parse(), Ok(IdSpec::One(7000)));
        assert_eq!("100-200".parse(), Ok(IdSpec::Range(100, 200)));
        assert!("200-100".parse::<IdSpec>().is_err());
        assert!("7000-".parse::<IdSpec>().is_err());
    }

    #[test]
    fn durations_and_timestamps() {
        assert_eq!("2h".parse(), Ok(Since::Ago(7_200_000)));
        assert_eq!("90".parse(), Ok(Since::Ago(90_000)));
        assert_eq!("1w".parse(), Ok(Since::Ago(604_800_000)));
        let parsed = "2026-08-20T00:00:00Z".parse::<Since>();
        assert!(
            matches!(parsed, Ok(Since::At(dt)) if dt.to_rfc3339() == "2026-08-20T00:00:00+00:00")
        );
        assert!("later".parse::<Since>().is_err());
    }

    #[test]
    fn defaults_match_tail() {
        let c = cli(&[]);
        assert_eq!(c.target_names(), vec!["System".to_owned()]);
        assert_eq!(c.lines, Start::Last(10));
        assert!(!c.follow());
        assert_eq!(c.format(), Format::Line);
    }

    #[test]
    fn headers_follow_tails_rules() {
        assert!(!cli(&[]).show_headers(1));
        assert!(cli(&[]).show_headers(2));
        assert!(cli(&["-v"]).show_headers(1));
        assert!(!cli(&["-q"]).show_headers(2));
    }

    #[test]
    fn dash_f_implies_follow_and_retry() {
        let c = cli(&["-F"]);
        assert!(c.follow() && c.retry());
        assert!(!cli(&["-f"]).retry());
    }

    #[test]
    fn full_is_an_alias_for_the_format() {
        assert_eq!(cli(&["--full"]).format(), Format::Full);
    }

    #[test]
    fn xpath_rejects_the_filter_flags() {
        assert!(Cli::try_parse_from(["evtail", "--xpath", "*", "--level", "err"]).is_err());
    }
}
