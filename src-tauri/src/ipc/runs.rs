//! Background flow-run commands: start/cancel/list. Thin wrappers over
//! `flow::runner`'s free-function API (`flow-runner.js`'s own module-level
//! `startRun`/`cancelRun`/`snapshotAll`) — every real body lives there;
//! this file's only job is resolving the two Tauri-specific handles those
//! functions need (`state.flow`, the live registry, and a production
//! [`crate::flow::runner::env::RunnerEnv`] built fresh per call), enforcing
//! `runs:start`'s own TOME-001 re-auth ceremony (see that command's doc
//! comment), and mapping the wire shape.
//!
//! `runs:start`/`runs:cancel`/`runs:list` never throw in the JS original —
//! every outcome (including a refusal) is a plain `{ id }` / `{ error }` /
//! `{ ok }` value the handler resolves with, matching `Ok(json!(...))`
//! here rather than an `Err`.

use serde_json::{json, Value};
use tauri::{AppHandle, Manager, State};

use crate::flow;
use crate::lock_gate;
use crate::state::AppState;

/// `runs:start` (`tome-ipc.js`'s `start: (flowPath) => call('runs_start', {
/// flowPath })` — `auth` is a Rust-only addition, absent from every
/// existing renderer call, so it deserializes to `None` exactly like
/// `ipc::airgap::airgap_unlock`'s own bare `Option<String>` parameters do
/// when the caller omits them). Resolves `{ id }` once every node is
/// planned and the first layer is spawning, `{ error }` for an ordinary
/// refusal, or `{ reauth: true, error }` for the TOME-001 ceremony below —
/// refusals happen before anything is written or spawned.
///
/// ## TOME-001 re-auth ceremony for background runs
///
/// A background flow run is a second, independent process-spawn path
/// alongside `ipc::pty::pty_create` — every node it launches is a real
/// `claude -p ...`-shaped headless process, gapped or not, per the SAME
/// `airgap-default` preference a freshly spawned interactive pane would
/// read (`flow::runner::start_run`'s own `(env.airgap_default)().await`).
/// Without a gate here, a compromised renderer could call `runs:start`
/// directly with an arbitrary flow path and get an unsandboxed,
/// network-open headless agent process spawned with zero fresh proof of
/// the user — exactly the threat `pty_create`'s own ceremony
/// (`ipc::pty::pty.rs`'s `pty_create`, ~line 522) exists to close, and
/// that a background run has no pane/window of its own to make any less
/// dangerous. This mirrors that ceremony's three-way outcome
/// (`ipc::pty::{evaluate_reauth, ReauthOutcome}`, reused rather than
/// duplicated) under a SEPARATE `"flow:unrestricted"` throttle bucket, so a
/// failed attempt here never counts against — or is masked by — the
/// terminal pane's own `"pty:unrestricted"` bucket.
///
/// `airgap_default` is resolved ONCE here, then frozen into the
/// `RunnerEnv` handed to `start_run` via
/// [`crate::flow::runner::env::frozen_airgap_default`] — see that
/// function's own doc comment for why re-reading the store a second,
/// independent time (inside `start_run`) would reopen a narrow TOCTOU gap
/// this gate is specifically meant to close.
///
/// No renderer today ever supplies `auth` (`tome-ipc.js`'s `runs.start`
/// takes only a `flowPath`), so in practice this gate currently only ever
/// reaches the "no credentials supplied" refusal — it fails CLOSED (no
/// run starts, nothing spawns) rather than silently falling back to the
/// pre-fix unsandboxed-spawn behavior. Collecting and forwarding a real
/// passphrase/TOTP payload from a renderer-side prompt (mirroring however
/// the terminal pane's own reauth UI works) is a follow-up outside this
/// backend slice's scope.
#[tauri::command]
pub async fn runs_start(
    app: AppHandle,
    state: State<'_, AppState>,
    flow_path: String,
    auth: Option<Value>,
) -> Result<Value, String> {
    lock_gate::guard(&state, "runs:start")?;

    let mut env = flow::runner::env::production_env(app.clone());
    let effective_gapped = (env.airgap_default)().await;
    // Freeze it — see `frozen_airgap_default`'s doc comment.
    env.airgap_default = flow::runner::env::frozen_airgap_default(effective_gapped);

    if !effective_gapped {
        let auth_configured = state
            .auth
            .lock()
            .expect("AppState.auth lock poisoned")
            .as_ref()
            .map(|a| a.status().configured)
            .unwrap_or(false);
        if crate::pty_authority::unrestricted_spawn_needs_reauth(effective_gapped, auth_configured) {
            let mut guard = state.auth.lock().expect("AppState.auth lock poisoned");
            let auth_lock = guard.as_mut().ok_or_else(|| "auth: not initialized".to_string())?;
            let wait = auth_lock.throttle_retry_in("flow:unrestricted");
            if wait > 0 {
                return Ok(json!({
                    "reauth": true,
                    "error": format!("Too many attempts. Wait {}s.", crate::ipc::auth::ceil_seconds(wait)),
                }));
            }
            let payload_supplied = auth.is_some();
            let verified = payload_supplied
                && {
                    let payload = auth.as_ref().expect("payload_supplied just checked Some");
                    if auth_lock.totp_active() {
                        payload.get("code").and_then(Value::as_str).is_some_and(|c| auth_lock.verify_totp(c))
                    } else {
                        payload.get("passphrase").and_then(Value::as_str).is_some_and(|p| auth_lock.verify_passphrase(p))
                    }
                };
            match crate::ipc::pty::evaluate_reauth(payload_supplied, verified) {
                crate::ipc::pty::ReauthOutcome::NeedsCredentials => {
                    return Ok(json!({
                        "reauth": true,
                        "error": "refused: this run would be unsandboxed and needs a fresh passphrase or code, which this pane can't collect yet — turn the air gap back on to run flows in the background",
                    }));
                }
                crate::ipc::pty::ReauthOutcome::Rejected => {
                    auth_lock.record_failure("flow:unrestricted");
                    return Ok(json!({"reauth": true, "error": "Incorrect passphrase or code."}));
                }
                crate::ipc::pty::ReauthOutcome::Verified => auth_lock.record_success("flow:unrestricted"),
            }
        }
    }

    // `app.state::<AppState>().inner()` (not the `state` parameter above)
    // — this needs a `'static` handle: `start_run`'s own scheduling loop
    // and every node's exit-await task outlive this single command
    // invocation, and `State<'_, AppState>`'s borrow does not. Mirrors
    // `ipc::airgap`'s `AirgapEnv::app_state` doing the same for its own
    // long-lived timer tasks.
    let runs = app.state::<AppState>().inner().flow.clone();
    Ok(flow::runner::start_run(runs, env, flow_path).await)
}

/// `runs:cancel` (`{ id }` — `tome-ipc.js` wraps the bare id). `{ ok: true
/// }` for both a real cancellation and a no-op (already finished), `{
/// error }` only for an unknown id.
#[tauri::command]
pub async fn runs_cancel(app: AppHandle, state: State<'_, AppState>, id: String) -> Result<Value, String> {
    lock_gate::guard(&state, "runs:cancel")?;
    let runs = app.state::<AppState>().inner().flow.clone();
    let env = flow::runner::env::production_env(app);
    Ok(flow::runner::cancel_run(&runs, &env, &id))
}

/// `runs:list` (no args) — the same array shape `runs:changed` pushes:
/// every run this session knows about, newest first.
#[tauri::command]
pub async fn runs_list(state: State<'_, AppState>) -> Result<Value, String> {
    lock_gate::guard(&state, "runs:list")?;
    Ok(flow::runner::snapshot_all(&state.flow))
}

#[cfg(test)]
mod tests {
    // `runs_cancel`/`runs_list` are thin wrappers with no logic of their
    // own beyond `lock_gate::guard` (covered by `lock_gate.rs`'s own
    // `channel_table_matches_lib_rs_registration` test, which proves these
    // three are registered under the exact wire-channel strings
    // `CHANNEL_OF_COMMAND` pins) and delegating to `flow::runner`'s free
    // functions, which carry this slice's real test coverage
    // (`flow::runner::tests`, ported from `test/flow-runner.test.js`).
    // `runs_start` additionally runs the TOME-001 re-auth ceremony above —
    // its pure decision core (`evaluate_reauth`/`ReauthOutcome`,
    // `pty_authority::unrestricted_spawn_needs_reauth`) is reused, not
    // duplicated, from `ipc::pty`/`pty_authority`, which carry that
    // logic's own test coverage; `frozen_airgap_default`'s own test
    // (`flow::runner::env::tests`) covers the one piece of genuinely new
    // logic this command adds. A `#[tauri::command]` fn cannot be called
    // directly without a live `AppHandle`/`State` (this crate enables no
    // `tauri` `test` feature — see `confine.rs`'s doc comment on the same
    // constraint), so there is nothing hermetic left to unit test in this
    // file itself.
    #[allow(unused_imports)]
    use super::*;
}
