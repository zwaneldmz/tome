//! Persistent event log owner: reads/appends `<app_data_dir>/events.jsonl`
//! and pushes new records to the renderer as they land. Ports
//! `src/main/events.js` (the electron-aware wrapper around the pure core in
//! `eventlog.rs`).
//!
//! Departures from a literal 1:1 port (see this slice's task notes for the
//! full, ranked list):
//!
//! - No `init_events(dir)` step. `events.js` needs one because Electron's
//!   `app.getPath('userData')` is only available once `app.whenReady()`
//!   fires, so it's resolved once and cached in a module-level `file`
//!   variable. Tauri's `AppHandle::path().app_data_dir()` is a pure
//!   computation over the `tauri.conf.json` identifier — callable at any
//!   time, cheap enough not to need caching — so every function here just
//!   takes the resolved directory directly (or an `&AppHandle` to resolve
//!   it from, for the two entry points real callers use:
//!   [`append`]/[`log_event`]/[`list`]).
//! - `events.js`'s `lines`/`sinceRewrite` amortized-rewrite counters are
//!   module-level `let`s (a singleton by virtue of the JS module system).
//!   The Rust equivalent — since this slice may not add fields to
//!   `AppState` (see this slice's task notes) — is a private `static`
//!   scoped to this file only ([`COUNTERS`] below), which is the same
//!   "this module owns one persistent counter for the process's one event
//!   log" shape, just spelled with `Mutex` instead of module scope.
//! - `appendFile(...).catch(() => {})` fire-and-forget in JS runs on
//!   Node's libuv thread pool. The direct Rust analog inside an async
//!   command would be `tokio::task::spawn_blocking`, but that requires an
//!   active Tokio runtime on the calling thread — a constraint future
//!   callers of `append`/`log_event` (Phase 2/3 modules ported from
//!   `egress.js`, `conductor.js`, `flow-runner.js`, ...) would then all
//!   have to satisfy. [`append`] instead spawns a plain OS thread, which
//!   works from any calling context and costs nothing meaningful at the
//!   log's actual call rate (human/agent-paced security events, not a hot
//!   loop).
//! - Unlike Electron's `userData` (which Electron itself creates before
//!   `whenReady` fires), Tauri does not guarantee `app_data_dir` exists.
//!   [`append`] creates it if missing (`store.rs`'s `set` already does the
//!   same for the JSON store, for the same reason); `events.js` never needs
//!   to.
//!
//! Testing boundary note: mirroring the JS side (where `eventlog.js`'s pure
//! core has a vitest suite and `events.js`'s file-owning wrapper has none),
//! the process-`static`- and `AppHandle`-touching entry points here
//! ([`append_line`], [`append`], [`log_event`], [`list`]) are not unit
//! tested directly — the former because `#[test]`s run in parallel threads
//! within one process and would contend over the shared `static`, the
//! latter because building a real or mocked `AppHandle` needs Tauri's
//! `test` cargo feature, which this crate's `Cargo.toml` does not enable
//! (out of scope for this slice to add — see task notes). The logic that
//! matters — the cap-trim housekeeping and the tail read — is factored out
//! into [`count_and_maybe_trim`] and [`read_tail`], which take an explicit
//! path and counters and so are fully unit tested below with `tempfile`.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use tauri::{AppHandle, Emitter, Manager};

use crate::eventlog::{self, EventRecord};

fn events_file_path(dir: &Path) -> PathBuf {
    dir.join("events.jsonl")
}

/// The amortized-rewrite bookkeeping `events.js` keeps as module-level
/// `let lines = null` / `let sinceRewrite = 0`. See the module doc comment
/// for why this is a file-scoped `static` rather than an `AppState` field.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Counters {
    /// Approximate line count, `None` until lazily seeded from the file on
    /// the first append (mirrors `lines === null`).
    lines: Option<usize>,
    since_rewrite: usize,
}

static COUNTERS: Mutex<Counters> = Mutex::new(Counters {
    lines: None,
    since_rewrite: 0,
});

/// One append's worth of `countAndMaybeTrim()`: bump `lines`, and every
/// 500th call, rewrite the file to the most recent `CAP` records if it grew
/// past the cap. Takes and returns the counters explicitly (rather than
/// through `COUNTERS`) so it's directly testable with a tempdir and no
/// process-global state — [`append_line`] is the thin stateful wrapper
/// real callers go through.
///
/// Every step swallows its own I/O errors and keeps going — logging must
/// never break the thing being logged, least of all because the log's own
/// housekeeping failed (mirrors every `.catch(() => {})` in
/// `countAndMaybeTrim`).
fn count_and_maybe_trim(path: &Path, counters: Counters) -> Counters {
    let mut lines = counters.lines.unwrap_or_else(|| seed_line_count(path));
    lines += 1;
    let since_rewrite = counters.since_rewrite + 1;
    if since_rewrite < 500 {
        return Counters {
            lines: Some(lines),
            since_rewrite,
        };
    }
    // sinceRewrite resets to 0 here regardless of whether a trim actually
    // happens below — pins the JS original's `sinceRewrite = 0` landing
    // before the `if (lines <= CAP) return`.
    if lines > eventlog::CAP {
        if let Some(new_len) = trim_to_cap(path) {
            lines = new_len;
        }
    }
    Counters {
        lines: Some(lines),
        since_rewrite: 0,
    }
}

fn seed_line_count(path: &Path) -> usize {
    std::fs::read_to_string(path)
        .map(|t| t.split('\n').filter(|s| !s.trim().is_empty()).count())
        .unwrap_or(0)
}

/// Rewrites `path` to the most recent `CAP` records; `None` on any I/O or
/// parse failure (left as-is, matching JS's `if (text === null) return`).
fn trim_to_cap(path: &Path) -> Option<usize> {
    let text = std::fs::read_to_string(path).ok()?;
    let parsed = eventlog::parse_events(&text);
    let tail = eventlog::tail_events(&parsed, eventlog::CAP);
    let mut out = String::new();
    for record in tail {
        out.push_str(&serde_json::to_string(record).ok()?);
        out.push('\n');
    }
    std::fs::write(path, out).ok()?;
    Some(tail.len())
}

/// Appends one already-built record's JSON line to `events.jsonl` under
/// `dir` (creating `dir` first — see the module doc comment), then runs the
/// cap-trim housekeeping against the process-wide [`COUNTERS`]. Not unit
/// tested directly (see the module doc comment); [`count_and_maybe_trim`]
/// carries the tested logic.
fn append_line(dir: &Path, line: &str) {
    let _ = std::fs::create_dir_all(dir);
    let path = events_file_path(dir);
    {
        use std::io::Write as _;
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
        {
            let _ = writeln!(f, "{line}");
        }
    }
    let mut counters = COUNTERS.lock().unwrap();
    *counters = count_and_maybe_trim(&path, *counters);
}

/// Most recent [`eventlog::TAIL`] records, oldest-first — mirrors
/// `readEvents()`. Missing file is a normal first-run state (empty, not an
/// error); malformed lines are skipped by `eventlog::parse_events`, not
/// surfaced.
pub fn read_tail(dir: &Path) -> Vec<serde_json::Value> {
    let path = events_file_path(dir);
    match std::fs::read_to_string(path) {
        Ok(text) => eventlog::tail_events(&eventlog::parse_events(&text), eventlog::TAIL).to_vec(),
        Err(_) => Vec::new(),
    }
}

/// Full `logEvent(kind, fields)` equivalent — the entry point other modules
/// should call once ported. `ipc::egress`'s `EgressEnv for AppHandle` impl
/// is the first real caller (`'egress:blocked'`/`'egress:unlock'`/
/// `'egress:relock'`); the conductor port (`'conductor:tool'`/
/// `'conductor:read'`) and the flow runner port (`'flow-run'`) are expected
/// to call this too once they land. Builds the record via
/// `eventlog::make_event` (defaulting `ts` to now) and hands it to
/// [`append`].
pub fn log_event<K: Into<String>>(
    app: &AppHandle,
    kind: &str,
    fields: Vec<(K, serde_json::Value)>,
) -> EventRecord {
    append(app, eventlog::make_event(kind, fields, None))
}

/// Lower-level half of [`log_event`]: takes an already-built
/// [`EventRecord`] (from `eventlog::make_event`, e.g. with an injected `ts`)
/// rather than building one from `kind`/`fields`. Fires the disk append off
/// on a background thread (never awaited — see the module doc comment for
/// why a plain thread rather than `spawn_blocking`), pushes
/// `events:appended` to the renderer, and returns the record — mirrors
/// `logEvent`'s synchronous fire-and-forget-then-return shape exactly.
#[allow(dead_code)] // no caller within this slice yet — see the module doc comment
pub fn append(app: &AppHandle, record: EventRecord) -> EventRecord {
    if let Ok(dir) = app.path().app_data_dir() {
        let line = record.to_json_line();
        std::thread::spawn(move || append_line(&dir, &line));
    }
    let _ = app.emit("events:appended", record.clone());
    record
}

/// `events_list` command body: the async, `AppHandle`-resolving counterpart
/// to [`read_tail`]. Runs the (blocking, small) file read on Tokio's
/// blocking pool so it never stalls the command executor. `app_data_dir`
/// failing to resolve collapses to `[]`, matching `readEvents()`'s "not
/// initialized yet" -> `[]` fallback (`if (!file) return []`).
pub async fn list(app: &AppHandle) -> Vec<serde_json::Value> {
    let Ok(dir) = app.path().app_data_dir() else {
        return Vec::new();
    };
    tokio::task::spawn_blocking(move || read_tail(&dir))
        .await
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tempdir() -> tempfile::TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    fn line_for(i: usize) -> String {
        eventlog::EventRecord {
            ts: format!("t{i}"),
            kind: "k".to_string(),
            fields: vec![("i".to_string(), json!(i))],
        }
        .to_json_line()
    }

    // ---- read_tail ----

    #[test]
    fn read_tail_of_missing_file_is_empty() {
        let dir = tempdir();
        assert_eq!(read_tail(dir.path()), Vec::<serde_json::Value>::new());
    }

    #[test]
    fn read_tail_returns_the_most_recent_tail_oldest_first() {
        let dir = tempdir();
        let mut text = String::new();
        for i in 0..(eventlog::TAIL + 10) {
            text.push_str(&line_for(i));
            text.push('\n');
        }
        std::fs::write(events_file_path(dir.path()), text).unwrap();
        let tail = read_tail(dir.path());
        assert_eq!(tail.len(), eventlog::TAIL);
        assert_eq!(tail[0]["i"], json!(10)); // first 10 dropped
        assert_eq!(tail[eventlog::TAIL - 1]["i"], json!(eventlog::TAIL + 9));
    }

    #[test]
    fn read_tail_skips_malformed_trailing_line() {
        let dir = tempdir();
        let text = format!("{}\n{{\"truncated", line_for(0));
        std::fs::write(events_file_path(dir.path()), text).unwrap();
        let tail = read_tail(dir.path());
        assert_eq!(tail, vec![json!({"ts": "t0", "kind": "k", "i": 0})]);
    }

    // ---- count_and_maybe_trim ----

    #[test]
    fn count_and_maybe_trim_just_increments_below_the_rewrite_cadence() {
        let dir = tempdir();
        let path = events_file_path(dir.path());
        std::fs::write(&path, "").unwrap();
        let next = count_and_maybe_trim(
            &path,
            Counters {
                lines: Some(10),
                since_rewrite: 0,
            },
        );
        assert_eq!(
            next,
            Counters {
                lines: Some(11),
                since_rewrite: 1
            }
        );
        // No trim attempted this far below the cadence: file left alone.
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "");
    }

    #[test]
    fn count_and_maybe_trim_resets_since_rewrite_at_500_even_without_a_trim() {
        let dir = tempdir();
        let path = events_file_path(dir.path());
        std::fs::write(&path, "").unwrap();
        // lines(11) stays under CAP(5000): the 500th call still resets the
        // counter, it just skips the actual rewrite — pins the JS
        // original's unconditional `sinceRewrite = 0` before its `if
        // (lines <= CAP) return`.
        let next = count_and_maybe_trim(
            &path,
            Counters {
                lines: Some(10),
                since_rewrite: 499,
            },
        );
        assert_eq!(
            next,
            Counters {
                lines: Some(11),
                since_rewrite: 0
            }
        );
    }

    #[test]
    fn count_and_maybe_trim_rewrites_to_cap_when_over_at_the_500th_call() {
        let dir = tempdir();
        let path = events_file_path(dir.path());
        let over = eventlog::CAP + 11;
        let mut text = String::new();
        for i in 0..over {
            text.push_str(&line_for(i));
            text.push('\n');
        }
        std::fs::write(&path, &text).unwrap();
        // lines counter says `over` already includes this call's append.
        let next = count_and_maybe_trim(
            &path,
            Counters {
                lines: Some(over),
                since_rewrite: 499,
            },
        );
        assert_eq!(
            next,
            Counters {
                lines: Some(eventlog::CAP),
                since_rewrite: 0
            }
        );
        let rewritten = std::fs::read_to_string(&path).unwrap();
        let parsed = eventlog::parse_events(&rewritten);
        assert_eq!(parsed.len(), eventlog::CAP);
        assert_eq!(parsed[0]["i"], json!(11)); // oldest 11 dropped
        assert_eq!(parsed[eventlog::CAP - 1]["i"], json!(over - 1));
    }

    #[test]
    fn count_and_maybe_trim_seeds_lines_lazily_from_the_file_when_none() {
        let dir = tempdir();
        let path = events_file_path(dir.path());
        // Three real lines plus a blank one, which the seed count (mirrors
        // `t.split('\n').filter((s) => s.trim()).length`) must not count.
        std::fs::write(
            &path,
            format!("{}\n{}\n{}\n\n", line_for(0), line_for(1), line_for(2)),
        )
        .unwrap();
        let next = count_and_maybe_trim(
            &path,
            Counters {
                lines: None,
                since_rewrite: 0,
            },
        );
        assert_eq!(
            next,
            Counters {
                lines: Some(4),
                since_rewrite: 1
            }
        ); // 3 seeded + 1 for this append
    }
}
