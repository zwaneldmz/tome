//! Pure core of the persistent event log (`app_data_dir/events.jsonl`): no
//! Tauri, no filesystem — just the record shape, JSONL parse, and the
//! append cap, so the whole thing is testable without an `AppHandle`.
//! Ports `src/main/lib/eventlog.js`; its vitest suite (`test/events.test.js`
//! — the filename is a slight misnomer, it pins this pure core, not
//! `src/main/events.js`) is the `#[cfg(test)]` module at the bottom of this
//! file. `events.rs` owns the actual file and is the caller.
//!
//! The log records security-relevant ACTIONS (conductor tool calls, air-gap
//! unlocks/relocks, blocked egress) — kinds + identifiers only, never tool
//! inputs/outputs or typed text, which may carry secrets.

use serde::Serialize;
use serde_json::Value;

/// Hard cap on retained entries: the file is append-only (no rotation), so
/// without a bound it grows forever. Reads tail the most recent `TAIL`.
pub const CAP: usize = 5000;
pub const TAIL: usize = 200;

/// One event record — `{ ts, kind, ...fields }` in `eventlog.js`. Kept as an
/// explicit `ts` / `kind` / ordered-`fields` triple rather than a bare
/// `serde_json::Value` object: this crate's `Cargo.toml` does not enable
/// serde_json's `preserve_order` feature (adding it is out of this slice's
/// scope — see this slice's task notes), so a `serde_json::Map` iterates
/// key-sorted, not insertion-ordered. Several vitest assertions pin the
/// exact on-disk JSON text (`ts` first, then `kind`, then the caller's field
/// order — e.g. `{"ts":"t1","kind":"airgap:blocked","paneId":"pty-2","host":"evil.com"}`),
/// so this type carries that order explicitly and serializes it by hand
/// (see the `Serialize` impl below) instead of going through
/// `Value::Object`.
#[derive(Debug, Clone, PartialEq)]
pub struct EventRecord {
    pub ts: String,
    pub kind: String,
    pub fields: Vec<(String, Value)>,
}

impl Serialize for EventRecord {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(Some(2 + self.fields.len()))?;
        map.serialize_entry("ts", &self.ts)?;
        map.serialize_entry("kind", &self.kind)?;
        for (k, v) in &self.fields {
            map.serialize_entry(k, v)?;
        }
        map.end()
    }
}

impl EventRecord {
    /// One compact JSON line, field order preserved — matches
    /// `JSON.stringify({ ts, kind, ...fields })` byte for byte for the
    /// primitive-valued fields every real call site uses.
    pub fn to_json_line(&self) -> String {
        serde_json::to_string(self).expect("EventRecord fields must be JSON-serializable")
    }
}

/// `ts` is the caller's (injectable in tests) so a record is a pure value —
/// mirrors `makeEvent(kind, fields, ts = new Date().toISOString())`. Pass
/// `None` to default to the current instant, formatted the way
/// `Date.prototype.toISOString()` does (`YYYY-MM-DDTHH:mm:ss.sssZ`, always
/// UTC).
pub fn make_event<K: Into<String>>(kind: &str, fields: Vec<(K, Value)>, ts: Option<String>) -> EventRecord {
    EventRecord {
        ts: ts.unwrap_or_else(now_iso8601),
        kind: kind.to_string(),
        fields: fields.into_iter().map(|(k, v)| (k.into(), v)).collect(),
    }
}

/// Returns a NEW lines vec (callers may reuse the input) with the record
/// appended as one JSON line, oldest dropped when over `CAP`.
///
/// Ports `eventlog.js`'s exported `appendEvent` faithfully including its
/// own shape: that function is exercised only by its own vitest suite too
/// — `events.js`'s real `logEvent` does its own direct `appendFile` +
/// `JSON.stringify` rather than importing/calling it (an O(1) disk append
/// vs. this function's O(n) "rewrite the whole in-memory line list" shape,
/// which is why the impure wrapper doesn't use it either — see
/// `events.rs`'s `append_line`). `#[allow(dead_code)]` here mirrors that:
/// unused outside `#[cfg(test)]` in both languages, not a Rust-side
/// regression.
#[allow(dead_code)]
pub fn append_event(lines: &[String], event: &EventRecord) -> Vec<String> {
    let mut next = Vec::with_capacity(lines.len() + 1);
    next.extend_from_slice(lines);
    next.push(event.to_json_line());
    if next.len() > CAP {
        let drop_n = next.len() - CAP;
        next.drain(0..drop_n);
    }
    next
}

/// Parses JSONL back into records, skipping blank and malformed lines — a
/// crash mid-append can leave a truncated final line, and that must not
/// break every read that follows. Non-object lines are dropped too.
///
/// One deliberate divergence from the JS original: JS's `typeof rec ===
/// 'object'` check also accepts a top-level JSON *array* line (`typeof []
/// === 'object'` in JS) — almost certainly an unintended looseness of that
/// check rather than a real feature (`makeEvent` never produces one, and no
/// vitest test exercises it). This port only accepts genuine JSON objects
/// (`Value::Object`), matching the doc comment's stated intent ("non-object
/// lines are dropped too"). Flagged in this slice's report as a judgment
/// call, not a pinned-test mismatch.
pub fn parse_events(text: &str) -> Vec<Value> {
    let mut out = Vec::new();
    for line in text.split('\n') {
        let s = line.trim();
        if s.is_empty() {
            continue;
        }
        if let Ok(v @ Value::Object(_)) = serde_json::from_str::<Value>(s) {
            out.push(v);
        }
    }
    out
}

/// Read-side helper: most recent `n`, oldest-first (the pane reverses for
/// newest-first display; live appends then simply prepend). Returns the
/// input slice unchanged (no copy) when already at or under `n` — the
/// borrowing equivalent of the JS identity-return the vitest suite pins
/// (`toBe`, not `toEqual`): callers that need ownership call `.to_vec()`.
pub fn tail_events(events: &[Value], n: usize) -> &[Value] {
    if events.len() > n {
        &events[events.len() - n..]
    } else {
        events
    }
}

/// Current instant as a `Date.prototype.toISOString()`-shaped UTC string.
/// No `chrono`/`time` crate is available to this slice (this crate's
/// `Cargo.toml` is out of scope to edit — see this slice's task notes), so
/// this hand-rolls the UTC calendar conversion via the well-known
/// days-since-epoch <-> civil-date algorithm (Howard Hinnant's
/// `civil_from_days`; the same technique `absl::CivilDay` and similar
/// libraries use). Cross-checked against independent references (`date -u
/// -r`, Python's `datetime`) in this module's tests below.
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
/// (`civil_from_days`), public domain. Correct for the full `i64` range;
/// `format_iso8601` only ever feeds it non-negative days (any real "now"),
/// but the euclidean division throughout keeps it correct for negative `z`
/// too.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32; // [1, 12]
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ---- makeEvent() / make_event() ----

    #[test]
    fn make_event_spreads_fields_over_ts_kind() {
        let rec = make_event(
            "airgap:unlock",
            vec![("paneId", json!("pty-3")), ("minutes", json!(15))],
            Some("2026-08-09T10:00:00.000Z".to_string()),
        );
        assert_eq!(
            rec,
            EventRecord {
                ts: "2026-08-09T10:00:00.000Z".to_string(),
                kind: "airgap:unlock".to_string(),
                fields: vec![("paneId".to_string(), json!("pty-3")), ("minutes".to_string(), json!(15))],
            }
        );
    }

    #[test]
    fn make_event_defaults_ts_to_an_iso_string_when_not_injected() {
        let rec = make_event("airgap:relock", vec![("paneId", json!("pty-1"))], None);
        assert_eq!(rec.kind, "airgap:relock");
        assert!(looks_like_iso8601(&rec.ts), "ts {:?} is not ISO8601-shaped", rec.ts);
    }

    fn looks_like_iso8601(s: &str) -> bool {
        // YYYY-MM-DDTHH:MM:SS.sssZ — the exact shape Date#toISOString()
        // produces and the shape `new Date(rec.ts).toISOString() ===
        // rec.ts` pins in the JS spec. No Date parser is available here, so
        // this checks the literal shape rather than round-tripping through
        // a parser.
        let b = s.as_bytes();
        b.len() == 24
            && b[4] == b'-'
            && b[7] == b'-'
            && b[10] == b'T'
            && b[13] == b':'
            && b[16] == b':'
            && b[19] == b'.'
            && b[23] == b'Z'
            && s[0..4].bytes().all(|c| c.is_ascii_digit())
            && s[5..7].bytes().all(|c| c.is_ascii_digit())
            && s[8..10].bytes().all(|c| c.is_ascii_digit())
            && s[11..13].bytes().all(|c| c.is_ascii_digit())
            && s[14..16].bytes().all(|c| c.is_ascii_digit())
            && s[17..19].bytes().all(|c| c.is_ascii_digit())
            && s[20..23].bytes().all(|c| c.is_ascii_digit())
    }

    /// Independent cross-check of `civil_from_days`/`format_iso8601`
    /// against real UTC conversions (`date -u -r <secs>` on macOS and
    /// Python's `datetime.fromtimestamp(secs, tz=utc)` agree on all three).
    /// No vitest equivalent exists for this — JS gets UTC formatting for
    /// free from `Date`; this is this slice's own correctness net for the
    /// hand-rolled calendar math `now_iso8601` needed in its absence.
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

    // ---- appendEvent() / append_event() ----

    #[test]
    fn append_event_appends_one_json_line_and_returns_a_new_vec() {
        let before: Vec<String> = vec![];
        let record = make_event(
            "airgap:blocked",
            vec![("paneId", json!("pty-2")), ("host", json!("evil.com"))],
            Some("t1".to_string()),
        );
        let after = append_event(&before, &record);
        assert_ne!(after, before);
        assert_eq!(
            after,
            vec!["{\"ts\":\"t1\",\"kind\":\"airgap:blocked\",\"paneId\":\"pty-2\",\"host\":\"evil.com\"}".to_string()]
        );
    }

    #[test]
    fn append_event_caps_at_5000_lines_dropping_the_oldest() {
        let mut lines: Vec<String> = Vec::new();
        for i in 0..CAP {
            let record = EventRecord {
                ts: format!("t{i}"),
                kind: "k".to_string(),
                fields: vec![("i".to_string(), json!(i))],
            };
            lines = append_event(&lines, &record);
        }
        assert_eq!(lines.len(), CAP);
        let newest = EventRecord {
            ts: "t-new".to_string(),
            kind: "k".to_string(),
            fields: vec![("i".to_string(), json!(CAP))],
        };
        lines = append_event(&lines, &newest);
        assert_eq!(lines.len(), CAP);
        let first: Value = serde_json::from_str(&lines[0]).unwrap();
        assert_eq!(first["i"], json!(1)); // t0 dropped
        let last: Value = serde_json::from_str(&lines[CAP - 1]).unwrap();
        assert_eq!(last["ts"], json!("t-new"));
    }

    // ---- parseEvents() / parse_events() ----

    #[test]
    fn parse_events_round_trips_appended_lines() {
        let mut lines: Vec<String> = Vec::new();
        lines = append_event(
            &lines,
            &make_event(
                "conductor:tool",
                vec![
                    ("tool", json!("open_pane")),
                    ("chatId", json!("chat-1")),
                    ("ok", json!(true)),
                    ("hint", json!("terminal")),
                ],
                Some("t1".to_string()),
            ),
        );
        lines = append_event(
            &lines,
            &make_event("airgap:relock", vec![("paneId", json!("pty-1"))], Some("t2".to_string())),
        );
        let text = lines.join("\n") + "\n";
        assert_eq!(
            parse_events(&text),
            vec![
                json!({"ts":"t1","kind":"conductor:tool","tool":"open_pane","chatId":"chat-1","ok":true,"hint":"terminal"}),
                json!({"ts":"t2","kind":"airgap:relock","paneId":"pty-1"}),
            ]
        );
    }

    #[test]
    fn parse_events_skips_a_truncated_final_line() {
        let text = "{\"ts\":\"t1\",\"kind\":\"airgap:unlock\",\"paneId\":\"pty-3\"}\n{\"ts\":\"t2\",\"kind\":\"airgap:unl";
        assert_eq!(parse_events(text), vec![json!({"ts":"t1","kind":"airgap:unlock","paneId":"pty-3"})]);
    }

    #[test]
    fn parse_events_skips_blank_lines_and_non_object_json() {
        assert_eq!(parse_events("\n  \n42\n\"x\"\nnull\n{}\n"), vec![json!({})]);
    }

    #[test]
    fn parse_events_parses_an_empty_missing_file_to_empty_vec() {
        assert_eq!(parse_events(""), Vec::<Value>::new());
    }

    #[test]
    fn parse_events_tolerates_crlf_line_endings() {
        let rec = "{\"ts\":\"t1\",\"kind\":\"airgap:relock\",\"paneId\":\"pty-1\"}";
        let text = format!("{rec}\r\n{rec}");
        let out = parse_events(&text);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0], json!({"ts":"t1","kind":"airgap:relock","paneId":"pty-1"}));
        for v in out[0].as_object().unwrap().values() {
            assert!(!v.as_str().unwrap_or_default().contains('\r'));
        }
    }

    // ---- tailEvents() / tail_events() ----

    #[test]
    fn tail_events_returns_the_whole_slice_when_under_tail() {
        let events = vec![json!({"kind":"a"}), json!({"kind":"b"})];
        let tail = tail_events(&events, TAIL);
        assert!(std::ptr::eq(tail.as_ptr(), events.as_ptr()));
        assert_eq!(tail.len(), events.len());
    }

    #[test]
    fn tail_events_at_exactly_tail_is_returned_by_identity() {
        // Pins current behavior: the >-vs->= boundary means "exactly at the
        // limit" is the same slice back (same backing pointer), not a copy.
        let events: Vec<Value> = (0..200).map(|i| json!({"kind": "k", "i": i})).collect();
        let tail = tail_events(&events, TAIL);
        assert!(std::ptr::eq(tail.as_ptr(), events.as_ptr()));
    }

    #[test]
    fn tail_events_honors_an_explicit_n() {
        let events: Vec<Value> = (0..10).map(|i| json!({"kind": "k", "i": i})).collect();
        let tail = tail_events(&events, 3);
        assert_eq!(tail.len(), 3);
        assert_eq!(tail[0]["i"], json!(7));
        let full = tail_events(&events, 10);
        assert!(std::ptr::eq(full.as_ptr(), events.as_ptr())); // exactly n: identity too
    }

    #[test]
    fn tail_events_keeps_the_most_recent_200_oldest_first() {
        assert_eq!(TAIL, 200);
        let events: Vec<Value> = (0..250).map(|i| json!({"kind": "k", "i": i})).collect();
        let tail = tail_events(&events, TAIL);
        assert_eq!(tail.len(), 200);
        assert_eq!(tail[0]["i"], json!(50));
        assert_eq!(tail[199]["i"], json!(249));
    }
}
