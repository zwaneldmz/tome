//! `tome-runner`'s own persistent event log —
//! `~/.local/state/tome-runner/events.jsonl`. A small, hand-rolled analog
//! of the desktop app's `eventlog.rs`/`events.rs` pair (main crate, out of
//! this slice's file surface, and `tauri::AppHandle`-coupled besides): the
//! same on-disk shape (one JSON object per line, `ts`/`kind` first, then
//! the caller's own fields in the order given) so a human or script
//! reading both logs sees one consistent format — but with no cap/trim
//! housekeeping. The desktop log grows with every interactive tool call
//! and pane action; this one gets at most a handful of lines per
//! `systemd`-timer-driven invocation, so unbounded growth here is a
//! `logrotate` problem for the server owner (see `docs/remote-runner.md`),
//! not something this binary needs to manage itself.
//!
//! [`append`] is this module's real entry point — it is also, directly,
//! what [`crate::runner_env::build`] wires up as the injected `RunnerEnv`'s
//! `log_event` closure.

use std::path::Path;

use serde_json::Value;

/// One `{"ts":...,"kind":...,...fields}` line, field order preserved.
/// Built by direct string concatenation rather than a `serde_json::Map` —
/// `serde_json::Map` iterates key-SORTED unless the `preserve_order`
/// feature is enabled, which this crate does not turn on (matching the
/// main crate's own `eventlog.rs::EventRecord` doc comment on the
/// identical constraint; this crate's dependency grant is `serde_json`
/// only, with no `serde` derive machinery to write a custom `Serialize`
/// impl the way that struct does — see `Cargo.toml`'s own note). Each key
/// and value is still run through `serde_json::to_string` individually,
/// so escaping/encoding stays exactly as correct as going through a real
/// serializer — only the ORDER is hand-controlled. `ts` is a parameter
/// (not computed here) so this stays pure and independently testable —
/// [`append`] is the only real caller, and always passes [`now_iso8601`].
fn build_line(ts: &str, kind: &str, fields: &[(String, Value)]) -> String {
    let mut out = String::from("{\"ts\":");
    out.push_str(&serde_json::to_string(ts).expect("a &str always serializes"));
    out.push_str(",\"kind\":");
    out.push_str(&serde_json::to_string(kind).expect("a &str always serializes"));
    for (k, v) in fields {
        out.push(',');
        out.push_str(&serde_json::to_string(k).expect("a &str always serializes"));
        out.push(':');
        out.push_str(&serde_json::to_string(v).expect("a serde_json::Value always serializes"));
    }
    out.push('}');
    out
}

/// Appends one event line to `<state_dir>/events.jsonl`, creating the
/// directory if it doesn't exist yet. Best-effort: a logging failure (disk
/// full, permissions, a read-only filesystem) must never abort the run
/// it's trying to record — mirrors every other `log_event`/`append` in
/// this codebase swallowing its own I/O errors rather than propagating
/// them into the thing being logged.
pub fn append(state_dir: &Path, kind: &str, fields: Vec<(String, Value)>) {
    let _ = std::fs::create_dir_all(state_dir);
    let path = state_dir.join("events.jsonl");
    let line = build_line(&now_iso8601(), kind, &fields);
    use std::io::Write as _;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = writeln!(f, "{line}");
    }
}

// ---- ISO8601 UTC ----
//
// Duplicates `eventlog.rs`'s (main crate) and `flow::runner::mod`'s
// (tome-flow crate) own PRIVATE `format_iso8601`/`civil_from_days` — see
// `flow::runner::mod`'s own doc comment on this exact duplication ("that
// module is a different slice's file"): both existing copies are either
// out of this slice's file surface or not `pub`, so a third small,
// self-contained copy is the established move here, not a new pattern.
// This crate has no `chrono`/`time` dependency (out of the grant).

fn now_iso8601() -> String {
    format_iso8601(std::time::SystemTime::now())
}

fn format_iso8601(t: std::time::SystemTime) -> String {
    let dur = t.duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    let secs = dur.as_secs() as i64;
    let millis = dur.subsec_millis();
    let days = secs.div_euclid(86_400);
    let secs_of_day = secs.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let hour = secs_of_day / 3600;
    let minute = (secs_of_day % 3600) / 60;
    let second = secs_of_day % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{millis:03}Z")
}

/// Days-since-1970-01-01 -> proleptic-Gregorian `(year, month, day)`.
/// Reference: <http://howardhinnant.github.io/date_algorithms.html>
/// (`civil_from_days`), public domain — same algorithm, same reference,
/// as every other copy of this function in this workspace.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn scratch_dir(tag: &str) -> std::path::PathBuf {
        // Hand-rolled temp dir — no `tempfile` dependency (see
        // `Cargo.toml`'s own note). Unique per test via pid + tag so
        // parallel test threads never collide.
        let dir = std::env::temp_dir().join(format!(
            "tome-runner-events-test-{}-{}-{tag}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    // ---- build_line ----

    #[test]
    fn build_line_puts_ts_then_kind_then_fields_in_order() {
        let line = build_line(
            "2026-08-09T10:00:00.000Z",
            "flow-run",
            &[
                ("run".to_string(), json!("abc123")),
                ("status".to_string(), json!("running")),
            ],
        );
        assert_eq!(
            line,
            r#"{"ts":"2026-08-09T10:00:00.000Z","kind":"flow-run","run":"abc123","status":"running"}"#
        );
    }

    #[test]
    fn build_line_with_no_fields_is_just_ts_and_kind() {
        let line = build_line("t1", "egress:blocked", &[]);
        assert_eq!(line, r#"{"ts":"t1","kind":"egress:blocked"}"#);
    }

    // ---- append ----

    #[test]
    fn append_creates_the_state_dir_and_writes_one_json_line() {
        let dir = scratch_dir("append-basic");
        let state_dir = dir.join("nested").join("state");
        append(
            &state_dir,
            "flow-run",
            vec![("run".to_string(), json!("r1"))],
        );
        let text = std::fs::read_to_string(state_dir.join("events.jsonl")).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 1);
        let parsed: Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(parsed["kind"], json!("flow-run"));
        assert_eq!(parsed["run"], json!("r1"));
        assert!(parsed["ts"].is_string());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn append_appends_rather_than_overwriting_across_calls() {
        let dir = scratch_dir("append-multi");
        append(&dir, "a", vec![]);
        append(&dir, "b", vec![]);
        append(&dir, "c", vec![]);
        let text = std::fs::read_to_string(dir.join("events.jsonl")).unwrap();
        let kinds: Vec<Value> = text
            .lines()
            .map(|l| serde_json::from_str::<Value>(l).unwrap()["kind"].clone())
            .collect();
        assert_eq!(kinds, vec![json!("a"), json!("b"), json!("c")]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- ISO8601 helpers ----

    #[test]
    fn format_iso8601_matches_independently_verified_epoch_seconds() {
        let cases: &[(u64, &str)] = &[
            (0, "1970-01-01T00:00:00.000Z"),
            (951_782_400, "2000-02-29T00:00:00.000Z"), // leap day
            (1_709_251_199, "2024-02-29T23:59:59.000Z"), // leap day, end of day
        ];
        for &(secs, expected) in cases {
            let t = std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs);
            assert_eq!(format_iso8601(t), expected);
        }
    }

    #[test]
    fn now_iso8601_looks_like_an_iso8601_utc_stamp() {
        let s = now_iso8601();
        let b = s.as_bytes();
        assert_eq!(b.len(), 24, "unexpected length: {s:?}");
        assert_eq!(b[b.len() - 1], b'Z');
    }
}
