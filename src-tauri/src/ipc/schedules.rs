//! The in-app scheduler's IPC surface (`schedules:list`/`schedules:set`/
//! `schedules:delete`) plus [`run_tick`], the 30-second driver
//! `lib.rs::spawn_schedule_ticker` calls into — see `crate::schedule`'s
//! module doc comment for the security rationale (consent-now/
//! re-verify-forever hashing, the always-gapped override, no catch-up
//! for missed slots) this file only threads through. Mirrors
//! `ipc::export`'s split exactly: the pure model/persistence/decision core
//! lives in the sibling root module (`crate::schedule`), this file is the
//! thin `AppHandle`/`State`-resolving wrapper around it.

use tauri::{AppHandle, Manager, State};

use serde_json::{json, Value};

use crate::state::AppState;
use crate::{events, flow, flow_env, lock_gate, schedule};

use std::path::PathBuf;

/// Same resolution every other command in this crate uses
/// (`ipc::export::app_data_dir`, `ipc::store::store_get`, ...).
fn app_data_dir(app: &AppHandle) -> Result<PathBuf, String> {
    app.path().app_data_dir().map_err(|e| e.to_string())
}

/// `schedules:list` (no args) — every persisted schedule, in stored order.
#[tauri::command]
pub async fn schedules_list(app: AppHandle, state: State<'_, AppState>) -> Result<Value, String> {
    lock_gate::guard(&state, "schedules:list")?;
    let dir = app_data_dir(&app)?;
    let store = tokio::task::spawn_blocking(move || schedule::load(&dir))
        .await
        .map_err(|e| e.to_string())?;
    serde_json::to_value(store.schedules).map_err(|e| e.to_string())
}

/// `schedules:set` (`{ id?, flowPath, when, enabled }`) — THE consent
/// ceremony for the scheduler (see `crate::schedule`'s module doc comment):
/// reads `flowPath` right now, sha1-hashes the exact text, and records that
/// hash as the new `flowSha1` baseline — never a caller-supplied hash, the
/// same "this call defines truth" shape `export::canonicalize` uses, unlike
/// `egress::consent_repo_allowlist`'s re-check against a presented hash
/// (there is nothing to re-check against yet; that re-check is
/// [`run_tick`]'s job on every later tick, against the hash THIS call just
/// wrote). Always clears `suspended` and always re-hashes, whether creating
/// a schedule (`id` omitted, or naming one this store does not have),
/// editing one, or just flipping `enabled` — there is exactly one mutation
/// path here, deliberately: preferences.js's enable/disable toggle and its
/// "Re-consent" action for a suspended schedule both round-trip through this
/// same command with the record's current field values.
///
/// The read is deliberately UNCONFINED (a plain `tokio::fs::read_to_string`,
/// not `flow::confine`) — `flowPath` arrives from `panels/flow.js`'s own
/// already-open flow panel (`this.path`), the same "renderer compromise
/// already equals user-privileged file access" trust bucket `fs.rs`'s module
/// doc comment carves out for `fs:readFile`. The real gate is downstream, at
/// every actual scheduled launch: `flow::runner::start_run`'s own
/// `can_open_file` check runs fresh in [`run_tick`], so a schedule can never
/// make main touch a file outside the open workspace folders, whatever path
/// was hashed here.
#[tauri::command]
pub async fn schedules_set(
    app: AppHandle,
    state: State<'_, AppState>,
    id: Option<String>,
    flow_path: String,
    when: schedule::When,
    enabled: bool,
) -> Result<Value, String> {
    lock_gate::guard(&state, "schedules:set")?;
    schedule::validate_when(&when)?;
    let text = tokio::fs::read_to_string(&flow_path)
        .await
        .map_err(|e| format!("could not read flow: {e}"))?;
    let flow_sha1 = crate::egress::sha1_hex(&text);
    let dir = app_data_dir(&app)?;
    let sched_id = tokio::task::spawn_blocking(move || -> Result<String, String> {
        let mut store = schedule::load(&dir);
        let sched_id = id
            .filter(|i| store.schedules.iter().any(|s| &s.id == i))
            .unwrap_or_else(|| schedule::new_schedule_id(&store.schedules));
        // Re-consenting an existing schedule must not reset its own run
        // history — lastRun is run_tick's bookkeeping, untouched by consent.
        let last_run = store
            .schedules
            .iter()
            .find(|s| s.id == sched_id)
            .and_then(|s| s.last_run.clone());
        store.schedules.retain(|s| s.id != sched_id);
        store.schedules.push(schedule::Schedule {
            id: sched_id.clone(),
            flow_path,
            when,
            flow_sha1,
            enabled,
            suspended: None,
            last_run,
        });
        schedule::save(&dir, &store)?;
        Ok(sched_id)
    })
    .await
    .map_err(|e| e.to_string())??;
    events::log_event(&app, "schedule:set", vec![("id", json!(sched_id))]);
    Ok(json!({"ok": true, "id": sched_id}))
}

/// `schedules:delete` (`{ id }`). Always `{ ok: true }`, even for an id with
/// nothing to delete — same idempotent, no-such-id-error shape
/// `export_revoke` uses.
#[tauri::command]
pub async fn schedules_delete(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> Result<Value, String> {
    lock_gate::guard(&state, "schedules:delete")?;
    let dir = app_data_dir(&app)?;
    let existed = tokio::task::spawn_blocking({
        let id = id.clone();
        move || -> Result<bool, String> {
            let mut store = schedule::load(&dir);
            let before = store.schedules.len();
            store.schedules.retain(|s| s.id != id);
            let existed = store.schedules.len() != before;
            schedule::save(&dir, &store)?;
            Ok(existed)
        }
    })
    .await
    .map_err(|e| e.to_string())??;
    if existed {
        events::log_event(&app, "schedule:delete", vec![("id", json!(id))]);
    }
    Ok(json!({"ok": true}))
}

/// The scheduler's one 30-second tick — called from the ticker
/// `lib.rs::spawn_schedule_ticker` spawns at boot. Never a
/// `#[tauri::command]`: nothing on the renderer side calls this directly,
/// and the "nothing spawns while locked" gate it enforces is not the IPC
/// lock gate (there is no IPC call here to guard) — it is
/// `schedule::plan_tick` reading `AppState.locked` itself, the same way
/// `flow::runner::start_run` being callable directly from main (rather than
/// only from `ipc::runs::runs_start`) is documented as deliberate: "the IPC
/// guard protects the renderer surface, not the engine."
///
/// All the actual DECIDING is delegated to `schedule::plan_tick`/
/// `schedule::decide_due_schedule` — both pure, both unit-tested without any
/// of the `AppHandle`/filesystem/runner plumbing this function exists only
/// to thread between them.
pub(crate) async fn run_tick(app: &AppHandle) {
    let state = app.state::<AppState>();
    let locked = *state.locked.read().expect("AppState.locked lock poisoned");
    let Ok(dir) = app.path().app_data_dir() else {
        return;
    };
    let load_dir = dir.clone();
    let mut store = match tokio::task::spawn_blocking(move || schedule::load(&load_dir)).await {
        Ok(s) => s,
        Err(_) => return, // the blocking task itself panicked — nothing safe to act on
    };

    let now = schedule::now_ms();
    let plan = schedule::plan_tick(&store.schedules, locked, now);
    if plan.locked_skip_count > 0 {
        // ONE event for the whole tick, carrying a count — never one event
        // per schedule, and never logged at all when nothing was due (a
        // locked app with no due schedules is not an event worth a line,
        // and this ticker runs forever, locked or not — the persistent
        // event log is capped, and "locked" can last for days).
        events::log_event(
            app,
            "schedule:skipped-locked",
            vec![("count", json!(plan.locked_skip_count))],
        );
    }
    if plan.run_ids.is_empty() {
        return;
    }

    // One snapshot for the whole tick: every due schedule's "already
    // running?" check reads the same instant, rather than racing a start
    // this same loop just kicked off for an earlier id in the list. The
    // snapshot alone is not enough, though — it predates every `start_run`
    // this loop itself calls, so it can never show one of THOSE as
    // running. `started_this_tick` closes that gap: two due schedules that
    // share a `flowPath` (nothing prevents `schedules_set` from creating
    // that) would otherwise both read `already_running == false` off the
    // same stale snapshot and both launch, defeating
    // `DueOutcome::AlreadyRunning`'s own "never start a second, overlapping
    // instance of the same flow" contract within a single tick.
    let snapshot = flow::runner::snapshot_all(&state.flow);
    let mut started_this_tick: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut changed = false;
    for id in &plan.run_ids {
        let Some(idx) = store.schedules.iter().position(|s| &s.id == id) else {
            continue; // deleted between plan_tick's read and now
        };
        let flow_path = store.schedules[idx].flow_path.clone();
        // Re-read and re-hash NOW — plan_tick/due_schedule_ids never touch
        // the filesystem, so nothing upstream of this line has verified the
        // file still matches what schedules_set last consented to.
        // Unreadable reads as a guaranteed mismatch below (an empty string
        // can never equal a real 40-hex-char sha1) — see
        // schedule::decide_due_schedule's own doc comment.
        let fresh_hash = match tokio::fs::read_to_string(&flow_path).await {
            Ok(text) => crate::egress::sha1_hex(&text),
            Err(_) => String::new(),
        };
        let already_running = schedule::flow_path_has_a_running_run(&snapshot, &flow_path)
            || started_this_tick.contains(&flow_path);
        match schedule::decide_due_schedule(&store.schedules[idx], &fresh_hash, already_running) {
            schedule::DueOutcome::Suspend => {
                store.schedules[idx].suspended = Some("hash-mismatch".to_string());
                changed = true;
                events::log_event(app, "schedule:suspended", vec![("id", json!(id))]);
            }
            schedule::DueOutcome::AlreadyRunning => {
                // Retried next tick — a live run of this exact flow path
                // already exists (this schedule's own prior start, or a
                // manual Run of the same file); a second concurrent
                // instance is never what "every N minutes" means.
            }
            schedule::DueOutcome::Start => {
                let mut env = flow_env::production_env(app.clone());
                // The override that makes an ungapped scheduled run
                // structurally impossible — see
                // schedule::SCHEDULED_RUN_EGRESS's doc comment. Never the
                // `egress-default` store preference: there is no
                // interactive user to ask at 3am, so this path never tries.
                env.egress_default =
                    flow_env::frozen_egress_default(schedule::SCHEDULED_RUN_EGRESS);
                let runs = state.inner().flow.clone();
                let res = flow::runner::start_run(runs, env, flow_path.clone()).await;
                if res.get("id").and_then(Value::as_str).is_some() {
                    store.schedules[idx].last_run =
                        Some(schedule::format_iso8601_ms(schedule::now_ms()));
                    changed = true;
                    // Only on an actual start — a refusal never created a
                    // live run, so it must not block another schedule at
                    // the same flow path from trying its own start later
                    // in this same loop.
                    started_this_tick.insert(flow_path.clone());
                }
                // A refusal (`{"error": ...}`) is left for the next tick to
                // retry — start_run never wrote or spawned anything on that
                // path (its own doc comment), so there is nothing to undo,
                // and a transient reason (workspace not synced yet at boot)
                // resolves itself without this schedule needing to be
                // touched by hand.
            }
        }
    }
    if changed {
        let save_dir = dir.clone();
        let _ = tokio::task::spawn_blocking(move || schedule::save(&save_dir, &store)).await;
    }
}

#[cfg(test)]
mod tests {
    // schedules_list/set/delete are thin wrappers: validation and hashing
    // live in `schedule::validate_when`/`schedules_set`'s own body (mirrored
    // by `schedule.rs`'s `#[cfg(test)]` suite for every pure piece), and
    // `lock_gate::guard`'s wiring is covered by
    // `lock_gate::tests::channel_table_matches_lib_rs_registration`, which
    // proves these three commands are registered under the exact
    // wire-channel strings `CHANNEL_OF_COMMAND` pins. `run_tick`'s own
    // decision logic (`schedule::plan_tick`/`decide_due_schedule`/
    // `flow_path_has_a_running_run`, and the gapped-frozen assertion) is
    // exhaustively covered in `schedule.rs` precisely so it does not need a
    // live `AppHandle`/`State` here — this crate enables no `tauri` `test`
    // feature (see `confine.rs`'s doc comment on the same constraint), so
    // there is nothing hermetic left to unit test in this file itself.
    #[allow(unused_imports)]
    use super::*;
}
