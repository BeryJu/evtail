# evtail

[![CI](https://github.com/BeryJu/evtail/actions/workflows/ci.yml/badge.svg)](https://github.com/BeryJu/evtail/actions/workflows/ci.yml)

`tail(1)` for the Windows Event Log.

`wevtutil qe` is a one-shot XML dump with no follow mode, and `Get-WinEvent` is
slow to start and awkward to pipe. `evtail` behaves the way `tail` does — `-n`
for the last N records, `-f` to stream new ones as they arrive — except its
"lines" are event records and its "files" are channels.

```console
$ evtail -n 3 System
2026-08-21 22:45:49 INFO    Microsoft-Windows-Kernel-General[16] The access history in hive ...
2026-08-21 23:13:29 WARN    Service Control Manager[7000] The X service failed to start ...
2026-08-21 23:14:02 ERROR   Microsoft-Windows-WindowsUpdateClient[20] Installation Failure: ...

$ evtail -f --level err Application
```

Single binary, no service, no polling. Built directly on the Windows Event Log
API (`wevtapi.dll`).

## Install

```console
cargo build --release
copy target\release\evtail.exe %USERPROFILE%\bin\
```

## Usage

```
evtail [OPTIONS] [TARGET]...
```

A `TARGET` is a channel name (`System`, `Application`,
`Microsoft-Windows-WinRM/Operational`) or a path to an exported `.evtx` file.
With no target, `evtail` reads `System`.

### tail-compatible options

| Flag | Meaning |
| --- | --- |
| `-n, --lines <[+]NUM>` | Last NUM events (default 10). With `+`, start at record id NUM. `-n 0` prints no history. |
| `-f, --follow` | Print new events as they are written. |
| `-F` | Same as `--follow --retry`. |
| `--retry` | Keep waiting for a channel that does not exist yet. |
| `-q, --quiet` | Never print the `==> TARGET <==` headers. |
| `-v, --verbose` | Always print them (the default is: only with several targets). |

### Event log options

| Flag | Meaning |
| --- | --- |
| `--level <LEVEL>` | `crit`, `err`, `warn`, `info`, `verbose`. A threshold, like `journalctl -p`: `--level err` shows errors and criticals. |
| `--provider <NAME>` | Only this provider. Repeatable, OR-ed. |
| `--id <ID[-ID]>` | Only these event ids, single or inclusive range. Repeatable, OR-ed. |
| `--since <WHEN>` | `30s`, `5m`, `2h`, `3d`, `1w`, or a timestamp (`2026-08-21 09:00`, RFC 3339). |
| `--xpath <QUERY>` | Raw XPath, used instead of the filter flags above. |
| `--format <FORMAT>` | `line` (default), `full`, `json`, `xml`. |
| `--full` | Same as `--format full`. |
| `--color <WHEN>` | `auto` (default), `always`, `never`. |
| `--utc` | Timestamps in UTC instead of local time. |
| `--list-channels` | Print every channel this machine knows about, and exit. |

### Examples

```console
evtail                                        # last 10 System events
evtail -f Application                         # follow one channel
evtail -f System Application                  # follow several, with headers
evtail -n 50 --level err --since 7d System    # last 50 errors from the past week
evtail --provider "Service Control Manager" --id 7000-7045 System
evtail --format json -n 20 System | jq 'select(.event_id == 16)'
evtail -n 100 C:\logs\export.evtx             # read an exported file
evtail --xpath "*[System[Level=2]]" System    # escape hatch for anything else
evtail --list-channels | findstr WinRM
```

Filters are compiled into a single XPath query that the Event Log service
evaluates, so `--id 7000 -n 5` stays fast on a channel holding a million
records.

## Notes

- **Elevation.** Most channels are readable by any user. `Security` is not;
  `evtail Security` from a normal prompt says so rather than printing an
  HRESULT.
- **`-f` needs a channel.** Files cannot grow, so `--follow` on a `.evtx` path
  is refused up front.
- **No gap at the handoff.** `-n 5 -f` reads the history, bookmarks the newest
  event, and subscribes from exactly there — no event is missed or repeated
  between the two steps.
- **Ctrl-C** exits cleanly with output flushed, and `evtail ... | head` exits
  quietly on the closed pipe.
- Exit status is 0 on success, 1 on error, with the message on stderr.

## How it works

- **Last N:** `EvtQuery` with `EvtQueryReverseDirection`, then the buffer is
  reversed so output is oldest-first like `tail`.
- **Follow:** `EvtCreateBookmark` on the newest event read, then `EvtSubscribe`
  with `EvtSubscribeStartAfterBookmark`.
- **Waiting:** the pull model — a signal event per subscription rather than a
  callback, so every `EvtNext` stays on the main thread. Several channels are
  followed by waiting on all their signal events at once, which is why
  `--follow` is capped at 63 targets.
- **Rendering:** one cached `EvtCreateRenderContext` for the System section,
  and `EvtFormatMessage` for the description, with publisher metadata handles
  cached per provider (misses included). When a publisher has no message table,
  the event's own `<Data>` values are shown instead — the same information Event
  Viewer falls back to.

All `unsafe` lives in `src/evt/`, behind RAII handle wrappers.

## Development

```console
cargo test
cargo clippy --all-targets
```

The integration tests in `tests/cli.rs` run the real binary against real
channels that a normal user can read, so they pass unelevated. To exercise
follow mode by hand — the `Windows PowerShell` channel gets events whenever a
PowerShell process starts:

```console
evtail -f "Windows PowerShell"      # then run `powershell -c exit` elsewhere
```

CI runs formatting, clippy, the tests and a release build on `windows-latest`,
and uploads the built `evtail.exe` as an artifact for every push and pull
request.

## Releases

Bump `version` in `Cargo.toml`, then tag and push:

```console
git tag v0.1.0
git push origin v0.1.0
```

The release workflow re-runs CI, refuses the tag if it does not match the crate
version, and publishes a GitHub release with a `.zip` per target — `x86_64` and
`aarch64` Windows — each containing `evtail.exe` and the README, alongside
sha256 checksums.
