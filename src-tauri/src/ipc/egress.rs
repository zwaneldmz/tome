//! The egress: proxy lifecycle, allowlist, consents, unlock/relock, repo
//! allowlist consent. Ports `src/main/egress.js` + the `egress:*` handler
//! bodies from `src/main/index.js` (~800-847) — exact return shapes,
//! including the auth-adjacent throttle/verify plumbing those handlers
//! inline directly rather than delegating to `authlock.js` alone.
//!
//! This file OWNS the integration between the three landed build slices:
//! `egress::EgressState` (pure pane-gapping bookkeeping + repo consent,
//! `egress/mod.rs`), `egress::proxy::PaneProxy` (the live per-pane loopback
//! proxy, `egress/proxy.rs`), and `egress::allowlist` (the hostname
//! matcher + `DEFAULT_ALLOW`, `egress/allowlist.rs`). Neither
//! `EgressState` nor `PaneProxy` talks to the other — see both modules'
//! own doc comments — so every place a pane's mode changes, THIS file
//! updates both: `EgressState` for the `egress:state` UI snapshot,
//! `PaneProxy` for the actual live enforcement. `ipc::pty::pty_create`
//! (a sibling file, this same slice) is the only other caller of the
//! `pub(crate)` helpers below — it creates a pane's `PaneProxy` and
//! registers it in `AppState.proxies`/`EgressState` in the first place.
//!
//! Deliberately NOT ported this phase: `egress.js`'s `loadAllowlist`/
//! `userAllow` (a `userData/egress.json` override file that REPLACES
//! `DEFAULT_ALLOW` when present — there is no IPC command exposing it in
//! either the JS original or this task's command list, and
//! `egress::EgressState` does not track it). [`effective_allow_patterns`]
//! is therefore always `DEFAULT_ALLOW ++ effective_repo_hosts()`, never a
//! user-shrunk-or-widened custom base — strictly more conservative than
//! the JS original's override path, not a fidelity gap that widens
//! anything. Flagged in this slice's task report.

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::time::Duration;

use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager, State};

use crate::egress::allowlist::DEFAULT_ALLOW;
use crate::egress::ConsentOutcome;
use crate::ipc::auth::{ceil_seconds, mark_unlocked};
use crate::{confine, events, lock_gate, state::AppState, totp};

// ---- shared integration helpers (also used by ipc::pty::pty_create) ----

/// The pane proxy's live allow set right now: shipped defaults plus every
/// currently-applied repo-consented host — mirrors `recompile()`'s
/// non-user-override half (see this module's doc comment for why the user
/// override itself has no port yet).
pub(crate) fn effective_allow_patterns(state: &AppState) -> Vec<String> {
    let mut patterns: Vec<String> = DEFAULT_ALLOW.iter().map(|s| s.to_string()).collect();
    patterns.extend(state.egress.effective_repo_hosts());
    patterns
}

/// Applies the current effective allow set to every live proxy — called
/// after a repo-consent grant/revoke, mirroring `recompile()` taking
/// effect for every pane immediately (the JS original has one shared
/// `allowMatchers` variable; here, an explicit fan-out, since each
/// `PaneProxy` holds its own independently-compiled set — see
/// `egress::proxy`'s doc comment on that split).
pub(crate) fn recompile_all_proxies(state: &AppState) {
    let patterns = effective_allow_patterns(state);
    let proxies = state
        .proxies
        .lock()
        .expect("AppState.proxies lock poisoned");
    for proxy in proxies.values() {
        proxy.set_allowed(patterns.clone());
    }
}

fn state_snapshot_json(state: &AppState) -> Value {
    serde_json::to_value(state.egress.state_snapshot())
        .expect("EgressStateSnapshot always serializes")
}

/// Seam over the two kinds of push [`schedule_unlock`]/[`relock_now`]/
/// [`create_gapped_pane_proxy`]/[`close_pane_and_proxy`] perform after
/// every pane state transition — the live event (`AppHandle::emit`) and
/// the persistent event log (`events::log_event`) — so those four
/// integration-glue functions can be driven under `#[cfg(test)]` against a
/// REAL `EgressState` and a REAL `PaneProxy`, with no live `tauri::Builder`
/// app: this crate enables no tauri `test` feature (`Cargo.toml`: `tauri =
/// { version = "2", features = [] }`), so nothing outside a running app can
/// construct a real `AppHandle` — and every one of those four functions
/// was otherwise hardwired to exactly that concrete type.
///
/// [`AppHandle`] is the production implementation (below) — every
/// `#[tauri::command]` wrapper in this file keeps taking a bare
/// `AppHandle` UNCHANGED and passes it straight through; Rust infers `E =
/// AppHandle` at every call site from the argument's concrete type,
/// including `ipc::pty::pty_create`/`pty_kill` (a different file, calling
/// [`create_gapped_pane_proxy`]/[`close_pane_and_proxy`]), so this is a
/// zero-caller-edit seam. `#[cfg(test)]`'s own implementation (this file's
/// `tests` module) wraps an owned `AppState` instead — `AppState::new()`
/// needs no live Tauri app either, it's a plain constructor.
pub(crate) trait EgressEnv: Clone + Send + Sync + 'static {
    /// The `AppState` this env is ultimately backed by. `AppHandle`'s impl
    /// resolves it via Tauri's managed-state lookup
    /// (`State::inner`, which — unlike `State` itself — carries the
    /// state's REAL lifetime rather than one tied to a temporary borrow,
    /// exactly the documented, intended use of that method); a test double
    /// just hands back a reference to the instance it owns directly.
    fn app_state(&self) -> &AppState;
    /// Fire-and-forget live push (mirrors every existing call site's own
    /// `let _ = app.emit(...)`).
    fn emit_json(&self, event: &str, payload: Value);
    /// Persistent-log push — `AppHandle`'s impl delegates to
    /// `events::log_event`.
    fn log(&self, kind: &str, fields: Vec<(&'static str, Value)>);
}

impl EgressEnv for AppHandle {
    fn app_state(&self) -> &AppState {
        self.state::<AppState>().inner()
    }
    fn emit_json(&self, event: &str, payload: Value) {
        let _ = self.emit(event, payload);
    }
    fn log(&self, kind: &str, fields: Vec<(&'static str, Value)>) {
        events::log_event(self, kind, fields);
    }
}

/// `pub(crate)`: `ipc::pty`'s `pty_kill`/`pty_create` error path and
/// [`schedule_unlock`]/[`relock_now`]/[`create_gapped_pane_proxy`] below
/// all push the same shape after mutating pane state; kept as one function
/// so `egress:state`'s wire shape is built in exactly one place.
pub(crate) fn push_state_event<E: EgressEnv>(env: &E, state: &AppState) {
    env.emit_json("egress:state", state_snapshot_json(state));
}

/// Spins up a fresh `PaneProxy` for `pane_id` — the creation-side
/// counterpart to [`close_pane_and_proxy`]: binds the loopback listener,
/// seeds it with the CURRENT effective allow set
/// ([`effective_allow_patterns`]), wires its blocked-callback to both the
/// live `egress:blocked` push (uncoalesced — mirrors `onEvent('blocked',
/// ...)` -> `win.webContents.send('egress:blocked', payload)`) and the
/// persistent event log (60s-coalesced — mirrors `logBlocked`/
/// `flushBlocked`, including the exact field-presence difference between
/// the immediate log (no `count`) and the trailing flush (`count` present,
/// only when `count >= 2`) — a `Coalesced` event's `count` can only ever be
/// `1` on the immediate fire, since the trailing flush suppresses itself
/// below 2, see `egress::proxy`'s own doc comment), registers it in both
/// `AppState.proxies` and `EgressState` (`register_pane`), and pushes
/// `egress:state` — mirrors `createPaneProxy`'s own `panes.set(...);
/// pushState()` on a successful bind.
///
/// `unix_socket_path`: `None` on macOS (`ipc::pty::pty_create`'s only
/// caller there — seatbelt needs no loopback bridge, so the proxy never
/// binds a second listener). `Some(path)` on Linux — Phase 4/slice L3's
/// wiring of the seam `egress::proxy::PaneProxy::spawn` has carried since
/// Phase 3 (see that function's own "Linux seam" doc comment): threaded
/// straight through so `tome-shim`'s in-namespace bridge has a bind-mounted
/// (bwrap) or directly-reachable (self-unshare — no mount namespace to
/// remap into, see `egress::linux`'s module doc comment) unix socket to
/// shovel bytes to. On a successful bind with a unix path, this also locks
/// the socket down to `0600` (THE DESIGN's own requirement — see
/// `egress::linux::secure_pane_socket_permissions`'s doc comment for why
/// `PaneProxy::spawn` itself doesn't do this: it has no opinion on Linux's
/// specific permission requirements, only on binding the socket). The
/// PARENT directory's own `0700` lockdown
/// (`egress::linux::ensure_pane_socket_dir`) is the caller's job, before
/// this function ever runs — `PaneProxy::spawn`'s `UnixListener::bind`
/// needs the directory to already exist.
///
/// `pub(crate)`: `ipc::pty::pty_create` is the only caller (its own doc
/// comment covers exactly when this path is taken vs. refused).
pub(crate) async fn create_gapped_pane_proxy<E: EgressEnv>(
    env: &E,
    state: &AppState,
    pane_id: &str,
    unix_socket_path: Option<PathBuf>,
) -> std::io::Result<std::sync::Arc<crate::egress::proxy::PaneProxy>> {
    let initial_allowed = effective_allow_patterns(state);
    let blocked_env = env.clone();
    let blocked_pane_id = pane_id.to_string();
    let on_blocked = move |event: crate::egress::proxy::BlockedEvent| match event {
        crate::egress::proxy::BlockedEvent::Attempt { host } => {
            blocked_env.emit_json(
                "egress:blocked",
                json!({"paneId": blocked_pane_id, "host": host}),
            );
        }
        crate::egress::proxy::BlockedEvent::Coalesced { host, count } => {
            let fields: Vec<(&'static str, Value)> = if count <= 1 {
                vec![("paneId", json!(blocked_pane_id)), ("host", json!(host))]
            } else {
                vec![
                    ("paneId", json!(blocked_pane_id)),
                    ("host", json!(host)),
                    ("count", json!(count)),
                ]
            };
            blocked_env.log("egress:blocked", fields);
        }
    };

    let proxy =
        crate::egress::proxy::PaneProxy::spawn(initial_allowed, unix_socket_path, on_blocked)
            .await?;
    // `#[cfg(unix)]`, not `target_os = "linux"` — matches
    // `secure_pane_socket_permissions`'s own gate (unix permission bits are
    // a unix-wide concept). Only ever non-`None` when the caller passed a
    // unix path in the first place (macOS's own call site never does), so
    // this is a no-op on every macOS spawn regardless of the `#[cfg]`.
    #[cfg(unix)]
    if let Some(path) = proxy.unix_path() {
        crate::egress::linux::secure_pane_socket_permissions(path)?;
    }
    let proxy = std::sync::Arc::new(proxy);
    state
        .proxies
        .lock()
        .expect("AppState.proxies lock poisoned")
        .insert(pane_id.to_string(), proxy.clone());
    state.egress.register_pane(pane_id);
    push_state_event(env, state);
    Ok(proxy)
}

/// Spawns a pane's filtered Docker gateway when sandboxed Docker is enabled,
/// returning the socket path to hand to the pane as `DOCKER_HOST`. Resolves
/// the host daemon socket itself (`egress::docker::resolve_daemon_socket`),
/// creates the gateway socket's `0700` parent dir, binds the gateway, and
/// registers it in `AppState.docker_gateways`. Returns `None` — and the pane
/// simply spawns WITHOUT Docker — when no daemon is reachable or the gateway
/// fails to bind; sandboxed Docker is a best-effort opt-in, never a spawn
/// failure.
///
/// `allowed_mount_roots` is the `egress::docker::DockerPolicy` mount
/// allow-list: the open workspace roots (so `-v $PWD:/app` style binds
/// resolve) — the caller supplies them from `state.open_folders`.
pub(crate) async fn create_docker_gateway(
    app: &AppHandle,
    state: &AppState,
    pane_id: &str,
    allowed_mount_roots: Vec<PathBuf>,
) -> Option<PathBuf> {
    let daemon = crate::egress::docker::resolve_daemon_socket()?;
    let socket_path = crate::egress::docker::gateway_socket_path(pane_id);
    let parent = socket_path.parent()?;
    if std::fs::create_dir_all(parent).is_err() {
        return None;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700));
    }
    let deny_app = app.clone();
    let deny_pane_id = pane_id.to_string();
    let on_deny = move |denied: crate::egress::docker::DockerDenied| {
        let reason = denied.reason;
        let _ = deny_app.emit(
            "docker:denied",
            json!({"paneId": deny_pane_id.clone(), "reason": reason.clone()}),
        );
        events::log_event(
            &deny_app,
            "docker:denied",
            vec![
                ("paneId", json!(deny_pane_id.clone())),
                ("reason", json!(reason)),
            ],
        );
    };
    let policy = crate::egress::docker::DockerPolicy {
        allowed_mount_roots,
    };
    let gateway =
        crate::egress::docker::DockerGateway::spawn(socket_path.clone(), daemon, policy, on_deny)
            .await
            .ok()?;
    state
        .docker_gateways
        .lock()
        .expect("AppState.docker_gateways lock poisoned")
        .insert(pane_id.to_string(), std::sync::Arc::new(gateway));
    Some(socket_path)
}

/// Tears down one pane's live proxy, cancels any scheduled auto-relock
/// timer, and drops its `EgressState` record — mirrors `closePane`
/// (idempotent: a second call, or a call for a pane that was never
/// gapped, finds nothing and is a safe no-op). Pushes `egress:state` ONLY
/// when a pane record actually existed — matching `closePane`'s own
/// `if (!st) return` short-circuit BEFORE its `pushState()` call, so an
/// ungapped pane's close (no `EgressState` entry was ever registered for
/// it) produces no spurious `egress:state` event.
///
/// `pub(crate)`: `ipc::pty::pty_create` calls this on a spawn that failed
/// after its proxy already came up (mirrors the JS original's `catch`
/// block: `egress.closePane(id); throw err`), and on every ordinary
/// `pty:kill`/pane-exit teardown, gapped or not.
pub(crate) fn close_pane_and_proxy<E: EgressEnv>(env: &E, state: &AppState, pane_id: &str) {
    if let Some((_, timer)) = state
        .relock_timers
        .lock()
        .expect("AppState.relock_timers lock poisoned")
        .remove(pane_id)
    {
        timer.abort();
    }
    if let Some(proxy) = state
        .proxies
        .lock()
        .expect("AppState.proxies lock poisoned")
        .remove(pane_id)
    {
        proxy.shutdown();
    }
    if let Some(gateway) = state
        .docker_gateways
        .lock()
        .expect("AppState.docker_gateways lock poisoned")
        .remove(pane_id)
    {
        gateway.shutdown();
    }
    if state.egress.close_pane(pane_id) {
        push_state_event(env, state);
    }
}

/// `unlockPane(paneId, minutes)` PLUS the live-enforcement half `egress.js`
/// gets for free from its one shared `allowMatchers`/`panes` map: widens
/// the pane's live `PaneProxy` mode, arms the real auto-relock timer
/// (cancelling any prior one for the same pane — mirrors `clearTimeout(
/// st.timer)`), and pushes `egress:state` — all ONLY when
/// `EgressState::unlock_pane` actually validated `minutes` and found a
/// known pane (TOME-019: no state change of ANY kind, pure or live, on a
/// forged/invalid request). `pub(crate)` so `egress_unlock` below can stay
/// a thin command wrapper around it.
pub(crate) fn schedule_unlock<E: EgressEnv>(
    env: &E,
    state: &AppState,
    pane_id: &str,
    minutes: i64,
) {
    let now = totp::now_ms() as i64;
    let Some(deadline_ms) = state.egress.unlock_pane(pane_id, minutes, now) else {
        return;
    };

    if let Some(proxy) = state
        .proxies
        .lock()
        .expect("AppState.proxies lock poisoned")
        .get(pane_id)
        .cloned()
    {
        proxy.unlock();
    }

    // Generation token: `AbortHandle::abort()` alone cannot guarantee the
    // OLD timer below never fires `relock_now` anyway — tokio cancellation
    // only takes effect at a task's NEXT suspension point, so if the old
    // timer's sleep had ALREADY elapsed and its continuation had ALREADY
    // resumed running by the time `.abort()` runs, that continuation keeps
    // going regardless (nothing suspends it again between `relock_now(...)`
    // and the async block ending). Concretely: a pane unlocked for 15
    // minutes, re-unlocked by an already-re-authenticated user right around
    // the old deadline, could have that fresh unlock silently reverted
    // moments later by the stale timer it raced against — with the
    // renderer having just received `{ ok: true }` from `egress:unlock`.
    // Minting a fresh generation HERE (before the old timer is even
    // aborted) and having every timer's continuation re-check, under
    // `relock_timers`' own lock, that its captured generation is STILL the
    // one on file for this pane — see [`claim_timer_if_current`] — closes
    // this regardless of whether `abort()` wins its race. See
    // `AppState::relock_timer_generation`'s doc comment and this file's
    // `a_superseding_unlock_prevents_the_stale_timers_generation_from_
    // being_claimed` test.
    let generation = state.relock_timer_generation.fetch_add(1, Ordering::SeqCst) + 1;

    if let Some((_, old)) = state
        .relock_timers
        .lock()
        .expect("AppState.relock_timers lock poisoned")
        .remove(pane_id)
    {
        old.abort(); // best-effort; the generation check above is the actual guarantee
    }
    let sleep_for = Duration::from_millis((deadline_ms - now).max(0) as u64);
    let env_for_timer = env.clone();
    let pane_id_for_timer = pane_id.to_string();
    let join = tauri::async_runtime::spawn(async move {
        tokio::time::sleep(sleep_for).await;
        let state = env_for_timer.app_state();
        if claim_timer_if_current(state, &pane_id_for_timer, generation) {
            relock_now(&env_for_timer, state, &pane_id_for_timer);
        }
        // else: superseded by a fresher `schedule_unlock` (or already
        // removed by a manual `egress:relock`/pane close) — a stale timer
        // must not relock a pane it no longer has any claim over.
    });
    state
        .relock_timers
        .lock()
        .expect("AppState.relock_timers lock poisoned")
        .insert(
            pane_id.to_string(),
            (generation, join.inner().abort_handle()),
        );

    env.log(
        "egress:unlock",
        vec![("paneId", json!(pane_id)), ("minutes", json!(minutes))],
    );
    push_state_event(env, state);
}

/// Claims pane `pane_id`'s scheduled auto-relock timer for `generation`,
/// atomically with respect to any concurrent `schedule_unlock`/manual
/// relock/pane-close: returns `true` (and removes the entry) only if
/// `generation` is STILL the one currently registered for this pane in
/// `relock_timers`. Called by a timer's spawned continuation immediately
/// after its sleep elapses, right before it would otherwise call
/// [`relock_now`] — see [`schedule_unlock`]'s doc comment for the exact
/// race this closes. On a `false` return the map is left untouched: the
/// entry present (if any) belongs to a NEWER timer, which must not be
/// disturbed by a stale one losing this check.
fn claim_timer_if_current(state: &AppState, pane_id: &str, generation: u64) -> bool {
    let mut timers = state
        .relock_timers
        .lock()
        .expect("AppState.relock_timers lock poisoned");
    let is_current = matches!(timers.get(pane_id), Some((g, _)) if *g == generation);
    if is_current {
        timers.remove(pane_id);
    }
    is_current
}

/// `relockPane(paneId)`'s live half: narrows the pane's `PaneProxy` back to
/// providers-only (which itself kills every tunnel that was only ever
/// admitted because the mode was `Open` — TOME-002, see `PaneProxy::relock`'s
/// doc comment) and pushes `egress:state` + the persistent `egress:relock`
/// log entry — but ONLY when `EgressState::relock_pane` reports the pane
/// was actually found (mirrors `if (!st) return` running before either).
/// Shared by [`egress_relock`] (immediate, user-initiated) and
/// [`schedule_unlock`]'s own auto-relock timer (via
/// [`claim_timer_if_current`]).
fn relock_now<E: EgressEnv>(env: &E, state: &AppState, pane_id: &str) {
    if !state.egress.relock_pane(pane_id) {
        return;
    }
    if let Some(proxy) = state
        .proxies
        .lock()
        .expect("AppState.proxies lock poisoned")
        .get(pane_id)
        .cloned()
    {
        proxy.relock();
    }
    env.log("egress:relock", vec![("paneId", json!(pane_id))]);
    push_state_event(env, state);
}

// ---- commands ----

/// Mirrors `{ ...egress.getState(), auth: authlock.authStatus() }`
/// (`egress:state`'s handler body) — `auth` here is bare `{configured,
/// totp}`, not the fuller `auth:status` shape (`unlocked`/`touchId`), same
/// distinction `ipc::auth::auth_status`'s own doc comment makes.
#[tauri::command]
pub async fn egress_state(state: State<'_, AppState>) -> Result<Value, String> {
    lock_gate::guard(&state, "egress:state")?;
    let mut snapshot = state_snapshot_json(&state);
    let (configured, totp) = {
        let guard = state.auth.lock().expect("AppState.auth lock poisoned");
        guard
            .as_ref()
            .map(|a| (a.status().configured, a.status().totp))
            .unwrap_or((false, false))
    };
    snapshot["auth"] = json!({"configured": configured, "totp": totp});
    Ok(snapshot)
}

/// `egress:unlock` (`{ paneId, passphrase, code, minutes }`): re-verifies a
/// second factor (TOTP if enrolled, else the passphrase — the app login
/// already proved one, but this channel demands it again per pane, exactly
/// like `auth:login`'s own throttle purpose is independent), then widens
/// the named pane. Mirrors the JS original's own looseness verbatim: once
/// re-auth succeeds, this returns `{ ok: true }` UNCONDITIONALLY —
/// `unlockPane`'s own return value (`false` for bad `minutes`/an unknown
/// pane id) is never consulted by the JS handler either. TOME-019's actual
/// safety property — no mutation on an invalid `minutes` — is still fully
/// intact regardless, since [`schedule_unlock`]/`EgressState::unlock_pane`
/// validate before touching anything; only this cosmetic response shape is
/// preserved as-is for exact fidelity.
#[tauri::command]
pub async fn egress_unlock(
    app: AppHandle,
    state: State<'_, AppState>,
    pane_id: String,
    passphrase: Option<String>,
    code: Option<String>,
    minutes: i64,
) -> Result<Value, String> {
    lock_gate::guard(&state, "egress:unlock")?;

    {
        let mut guard = state.auth.lock().expect("AppState.auth lock poisoned");
        let auth = guard
            .as_mut()
            .ok_or_else(|| "auth: not initialized".to_string())?;
        let wait = auth.throttle_retry_in("egress:unlock");
        if wait > 0 {
            return Ok(json!({
                "ok": false,
                "error": format!("Too many attempts — try again in {}s.", ceil_seconds(wait)),
            }));
        }
        let totp_active = auth.totp_active();
        let verified = if totp_active {
            code.as_deref().is_some_and(|c| auth.verify_totp(c))
        } else {
            passphrase
                .as_deref()
                .is_some_and(|p| auth.verify_passphrase(p))
        };
        if !verified {
            auth.record_failure("egress:unlock");
            let error = if totp_active {
                "Wrong 2FA code."
            } else {
                "Wrong passphrase."
            };
            return Ok(json!({"ok": false, "error": error}));
        }
        auth.record_success("egress:unlock");
    }

    schedule_unlock(&app, &state, &pane_id, minutes);
    Ok(json!({"ok": true}))
}

/// `egress:relock` (bare `paneId`, not an object — see `tome-ipc.js`'s
/// `relock: (paneId) => call('egress_relock', { paneId })`, which wraps it
/// into `{ paneId }` for Tauri's named-argument convention). Immediate,
/// unauthenticated (narrowing egress is never privileged — only widening
/// it is) — mirrors `relockPane` being callable with no re-auth in the JS
/// original.
#[tauri::command]
pub async fn egress_relock(
    app: AppHandle,
    state: State<'_, AppState>,
    pane_id: String,
) -> Result<Value, String> {
    lock_gate::guard(&state, "egress:relock")?;
    if let Some((_, timer)) = state
        .relock_timers
        .lock()
        .expect("AppState.relock_timers lock poisoned")
        .remove(&pane_id)
    {
        timer.abort();
    }
    relock_now(&app, &state, &pane_id);
    Ok(json!({}))
}

/// `egress:setup` (`{ passphrase }`): first-run (or post-factory-reset,
/// though this build has no reset path either) passphrase configuration.
/// Refuses if already configured — `setPassphrase` itself has no such
/// guard (it happily overwrites, which is how a passphrase CHANGE works
/// elsewhere); this handler's own guard is what makes `egress:setup`
/// specifically a first-time-only door, matching the JS original's
/// `if (authlock.authStatus().configured) return { ok: false, error:
/// 'Already configured.' }`. Marks the session unlocked on success — "first-
/// run setup happens at the lock screen," per the JS original's comment.
#[tauri::command]
pub async fn egress_setup(state: State<'_, AppState>, passphrase: String) -> Result<Value, String> {
    lock_gate::guard(&state, "egress:setup")?;
    {
        let mut guard = state.auth.lock().expect("AppState.auth lock poisoned");
        let auth = guard
            .as_mut()
            .ok_or_else(|| "auth: not initialized".to_string())?;
        if auth.status().configured {
            return Ok(json!({"ok": false, "error": "Already configured."}));
        }
        if let Err(e) = auth.set_passphrase(&passphrase) {
            return Ok(json!({"ok": false, "error": e.to_string()}));
        }
    }
    mark_unlocked(&state);
    Ok(json!({"ok": true}))
}

/// `egress:enrollTotp` (no args). Mirrors `authlock.enrollTotp()`'s own
/// contract exactly: resolves `{ secret, uri }` on success, REJECTS (not a
/// `{ok:false}` shape) on the TOME-005 active-factor guard — an `Err`
/// here rejects the Tauri `invoke()` promise the same way a thrown JS
/// `Error` rejects an Electron one.
#[tauri::command]
pub async fn egress_enroll_totp(state: State<'_, AppState>) -> Result<Value, String> {
    lock_gate::guard(&state, "egress:enrollTotp")?;
    let mut guard = state.auth.lock().expect("AppState.auth lock poisoned");
    let auth = guard
        .as_mut()
        .ok_or_else(|| "auth: not initialized".to_string())?;
    auth.enroll_totp()
        .map(|e| serde_json::to_value(e).expect("TotpEnrollment always serializes"))
        .map_err(|e| e.to_string())
}

/// `egress:confirmTotp` (`{ code }`). Mirrors `authlock.confirmTotp(code)`
/// exactly: resolves a plain BOOLEAN (`true`/`false` for right/wrong code
/// — not an `{ok:...}` object; `src/renderer/egress-ui.js`'s
/// `if (await tome.egress.confirmTotp(code.value))` reads it as one), and
/// only `Err`s on an actual save failure.
#[tauri::command]
pub async fn egress_confirm_totp(
    state: State<'_, AppState>,
    code: String,
) -> Result<Value, String> {
    lock_gate::guard(&state, "egress:confirmTotp")?;
    let mut guard = state.auth.lock().expect("AppState.auth lock poisoned");
    let auth = guard
        .as_mut()
        .ok_or_else(|| "auth: not initialized".to_string())?;
    auth.confirm_totp(&code)
        .map(|ok| json!(ok))
        .map_err(|e| e.to_string())
}

/// `egress:readRepoAllowlist` (`{ root }`). Main is the sole authority: it
/// resolves `root` through the SAME confinement boundary every other
/// workspace-scoped command uses (`confine::confined_real_path`), then
/// reads/hashes/validates `.tome/egress.json` itself — the renderer never
/// supplies hosts, only a root to check.
#[tauri::command]
pub async fn egress_read_repo_allowlist(
    state: State<'_, AppState>,
    root: String,
) -> Result<Value, String> {
    lock_gate::guard(&state, "egress:readRepoAllowlist")?;
    let report = state
        .egress
        .read_repo_allowlist(&root, |p| confine::confined_real_path(&state, p).ok());
    Ok(serde_json::to_value(report).expect("RepoAllowlistReport always serializes"))
}

/// `egress:consentRepoAllowlist` (`{ root, hash }`). TOCTOU-safe by
/// construction (`EgressState::consent_repo_allowlist` re-reads and
/// re-hashes before ever comparing `hash`) — the renderer's `hash` is
/// proof it saw a specific file content, never a value that itself takes
/// effect. On success, widens every currently-live gapped pane's egress
/// immediately (mirrors `recompile()`'s module-wide effect) — see
/// [`recompile_all_proxies`].
#[tauri::command]
pub async fn egress_consent_repo_allowlist(
    state: State<'_, AppState>,
    root: String,
    hash: String,
) -> Result<Value, String> {
    lock_gate::guard(&state, "egress:consentRepoAllowlist")?;
    let outcome = state.egress.consent_repo_allowlist(&root, &hash, |p| {
        confine::confined_real_path(&state, p).ok()
    });
    match outcome {
        ConsentOutcome::Ok { applied, rejected } => {
            recompile_all_proxies(&state);
            Ok(json!({"ok": true, "applied": applied, "rejected": rejected}))
        }
        ConsentOutcome::Err(error) => Ok(json!({"ok": false, "error": error})),
    }
}

/// `egress:revokeRepoAllowlist` (`{ root }`). Always `{ ok: true }`, even
/// for a root with no consent to revoke — matches the JS original exactly.
/// Also re-applies the (now possibly narrower) effective allow set to
/// every live pane, unconditionally — `revokeRepoAllowlist` calls
/// `recompile()` unconditionally too, a harmless no-op when nothing
/// actually changed.
#[tauri::command]
pub async fn egress_revoke_repo_allowlist(
    state: State<'_, AppState>,
    root: String,
) -> Result<Value, String> {
    lock_gate::guard(&state, "egress:revokeRepoAllowlist")?;
    state.egress.revoke_repo_allowlist(&root);
    recompile_all_proxies(&state);
    Ok(json!({"ok": true}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex as StdMutex};

    // ---- EgressEnv test double — see that trait's own doc comment ----

    /// Wraps an owned `AppState` (no live Tauri app needed —
    /// `AppState::new()` is a plain constructor) and records every push
    /// instead of touching a real event bus, so
    /// schedule_unlock/relock_now/create_gapped_pane_proxy/
    /// close_pane_and_proxy below can be driven end-to-end against a REAL
    /// `EgressState` and a REAL `PaneProxy`.
    #[derive(Clone)]
    struct TestEnv {
        state: Arc<AppState>,
        pushes: Arc<StdMutex<Vec<(String, Value)>>>,
    }

    impl TestEnv {
        fn new() -> Self {
            Self {
                state: Arc::new(AppState::new()),
                pushes: Arc::new(StdMutex::new(Vec::new())),
            }
        }
    }

    impl EgressEnv for TestEnv {
        fn app_state(&self) -> &AppState {
            &self.state
        }
        fn emit_json(&self, event: &str, payload: Value) {
            self.pushes
                .lock()
                .unwrap()
                .push((event.to_string(), payload));
        }
        fn log(&self, kind: &str, fields: Vec<(&'static str, Value)>) {
            let obj: serde_json::Map<String, Value> = fields
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect();
            self.pushes
                .lock()
                .unwrap()
                .push((kind.to_string(), Value::Object(obj)));
        }
    }

    // ---- integration glue: schedule_unlock / relock_now /
    // create_gapped_pane_proxy / close_pane_and_proxy driven end-to-end
    // against a real EgressState + a real PaneProxy (TestEnv stands in for
    // the AppHandle none of this can construct outside a running app) ----

    #[tokio::test]
    async fn schedule_unlock_widens_both_egress_state_and_the_live_proxy() {
        let env = TestEnv::new();
        env.state.egress.register_pane("pty-1");
        let proxy = Arc::new(
            crate::egress::proxy::PaneProxy::spawn(vec![], None, |_| {})
                .await
                .unwrap(),
        );
        env.state
            .proxies
            .lock()
            .unwrap()
            .insert("pty-1".to_string(), proxy.clone());
        assert_eq!(proxy.mode(), crate::egress::proxy::Mode::Providers);

        schedule_unlock(&env, &env.state, "pty-1", 15);

        assert_eq!(
            env.state.egress.pane_mode("pty-1"),
            Some(crate::egress::PaneMode::Open)
        );
        assert_eq!(proxy.mode(), crate::egress::proxy::Mode::Open);
        assert!(env
            .pushes
            .lock()
            .unwrap()
            .iter()
            .any(|(k, _)| k == "egress:unlock"));
        assert!(env
            .state
            .relock_timers
            .lock()
            .unwrap()
            .contains_key("pty-1"));
    }

    #[tokio::test]
    async fn schedule_unlock_on_an_unknown_pane_touches_neither_egress_state_nor_any_proxy() {
        // TOME-019: an invalid/forged unlock must not partially apply.
        let env = TestEnv::new();
        schedule_unlock(&env, &env.state, "ghost", 15);
        assert_eq!(env.state.egress.pane_mode("ghost"), None);
        assert!(env.state.relock_timers.lock().unwrap().is_empty());
        assert!(env.pushes.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn relock_now_narrows_both_egress_state_and_the_live_proxy() {
        let env = TestEnv::new();
        env.state.egress.register_pane("pty-1");
        let proxy = Arc::new(
            crate::egress::proxy::PaneProxy::spawn(vec![], None, |_| {})
                .await
                .unwrap(),
        );
        env.state
            .proxies
            .lock()
            .unwrap()
            .insert("pty-1".to_string(), proxy.clone());
        schedule_unlock(&env, &env.state, "pty-1", 15);
        assert_eq!(proxy.mode(), crate::egress::proxy::Mode::Open);

        relock_now(&env, &env.state, "pty-1");

        assert_eq!(
            env.state.egress.pane_mode("pty-1"),
            Some(crate::egress::PaneMode::Providers)
        );
        assert_eq!(proxy.mode(), crate::egress::proxy::Mode::Providers);
        assert!(env
            .pushes
            .lock()
            .unwrap()
            .iter()
            .any(|(k, _)| k == "egress:relock"));
    }

    #[tokio::test]
    async fn create_gapped_pane_proxy_registers_a_live_proxy_reachable_on_its_reported_port() {
        let env = TestEnv::new();
        let proxy = create_gapped_pane_proxy(&env, &env.state, "pty-1", None)
            .await
            .unwrap();

        assert_eq!(
            env.state.egress.pane_mode("pty-1"),
            Some(crate::egress::PaneMode::Providers)
        );
        assert!(env.state.proxies.lock().unwrap().contains_key("pty-1"));
        assert!(tokio::net::TcpStream::connect(("127.0.0.1", proxy.port()))
            .await
            .is_ok());
        assert!(env
            .pushes
            .lock()
            .unwrap()
            .iter()
            .any(|(k, _)| k == "egress:state"));
        assert!(
            proxy.unix_path().is_none(),
            "no unix path was requested — none should be bound"
        );
    }

    // Phase 4/slice L3: the Linux loopback-bridge seam — `unix_socket_path`
    // threaded all the way from this function's own parameter down to a
    // REAL bound-and-locked-down socket file, the exact wiring
    // `ipc::pty::pty_create`'s Linux gapped branch depends on.
    #[cfg(unix)]
    #[tokio::test]
    async fn create_gapped_pane_proxy_binds_and_locks_down_a_given_unix_socket_path() {
        use std::os::unix::fs::PermissionsExt;
        let env = TestEnv::new();
        let dir = tempfile::tempdir().unwrap();
        let sock_path = dir.path().join("pane-pty-1.sock");

        let proxy = create_gapped_pane_proxy(&env, &env.state, "pty-1", Some(sock_path.clone()))
            .await
            .unwrap();

        assert_eq!(proxy.unix_path(), Some(&sock_path));
        assert!(sock_path.exists());
        let mode = std::fs::metadata(&sock_path).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o600,
            "the loopback-bridge socket must be locked to 0600, got {mode:o}"
        );
    }

    #[tokio::test]
    async fn close_pane_and_proxy_tears_down_the_live_proxy_and_cancels_its_timer() {
        let env = TestEnv::new();
        let proxy = create_gapped_pane_proxy(&env, &env.state, "pty-1", None)
            .await
            .unwrap();
        let port = proxy.port();
        drop(proxy); // this fn's own Arc, not AppState.proxies' — the map below still holds one
        schedule_unlock(&env, &env.state, "pty-1", 15); // arms a (long) real timer

        close_pane_and_proxy(&env, &env.state, "pty-1");

        assert!(env.state.proxies.lock().unwrap().get("pty-1").is_none());
        assert!(env
            .state
            .relock_timers
            .lock()
            .unwrap()
            .get("pty-1")
            .is_none());
        assert_eq!(env.state.egress.pane_state("pty-1"), None);
        // The real listener must actually be down, not just forgotten about.
        // `proxy.shutdown()` signals the listener's accept loop rather than
        // synchronously joining it, so the OS-level socket close can lag
        // this call by a scheduler tick or two — same tolerance brain.rs's
        // own `start_watch_with_detects_a_change_and_fires_after_the_debounce`
        // test gives its async teardown, applied here to a much shorter
        // deadline (this has no real debounce to wait out, just scheduling
        // slack under a busy `cargo test` run).
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            let connect = tokio::net::TcpStream::connect(("127.0.0.1", port)).await;
            if connect.is_err() {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "listener on port {port} never went down"
            );
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }

    #[tokio::test]
    async fn a_superseding_unlock_prevents_the_stale_timers_generation_from_being_claimed() {
        // Regression test for the schedule_unlock timer-replacement race:
        // AbortHandle::abort() alone cannot guarantee a stale timer, whose
        // sleep had already elapsed by the time a fresh unlock's abort()
        // call runs, never calls relock_now anyway — see
        // AppState::relock_timer_generation's doc comment. Rather than
        // trying to honestly win a real race against a live short timer
        // (`ALLOWED_UNLOCK_MINUTES`'s minimum is 15 real minutes — not
        // something a unit test can wait out — and even a shorter one
        // would be flaky by construction, exactly why
        // `connect_upstream_rechecked` in proxy.rs takes a synchronous
        // injection hook instead of racing a real connect too), this
        // drives `claim_timer_if_current` — the EXACT primitive both the
        // real timer task and this test call — directly.
        let env = TestEnv::new();
        env.state.egress.register_pane("pty-1");

        schedule_unlock(&env, &env.state, "pty-1", 15);
        let stale_generation = env
            .state
            .relock_timers
            .lock()
            .unwrap()
            .get("pty-1")
            .unwrap()
            .0;

        // The user re-authenticates and unlocks again before the first
        // timer's deadline — this must supersede it with a fresh
        // generation.
        schedule_unlock(&env, &env.state, "pty-1", 30);
        let current_generation = env
            .state
            .relock_timers
            .lock()
            .unwrap()
            .get("pty-1")
            .unwrap()
            .0;
        assert_ne!(stale_generation, current_generation);

        // What the STALE timer's own post-sleep continuation checks,
        // simulated directly: it must find itself superseded and must NOT
        // remove the current (newer) entry.
        assert!(!claim_timer_if_current(
            &env.state,
            "pty-1",
            stale_generation
        ));
        assert!(env
            .state
            .relock_timers
            .lock()
            .unwrap()
            .contains_key("pty-1"));
        assert_eq!(
            env.state.egress.pane_mode("pty-1"),
            Some(crate::egress::PaneMode::Open),
            "the stale timer's relock_now must never be allowed to run"
        );

        // The CURRENT (real) timer's generation remains claimable exactly
        // once — proving a NON-superseded timer still works normally.
        assert!(claim_timer_if_current(
            &env.state,
            "pty-1",
            current_generation
        ));
        assert!(env
            .state
            .relock_timers
            .lock()
            .unwrap()
            .get("pty-1")
            .is_none());
    }

    // ---- effective_allow_patterns — the integration seam mod.rs's own doc
    // comment says has no test of its own within that file (it never
    // learns about DEFAULT_ALLOW), and allowlist.rs never learns about
    // repo consents ----

    #[test]
    fn effective_allow_patterns_with_no_consents_is_exactly_default_allow() {
        let state = AppState::new();
        let mut patterns = effective_allow_patterns(&state);
        patterns.sort();
        let mut expected: Vec<String> = DEFAULT_ALLOW.iter().map(|s| s.to_string()).collect();
        expected.sort();
        assert_eq!(patterns, expected);
    }

    #[test]
    fn effective_allow_patterns_unions_default_allow_with_consented_repo_hosts() {
        let dir = tempfile::tempdir().unwrap();
        let tome_dir = dir.path().join(".tome");
        std::fs::create_dir_all(&tome_dir).unwrap();
        let file = tome_dir.join("egress.json");
        std::fs::write(&file, r#"{"allow":["extra.example.com"]}"#).unwrap();
        let root = dir.path().to_str().unwrap();
        let resolve = |_p: &std::path::Path| Some(file.clone());

        let state = AppState::new();
        let report = state.egress.read_repo_allowlist(root, resolve);
        let crate::egress::RepoAllowlistReport::Present { hash, .. } = report else {
            panic!("expected Present")
        };
        state.egress.consent_repo_allowlist(root, &hash, resolve);

        let patterns = effective_allow_patterns(&state);
        assert!(patterns.contains(&"extra.example.com".to_string()));
        for p in DEFAULT_ALLOW {
            assert!(
                patterns.contains(&p.to_string()),
                "missing default pattern {p}"
            );
        }
    }

    #[test]
    fn recompile_all_proxies_is_a_harmless_no_op_with_no_live_proxies() {
        let state = AppState::new();
        recompile_all_proxies(&state); // must not panic with an empty proxies map
    }

    // ---- close_pane_and_proxy: no-op for a pane that was never registered
    // (mirrors closePane's `if (!st) return`) ----

    #[tokio::test]
    async fn close_pane_and_proxy_on_an_unregistered_pane_touches_nothing() {
        // Now exercised through the REAL function (see `EgressEnv`'s doc
        // comment for how `TestEnv` makes that possible) rather than only
        // its no-AppHandle-needed inner half: proves the early-return
        // branch really does short-circuit before any push happens, not
        // just that `EgressState::close_pane` alone is a no-op.
        let env = TestEnv::new();
        close_pane_and_proxy(&env, &env.state, "never-existed");
        assert!(env.state.relock_timers.lock().unwrap().is_empty());
        assert!(env.state.proxies.lock().unwrap().is_empty());
        assert!(
            env.pushes.lock().unwrap().is_empty(),
            "an unregistered pane's close must push nothing"
        );
    }
}
