//! PTY lifecycle. Ports `src/main/index.js`'s `pty:*` handlers (spawn via
//! `portable-pty`, the 4ms/64KB output batcher, explicit kill/reap) plus
//! `src/main/lib/{agent-spawn,agent-env,pty-authority,custom-agents}.js`
//! for spawn vetting. `pty:data` streams to the renderer over a Tauri
//! Channel per pane; `pty:exit` still goes out on the global event bus
//! (`app.emit`, via the `on_exit` closure `pty_create` hands
//! `crate::pty::Registry::spawn_terminal`/`spawn_raw`) — that split matches
//! the already-committed renderer contract (`tome-ipc.js`'s separate
//! `onData`/`onExit` wiring; see `crate::pty`'s module doc comment for why
//! sending `pty:exit` down the Channel instead would silently break it).
//!
//! `pty_write`/`pty_resize` are thin wrappers over `crate::pty::Registry`
//! (`state.pty`) — Phase 2 slice P1's work (see that module's doc comment
//! for the batcher/reader/kill mechanism). `pty_kill` additionally tears
//! down the pane's air-gap proxy (`ipc::airgap::close_pane_and_proxy`),
//! matching `index.js`'s `ipcMain.on('pty:kill', ...)` calling
//! `airgap.closePane(id)` immediately rather than waiting for the killed
//! process's own exit event.
//!
//! `pty_create` below is this phase's (Phase 3, Task A4) integration:
//! reconciles Phase 2's PTY mechanism + spawn-policy ports
//! (`crate::agent_spawn`, `crate::custom_agents`, `crate::pty_authority`,
//! `crate::agent_env`, `crate::login_env`) with the real air-gap
//! enforcement — `crate::airgap::proxy::PaneProxy` (the live loopback
//! CONNECT/HTTP proxy) and, on macOS, an actual `sandbox-exec` wrap built
//! from `crate::airgap::seatbelt::seatbelt_profile`. This CLOSES Phase 2's
//! interim gap: every resolved agent is no longer refused outright, and a
//! gapped pane's egress is no longer merely logged-and-ignored — it is
//! enforced.
//!
//! ## The unified spawn path, and why it now covers agents too
//!
//! A single code path spawns EVERY pane — agent or plain terminal, gapped
//! or not — mirroring `createPty`'s own structure exactly (it never
//! branches into two separate spawn mechanisms; only `agentCmd`,
//! `resolveAgentSecrets()`, and the proxy/sandbox wrap are conditional on
//! `isAgent`/`gapped`). [`build_pty_command`] builds the `CommandBuilder`
//! either way (`agent_cmd: None` for a plain login shell, `Some(cmd)` for
//! `-c <cmd>`); [`pane_env`] builds the environment either way (secrets
//! only when `is_agent`, proxy vars only when gapped) via
//! `agent_env::compose_agent_env`.
//!
//! ## Fail-closed rules this file enforces (TOME-001/002, non-negotiable)
//!
//! - **Every ungapped pane spawn — agent OR plain terminal — needs a fresh
//!   re-auth ceremony once a passphrase is configured.** This is NOT
//!   agent-specific: `pty_authority::unrestricted_spawn_needs_reauth`'s own
//!   signature takes no `is_agent` parameter, because an ungapped pty is
//!   "an unsandboxed shell with the user's full privileges and open
//!   network access" (`src/renderer/lock.js`'s own re-auth-prompt copy)
//!   regardless of what's running inside it. See [`evaluate_reauth`].
//! - **A gapped pane on any OS other than macOS is refused outright, not
//!   silently spawned unenforced.** Real Linux sandbox enforcement
//!   (bubblewrap + `tome-shim`) is Phase 4 — see [`resolve_gapped_spawn`].
//!   This is the exact TOME-001 hole (`sandbox = null` off-darwin, proxy
//!   env vars set but nothing enforcing them) this whole rewrite exists to
//!   close; refusing is strictly safer than the Electron original's
//!   silent full-open egress. Terminal panes still work fine ungapped, or
//!   gapped on macOS — only the gapped+non-macOS combination refuses.
//! - **A pane's proxy is created BEFORE the process spawns, and torn down
//!   if the spawn then fails** — mirrors `createPty`'s own
//!   `catch (err) { airgap.closePane(id); throw err }": a proxy that
//!   came up must never outlive a failed spawn as an orphaned, useless
//!   listener.

use std::collections::HashMap;
use std::path::Path;

use portable_pty::{CommandBuilder, PtySize};
use serde::Deserialize;
use serde_json::{json, Value};
use tauri::ipc::Channel;
use tauri::{AppHandle, Emitter, Manager, State};

use crate::agent_spawn::{self, AgentEntry};
use crate::ipc::airgap::{close_pane_and_proxy, create_gapped_pane_proxy};
use crate::ipc::auth::ceil_seconds;
use crate::{agent_env, airgap, custom_agents, events, eventlog, lock_gate, login_env, pty_authority, state::AppState, store};

/// Wire shape of `pty:create`'s options object. `tome-ipc.js`'s
/// `pty.create: (opts) => { const ch = new Channel(); ...; return
/// call('pty_create', { opts, onData: ch }) }` forwards the renderer's
/// `opts` object verbatim — see `src/renderer/panels/terminal.js`'s
/// `tome.pty.create({ id, kind, cwd, airgap, ws, model, auth })` for the
/// actual call site — which `src/main/index.js`'s handler destructures as
/// `{ id, kind, cwd, airgap: gapped, ws, model, auth }` (line 633).
///
/// `ws`/`model` are accepted here — so a real renderer payload always
/// deserializes cleanly, and the struct documents the full wire contract —
/// but `ws` is write-only this phase: it only ever mattered for
/// `brain::ensureBrain`/`TOME_BRAIN` (Phase 5; no `brain.rs` module exists
/// in this tree yet). `model`/`auth` ARE wired below (model pinning via
/// `agent_spawn::build_agent_spawn_from`; `auth` is the TOME-001 re-auth
/// ceremony's credential payload).
#[derive(Debug, Deserialize)]
pub struct PtyCreateOpts {
    pub id: String,
    pub kind: String,
    pub cwd: Option<String>,
    pub airgap: Option<bool>,
    #[allow(dead_code)] // Phase 5 (brain.rs does not exist yet) — see struct doc comment
    pub ws: Option<String>,
    pub model: Option<String>,
    pub auth: Option<Value>,
}

/// The macOS `sandbox-exec` wrap around a gapped pane's command line —
/// `spawnArgs = [...sandbox.args, spawnCmd, ...spawnArgs]; spawnCmd =
/// sandbox.cmd` in the JS original. Built by [`resolve_gapped_spawn`],
/// consumed by [`build_pty_command`].
struct SandboxWrap {
    cmd: String,
    args: Vec<String>,
}

/// What `pty_create` does for a gapped pane, once its `PaneProxy` is
/// already up — never constructed for an ungapped pane (which needs
/// neither).
enum GappedSpawnDecision {
    /// macOS: wrap the spawn in `sandbox-exec -p <profile>`. The literal
    /// path, not `airgap::seatbelt::SANDBOX_EXEC_PATH` (that const is
    /// `#[cfg(target_os = "macos")]`-gated for a good reason of its own —
    /// see that module's doc comment — but this function is deliberately
    /// OS-unconditional so `#[cfg(test)]` can exercise both branches on
    /// any host; the literal and the const are byte-identical on the one
    /// OS where either is ever actually used).
    Sandbox { cmd: &'static str, args: Vec<String> },
    /// Any OS other than macOS: bubblewrap/`tome-shim` enforcement is
    /// Phase 4 — refuse rather than spawn a gapped pane with nothing
    /// actually enforcing its proxy env vars (the exact TOME-001 hole this
    /// rewrite exists to close).
    RefuseUnsupportedOs,
}

/// Pure decision core behind the gapped-pane fail-closed rule (see this
/// module's doc comment). `is_macos` is `cfg!(target_os = "macos")` at the
/// one real call site; parameterized here so `#[cfg(test)]` can exercise
/// the refusal branch even when this crate's own test suite happens to run
/// ON macOS.
fn resolve_gapped_spawn(is_macos: bool, seatbelt_profile: String) -> GappedSpawnDecision {
    if is_macos {
        GappedSpawnDecision::Sandbox {
            cmd: "/usr/bin/sandbox-exec",
            args: vec!["-p".to_string(), seatbelt_profile],
        }
    } else {
        GappedSpawnDecision::RefuseUnsupportedOs
    }
}

/// The TOME-001 re-auth ceremony's three possible outcomes — see this
/// module's doc comment and [`evaluate_reauth`].
enum ReauthOutcome {
    /// No `auth` payload arrived at all — the renderer's first attempt,
    /// before it has collected anything. No failure recorded.
    NeedsCredentials,
    /// A payload arrived but didn't verify. Caller records a failure.
    Rejected,
    /// A payload arrived and verified. Caller records a success and
    /// proceeds to spawn.
    Verified,
}

/// Pure decision core of the re-auth ceremony's outcome — everything AFTER
/// the caller has already computed whether a credential payload was
/// supplied (`opts.auth.is_some()`) and, if so, whether it actually
/// verified (`auth.totp_active() ? auth.verify_totp(...) :
/// auth.verify_passphrase(...)`, evaluated once at the call site — kept
/// out of this function so it needs no live `AuthLock` to test). Mirrors
/// the JS original's exact shape: `const ok = auth && (... ? verifyTotp(...)
/// : verifyPassphrase(...)); if (!ok) { if (auth) recordFailure(...); return
/// { reauth: true, error: auth ? '...' : null } }`.
fn evaluate_reauth(payload_supplied: bool, verified: bool) -> ReauthOutcome {
    if !payload_supplied {
        ReauthOutcome::NeedsCredentials
    } else if verified {
        ReauthOutcome::Verified
    } else {
        ReauthOutcome::Rejected
    }
}

/// Builds the `CommandBuilder` for EVERY pane this phase spawns — agent or
/// plain terminal, gapped or not. `agent_cmd` is
/// `agent_spawn::build_agent_spawn_from`'s output (`None` for `terminal`,
/// `Some(cmd)` otherwise); `sandbox`, when present, wraps the whole thing
/// in `sandbox-exec` exactly like `index.js`'s own `if (sandbox) {
/// spawnArgs = [...sandbox.args, spawnCmd, ...spawnArgs]; spawnCmd =
/// sandbox.cmd }`.
fn build_pty_command(
    shell: &str,
    agent_cmd: Option<&str>,
    cwd: &Path,
    env: &[(String, String)],
    sandbox: Option<&SandboxWrap>,
) -> CommandBuilder {
    let mut spawn_cmd = shell.to_string();
    let mut spawn_args: Vec<String> = match agent_cmd {
        Some(cmd) => vec!["-l".to_string(), "-c".to_string(), cmd.to_string()],
        None => vec!["-l".to_string()],
    };
    if let Some(sb) = sandbox {
        let mut wrapped = sb.args.clone();
        wrapped.push(spawn_cmd);
        wrapped.extend(spawn_args);
        spawn_args = wrapped;
        spawn_cmd = sb.cmd.clone();
    }

    let mut cmd = CommandBuilder::new(&spawn_cmd);
    for a in &spawn_args {
        cmd.arg(a);
    }
    cmd.cwd(cwd);
    // See build_terminal_command's original note (still true here): this
    // process's own environment must never leak into a pty child
    // unfiltered (TOME-007) — `env_clear()` wipes portable-pty's default
    // inherited seed, and every pair in `env` is the ONLY thing the child
    // ends up with.
    cmd.env_clear();
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd
}

/// `pty:create` (`src/main/index.js` lines 633-787: the `ipcMain.handle`
/// wrapper plus `createPty`). Follows the original's order exactly:
///
/// 1. Resolve `kind` against the built-in + vetted-custom agent list.
///    Anything neither a known agent nor `"terminal"` is a silent no-op.
/// 2. Resolve gapping (`pty_authority::resolve_gapping`) — the renderer may
///    ask for MORE isolation than policy wants, never less (TOME-001).
/// 3. TOME-001 re-auth ceremony, for EVERY ungapped spawn (agent or
///    terminal) once a passphrase is configured — see this module's doc
///    comment. Runs BEFORE resolving the spawn cwd, matching `createPty`'s
///    own order.
/// 4. Resolve the spawn cwd against the open workspace roots.
/// 5. If gapped: `resolve_gapped_spawn` — macOS gets a real `PaneProxy` +
///    `sandbox-exec` wrap; anything else refuses outright, fail-closed.
/// 6. Build the command line + environment (secrets only if agent, proxy
///    vars only if gapped) and spawn via `state.pty.spawn_raw` — the
///    primitive `spawn_terminal` is itself built on, per that function's
///    own doc comment inviting exactly this direct call for the agent
///    path once it existed.
#[tauri::command]
pub async fn pty_create(
    app: AppHandle,
    state: State<'_, AppState>,
    opts: PtyCreateOpts,
    on_data: Channel<Value>,
) -> Result<Value, String> {
    lock_gate::guard(&state, "pty:create")?;
    let locked = *state.locked.read().unwrap();
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;

    let customs = {
        let customs_dir = dir.clone();
        tokio::task::spawn_blocking(move || store::get(&customs_dir, "custom-agents", locked))
            .await
            .map_err(|e| e.to_string())?
    };
    let agents = custom_agents::merge_agents(agent_spawn::AGENTS, &customs);
    let is_agent = is_agent_kind(&agents, &opts.kind);
    if !is_agent && opts.kind != "terminal" {
        return Ok(json!({}));
    }

    let airgap_default = {
        let default_dir = dir.clone();
        tokio::task::spawn_blocking(move || store::get(&default_dir, "airgap-default", locked))
            .await
            .map_err(|e| e.to_string())?
    };
    // `!== false` in the JS original: an absent key (`Value::Null`) and
    // anything else but the literal `false` all mean "gap by default".
    let policy_default = airgap_default != json!(false);
    let effective_gapped = pty_authority::resolve_gapping(opts.airgap.unwrap_or(false), policy_default);

    // ---- TOME-001 re-auth ceremony (before resolving cwd — matches
    // createPty's own order) ----
    if !effective_gapped {
        let auth_configured = {
            let guard = state.auth.lock().expect("AppState.auth lock poisoned");
            guard.as_ref().map(|a| a.status().configured).unwrap_or(false)
        };
        if pty_authority::unrestricted_spawn_needs_reauth(effective_gapped, auth_configured) {
            let mut guard = state.auth.lock().expect("AppState.auth lock poisoned");
            let auth = guard.as_mut().ok_or_else(|| "auth: not initialized".to_string())?;
            let wait = auth.throttle_retry_in("pty:unrestricted");
            if wait > 0 {
                return Ok(json!({
                    "reauth": true,
                    "error": format!("Too many attempts. Wait {}s.", ceil_seconds(wait)),
                }));
            }
            let payload_supplied = opts.auth.is_some();
            let verified = payload_supplied
                && {
                    let payload = opts.auth.as_ref().expect("payload_supplied just checked Some");
                    if auth.totp_active() {
                        payload.get("code").and_then(Value::as_str).is_some_and(|c| auth.verify_totp(c))
                    } else {
                        payload
                            .get("passphrase")
                            .and_then(Value::as_str)
                            .is_some_and(|p| auth.verify_passphrase(p))
                    }
                };
            match evaluate_reauth(payload_supplied, verified) {
                ReauthOutcome::NeedsCredentials => {
                    return Ok(json!({"reauth": true, "error": Value::Null}));
                }
                ReauthOutcome::Rejected => {
                    auth.record_failure("pty:unrestricted");
                    return Ok(json!({"reauth": true, "error": "Incorrect passphrase or code."}));
                }
                ReauthOutcome::Verified => auth.record_success("pty:unrestricted"),
            }
        }
    }

    let home = std::env::home_dir().unwrap_or_default();
    let open_folders = state.open_folders.read().unwrap().clone();
    // `resolve_spawn_cwd` ends in a `std::fs::metadata` call — run it off
    // this async command's worker thread, same as the two `store::get`
    // calls above.
    let cwd = opts.cwd.clone();
    let spawn_cwd = tokio::task::spawn_blocking(move || {
        pty_authority::resolve_spawn_cwd(cwd.as_deref(), &open_folders, &home)
    })
    .await
    .map_err(|e| e.to_string())?;

    // ---- gapped-pane setup: live proxy + (macOS-only) sandbox wrap, or
    // fail-closed refusal — see this module's doc comment ----
    let mut proxy_port: Option<u16> = None;
    let mut sandbox: Option<SandboxWrap> = None;
    if effective_gapped {
        match resolve_gapped_spawn(cfg!(target_os = "macos"), airgap::seatbelt::seatbelt_profile(&dir)) {
            GappedSpawnDecision::Sandbox { cmd, args } => {
                let proxy = create_gapped_pane_proxy(&app, &state, &opts.id).await.map_err(|e| e.to_string())?;
                proxy_port = Some(proxy.port());
                sandbox = Some(SandboxWrap { cmd: cmd.to_string(), args });
            }
            GappedSpawnDecision::RefuseUnsupportedOs => {
                events::append(
                    &app,
                    eventlog::make_event(
                        "pty:blocked",
                        vec![
                            ("paneId", json!(opts.id)),
                            ("kind", json!(opts.kind)),
                            ("gapped", json!(true)),
                            ("reason", json!("linux sandbox lands in phase 4")),
                        ],
                        None,
                    ),
                );
                return Err(
                    "gapped panes are not yet enforced on this OS (linux sandbox lands in phase 4) — refusing to spawn unenforced"
                        .to_string(),
                );
            }
        }
    }

    let agent_cmd = agent_spawn::build_agent_spawn_from(&agents, &opts.kind, opts.model.as_deref());

    let login = login_env::login_env().await;
    let process_env: HashMap<String, String> = std::env::vars().collect();
    let secrets = if is_agent { login.secrets.clone() } else { HashMap::new() };
    let extras = agent_env::AgentEnvExtras {
        is_agent,
        secrets,
        brain_path: None,     // Phase 5 — see PtyCreateOpts's doc comment
        core_vault_root: None, // Phase 5
        proxy_port,
    };
    let env = pane_env(&process_env, &login.path, &extras);

    let cmd = build_pty_command(&login.shell, agent_cmd.as_deref(), &spawn_cwd, &env, sandbox.as_ref());
    let size = PtySize { rows: 24, cols: 80, pixel_width: 0, pixel_height: 0 };

    let exit_id = opts.id.clone();
    let exit_app = app.clone();
    let spawn_result = state
        .pty
        .spawn_raw(opts.id.clone(), cmd, size, on_data, move |exit_code| {
            let _ = exit_app.emit("pty:exit", json!({"id": exit_id, "exitCode": exit_code}));
            let exit_state = exit_app.state::<AppState>();
            close_pane_and_proxy(&exit_app, &exit_state, &exit_id);
        })
        .await;

    if let Err(err) = spawn_result {
        // The proxy came up (if gapped) before the spawn attempt — a
        // failed spawn must not strand it listening on loopback.
        close_pane_and_proxy(&app, &state, &opts.id);
        return Err(err);
    }
    Ok(json!({}))
}

/// `pty:write` (`tome-ipc.js`'s `write: (id, data) => fire('pty_write', {
/// id, data })`) — fire-and-forget from the renderer, so this never
/// surfaces a per-call error; an unknown pane id is a silent no-op, same as
/// the Electron original's `ptys.get(id)?.write(data)`.
#[tauri::command]
pub async fn pty_write(
    state: State<'_, AppState>,
    id: String,
    data: String,
) -> Result<Value, String> {
    lock_gate::guard(&state, "pty:write")?;
    state.pty.write(&id, &data);
    Ok(json!({}))
}

/// `pty:resize` (`fire('pty_resize', { id, cols, rows })`) — same
/// no-op-on-unknown-id contract as `pty_write`.
#[tauri::command]
pub async fn pty_resize(
    state: State<'_, AppState>,
    id: String,
    cols: u16,
    rows: u16,
) -> Result<Value, String> {
    lock_gate::guard(&state, "pty:resize")?;
    state.pty.resize(&id, cols, rows);
    Ok(json!({}))
}

/// `pty:kill` (`fire('pty_kill', { id })`) — signals and reaps the pane's
/// child process (see `crate::pty::Registry::kill`'s doc comment for the
/// full kill/drop/reap sequence and why it doesn't block on the reap
/// itself), THEN tears down its air-gap proxy immediately — mirrors the JS
/// original's `ptys.get(id)?.kill(); ptys.delete(id); conductor.forget(id);
/// airgap.closePane(id)`, which closes the pane's egress the moment a kill
/// is requested rather than waiting for the killed process's own exit
/// event. `on_exit` (fired later, once the killed process is actually
/// reaped — see `crate::pty`'s module doc comment) calls
/// `close_pane_and_proxy` too; that second call is a safe no-op by then
/// (idempotent, matching `closePane`'s own contract). An unknown/
/// already-gone pane id is a safe no-op throughout, same as the Electron
/// original's optional chaining.
#[tauri::command]
pub async fn pty_kill(app: AppHandle, state: State<'_, AppState>, id: String) -> Result<Value, String> {
    lock_gate::guard(&state, "pty:kill")?;
    state.pty.kill(&id).await;
    close_pane_and_proxy(&app, &state, &id);
    Ok(json!({}))
}

/// This phase's central security predicate: true for ANY resolved agent —
/// built-in or vetted custom — in `agents`; false for a plain terminal or an
/// unrecognized kind. Split out from `pty_create`'s body so the fail-closed
/// decision is unit-testable without a live `AppHandle`/`State`.
fn is_agent_kind(agents: &[AgentEntry], kind: &str) -> bool {
    agents.iter().any(|a| a.id == kind)
}

/// The env every pane (agent or plain terminal, gapped or not) is spawned
/// with: the current process's environment with `PATH` overridden to the
/// login shell's harvested value FIRST, then run through
/// `agent_env::compose_agent_env`'s allowlist. `extras.is_agent`/
/// `extras.proxy_port` are what make this the same helper for both a
/// gapped agent pane (secrets AND proxy vars) and a plain ungapped
/// terminal (`AgentEnvExtras::default()` — neither).
///
/// Mirrors the JS original's NET EFFECT — `ensureLoginEnv()` mutates
/// `process.env.PATH` in place before `buildAgentBaseEnv(process.env)` ever
/// reads it — without the in-place mutation; see `login_env.rs`'s module
/// doc comment for why this port returns data instead of mutating global
/// process state.
fn pane_env(process_env: &HashMap<String, String>, login_path: &str, extras: &agent_env::AgentEnvExtras) -> Vec<(String, String)> {
    let mut base = process_env.clone();
    base.insert("PATH".to_string(), login_path.to_string());
    agent_env::compose_agent_env(&base, extras).into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // ================= PtyCreateOpts — the renderer wire shape =================

    #[test]
    fn pty_create_opts_deserializes_the_full_renderer_payload() {
        let raw = json!({
            "id": "pane-1",
            "kind": "claude",
            "cwd": "/work/proj",
            "airgap": true,
            "ws": "/work/proj",
            "model": "haiku",
            "auth": { "passphrase": "x" },
        });
        let opts: PtyCreateOpts = serde_json::from_value(raw).unwrap();
        assert_eq!(opts.id, "pane-1");
        assert_eq!(opts.kind, "claude");
        assert_eq!(opts.cwd.as_deref(), Some("/work/proj"));
        assert_eq!(opts.airgap, Some(true));
        assert_eq!(opts.ws.as_deref(), Some("/work/proj"));
        assert_eq!(opts.model.as_deref(), Some("haiku"));
        assert!(opts.auth.is_some());
    }

    #[test]
    fn pty_create_opts_tolerates_a_bare_terminal_payload() {
        // panels/terminal.js always spreads every key, but cwd/airgap/ws/
        // model/auth are each `undefined` for the common case (a fresh
        // terminal pane, no pinned model, no workspace open, no prior
        // reauth attempt) — JSON.stringify drops an `undefined` property
        // entirely, so the object Tauri actually deserializes can be
        // missing any/all of them.
        let raw = json!({ "id": "pane-2", "kind": "terminal" });
        let opts: PtyCreateOpts = serde_json::from_value(raw).unwrap();
        assert_eq!(opts.cwd, None);
        assert_eq!(opts.airgap, None);
        assert_eq!(opts.ws, None);
        assert_eq!(opts.model, None);
        assert!(opts.auth.is_none());
    }

    #[test]
    fn pty_create_opts_rejects_a_payload_missing_the_required_id_or_kind() {
        assert!(serde_json::from_value::<PtyCreateOpts>(json!({ "kind": "terminal" })).is_err());
        assert!(serde_json::from_value::<PtyCreateOpts>(json!({ "id": "pane-3" })).is_err());
    }

    // ================= is_agent_kind — the fail-closed predicate =================

    fn builtins_only() -> Vec<AgentEntry> {
        custom_agents::merge_agents(agent_spawn::AGENTS, &Value::Null)
    }

    #[test]
    fn is_agent_kind_true_for_every_builtin() {
        let agents = builtins_only();
        for kind in agent_spawn::AGENTS {
            assert!(is_agent_kind(&agents, kind), "{kind} should be an agent kind");
        }
    }

    #[test]
    fn is_agent_kind_true_for_a_vetted_custom_agent() {
        let customs = json!([{ "id": "aider", "label": "Aider", "bin": "aider" }]);
        let agents = custom_agents::merge_agents(agent_spawn::AGENTS, &customs);
        assert!(is_agent_kind(&agents, "aider"));
    }

    #[test]
    fn is_agent_kind_false_for_a_plain_terminal() {
        assert!(!is_agent_kind(&builtins_only(), "terminal"));
    }

    #[test]
    fn is_agent_kind_false_for_an_unrecognized_kind() {
        assert!(!is_agent_kind(&builtins_only(), "some-unknown-kind"));
    }

    #[test]
    fn is_agent_kind_false_for_a_custom_id_that_was_never_vetted_in() {
        assert!(!is_agent_kind(&builtins_only(), "aider"));
    }

    #[test]
    fn is_agent_kind_false_for_a_custom_that_failed_vetting() {
        let customs = json!([{ "id": "evil", "label": "Evil", "bin": "/bin/sh" }]);
        let agents = custom_agents::merge_agents(agent_spawn::AGENTS, &customs);
        assert!(!is_agent_kind(&agents, "evil"));
    }

    // ================= pane_env — the login-shell PATH override + layering ================

    #[test]
    fn pane_env_overrides_path_with_the_login_shell_value() {
        let mut process_env = HashMap::new();
        process_env.insert("PATH".to_string(), "/usr/bin:/bin".to_string()); // launchd's bare PATH
        process_env.insert("HOME".to_string(), "/Users/tester".to_string());
        let harvested = "/usr/bin:/bin:/opt/homebrew/bin:/Users/tester/.local/bin";
        let env = pane_env(&process_env, harvested, &agent_env::AgentEnvExtras::default());
        let path = env.iter().find(|(k, _)| k == "PATH").map(|(_, v)| v.as_str());
        assert_eq!(path, Some(harvested));
    }

    #[test]
    fn pane_env_never_carries_a_provider_secret_for_a_plain_terminal() {
        let mut process_env = HashMap::new();
        process_env.insert("ANTHROPIC_API_KEY".to_string(), "sk-ant-should-not-leak".to_string());
        let env = pane_env(&process_env, "/usr/bin", &agent_env::AgentEnvExtras::default());
        assert!(
            env.iter().all(|(k, _)| k != "ANTHROPIC_API_KEY"),
            "pane_env with is_agent:false must never carry a provider credential"
        );
    }

    #[test]
    fn pane_env_carries_secrets_only_when_is_agent_is_set() {
        let mut secrets = HashMap::new();
        secrets.insert("ANTHROPIC_API_KEY".to_string(), "sk-ant-x".to_string());
        let extras = agent_env::AgentEnvExtras { is_agent: true, secrets, ..Default::default() };
        let env = pane_env(&HashMap::new(), "/usr/bin", &extras);
        assert_eq!(env.iter().find(|(k, _)| k == "ANTHROPIC_API_KEY").map(|(_, v)| v.as_str()), Some("sk-ant-x"));
    }

    #[test]
    fn pane_env_carries_proxy_vars_only_when_gapped() {
        let extras = agent_env::AgentEnvExtras { proxy_port: Some(54321), ..Default::default() };
        let env = pane_env(&HashMap::new(), "/usr/bin", &extras);
        assert_eq!(
            env.iter().find(|(k, _)| k == "HTTP_PROXY").map(|(_, v)| v.as_str()),
            Some("http://127.0.0.1:54321")
        );
        let ungapped = pane_env(&HashMap::new(), "/usr/bin", &agent_env::AgentEnvExtras::default());
        assert!(ungapped.iter().all(|(k, _)| k != "HTTP_PROXY"));
    }

    #[test]
    fn pane_env_still_sets_the_fixed_term_pair() {
        let env = pane_env(&HashMap::new(), "/usr/bin", &agent_env::AgentEnvExtras::default());
        let get = |k: &str| env.iter().find(|(key, _)| key == k).map(|(_, v)| v.clone());
        assert_eq!(get("TERM"), Some("xterm-256color".to_string()));
        assert_eq!(get("COLORTERM"), Some("truecolor".to_string()));
    }

    // ================= resolve_gapped_spawn — TOME-001's Linux fail-closed rule ================

    #[test]
    fn resolve_gapped_spawn_wraps_in_sandbox_exec_on_macos() {
        match resolve_gapped_spawn(true, "(version 1)".to_string()) {
            GappedSpawnDecision::Sandbox { cmd, args } => {
                assert_eq!(cmd, "/usr/bin/sandbox-exec");
                assert_eq!(args, vec!["-p".to_string(), "(version 1)".to_string()]);
            }
            GappedSpawnDecision::RefuseUnsupportedOs => panic!("expected Sandbox on macOS"),
        }
    }

    #[test]
    fn resolve_gapped_spawn_refuses_on_any_non_macos_target() {
        assert!(matches!(
            resolve_gapped_spawn(false, "(version 1)".to_string()),
            GappedSpawnDecision::RefuseUnsupportedOs
        ));
    }

    // ================= build_pty_command =================

    #[test]
    fn build_pty_command_is_a_bare_login_shell_for_a_terminal_pane() {
        let cmd = build_pty_command("/bin/sh", None, Path::new("/tmp"), &[], None);
        let argv: Vec<String> = cmd.get_argv().iter().map(|s| s.to_string_lossy().into_owned()).collect();
        assert_eq!(argv, vec!["/bin/sh".to_string(), "-l".to_string()]);
        assert_eq!(cmd.get_cwd().unwrap().to_string_lossy(), "/tmp");
    }

    #[test]
    fn build_pty_command_runs_the_agent_command_via_dash_c() {
        let cmd = build_pty_command("/bin/zsh", Some("claude"), Path::new("/work"), &[], None);
        let argv: Vec<String> = cmd.get_argv().iter().map(|s| s.to_string_lossy().into_owned()).collect();
        assert_eq!(argv, vec!["/bin/zsh".to_string(), "-l".to_string(), "-c".to_string(), "claude".to_string()]);
    }

    #[test]
    fn build_pty_command_wraps_the_whole_line_in_sandbox_exec_when_gapped() {
        let sandbox = SandboxWrap { cmd: "/usr/bin/sandbox-exec".to_string(), args: vec!["-p".to_string(), "PROFILE".to_string()] };
        let cmd = build_pty_command("/bin/zsh", Some("claude"), Path::new("/work"), &[], Some(&sandbox));
        let argv: Vec<String> = cmd.get_argv().iter().map(|s| s.to_string_lossy().into_owned()).collect();
        assert_eq!(
            argv,
            vec![
                "/usr/bin/sandbox-exec".to_string(),
                "-p".to_string(),
                "PROFILE".to_string(),
                "/bin/zsh".to_string(),
                "-l".to_string(),
                "-c".to_string(),
                "claude".to_string(),
            ]
        );
    }

    #[test]
    fn build_pty_command_env_is_exactly_what_was_given_not_merged_with_this_process() {
        let env = vec![("PATH".to_string(), "/usr/bin".to_string()), ("HOME".to_string(), "/home/x".to_string())];
        let cmd = build_pty_command("/bin/sh", None, Path::new("/tmp"), &env, None);
        assert_eq!(cmd.get_env("PATH").unwrap().to_string_lossy(), "/usr/bin");
        assert_eq!(cmd.get_env("HOME").unwrap().to_string_lossy(), "/home/x");
        assert!(
            std::env::var("USER").is_ok() || std::env::var("LOGNAME").is_ok(),
            "test precondition: expected USER or LOGNAME to be set in the test process"
        );
        assert!(cmd.get_env("USER").is_none() || std::env::var("USER").is_err());
    }

    // ================= evaluate_reauth — TOME-001's three-way outcome =================

    #[test]
    fn evaluate_reauth_needs_credentials_when_nothing_was_supplied() {
        assert!(matches!(evaluate_reauth(false, false), ReauthOutcome::NeedsCredentials));
    }

    #[test]
    fn evaluate_reauth_rejects_a_supplied_but_wrong_credential() {
        assert!(matches!(evaluate_reauth(true, false), ReauthOutcome::Rejected));
    }

    #[test]
    fn evaluate_reauth_accepts_a_verified_credential() {
        assert!(matches!(evaluate_reauth(true, true), ReauthOutcome::Verified));
    }

    // ================= pty_authority integration sanity (already pinned in
    // pty_authority.rs — these two just prove the wiring above reads them
    // the way pty_create actually calls them) =================

    #[test]
    fn ungapped_spawn_with_configured_auth_needs_the_reauth_ceremony() {
        assert!(pty_authority::unrestricted_spawn_needs_reauth(false, true));
    }

    #[test]
    fn gapped_spawn_never_needs_the_reauth_ceremony_regardless_of_auth_config() {
        assert!(!pty_authority::unrestricted_spawn_needs_reauth(true, true));
        assert!(!pty_authority::unrestricted_spawn_needs_reauth(true, false));
    }
}
