//! The in-app flow scheduler (plan §Flow products pipeline, step 1.7):
//! persisted schedules that fire a background flow run without anyone
//! sitting at the app — `flow-schedules.json` (main-owned, 0600, RESERVED in
//! `store_keys::RESERVED_KEYS` so `store:set` can never forge one, same
//! discipline `export.rs`'s own doc comment describes for
//! `export-destinations.json`) plus the pure decision core `ipc::schedules`'
//! `run_tick` drives every 30 seconds. `ipc::schedules` is this module's only
//! caller — see that file for the `#[tauri::command]` wrappers
//! (`schedules_list`/`schedules_set`/`schedules_delete`) and the ticker
//! driver (`run_tick`) that turns this module's pure verdicts into an actual
//! `flow::runner::start_run` call.
//!
//! ## Consent now, re-verification forever — not a TOCTOU re-check at set time
//!
//! Unlike `airgap::consent_repo_allowlist` (which re-checks a caller-
//! PRESENTED hash against fresh content, because the content is authored by
//! someone else — a repo's committed `.tome/airgap.json`), [`Schedule::flow_sha1`]
//! is never compared against anything at the moment it is written:
//! `schedules_set` reads `flowPath` right now, hashes exactly that, and
//! records the result as the new baseline — the same "this call defines
//! truth, there is nothing to disagree with yet" shape `export::canonicalize`
//! uses for a destination record. The re-verification this module exists to
//! make possible happens continuously afterward, once per tick
//! ([`decide_due_schedule`]): the CURRENTLY stored hash is compared against a
//! FRESH read of the same file, and a mismatch suspends the schedule
//! ([`DueOutcome::Suspend`]) rather than run content nobody has reviewed
//! since it changed — the scheduler's own analogue of the repo allowlist's
//! re-prompt-on-change.
//!
//! ## Always air-gapped — an override, not a preference read
//!
//! [`SCHEDULED_RUN_AIRGAP`] is the one value `ipc::schedules::run_tick`
//! freezes every scheduled run's `RunnerEnv::airgap_default` to
//! (`flow_env::frozen_airgap_default`) — never the `airgap-default`
//! store preference an interactive `runs:start` call resolves. There is no
//! user at the keyboard for a run that fires at 3am to ask, so this path
//! never gets to ask: an ungapped scheduled run is not a state this crate can
//! reach, not a default this crate merely ships.
//!
//! ## Daily schedules, UTC, no calendar dependency
//!
//! `When::Daily`'s "has today's HH:MM UTC arrived" check
//! ([`next_due`]) is pure `i64` millisecond arithmetic — a UTC day is a fixed
//! 86_400_000 ms with no leap seconds in Unix time, so "the start of today"
//! is `now - now.rem_euclid(DAY_MS)`, no calendar conversion required. Real
//! calendar math (`civil_from_days`/`days_from_civil`, a third copy of the
//! algorithm `eventlog.rs` and `flow::runner` each already carry their own
//! private copy of — see `flow_env`'s doc comment on the same
//! duplication constraint applied elsewhere) is needed only to format/parse
//! the persisted `lastRun` string into something human-readable, never for
//! deciding due-ness. v1 ships no `chrono` dependency; the UI names the unit
//! ("(UTC)") rather than converting to the user's local zone.
//!
//! ## No catch-up for missed slots
//!
//! [`next_due`] only ever asks "has TODAY's slot (or, for an interval, the
//! current period) arrived", never "how many slots were missed while the app
//! was closed or locked". A daily schedule that missed three days fires
//! once, for today, the moment the app next checks — not three times to
//! "catch up". This is a deliberate simplification, not an oversight: a
//! flow run has side effects (it spawns real agent processes), and silently
//! replaying N missed side-effecting runs the moment the app reopens is a
//! surprise no user asked for.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// `<app_data_dir>/flow-schedules.json` — same directory
/// `export-destinations.json`/`airgap-repo-consents.json` resolve against
/// (`lib.rs`'s `boot_auth_and_airgap`).
pub const FILE_NAME: &str = "flow-schedules.json";

/// A scheduled run is always gapped — see the module doc comment. Named and
/// tested on its own (rather than an inline `true` at
/// `ipc::schedules::run_tick`'s one call site) so the property "a scheduled
/// run's air gap is a constant, never a variable read from anywhere" is one
/// grep away and independently asserted, not resting on a reviewer noticing
/// a bare literal.
pub const SCHEDULED_RUN_AIRGAP: bool = true;

/// `{"kind":"interval","minutes":N}` / `{"kind":"daily","hour":H,"minute":M}`
/// — the two repeat shapes the flow.js "Schedule…" form offers. Times are
/// always UTC (see the module doc comment); `#[serde(tag = "kind")]` matches
/// `export::Destination`'s own tagged-enum wire shape.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum When {
    Interval { minutes: u32 },
    Daily { hour: u32, minute: u32 },
}

/// One persisted schedule. Field order is the persisted JSON key order
/// (`serde_json` serializes a struct in declaration order) — matches the
/// exact shape this slice's plan pins: `id`, `flowPath`, `when`, `flowSha1`,
/// `enabled`, `suspended`, `lastRun`. `suspended`/`last_run` are never
/// `skip_serializing_if`-omitted: the plan's own literal shows both keys
/// always present (`null` when unset), not dropped.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Schedule {
    pub id: String,
    #[serde(rename = "flowPath")]
    pub flow_path: String,
    pub when: When,
    #[serde(rename = "flowSha1")]
    pub flow_sha1: String,
    pub enabled: bool,
    /// `null` normally; `"hash-mismatch"` once [`decide_due_schedule`] finds
    /// the live file no longer hashes to `flow_sha1`. The only way back to
    /// `null` is a fresh `schedules_set` call — see the module doc comment.
    pub suspended: Option<String>,
    /// ISO-8601 UTC (`format_iso8601_ms`), or `null` before this schedule's
    /// first successful start.
    #[serde(rename = "lastRun")]
    pub last_run: Option<String>,
}

/// The persisted file's whole shape: `{"version":1,"schedules":[...]}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Schedules {
    pub version: u32,
    pub schedules: Vec<Schedule>,
}

impl Default for Schedules {
    fn default() -> Self {
        Self {
            version: 1,
            schedules: Vec::new(),
        }
    }
}

fn file_path(dir: &Path) -> PathBuf {
    dir.join(FILE_NAME)
}

/// Missing file, corrupt JSON, or a shape that doesn't parse as [`Schedules`]
/// all collapse to a fresh, empty v1 store — the same "unreadable = start
/// fresh" discipline `export::load`/`airgap::AirgapState::load_repo_consents`
/// already apply to their own main-owned files.
pub fn load(dir: &Path) -> Schedules {
    let Ok(text) = std::fs::read_to_string(file_path(dir)) else {
        return Schedules::default();
    };
    serde_json::from_str(&text).unwrap_or_default()
}

/// Writes the whole store back, 0600 — same discipline `export::save` uses.
pub fn save(dir: &Path, data: &Schedules) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let path = file_path(dir);
    let text = serde_json::to_string_pretty(data).map_err(|e| e.to_string())?;
    std::fs::write(&path, text).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Shape-only validation — `minutes` must be able to elapse, `hour`/`minute`
/// must be a real time of day. Pure, called by `schedules_set` before any
/// I/O; kept here (not inlined in the ipc layer) so it is unit-testable
/// without a live `AppHandle`.
pub fn validate_when(when: &When) -> Result<(), String> {
    match when {
        When::Interval { minutes } if *minutes == 0 => {
            Err("minutes must be at least 1".to_string())
        }
        When::Daily { hour, .. } if *hour > 23 => Err("hour must be 0-23".to_string()),
        When::Daily { minute, .. } if *minute > 59 => Err("minute must be 0-59".to_string()),
        When::Interval { .. } | When::Daily { .. } => Ok(()),
    }
}

fn to_base36(mut n: u128) -> String {
    const DIGITS: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    if n == 0 {
        return "0".to_string();
    }
    let mut buf = Vec::new();
    while n > 0 {
        buf.push(DIGITS[(n % 36) as usize]);
        n /= 36;
    }
    buf.reverse();
    String::from_utf8(buf).expect("base36 digits are always valid UTF-8")
}

/// `sched-<ts36>`, collision-retried against `existing` — the same shape
/// `flow::runner`'s own (private, and so not reusable here — see the module
/// doc comment on this crate's established duplication-over-reach-in
/// pattern) `new_run_id` uses for run ids.
pub fn new_schedule_id(existing: &[Schedule]) -> String {
    let millis = now_ms().max(0) as u128;
    let base = to_base36(millis);
    let mut id = format!("sched-{base}");
    let mut n = 2;
    while existing.iter().any(|s| s.id == id) {
        id = format!("sched-{base}-{n}");
        n += 1;
    }
    id
}

/// Milliseconds since the Unix epoch — this module's one unit of time
/// throughout ([`next_due`]'s arithmetic, `lastRun`'s persisted string).
/// Thin wrapper over `totp::now_ms` (already `pub`, already this crate's one
/// clock read for a security-relevant deadline — see
/// `ipc::airgap::schedule_unlock`'s identical `as i64` cast) rather than an
/// independent `SystemTime::now()` call: one clock source, read the same
/// way, everywhere a "now" matters in this crate.
pub fn now_ms() -> i64 {
    crate::totp::now_ms() as i64
}

const MINUTE_MS: i64 = 60_000;
const HOUR_MS: i64 = 3_600_000;
const DAY_MS: i64 = 86_400_000;

/// Pure: is a run due for `when`, given `last_run` (epoch ms, `None` if this
/// schedule has never started successfully) and the current instant `now`
/// (epoch ms)? `Some(due_at)` — `due_at` is the slot boundary that has
/// arrived, always `<= now` — means yes; `None` means nothing is due yet.
///
/// - `Interval { minutes }`: never-run is due immediately (`Some(now)`);
///   otherwise due once `minutes` have elapsed since `last_run`.
/// - `Daily { hour, minute }`: due once `now` is past TODAY's `hour:minute`
///   UTC slot AND `last_run` (if any) is from before that same slot — a
///   schedule that already ran for today's slot does not fire again just
///   because the tick loop keeps checking every 30s for the rest of the day.
pub fn next_due(when: &When, last_run: Option<i64>, now: i64) -> Option<i64> {
    match when {
        When::Interval { minutes } => {
            let period_ms = i64::from(*minutes).saturating_mul(MINUTE_MS);
            let due_at = match last_run {
                None => now,
                Some(lr) => lr.saturating_add(period_ms),
            };
            (due_at <= now).then_some(due_at)
        }
        When::Daily { hour, minute } => {
            let day_start = now - now.rem_euclid(DAY_MS);
            let slot = day_start + i64::from(*hour) * HOUR_MS + i64::from(*minute) * MINUTE_MS;
            if now < slot {
                return None; // today's slot has not arrived yet — a stale
                             // last_run from a previous day never fires it early
            }
            match last_run {
                Some(lr) if lr >= slot => None, // already ran for today's slot
                _ => Some(slot),
            }
        }
    }
}

/// Every enabled, non-suspended schedule whose [`next_due`] resolves `Some`
/// as of `now` — pure, no filesystem or lock-state involved (that split is
/// [`plan_tick`]'s job).
fn due_schedule_ids(schedules: &[Schedule], now: i64) -> Vec<String> {
    schedules
        .iter()
        .filter(|s| s.enabled && s.suspended.is_none())
        .filter(|s| {
            let last_run = s.last_run.as_deref().and_then(parse_iso8601_ms);
            next_due(&s.when, last_run, now).is_some()
        })
        .map(|s| s.id.clone())
        .collect()
}

/// One 30s tick's verdict, pure and fully unit-testable without `tauri` —
/// the "ticker skip-while-locked" decision extracted from
/// `ipc::schedules::run_tick`'s otherwise-`AppHandle`-shaped body.
/// `locked_skip_count` is how many schedules WOULD have started this tick
/// had the app not been locked (only meaningful, and only ever non-zero,
/// when `locked` was `true`); `run_ids` is which schedules the caller should
/// actually attempt when `locked` was `false`. Never both at once — a
/// locked tick starts nothing, full stop, and reports why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TickPlan {
    pub locked_skip_count: usize,
    pub run_ids: Vec<String>,
}

pub fn plan_tick(schedules: &[Schedule], locked: bool, now: i64) -> TickPlan {
    let due = due_schedule_ids(schedules, now);
    if locked {
        TickPlan {
            locked_skip_count: due.len(),
            run_ids: Vec::new(),
        }
    } else {
        TickPlan {
            locked_skip_count: 0,
            run_ids: due,
        }
    }
}

/// What `run_tick` should do about one due, unlocked schedule — pure given
/// the caller's already-fetched inputs (a fresh hash of the flow file's
/// CURRENT content, and whether a run of the same flow path is already
/// live), so the hash-suspend path and the already-running skip are both
/// unit-testable with plain strings/bools, no real file or live `Runner`
/// registry required.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DueOutcome {
    /// `fresh_hash` no longer matches `schedule.flow_sha1` — the flow file
    /// changed (or is unreadable — see [`decide_due_schedule`]'s doc
    /// comment) since the last `schedules_set`. Never run unreviewed
    /// content; suspend instead.
    Suspend,
    /// A run of this exact flow path is already live — never start a
    /// second, overlapping instance of the same flow.
    AlreadyRunning,
    /// Clear to launch.
    Start,
}

/// `fresh_hash` empty-string reads as a guaranteed mismatch (a real sha1 hex
/// digest is never empty) — the caller passes `""` when the flow file could
/// not even be read, so "unreadable" and "changed" collapse to the same
/// safe outcome without a separate branch here.
pub fn decide_due_schedule(
    schedule: &Schedule,
    fresh_hash: &str,
    flow_already_running: bool,
) -> DueOutcome {
    if fresh_hash != schedule.flow_sha1 {
        return DueOutcome::Suspend;
    }
    if flow_already_running {
        return DueOutcome::AlreadyRunning;
    }
    DueOutcome::Start
}

/// True when `snapshot` (the exact shape `flow::runner::snapshot_all`
/// returns) already has a `"running"` entry for `flow_path` — the ticker's
/// single-flight guard. Takes the already-fetched snapshot `Value` rather
/// than `&Runner` so it is unit-testable with a hand-built JSON array, no
/// live registry required.
pub fn flow_path_has_a_running_run(snapshot: &Value, flow_path: &str) -> bool {
    snapshot
        .as_array()
        .map(|runs| {
            runs.iter()
                .any(|r| r["flowPath"] == flow_path && r["status"] == "running")
        })
        .unwrap_or(false)
}

// ---- ISO-8601 UTC millisecond round trip (lastRun's persisted format) ----
//
// Full calendar math, not `chrono` (v1 has no chrono dependency — see the
// module doc comment). Both directions are self-contained to this module: no
// external ISO string is ever parsed here — `schedules_set` never accepts a
// caller-supplied `lastRun` — so the parser only ever has to round-trip
// exactly what `format_iso8601_ms` itself writes, not the full ISO-8601
// grammar.

/// Duplicated from the identical algorithm `eventlog.rs` and
/// `flow::runner::mod` each already carry their own private copy of (Howard
/// Hinnant's `civil_from_days`) — a third copy, for the same reason the
/// second one exists: see `flow_env`'s doc comment on this crate's
/// established "small and self-contained enough that duplicating it costs
/// far less than reaching into a different slice's private fn" pattern.
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

/// The inverse of [`civil_from_days`] (Howard Hinnant's `days_from_civil`) —
/// needed here (and not by either existing copy above, which only ever
/// format, never parse) because `lastRun` has to round-trip: this module
/// writes it, and `next_due` needs it back as milliseconds.
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // [0, 399]
    let mp = (i64::from(month) + 9) % 12; // [0, 11]: Mar=0 .. Feb=11
    let doy = (153 * mp + 2) / 5 + i64::from(day) - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146_097 + doe - 719_468
}

/// `YYYY-MM-DDTHH:MM:SS.mmmZ` — matches `flow::runner::mod`'s own
/// `format_iso8601` shape exactly (a different private copy, taking a
/// `SystemTime` rather than raw millis — this module works in millis
/// throughout, see [`now_ms`]'s doc comment), so a schedule's `lastRun`
/// reads identically to a run's own `started`/`ended` timestamps.
pub fn format_iso8601_ms(ms: i64) -> String {
    let millis = ms.rem_euclid(1000);
    let secs_total = ms.div_euclid(1000);
    let days = secs_total.div_euclid(86_400);
    let secs_of_day = secs_total.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let hour = secs_of_day / 3600;
    let minute = (secs_of_day % 3600) / 60;
    let second = secs_of_day % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{millis:03}Z")
}

/// Inverse of [`format_iso8601_ms`]. `None` on anything that doesn't match
/// that exact fixed-width shape byte-for-byte — deliberately strict (see
/// this section's own doc comment on why a lenient general parser is not
/// needed): a `lastRun` that fails to parse is treated as `None` by
/// [`due_schedule_ids`]'s caller, i.e. "never run", which is the safe
/// direction to fail in (it makes a schedule MORE eager to run again, never
/// silently stuck refusing forever on a value this module itself wrote).
pub fn parse_iso8601_ms(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() != 24
        || b[4] != b'-'
        || b[7] != b'-'
        || b[10] != b'T'
        || b[13] != b':'
        || b[16] != b':'
        || b[19] != b'.'
        || b[23] != b'Z'
    {
        return None;
    }
    let num = |from: usize, to: usize| s.get(from..to)?.parse::<i64>().ok();
    let year = num(0, 4)?;
    let month = num(5, 7)?;
    let day = num(8, 10)?;
    let hour = num(11, 13)?;
    let minute = num(14, 16)?;
    let second = num(17, 19)?;
    let millis = num(20, 23)?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let days = days_from_civil(year, month as u32, day as u32);
    Some((days * 86_400 + hour * 3600 + minute * 60 + second) * 1000 + millis)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sched(
        id: &str,
        when: When,
        enabled: bool,
        suspended: Option<&str>,
        last_run: Option<i64>,
    ) -> Schedule {
        Schedule {
            id: id.to_string(),
            flow_path: "/ws/.tome/flows/x.flow.json".to_string(),
            when,
            flow_sha1: "deadbeef".to_string(),
            enabled,
            suspended: suspended.map(str::to_string),
            last_run: last_run.map(format_iso8601_ms),
        }
    }

    // ---- next_due: interval ----

    #[test]
    fn next_due_interval_never_run_is_due_immediately() {
        let now = 10 * DAY_MS;
        assert_eq!(
            next_due(&When::Interval { minutes: 30 }, None, now),
            Some(now)
        );
    }

    #[test]
    fn next_due_interval_not_yet_due_before_the_boundary() {
        let last_run = 10 * DAY_MS;
        let now = last_run + 29 * MINUTE_MS;
        assert_eq!(
            next_due(&When::Interval { minutes: 30 }, Some(last_run), now),
            None
        );
    }

    #[test]
    fn next_due_interval_due_exactly_at_the_boundary() {
        let last_run = 10 * DAY_MS;
        let now = last_run + 30 * MINUTE_MS;
        assert_eq!(
            next_due(&When::Interval { minutes: 30 }, Some(last_run), now),
            Some(now)
        );
    }

    #[test]
    fn next_due_interval_due_well_after_the_boundary() {
        let last_run = 10 * DAY_MS;
        let now = last_run + 90 * MINUTE_MS;
        assert_eq!(
            next_due(&When::Interval { minutes: 30 }, Some(last_run), now),
            Some(last_run + 30 * MINUTE_MS)
        );
    }

    // ---- next_due: daily ----

    #[test]
    fn next_due_daily_not_due_before_todays_slot() {
        let now = 10 * DAY_MS + 8 * HOUR_MS; // 08:00, slot is 09:00
        assert_eq!(
            next_due(&When::Daily { hour: 9, minute: 0 }, None, now),
            None
        );
    }

    #[test]
    fn next_due_daily_due_after_slot_when_never_run() {
        let now = 10 * DAY_MS + 14 * HOUR_MS;
        let slot = 10 * DAY_MS + 9 * HOUR_MS;
        assert_eq!(
            next_due(&When::Daily { hour: 9, minute: 0 }, None, now),
            Some(slot)
        );
    }

    #[test]
    fn next_due_daily_due_after_slot_when_last_run_was_yesterday() {
        let now = 10 * DAY_MS + 14 * HOUR_MS;
        let slot = 10 * DAY_MS + 9 * HOUR_MS;
        let yesterdays_run = 9 * DAY_MS + 9 * HOUR_MS;
        assert_eq!(
            next_due(
                &When::Daily { hour: 9, minute: 0 },
                Some(yesterdays_run),
                now
            ),
            Some(slot)
        );
    }

    #[test]
    fn next_due_daily_not_due_again_once_already_run_today() {
        let now = 10 * DAY_MS + 14 * HOUR_MS;
        let ran_today = 10 * DAY_MS + 9 * HOUR_MS + 5 * MINUTE_MS;
        assert_eq!(
            next_due(&When::Daily { hour: 9, minute: 0 }, Some(ran_today), now),
            None
        );
    }

    #[test]
    fn next_due_daily_wraps_across_midnight_without_firing_early() {
        // Just past midnight, before today's slot — a run from LATE
        // yesterday (which is itself after YESTERDAY's slot) must not make
        // today's not-yet-arrived slot fire early.
        let now = 10 * DAY_MS + 1 * HOUR_MS;
        let late_yesterday = 9 * DAY_MS + 23 * HOUR_MS;
        assert_eq!(
            next_due(
                &When::Daily { hour: 9, minute: 0 },
                Some(late_yesterday),
                now
            ),
            None
        );
    }

    // ---- due_schedule_ids / plan_tick ----

    #[test]
    fn due_schedule_ids_excludes_disabled_and_suspended_even_when_due() {
        let now = 10 * DAY_MS;
        let schedules = vec![
            sched("a", When::Interval { minutes: 5 }, true, None, None),
            sched(
                "b-disabled",
                When::Interval { minutes: 5 },
                false,
                None,
                None,
            ),
            sched(
                "c-suspended",
                When::Interval { minutes: 5 },
                true,
                Some("hash-mismatch"),
                None,
            ),
        ];
        assert_eq!(due_schedule_ids(&schedules, now), vec!["a".to_string()]);
    }

    #[test]
    fn plan_tick_locked_reports_a_count_but_starts_nothing() {
        let now = 10 * DAY_MS;
        let schedules = vec![
            sched("a", When::Interval { minutes: 5 }, true, None, None),
            sched(
                "b-not-due",
                When::Interval { minutes: 5 },
                true,
                None,
                Some(now),
            ),
        ];
        let plan = plan_tick(&schedules, true, now);
        assert_eq!(
            plan,
            TickPlan {
                locked_skip_count: 1,
                run_ids: Vec::new()
            }
        );
    }

    #[test]
    fn plan_tick_locked_with_nothing_due_reports_zero() {
        let now = 10 * DAY_MS;
        let schedules = vec![sched(
            "a",
            When::Interval { minutes: 5 },
            true,
            None,
            Some(now),
        )];
        assert_eq!(
            plan_tick(&schedules, true, now),
            TickPlan {
                locked_skip_count: 0,
                run_ids: Vec::new()
            }
        );
    }

    #[test]
    fn plan_tick_unlocked_returns_the_due_ids_and_no_skip_count() {
        let now = 10 * DAY_MS;
        let schedules = vec![sched("a", When::Interval { minutes: 5 }, true, None, None)];
        assert_eq!(
            plan_tick(&schedules, false, now),
            TickPlan {
                locked_skip_count: 0,
                run_ids: vec!["a".to_string()]
            }
        );
    }

    // ---- decide_due_schedule (hash-suspend path) ----

    #[test]
    fn decide_due_schedule_suspends_on_hash_mismatch() {
        let s = sched("a", When::Interval { minutes: 5 }, true, None, None);
        assert_eq!(
            decide_due_schedule(&s, "not-the-stored-hash", false),
            DueOutcome::Suspend
        );
    }

    #[test]
    fn decide_due_schedule_suspends_when_the_file_could_not_be_read() {
        let s = sched("a", When::Interval { minutes: 5 }, true, None, None);
        // Caller passes "" for an unreadable file — never a silent match.
        assert_eq!(decide_due_schedule(&s, "", false), DueOutcome::Suspend);
    }

    #[test]
    fn decide_due_schedule_skips_when_the_same_flow_is_already_running() {
        let mut s = sched("a", When::Interval { minutes: 5 }, true, None, None);
        s.flow_sha1 = "matching".to_string();
        assert_eq!(
            decide_due_schedule(&s, "matching", true),
            DueOutcome::AlreadyRunning
        );
    }

    #[test]
    fn decide_due_schedule_starts_when_hash_matches_and_nothing_is_running() {
        let mut s = sched("a", When::Interval { minutes: 5 }, true, None, None);
        s.flow_sha1 = "matching".to_string();
        assert_eq!(
            decide_due_schedule(&s, "matching", false),
            DueOutcome::Start
        );
    }

    // ---- flow_path_has_a_running_run ----

    #[test]
    fn flow_path_has_a_running_run_true_only_for_a_running_status() {
        let snapshot = json!([
            {"flowPath": "/ws/a.flow.json", "status": "running"},
            {"flowPath": "/ws/b.flow.json", "status": "done"},
        ]);
        assert!(flow_path_has_a_running_run(&snapshot, "/ws/a.flow.json"));
        assert!(!flow_path_has_a_running_run(&snapshot, "/ws/b.flow.json"));
        assert!(!flow_path_has_a_running_run(
            &snapshot,
            "/ws/nope.flow.json"
        ));
    }

    #[test]
    fn flow_path_has_a_running_run_false_for_an_empty_snapshot() {
        assert!(!flow_path_has_a_running_run(&json!([]), "/ws/a.flow.json"));
    }

    // ---- the gapped-frozen assertion ----

    #[tokio::test]
    async fn scheduled_run_airgap_is_unconditionally_frozen_true() {
        assert!(SCHEDULED_RUN_AIRGAP);
        let frozen = crate::flow_env::frozen_airgap_default(SCHEDULED_RUN_AIRGAP);
        assert!(
            (frozen)().await,
            "a scheduled run's air gap must never resolve false"
        );
    }

    // ---- validate_when ----

    #[test]
    fn validate_when_rejects_zero_minutes() {
        assert!(validate_when(&When::Interval { minutes: 0 }).is_err());
    }

    #[test]
    fn validate_when_rejects_an_out_of_range_daily_time() {
        assert!(validate_when(&When::Daily {
            hour: 24,
            minute: 0
        })
        .is_err());
        assert!(validate_when(&When::Daily {
            hour: 0,
            minute: 60
        })
        .is_err());
    }

    #[test]
    fn validate_when_accepts_good_values() {
        assert!(validate_when(&When::Interval { minutes: 1 }).is_ok());
        assert!(validate_when(&When::Daily {
            hour: 23,
            minute: 59
        })
        .is_ok());
        assert!(validate_when(&When::Daily { hour: 0, minute: 0 }).is_ok());
    }

    // ---- iso8601 round trip ----

    #[test]
    fn iso8601_epoch_zero_formats_to_the_expected_string() {
        assert_eq!(format_iso8601_ms(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(parse_iso8601_ms("1970-01-01T00:00:00.000Z"), Some(0));
    }

    #[test]
    fn iso8601_one_day_formats_and_parses_back() {
        assert_eq!(format_iso8601_ms(DAY_MS), "1970-01-02T00:00:00.000Z");
        assert_eq!(parse_iso8601_ms("1970-01-02T00:00:00.000Z"), Some(DAY_MS));
    }

    #[test]
    fn iso8601_round_trips_a_leap_day_with_nonzero_millis() {
        let s = "2024-02-29T12:34:56.789Z";
        let ms = parse_iso8601_ms(s).expect("valid ISO string must parse");
        assert_eq!(format_iso8601_ms(ms), s);
    }

    #[test]
    fn iso8601_round_trips_now_ms_itself() {
        let now = now_ms();
        let s = format_iso8601_ms(now);
        assert_eq!(parse_iso8601_ms(&s), Some(now));
    }

    #[test]
    fn parse_iso8601_ms_rejects_malformed_input() {
        for bad in [
            "",
            "not a date",
            "2024-02-29T12:34:56.789",  // missing trailing Z
            "2024-02-29 12:34:56.789Z", // missing T
            "2024-13-01T00:00:00.000Z", // month 13
            "2024-02-29T12:34:56.78Z",  // millis too short -> wrong length
        ] {
            assert_eq!(parse_iso8601_ms(bad), None, "should reject {bad:?}");
        }
    }

    // ---- new_schedule_id ----

    #[test]
    fn new_schedule_id_has_the_sched_prefix() {
        let id = new_schedule_id(&[]);
        assert!(id.starts_with("sched-"));
    }

    #[test]
    fn new_schedule_id_avoids_a_forced_collision() {
        let forced = new_schedule_id(&[]);
        let existing = vec![sched(
            &forced,
            When::Interval { minutes: 5 },
            true,
            None,
            None,
        )];
        assert_ne!(new_schedule_id(&existing), forced);
    }

    // ---- load / save round trip ----

    #[test]
    fn save_then_load_round_trips_and_writes_0600() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Schedules::default();
        store.schedules.push(sched(
            "sched-1",
            When::Daily {
                hour: 9,
                minute: 30,
            },
            true,
            None,
            None,
        ));
        save(dir.path(), &store).unwrap();

        let loaded = load(dir.path());
        assert_eq!(loaded.version, 1);
        assert_eq!(loaded.schedules, store.schedules);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(file_path(dir.path()))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn load_of_a_missing_file_is_an_empty_v1_store() {
        let dir = tempfile::tempdir().unwrap();
        let store = load(dir.path());
        assert_eq!(store.version, 1);
        assert!(store.schedules.is_empty());
    }

    #[test]
    fn load_of_corrupt_json_is_an_empty_v1_store_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(file_path(dir.path()), b"{not json").unwrap();
        let store = load(dir.path());
        assert_eq!(store.version, 1);
        assert!(store.schedules.is_empty());
    }

    // ---- wire shape ----

    #[test]
    fn when_serializes_to_the_pinned_tagged_shape() {
        assert_eq!(
            serde_json::to_value(When::Interval { minutes: 15 }).unwrap(),
            json!({"kind": "interval", "minutes": 15})
        );
        assert_eq!(
            serde_json::to_value(When::Daily { hour: 9, minute: 5 }).unwrap(),
            json!({"kind": "daily", "hour": 9, "minute": 5})
        );
    }

    #[test]
    fn schedule_serializes_suspended_and_last_run_as_explicit_null_not_omitted() {
        let s = sched("sched-1", When::Interval { minutes: 5 }, true, None, None);
        let v = serde_json::to_value(&s).unwrap();
        assert!(v.get("suspended").is_some());
        assert_eq!(v["suspended"], Value::Null);
        assert!(v.get("lastRun").is_some());
        assert_eq!(v["lastRun"], Value::Null);
    }
}
