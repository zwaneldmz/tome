//! PTY lifecycle. Ports `src/main/index.js`'s `pty:*` handlers (spawn via
//! `portable-pty`, the 4ms/64KB output batcher, explicit kill/reap) plus
//! `src/main/lib/{agent-spawn,agent-env,pty-authority,custom-agents}.js`
//! for spawn vetting. `pty:data` streams to the renderer over a Tauri
//! Channel per pane; `pty:exit` still goes out on the global event bus
//! (`app.emit`, via the `on_exit` closure `pty_create` hands
//! `crate::pty::Registry::spawn_terminal`) — that split matches the
//! already-committed renderer contract (`tome-ipc.js`'s separate
//! `onData`/`onExit` wiring; see `crate::pty`'s module doc comment for why
//! sending `pty:exit` down the Channel instead would silently break it).
//!
//! `pty_write`/`pty_resize`/`pty_kill` are thin wrappers over
//! `crate::pty::Registry` (`state.pty`) — Phase 2 slice P1's work (see that
//! module's doc comment for the batcher/reader/kill mechanism). `pty_create`
//! below is this phase's integration slice (P4): the full `pty:create`
//! handler, reconciling P1's PTY mechanism with P2/P3's spawn-policy ports
//! (`crate::agent_spawn`, `crate::custom_agents`, `crate::pty_authority`,
//! `crate::agent_env`, `crate::login_env`). See `pty_create`'s own doc
//! comment for the exact order it follows and this phase's fail-closed
//! security rule.

use std::collections::HashMap;

use serde::Deserialize;
use serde_json::{json, Value};
use tauri::ipc::Channel;
use tauri::{AppHandle, Emitter, Manager, State};

use crate::agent_spawn::{self, AgentEntry};
use crate::pty::TerminalOpts;
use crate::{agent_env, custom_agents, events, eventlog, lock_gate, login_env, pty_authority, state::AppState, store};

/// Wire shape of `pty:create`'s options object. `tome-ipc.js`'s
/// `pty.create: (opts) => { const ch = new Channel(); ...; return
/// call('pty_create', { opts, onData: ch }) }` forwards the renderer's
/// `opts` object verbatim — see `src/renderer/panels/terminal.js`'s
/// `tome.pty.create({ id, kind, cwd, airgap, ws, model, auth })` for the
/// actual call site — which `src/main/index.js`'s handler destructures as
/// `{ id, kind, cwd, airgap: gapped, ws, model, auth }` (line 633).
///
/// `ws`/`model`/`auth` are accepted here — so a real renderer payload always
/// deserializes cleanly, and the struct documents the full wire contract —
/// but are write-only this phase: `ws` needs `brain::ensureBrain` (Phase 5;
/// no `brain.rs` module exists in this tree yet), `model` only ever mattered
/// for building an AGENT command line (this phase never builds one — see
/// `pty_create`'s doc comment), and `auth` only ever mattered for the
/// ungapped re-auth ceremony (`unrestrictedSpawnNeedsReauth`, explicitly
/// deferred to Phase 3 by this phase's binding decision).
#[allow(dead_code)]
#[derive(Debug, Deserialize)]
pub struct PtyCreateOpts {
    pub id: String,
    pub kind: String,
    pub cwd: Option<String>,
    pub airgap: Option<bool>,
    pub ws: Option<String>,
    pub model: Option<String>,
    pub auth: Option<Value>,
}

/// `pty:create` (`src/main/index.js` lines 633-787: the `ipcMain.handle`
/// wrapper plus `createPty`). Follows the original's order:
///
/// 1. Resolve `kind` against the built-in + vetted-custom agent list
///    (`custom_agents::merge_agents`). Anything neither a known agent nor
///    `"terminal"` is a silent no-op — mirrors the JS original's bare
///    `return` (a stale flow.json, or a custom agent removed since the
///    renderer last read the list).
/// 2. Resolve gapping (`pty_authority::resolve_gapping`) — the renderer may
///    ask for MORE isolation than policy wants, never less (TOME-001).
/// 3. Resolve the spawn cwd (`pty_authority::resolve_spawn_cwd`) against the
///    open workspace roots.
/// 4. Branch: a `"terminal"` pane spawns for real, over the same mechanism
///    `pty_write`/`pty_resize`/`pty_kill` already use (`state.pty`,
///    `crate::pty::Registry::spawn_terminal`) — with `PATH` from
///    `crate::login_env`'s harvested login-shell environment, not this
///    process's own (Finder/Spotlight launches inherit launchd's bare
///    PATH). ANY resolved agent — built-in or custom, gapped or ungapped —
///    is refused outright.
///
/// SECURITY (fail closed, non-negotiable — this phase's binding decision):
/// airgap enforcement for a gapped agent pane, and the re-auth ceremony for
/// an ungapped one, are both Phase 3/4 work that does not exist in this tree
/// yet. Spawning an agent here regardless would be exactly the
/// silent-degrade TOME-001 exists to close — an "air-gapped" agent pane
/// with no actual gap. So every resolved agent is refused, unconditionally,
/// independent of `effective_gapped`. The refusal is logged to the
/// persistent security event log (`events::append`): the JS original never
/// refuses an agent spawn at all, so there is no `logEvent` call site to
/// port here verbatim — this mirrors the shape `airgap.js`'s
/// `logEvent('airgap:blocked', { paneId, host })` already established for
/// "a spawn-adjacent security decision happened" (kind + identifiers only,
/// no free text — see `eventlog.rs`'s module doc comment).
///
/// A `"terminal"` pane, by contrast, always spawns in this phase —
/// REGARDLESS of `effective_gapped` — because that is simply where the
/// build is: there is no sandbox to put a gapped pane into yet (Phase 3/4).
/// This matches the plan's own phase breakdown (Phase 2 is PTY + spawn
/// POLICY; airgap enforcement is Phase 3/4) and this phase's binding
/// decision's own words, which single out "AGENT spawn" for the fail-closed
/// rule, not every pane.
///
/// NOT ported (see their own modules'/this phase's doc comments for why):
/// `unrestrictedSpawnNeedsReauth`'s re-auth ceremony (Phase 3 — and moot
/// here regardless, since every JS path that would reach it for an agent is
/// a spawn this phase already refuses before getting that far);
/// `conductor.register`/`.record`/`.markExited` and `airgap.closePane`
/// (neither `conductor.rs` nor `airgap.rs` exist as ported modules yet —
/// Phase 5 and Phase 3/4 respectively); `buildAgentSpawnFrom`'s actual
/// command-line construction (building a command line for an agent pane
/// this function is about to refuse to spawn would have no effect on
/// anything); the outer JS handler's duplicate `pty:data` red-text push on
/// a thrown error (the renderer's own `catch` in `panels/terminal.js`
/// already writes the identical text locally on ANY rejected
/// `pty.create()`, so a plain `Err` here produces the same user-visible
/// outcome without a second, redundant channel send).
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

    let customs_dir = dir.clone();
    let customs = tokio::task::spawn_blocking(move || store::get(&customs_dir, "custom-agents", locked))
        .await
        .map_err(|e| e.to_string())?;
    let agents = custom_agents::merge_agents(agent_spawn::AGENTS, &customs);
    let is_agent = is_agent_kind(&agents, &opts.kind);
    if !is_agent && opts.kind != "terminal" {
        return Ok(json!({}));
    }

    let airgap_default = tokio::task::spawn_blocking(move || store::get(&dir, "airgap-default", locked))
        .await
        .map_err(|e| e.to_string())?;
    // `!== false` in the JS original: an absent key (`Value::Null`) and
    // anything else but the literal `false` all mean "gap by default".
    let policy_default = airgap_default != json!(false);
    let effective_gapped = pty_authority::resolve_gapping(opts.airgap.unwrap_or(false), policy_default);

    let home = std::env::home_dir().unwrap_or_default();
    let open_folders = state.open_folders.read().unwrap().clone();
    // `resolve_spawn_cwd` ends in a `std::fs::metadata` call (see its own
    // doc comment) — run it off this async command's worker thread, same
    // as the two `store::get` calls above, so a slow or momentarily
    // unresponsive filesystem under the resolved cwd (a network mount, an
    // un-materialized iCloud Drive/Dropbox placeholder — plausible on
    // macOS, this build's primary target) can't stall every other task
    // sharing that Tokio worker — other panes' 4ms batcher flushes
    // included — for however long the stat() takes.
    let cwd = opts.cwd.clone();
    let spawn_cwd = tokio::task::spawn_blocking(move || {
        pty_authority::resolve_spawn_cwd(cwd.as_deref(), &open_folders, &home)
    })
    .await
    .map_err(|e| e.to_string())?;

    if is_agent {
        events::append(
            &app,
            eventlog::make_event(
                "pty:blocked",
                vec![
                    ("paneId", json!(opts.id)),
                    ("kind", json!(opts.kind)),
                    ("gapped", json!(effective_gapped)),
                ],
                None,
            ),
        );
        return Err("unimplemented: agent spawns land with the airgap port (phase 3)".to_string());
    }

    // A terminal pane always spawns this phase, even when
    // `effective_gapped` is true — there is no sandbox to put it in yet
    // (Phase 3/4; see this fn's doc comment and this phase's binding
    // decision, which singles out only AGENT spawns for the fail-closed
    // refusal above). That is a real, if temporary and explicitly scoped,
    // gap between the resolved policy and what actually runs — blocking it
    // here instead would leave a default install (`airgap-default` unset,
    // which resolves to gapped) unable to open ANY pane at all this phase.
    // Logging it, without blocking the spawn, keeps that interim posture
    // auditable instead of silent, mirroring the refusal log above.
    if effective_gapped {
        events::append(
            &app,
            eventlog::make_event(
                "pty:unconfined",
                vec![("paneId", json!(opts.id)), ("kind", json!(opts.kind))],
                None,
            ),
        );
    }

    let login = login_env::login_env().await;
    let process_env: HashMap<String, String> = std::env::vars().collect();
    let env = terminal_env(&process_env, &login.path);

    let terminal_opts = TerminalOpts {
        id: opts.id,
        shell: login.shell.clone(),
        cwd: spawn_cwd,
        env,
        cols: 80,
        rows: 24,
    };
    // `Registry::spawn_raw` calls this exactly once, after the pane's
    // process has actually been reaped — the ONLY place `pty:exit` is
    // produced (see `crate::pty`'s module doc comment). Emitting it here,
    // on the global event bus, is what `tome-ipc.js`'s `pty.onExit` (a
    // plain `listen('pty:exit', cb)`, wired independently of the `onData`
    // Channel) already expects — no renderer change needed.
    let exit_id = terminal_opts.id.clone();
    let exit_app = app.clone();
    state
        .pty
        .spawn_terminal(terminal_opts, on_data, move |exit_code| {
            let _ = exit_app.emit("pty:exit", json!({"id": exit_id, "exitCode": exit_code}));
        })
        .await?;
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
/// itself); `pty:exit` follows independently once the pane's own reader
/// task observes the child's exit. An unknown/already-gone pane id is a
/// safe no-op, same as the Electron original's `ptys.get(id)?.kill()`.
#[tauri::command]
pub async fn pty_kill(state: State<'_, AppState>, id: String) -> Result<Value, String> {
    lock_gate::guard(&state, "pty:kill")?;
    state.pty.kill(&id).await;
    Ok(json!({}))
}

/// This phase's central security predicate: true for ANY resolved agent —
/// built-in or vetted custom — in `agents`; false for a plain terminal or an
/// unrecognized kind. Split out from `pty_create`'s body so the fail-closed
/// decision is unit-testable without a live `AppHandle`/`State` (this crate
/// cannot construct either outside a running Tauri app — no `tauri::test`
/// feature is enabled, and `Cargo.toml` is out of this slice's scope to
/// edit; see `confine.rs`'s doc comment for the same constraint elsewhere in
/// this crate).
fn is_agent_kind(agents: &[AgentEntry], kind: &str) -> bool {
    agents.iter().any(|a| a.id == kind)
}

/// The env `spawn_terminal` receives for a terminal pane: the current
/// process's environment with `PATH` overridden to the login shell's
/// harvested value FIRST, then run through `agent_env::compose_agent_env`'s
/// allowlist (`is_agent: false` hardcoded here, not threaded in from a
/// caller — a plain terminal never gets provider secrets, mirroring
/// `buildAgentEnv`'s `if (agent) Object.assign(...)` gate; the only caller
/// of this function is the terminal branch of `pty_create`, which is why
/// there is no `is_agent` parameter to get wrong).
///
/// Mirrors the JS original's NET EFFECT — `ensureLoginEnv()` mutates
/// `process.env.PATH` in place before `buildAgentBaseEnv(process.env)` ever
/// reads it — without the in-place mutation; see `login_env.rs`'s module
/// doc comment for why this port returns data instead of mutating global
/// process state.
fn terminal_env(process_env: &HashMap<String, String>, login_path: &str) -> Vec<(String, String)> {
    let mut base = process_env.clone();
    base.insert("PATH".to_string(), login_path.to_string());
    agent_env::compose_agent_env(
        &base,
        &agent_env::AgentEnvExtras {
            is_agent: false,
            ..Default::default()
        },
    )
    .into_iter()
    .collect()
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
        // "aider" was never added to the customs store value here — it must
        // not be treated as an agent just because it looks like a plausible
        // custom id elsewhere in this file's other tests.
        assert!(!is_agent_kind(&builtins_only(), "aider"));
    }

    #[test]
    fn is_agent_kind_false_for_a_custom_that_failed_vetting() {
        // An invalid custom entry (bin looks like a path) never makes it
        // into the merged list at all — refused at the custom_agents door,
        // not silently treated as a non-agent kind further down.
        let customs = json!([{ "id": "evil", "label": "Evil", "bin": "/bin/sh" }]);
        let agents = custom_agents::merge_agents(agent_spawn::AGENTS, &customs);
        assert!(!is_agent_kind(&agents, "evil"));
    }

    // ================= terminal_env — the login-shell PATH override =================

    #[test]
    fn terminal_env_overrides_path_with_the_login_shell_value() {
        let mut process_env = HashMap::new();
        process_env.insert("PATH".to_string(), "/usr/bin:/bin".to_string()); // launchd's bare PATH
        process_env.insert("HOME".to_string(), "/Users/tester".to_string());
        let harvested = "/usr/bin:/bin:/opt/homebrew/bin:/Users/tester/.local/bin";
        let env = terminal_env(&process_env, harvested);
        let path = env.iter().find(|(k, _)| k == "PATH").map(|(_, v)| v.as_str());
        assert_eq!(path, Some(harvested));
    }

    #[test]
    fn terminal_env_never_carries_a_provider_secret() {
        // is_agent: false is hardcoded inside terminal_env itself, not
        // threaded in from a caller — there is no parameter through which a
        // provider credential could reach a terminal pane's env via this
        // function, today or after an unrelated future edit.
        let mut process_env = HashMap::new();
        process_env.insert("ANTHROPIC_API_KEY".to_string(), "sk-ant-should-not-leak".to_string());
        let env = terminal_env(&process_env, "/usr/bin");
        assert!(
            env.iter().all(|(k, _)| k != "ANTHROPIC_API_KEY"),
            "terminal_env must never carry a provider credential, even one already sitting in process_env"
        );
    }

    #[test]
    fn terminal_env_still_sets_the_fixed_term_pair() {
        let env = terminal_env(&HashMap::new(), "/usr/bin");
        let get = |k: &str| env.iter().find(|(key, _)| key == k).map(|(_, v)| v.clone());
        assert_eq!(get("TERM"), Some("xterm-256color".to_string()));
        assert_eq!(get("COLORTERM"), Some("truecolor".to_string()));
    }
}
