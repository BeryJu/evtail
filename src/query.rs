//! Turns the filter flags into a single XPath query.
//!
//! The query goes to the Event Log service, so `--id 7000 -n 5` stays fast on a
//! channel holding a million records: the service walks the log, we don't.
//!
//! These strings are passed straight to `EvtQuery`/`EvtSubscribe`, *not*
//! embedded in a `<QueryList>` document, so they must not be XML-escaped —
//! `<=` is written literally.

use crate::cli::{IdSpec, Level, Since};

#[derive(Debug, Default)]
pub struct Filters<'a> {
    pub level: Option<Level>,
    pub providers: &'a [String],
    pub ids: &'a [IdSpec],
    pub since: Option<&'a Since>,
    /// From tail's `-n +NUM`.
    pub min_record: Option<u64>,
    /// `--xpath`, which replaces everything above.
    pub xpath: Option<&'a str>,
}

impl Filters<'_> {
    /// The XPath to hand to the Event Log API, or `None` for "everything".
    pub fn to_xpath(&self) -> Option<String> {
        if let Some(raw) = self.xpath {
            // A raw query is used verbatim; `min_record` is applied while
            // reading instead, since two XPath expressions cannot be and-ed.
            return Some(raw.to_owned());
        }

        let mut terms: Vec<String> = Vec::new();

        if !self.providers.is_empty() {
            let names: Vec<String> = self
                .providers
                .iter()
                .map(|p| format!("Provider[@Name={}]", string_literal(p)))
                .collect();
            terms.push(format!("({})", names.join(" or ")));
        }

        if !self.ids.is_empty() {
            let ids: Vec<String> = self
                .ids
                .iter()
                .map(|id| match *id {
                    IdSpec::One(n) => format!("EventID={n}"),
                    IdSpec::Range(lo, hi) => format!("(EventID>={lo} and EventID<={hi})"),
                })
                .collect();
            terms.push(format!("({})", ids.join(" or ")));
        }

        if let Some(level) = self.level {
            terms.push(level_term(level));
        }

        match self.since {
            // `timediff` is evaluated by the service against its own clock.
            Some(Since::Ago(ms)) => {
                terms.push(format!("TimeCreated[timediff(@SystemTime) <= {ms}]"));
            }
            Some(Since::At(at)) => {
                let stamp = at.format("%Y-%m-%dT%H:%M:%S%.3fZ");
                terms.push(format!("TimeCreated[@SystemTime>='{stamp}']"));
            }
            None => {}
        }

        if let Some(record) = self.min_record {
            terms.push(format!("EventRecordID>={record}"));
        }

        if terms.is_empty() {
            None
        } else {
            Some(format!("*[System[{}]]", terms.join(" and ")))
        }
    }

    /// Record id that has to be enforced while reading because a raw `--xpath`
    /// left no room to express it in the query.
    pub fn client_side_min_record(&self) -> Option<u64> {
        self.xpath.and(self.min_record)
    }
}

/// Severity is a threshold, like `journalctl -p`: `--level err` means error and
/// worse. Level 0 (`LogAlways`) is displayed as Information by Event Viewer, so
/// a plain `Level<=N` would wrongly let it through for anything below Info.
fn level_term(level: Level) -> String {
    let n = level.num();
    if n >= 4 {
        format!("Level<={n}")
    } else {
        format!("(Level<={n} and Level!=0)")
    }
}

/// XPath 1.0 has no escape sequence inside string literals, so a name
/// containing a quote has to be assembled with `concat()`.
fn string_literal(s: &str) -> String {
    if !s.contains('\'') {
        return format!("'{s}'");
    }
    if !s.contains('"') {
        return format!("\"{s}\"");
    }
    let parts = s
        .split('\'')
        .map(|chunk| format!("'{chunk}'"))
        .collect::<Vec<_>>()
        .join(",\"'\",");
    format!("concat({parts})")
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use chrono::DateTime;

    use super::*;

    fn xpath(f: &Filters<'_>) -> String {
        f.to_xpath().unwrap_or_default()
    }

    #[test]
    fn no_filters_means_no_query() {
        assert_eq!(Filters::default().to_xpath(), None);
    }

    #[test]
    fn severity_is_a_threshold_that_excludes_log_always() {
        let f = Filters {
            level: Some(Level::Warn),
            ..Default::default()
        };
        assert_eq!(xpath(&f), "*[System[(Level<=3 and Level!=0)]]");

        // Info and below include level 0, which renders as Information.
        let f = Filters {
            level: Some(Level::Info),
            ..Default::default()
        };
        assert_eq!(xpath(&f), "*[System[Level<=4]]");
    }

    #[test]
    fn ids_and_ranges_are_or_ed() {
        let ids = [IdSpec::One(7000), IdSpec::Range(100, 200)];
        let f = Filters {
            ids: &ids,
            ..Default::default()
        };
        assert_eq!(
            xpath(&f),
            "*[System[(EventID=7000 or (EventID>=100 and EventID<=200))]]"
        );
    }

    #[test]
    fn providers_are_or_ed() {
        let names = ["Application Error".to_owned(), "Winlogon".to_owned()];
        let f = Filters {
            providers: &names,
            ..Default::default()
        };
        assert_eq!(
            xpath(&f),
            "*[System[(Provider[@Name='Application Error'] or Provider[@Name='Winlogon'])]]"
        );
    }

    #[test]
    fn provider_quotes_are_escaped_not_interpolated() {
        let names = ["Bob's Service".to_owned()];
        let f = Filters {
            providers: &names,
            ..Default::default()
        };
        assert_eq!(xpath(&f), "*[System[(Provider[@Name=\"Bob's Service\"])]]");

        let names = ["say \"hi\" from Bob's".to_owned()];
        let f = Filters {
            providers: &names,
            ..Default::default()
        };
        assert_eq!(
            xpath(&f),
            "*[System[(Provider[@Name=concat('say \"hi\" from Bob',\"'\",'s')])]]"
        );
    }

    #[test]
    fn relative_since_becomes_timediff_millis() {
        let since = Since::Ago(7_200_000);
        let f = Filters {
            since: Some(&since),
            ..Default::default()
        };
        assert_eq!(
            xpath(&f),
            "*[System[TimeCreated[timediff(@SystemTime) <= 7200000]]]"
        );
    }

    #[test]
    fn absolute_since_becomes_a_systemtime_comparison() {
        let at = DateTime::from_timestamp(1_755_000_000, 0).unwrap();
        let since = Since::At(at);
        let f = Filters {
            since: Some(&since),
            ..Default::default()
        };
        assert_eq!(
            xpath(&f),
            "*[System[TimeCreated[@SystemTime>='2025-08-12T12:00:00.000Z']]]"
        );
    }

    #[test]
    fn terms_combine_with_and() {
        let ids = [IdSpec::One(7000)];
        let names = ["Service Control Manager".to_owned()];
        let f = Filters {
            level: Some(Level::Err),
            providers: &names,
            ids: &ids,
            min_record: Some(42),
            ..Default::default()
        };
        assert_eq!(
            xpath(&f),
            "*[System[(Provider[@Name='Service Control Manager']) and (EventID=7000) \
             and (Level<=2 and Level!=0) and EventRecordID>=42]]"
        );
    }

    #[test]
    fn raw_xpath_wins_and_defers_the_record_filter() {
        let f = Filters {
            xpath: Some("*[System[Level=2]]"),
            min_record: Some(9),
            ..Default::default()
        };
        assert_eq!(xpath(&f), "*[System[Level=2]]");
        assert_eq!(f.client_side_min_record(), Some(9));
    }

    #[test]
    fn generated_queries_keep_the_record_filter_server_side() {
        let f = Filters {
            min_record: Some(9),
            ..Default::default()
        };
        assert_eq!(xpath(&f), "*[System[EventRecordID>=9]]");
        assert_eq!(f.client_side_min_record(), None);
    }
}
