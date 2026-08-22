//! Assistant-run headless agents — the `run_agent` conductor tool's
//! production backend. One prompt in, one agent CLI runs sandboxed in the
//! background, its output comes back as the tool result.
//!
//! Reuses exactly the pieces a flow node's headless spawn uses
//! (`agent_spawn::build_headless_spawn` for the argv, `flow_env`'s
//! production env builder for the sandbox/gap, the runner's
//! `spawn_process` for the spawn), so an agent the assistant runs obeys
//! the same containment rules as one a flow runs: macOS seatbelt / Linux
//! bwrap + Landlock, egress-gapped with only model-provider domains
//! reachable through the pane proxy. `cwd` is the first open workspace
//! folder; everything the agent writes lands inside its sandbox's
//! write-allow set, same as a pane.
//!
//! ## Bounds
//!
//! A headless agent is a long-lived, expensive child: the run is bounded
//! by [`RUN_TIMEOUT`] (SIGTERM, then SIGKILL after a grace period) and the
//! returned text is capped to [`OUTPUT_CAP`] bytes, tail-kept. There is no
//! mid-run cancel: the chat's abort token is not threaded into this seam
//! (the tool result is the only rendezvous), so an aborted turn may leave
//! the agent running until it finishes or the timeout fires — same
//! documented posture as a background flow node, whose run outlives the
//! pane that started it.

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde_json::json;
use tauri::{AppHandle, Emitter, Manager};
use tokio::io::{AsyncBufReadExt, BufReader};

use crate::flow::runner::env::SandboxWrap;
use crate::flow::runner::spawn::{spawn_process, SpawnOutcome, SpawnRequest};

use crate::agent_spawn;
use crate::flow_env;

const RUN_TIMEOUT: Duration = Duration::from_secs(600);
const KILL_GRACE: Duration = Duration::from_secs(3);
const OUTPUT_CAP: usize = 24_000;

static SEQ: AtomicU64 = AtomicU64::new(0);

/// What the tool loop hands the seam: the agent CLI kind (a built-in —
/// only the three built-ins have headless templates), an optional model
/// pin, and the task prompt.
pub struct RunAgentRequest {
    pub chat_id: String,
    pub kind: String,
    pub model: Option<String>,
    pub prompt: String,
}

/// Runs one agent headless. Emits `conductor:agent` lifecycle events
/// (`started` / `done` / `failed`) for the renderer's chat chip, and
/// resolves to the agent's output tail (or an `Err` describing the
/// failure — refused kind, env/sandbox setup failure, timeout).
pub async fn run_headless_agent(
    app: &AppHandle,
    req: &RunAgentRequest,
    cwd: &Path,
) -> Result<String, String> {
    let Some(spawn) =
        agent_spawn::build_headless_spawn(&req.kind, req.model.as_deref(), Some(&req.prompt))
    else {
        return Err(format!(
            "cannot run '{}' headless — not a headless-capable agent CLI",
            req.kind
        ));
    };

    let pane_id = format!("assistant-a{}", SEQ.fetch_add(1, Ordering::Relaxed));
    let argv: Vec<String> = std::iter::once(spawn.cmd.clone())
        .chain(spawn.args.iter().cloned())
        .collect();
    let _ = app.emit(
        "conductor:agent",
        json!({ "chatId": req.chat_id, "kind": req.kind, "status": "started" }),
    );

    // Same env builder a flow node uses: sandbox + egress gap + model-only
    // proxy. Failures here (seatbelt refused, bwrap missing) are the
    // tool's result, not a hang.
    let built = match flow_env::build_production_agent_env(app, &pane_id, true, argv.clone(), cwd)
        .await
    {
        Ok(b) => b,
        Err(msg) => {
            crate::ipc::egress::close_pane_and_proxy(app, app.state::<crate::state::AppState>().inner(), &pane_id);
            let _ = app.emit(
                "conductor:agent",
                json!({ "chatId": req.chat_id, "kind": req.kind, "status": "failed" }),
            );
            return Err(format!("could not prepare the agent environment: {msg}"));
        }
    };

    let (cmd, args): (String, Vec<String>) = match &built.sandbox {
        None => (argv[0].clone(), argv[1..].to_vec()),
        Some(SandboxWrap::Prefix { cmd, args }) => {
            let mut a = args.clone();
            a.extend(argv.iter().cloned());
            (cmd.clone(), a)
        }
        Some(SandboxWrap::Full { argv }) => (argv[0].clone(), argv[1..].to_vec()),
    };

    let mut spawned = match spawn_process(SpawnRequest {
        cmd,
        args,
        cwd: cwd.to_path_buf(),
        env: built.env,
    }) {
        SpawnOutcome::Started(s) => s,
        SpawnOutcome::Failed(e) => {
            crate::ipc::egress::close_pane_and_proxy(app, app.state::<crate::state::AppState>().inner(), &pane_id);
            let _ = app.emit(
                "conductor:agent",
                json!({ "chatId": req.chat_id, "kind": req.kind, "status": "failed" }),
            );
            return Err(format!("could not spawn {}: {e}", req.kind));
        }
    };

    // Stream both pipes into one capped buffer; then wait for the exit.
    let mut text = String::new();
    let mut out = BufReader::new(
        spawned
            .stdout
            .take()
            .ok_or("agent stdout pipe missing")?,
    )
    .lines();
    let mut err = BufReader::new(
        spawned
            .stderr
            .take()
            .ok_or("agent stderr pipe missing")?,
    )
    .lines();
    let (mut out_done, mut err_done) = (false, false);
    while !(out_done && err_done) {
        tokio::select! {
            r = out.next_line(), if !out_done => match r {
                Ok(Some(line)) => push_capped(&mut text, line),
                Ok(None) => out_done = true,
                Err(_) => out_done = true,
            },
            r = err.next_line(), if !err_done => match r {
                Ok(Some(line)) => push_capped(&mut text, line),
                Ok(None) => err_done = true,
                Err(_) => err_done = true,
            },
        }
    }

    // The pipes have closed; the process may take a beat to reap. Timeout
    // is the bound on the WHOLE run — a wedged child with closed pipes
    // gets SIGTERM, then SIGKILL after the grace period.
    let exit = match tokio::time::timeout(RUN_TIMEOUT, &mut spawned.exit).await {
        Ok(Ok(exit)) => exit,
        Ok(Err(_)) => {
            crate::ipc::egress::close_pane_and_proxy(app, app.state::<crate::state::AppState>().inner(), &pane_id);
            let _ = app.emit(
                "conductor:agent",
                json!({ "chatId": req.chat_id, "kind": req.kind, "status": "failed" }),
            );
            return Err("agent ended without an exit status".to_string());
        }
        Err(_) => {
            let _ = (spawned.kill)(15);
            let _ = tokio::time::timeout(KILL_GRACE, &mut spawned.exit).await;
            let _ = (spawned.kill)(9);
            crate::ipc::egress::close_pane_and_proxy(app, app.state::<crate::state::AppState>().inner(), &pane_id);
            let _ = app.emit(
                "conductor:agent",
                json!({ "chatId": req.chat_id, "kind": req.kind, "status": "failed" }),
            );
            return Err(format!("agent timed out after {}s", RUN_TIMEOUT.as_secs()));
        }
    };

    crate::ipc::egress::close_pane_and_proxy(app, app.state::<crate::state::AppState>().inner(), &pane_id);
    let _ = app.emit(
        "conductor:agent",
        json!({ "chatId": req.chat_id, "kind": req.kind, "status": "done" }),
    );

    if text.is_empty() {
        return Err(format!(
            "agent exited {} with no output",
            exit.code.unwrap_or(-1)
        ));
    }
    Ok(text)
}

/// Appends a line (plus newline) to the capped output buffer, trimming
/// from the FRONT — the tail is the useful part of an agent run, same
/// boundary-snap idiom as `conductor::tools`' file reads.
fn push_capped(text: &mut String, line: String) {
    text.push_str(&line);
    text.push('\n');
    if text.len() > OUTPUT_CAP {
        let mut cut = text.len() - OUTPUT_CAP;
        while cut < text.len() && !text.is_char_boundary(cut) {
            cut += 1;
        }
        text.drain(..cut);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_capped_keeps_the_tail_and_respects_char_boundaries() {
        let mut s = String::new();
        for i in 0..4000 {
            push_capped(&mut s, format!("line number {i:04} with some padding"));
        }
        assert!(s.len() <= OUTPUT_CAP);
        assert!(s.ends_with("line number 3999 with some padding\n"));
        assert!(!s.contains("line number 0000"), "head should be trimmed");

        let mut s = String::new();
        for _ in 0..3000 {
            push_capped(&mut s, "éééééééééé".to_string());
        }
        assert!(s.len() <= OUTPUT_CAP);
        assert!(s.is_char_boundary(s.len() - 1));
    }
}
