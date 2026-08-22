//! End-to-end tests against the real Event Log.
//!
//! Everything here reads channels a normal user can read, so the suite passes
//! without an elevated prompt. Machines differ, so the assertions are about
//! shape and invariants rather than specific events; a channel that happens to
//! be empty must not fail the build.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::process::{Command, Output};

fn evtail(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_evtail"))
        .args(args)
        .output()
        .expect("failed to run evtail")
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn json_lines(args: &[&str]) -> Vec<serde_json::Value> {
    let out = evtail(args);
    assert!(out.status.success(), "evtail {args:?} failed");
    stdout(&out)
        .lines()
        .map(|line| serde_json::from_str(line).expect("each line must be one JSON object"))
        .collect()
}

#[test]
fn reads_at_most_n_events() {
    let out = evtail(&["-n", "5", "Application"]);
    assert!(
        out.status.success(),
        "stderr: {:?}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(stdout(&out).lines().count() <= 5);
}

#[test]
fn zero_lines_prints_nothing() {
    let out = evtail(&["-n", "0", "Application"]);
    assert!(out.status.success());
    assert_eq!(stdout(&out), "");
}

#[test]
fn json_carries_the_expected_fields() {
    let events = json_lines(&["-n", "3", "--format", "json", "Application"]);
    for ev in &events {
        for field in [
            "time",
            "target",
            "level",
            "level_name",
            "provider",
            "event_id",
            "message",
        ] {
            assert!(ev.get(field).is_some(), "missing `{field}` in {ev}");
        }
        assert_eq!(ev["target"], "Application");
    }
}

#[test]
fn events_come_out_oldest_first_like_tail() {
    let events = json_lines(&["-n", "20", "--format", "json", "--utc", "Application"]);
    let times: Vec<&str> = events.iter().filter_map(|e| e["time"].as_str()).collect();
    // RFC 3339 in UTC sorts lexicographically.
    assert!(
        times.windows(2).all(|w| w[0] <= w[1]),
        "events are not in chronological order: {times:?}"
    );
}

#[test]
fn id_filter_reaches_the_query() {
    let events = json_lines(&[
        "-n",
        "5",
        "--format",
        "json",
        "--id",
        "1000-1030",
        "Application",
    ]);
    for ev in &events {
        let id = ev["event_id"].as_u64().unwrap_or_default();
        assert!(
            (1000..=1030).contains(&id),
            "event id {id} escaped the filter"
        );
    }
}

#[test]
fn level_filter_excludes_less_severe_events() {
    let events = json_lines(&[
        "-n",
        "10",
        "--format",
        "json",
        "--level",
        "err",
        "Application",
    ]);
    for ev in &events {
        let level = ev["level"].as_u64().unwrap_or_default();
        assert!(
            (1..=2).contains(&level),
            "level {level} is not error-or-worse"
        );
    }
}

#[test]
fn xml_format_emits_event_xml() {
    let out = evtail(&["-n", "1", "--format", "xml", "Application"]);
    assert!(out.status.success());
    let text = stdout(&out);
    if !text.trim().is_empty() {
        assert!(text.contains("<Event"), "not event XML: {text}");
        assert!(text.contains("<System>"));
    }
}

#[test]
fn headers_appear_only_for_several_targets() {
    let single = evtail(&["-n", "1", "Application"]);
    assert!(!stdout(&single).contains("==>"));

    let multi = evtail(&["-n", "1", "Application", "System"]);
    assert!(multi.status.success());
    let text = stdout(&multi);
    // A header only appears for a target that produced an event, so a machine
    // with both channels empty is silent rather than wrong.
    if !text.trim().is_empty() {
        assert!(text.contains("==> Application <==") || text.contains("==> System <=="));
    }
}

#[test]
fn list_channels_includes_the_usual_ones() {
    let out = evtail(&["--list-channels"]);
    assert!(out.status.success());
    let text = stdout(&out);
    assert!(text.lines().any(|l| l == "Application"));
    assert!(text.lines().any(|l| l == "System"));
}

#[test]
fn unknown_channel_fails_with_a_useful_message() {
    let out = evtail(&["definitely-not-a-channel"]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("definitely-not-a-channel"),
        "stderr was: {err}"
    );
    assert!(err.contains("--list-channels"), "stderr was: {err}");
}

#[test]
fn following_a_file_is_refused_before_any_output() {
    let out = evtail(&["-f", "some-export.evtx"]);
    assert!(!out.status.success());
    assert_eq!(
        stdout(&out),
        "",
        "nothing should be printed before the refusal"
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("--follow needs a channel"),
        "stderr was: {err}"
    );
}

#[test]
fn bad_arguments_are_rejected() {
    assert!(!evtail(&["-n", "many", "Application"]).status.success());
    assert!(!evtail(&["--id", "12-3", "Application"]).status.success());
    assert!(
        !evtail(&["--since", "yesterday", "Application"])
            .status
            .success()
    );
    assert!(
        !evtail(&["--xpath", "*", "--level", "err", "Application"])
            .status
            .success()
    );
}
