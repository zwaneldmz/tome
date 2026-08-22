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

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde_json::json;
use tauri::AppHandle;
use tokio::io::{AsyncBufReadExt, BufReader};

use crate::flow::runner::env::{BoxFuture, BuiltEnv, SandboxWrap};
use crate::flow::runner::spawn::{spawn_process, SpawnOutcome, SpawnRequest};
use crate::ipc::egress::{close_pane_and_proxy, EgressEnv};

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

/// The seam this file's spawn path reads instead of touching a concrete
/// [`AppHandle`] — nothing outside a running app can construct one (this
/// crate enables no tauri `test` feature, exactly as `EgressEnv`'s own doc
/// comment lays out), so this trait extends that existing seam with the
/// two AppHandle-only pieces the headless run needs: the production env
/// builder and the spawn backend. `#[cfg(test)]` drives this trait against
/// an owned `AppState::new()` + tempdirs while keeping every REAL piece —
/// the production env (`flow_env::build_production_agent_env_for`: live
/// `PaneProxy`, seatbelt/bwrap wrap, proxy env vars) and the real
/// `spawn_process` — the same trick `ipc::egress`'s own tests use to run a
/// real `PaneProxy` with no live app.
pub(crate) trait AgentRunEnv: EgressEnv {
    /// The real production env for one headless run — sandbox wrap + the
    /// egress-gapped env, exactly what
    /// `flow_env::build_production_agent_env` builds. `gapped` is not part
    /// of the seam: a headless agent run is ALWAYS gapped (see
    /// `run_headless`), so the seam has one job and no knob.
    fn build_env(
        &self,
        pane_id: String,
        inner_argv: Vec<String>,
        cwd: PathBuf,
    ) -> BoxFuture<Result<BuiltEnv, String>>;
    /// Spawns the built command — production is `spawn_process` itself;
    /// a test records the request and then delegates to the same real
    /// spawn, so the argv/env assertions see the exact bytes the real
    /// child gets.
    fn spawn(&self, req: SpawnRequest) -> SpawnOutcome;
}

impl AgentRunEnv for AppHandle {
    fn build_env(
        &self,
        pane_id: String,
        inner_argv: Vec<String>,
        cwd: PathBuf,
    ) -> BoxFuture<Result<BuiltEnv, String>> {
        let app = self.clone();
        Box::pin(async move {
            flow_env::build_production_agent_env(&app, &pane_id, true, inner_argv, &cwd).await
        })
    }

    fn spawn(&self, req: SpawnRequest) -> SpawnOutcome {
        spawn_process(req)
    }
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
    run_headless::<AppHandle>(app, req, cwd, RUN_TIMEOUT).await
}

/// The real headless-run body, generic over the [`AgentRunEnv`] seam so
/// tests can drive it with no live app, and over the run bound so a test
/// can prove the timeout fires without waiting out the production ten
/// minutes. Everything else — kind vetting, the sandbox wrap, the egress
/// gap, output streaming, the SIGTERM→SIGKILL kill ladder — is the
/// production code itself.
async fn run_headless<E: AgentRunEnv>(
    env: &E,
    req: &RunAgentRequest,
    cwd: &Path,
    run_timeout: Duration,
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
    env.emit_json(
        "conductor:agent",
        json!({ "chatId": req.chat_id, "kind": req.kind, "status": "started" }),
    );

    // Same env builder a flow node uses: sandbox + egress gap + model-only
    // proxy. Failures here (seatbelt refused, bwrap missing) are the
    // tool's result, not a hang.
    let built = match env
        .build_env(pane_id.clone(), argv.clone(), cwd.to_path_buf())
        .await
    {
        Ok(b) => b,
        Err(msg) => {
            close_pane_and_proxy(env, env.app_state(), &pane_id);
            env.emit_json(
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

    let mut spawned = match env.spawn(SpawnRequest {
        cmd,
        args,
        cwd: cwd.to_path_buf(),
        env: built.env,
    }) {
        SpawnOutcome::Started(s) => s,
        SpawnOutcome::Failed(e) => {
            close_pane_and_proxy(env, env.app_state(), &pane_id);
            env.emit_json(
                "conductor:agent",
                json!({ "chatId": req.chat_id, "kind": req.kind, "status": "failed" }),
            );
            return Err(format!("could not spawn {}: {e}", req.kind));
        }
    };

    // ONE deadline over the WHOLE remainder of the run — draining both
    // pipes AND reaping the exit — so the bound holds even for a child
    // that wedges with its pipes still open (the common hang shape: stuck
    // mid-generation). A bound that only starts once both pipes hit EOF
    // bounds nothing at all, and an agent spewing past OUTPUT_CAP forever
    // would otherwise spin this loop with no exit either.
    let run = async {
        // Stream both pipes into one capped buffer; then wait for the exit.
        let mut text = String::new();
        let mut out =
            BufReader::new(spawned.stdout.take().ok_or("agent stdout pipe missing")?).lines();
        let mut err =
            BufReader::new(spawned.stderr.take().ok_or("agent stderr pipe missing")?).lines();
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
        let exit = (&mut spawned.exit)
            .await
            .map_err(|_| "agent ended without an exit status".to_string())?;
        Ok::<_, String>((text, exit))
    };
    let timed = tokio::time::timeout(run_timeout, run);
    let outcome = timed.await;
    let (text, exit) = match outcome {
        Ok(Ok(ok)) => ok,
        Ok(Err(pipe_err)) => {
            close_pane_and_proxy(env, env.app_state(), &pane_id);
            env.emit_json(
                "conductor:agent",
                json!({ "chatId": req.chat_id, "kind": req.kind, "status": "failed" }),
            );
            return Err(pipe_err);
        }
        Err(_) => {
            // Wedged — open pipes or an exit that never resolved. The
            // whole-run bound fired: SIGTERM, then SIGKILL after the
            // grace period.
            let _ = (spawned.kill)(15);
            let _ = tokio::time::timeout(KILL_GRACE, &mut spawned.exit).await;
            let _ = (spawned.kill)(9);
            close_pane_and_proxy(env, env.app_state(), &pane_id);
            env.emit_json(
                "conductor:agent",
                json!({ "chatId": req.chat_id, "kind": req.kind, "status": "failed" }),
            );
            return Err(format!("agent timed out after {}s", run_timeout.as_secs()));
        }
    };

    close_pane_and_proxy(env, env.app_state(), &pane_id);
    env.emit_json(
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
    use std::sync::{Arc, Mutex as StdMutex};

    use serde_json::Value;

    use crate::state::AppState;

    /// Wraps an owned `AppState` + tempdirs and stands in for the live
    /// `AppHandle` this seam exists because no test can construct — the
    /// same double shape `ipc::egress`'s own tests use. Every capability
    /// is REAL: `build_env` calls the production
    /// `flow_env::build_production_agent_env_for` (live `PaneProxy`, real
    /// seatbelt profile, real proxy env vars), and `spawn` records the
    /// request then delegates to the real `spawn_process`.
    #[derive(Clone)]
    struct TestEnv {
        state: Arc<AppState>,
        /// Stands in for `app.path().app_data_dir()` — the production
        /// seatbelt profile denies reads/writes of this exact dir, so a
        /// tempdir keeps the test from denying anything real.
        config_dir: Arc<tempfile::TempDir>,
        /// The headless agent's cwd — the production `cwd` is the first
        /// open workspace folder; a tempdir keeps the run hermetic.
        workspace: Arc<tempfile::TempDir>,
        /// Where the fake agent CLI lives (see `AgentRunEnv::build_env`).
        /// A full `TempDir` (not a bare path) so the directory outlives
        /// the run.
        bin_dir: Arc<tempfile::TempDir>,
        events: Arc<StdMutex<Vec<(String, Value)>>>,
        spawns: Arc<StdMutex<Vec<SpawnRequest>>>,
    }

    impl TestEnv {
        fn new() -> Self {
            Self {
                state: Arc::new(AppState::new()),
                config_dir: Arc::new(tempfile::tempdir().expect("config tempdir")),
                workspace: Arc::new(tempfile::tempdir().expect("workspace tempdir")),
                bin_dir: Arc::new(tempfile::tempdir().expect("bin tempdir")),
                events: Arc::new(StdMutex::new(Vec::new())),
                spawns: Arc::new(StdMutex::new(Vec::new())),
            }
        }

        fn agent_events(&self) -> Vec<Value> {
            self.events
                .lock()
                .unwrap()
                .iter()
                .filter(|(k, _)| k == "conductor:agent")
                .map(|(_, v)| v.clone())
                .collect()
        }
    }

    impl EgressEnv for TestEnv {
        fn app_state(&self) -> &AppState {
            &self.state
        }
        fn emit_json(&self, event: &str, payload: Value) {
            self.events
                .lock()
                .unwrap()
                .push((event.to_string(), payload));
        }
        fn log(&self, kind: &str, fields: Vec<(&'static str, Value)>) {
            let obj: serde_json::Map<String, Value> = fields
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect();
            self.events
                .lock()
                .unwrap()
                .push((kind.to_string(), Value::Object(obj)));
        }
    }

    impl AgentRunEnv for TestEnv {
        fn build_env(
            &self,
            pane_id: String,
            inner_argv: Vec<String>,
            cwd: PathBuf,
        ) -> BoxFuture<Result<BuiltEnv, String>> {
            let this = self.clone();
            Box::pin(async move {
                let mut built = crate::flow_env::build_production_agent_env_for(
                    &this,
                    this.config_dir.path(),
                    &pane_id,
                    inner_argv,
                    &cwd,
                )
                .await?;
                // `build_headless_spawn` emits the BARE kind name (`pi`,
                // not a path) and `spawn_process` resolves it through the
                // env's PATH — which the production builder just set to the
                // login shell's. The test's fake CLI must therefore be
                // PREPENDED onto that PATH — not appended: a host that
                // already has a real `pi`/`claude`/`opencode` would resolve
                // that instead (this test's first run did exactly that, and
                // a REAL agent ran). PATH's CONTENT is not a containment
                // flag (the proxy vars, the seatbelt/bwrap wrap, and the
                // config-dir denials are — all left untouched), so this
                // keeps the production env intact while pinning the fake.
                for (k, v) in built.env.iter_mut() {
                    if k == "PATH" {
                        let mut prepended = this.bin_dir.path().to_string_lossy().into_owned();
                        prepended.push(':');
                        prepended.push_str(v);
                        *v = prepended;
                    }
                }
                Ok(built)
            })
        }

        fn spawn(&self, req: SpawnRequest) -> SpawnOutcome {
            self.spawns.lock().unwrap().push(req.clone());
            spawn_process(req)
        }
    }

    /// Writes an executable `#!/bin/sh` fake agent CLI named `name` into
    /// the env's bin dir — the same trick the crate's other real-spawn
    /// tests use (`/bin/sh` is the only binary they trust to exist).
    fn write_fake_agent(bin_dir: &Path, name: &str, body: &str) {
        let path = bin_dir.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("write fake agent");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
                .expect("chmod fake agent");
        }
    }

    fn req(kind: &str, prompt: &str) -> RunAgentRequest {
        RunAgentRequest {
            chat_id: "c1".to_string(),
            kind: kind.to_string(),
            model: None,
            prompt: prompt.to_string(),
        }
    }

    // ---- the real spawn path, driven end-to-end through the production
    // env builder and the real process spawn. macOS-only: the production
    // gapped branch there is `sandbox-exec`, which exists on every macOS
    // dev/CI host; the Linux branch needs bwrap/userns (probed at runtime,
    // often absent) and is covered by linux_sandbox_integration_tests.rs
    // in its own dedicated CI job instead.

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn a_headless_run_spawns_the_sandbox_wrap_and_returns_the_agents_output() {
        let env = TestEnv::new();
        write_fake_agent(
            env.bin_dir.path(),
            "pi",
            // The child observes its OWN environment and argv-adjacent
            // reality and reports it back on stdout — every containment
            // assertion below is the sandboxed child's own testimony, not
            // the test harness's.
            "echo 'fake agent stdout line'; \
             echo 'fake agent stderr line' >&2; \
             echo \"proxy_port=${HTTP_PROXY#http://127.0.0.1:}\"; \
             exit 0",
        );

        let out = run_headless(
            &env,
            &req("pi", "hello"),
            env.workspace.path(),
            Duration::from_secs(30),
        )
        .await;

        let text = out.expect("run should succeed");
        assert!(text.contains("fake agent stdout line"), "got: {text}");
        assert!(
            text.contains("fake agent stderr line"),
            "stderr must be merged into the result: {text}"
        );
        let proxy_port: u16 = text
            .lines()
            .find_map(|l| l.strip_prefix("proxy_port="))
            .expect("child should report the proxy port it saw")
            .parse()
            .expect("proxy port should be numeric");

        // The request recorded at the spawn seam — the exact bytes the real
        // child got. The sandbox wrap is a PREFIX: sandbox-exec -p <profile>
        // and only then the vetted headless argv.
        let spawns = env.spawns.lock().unwrap();
        assert_eq!(spawns.len(), 1, "exactly one spawn per run");
        let s = &spawns[0];
        assert_eq!(s.cmd, "/usr/bin/sandbox-exec");
        assert_eq!(
            &s.args[2..],
            &["pi".to_string(), "-p".to_string(), "hello".to_string()],
            "the vetted headless argv rides AFTER the wrap, untouched"
        );
        let profile = &s.args[1];
        assert!(s.args[0] == "-p");
        assert!(profile.contains("(deny network-outbound)"));
        assert!(
            profile.contains(&format!(
                "(allow network-outbound (remote ip \"localhost:{proxy_port}\"))"
            )),
            "the seatbelt profile must name the pane proxy's port: {profile}"
        );
        assert!(
            profile.contains(&format!(
                "(deny file-read* (subpath \"{}\"))",
                env.config_dir.path().display()
            )),
            "the seatbelt profile must deny the app config dir"
        );

        // The env the child actually received: proxy vars pointing at the
        // live pane proxy (the egress gap), and the child's own reported
        // port must agree with the profile's — one proxy, all three views.
        let env_map: std::collections::HashMap<&str, &str> = s
            .env
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        let proxy_url = format!("http://127.0.0.1:{proxy_port}");
        for key in ["HTTP_PROXY", "HTTPS_PROXY", "http_proxy", "https_proxy"] {
            assert_eq!(
                env_map.get(key),
                Some(&proxy_url.as_str()),
                "{key} must point at the pane proxy"
            );
        }
        assert_eq!(s.cwd, *env.workspace.path());

        let events = env.agent_events();
        assert_eq!(events[0]["status"], "started");
        assert_eq!(events[1]["status"], "done");
        assert_eq!(events.len(), 2, "no failed event on the happy path");
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn a_wedged_headless_agent_is_bounded_by_the_run_timeout() {
        let env = TestEnv::new();
        // Wedges with BOTH pipes still open — the hang shape the old code
        // had no bound for at all (its timeout only started after EOF).
        write_fake_agent(env.bin_dir.path(), "claude", "while true; do sleep 1; done");

        let start = std::time::Instant::now();
        let out = run_headless(
            &env,
            &req("claude", "something slow"),
            env.workspace.path(),
            Duration::from_secs(2),
        )
        .await;
        let elapsed = start.elapsed();

        let err = out.expect_err("a wedged run must fail");
        assert!(
            err.contains("timed out after 2s"),
            "the error must name the timeout: {err}"
        );
        assert!(
            elapsed >= Duration::from_secs(2),
            "the bound must have come from the timeout firing, not an earlier failure: {elapsed:?}"
        );
        assert!(
            elapsed < Duration::from_secs(30),
            "the bound must actually bound (SIGTERM, grace, SIGKILL): {elapsed:?}"
        );

        let events = env.agent_events();
        assert_eq!(events[0]["status"], "started");
        assert_eq!(events[1]["status"], "failed");
        assert_eq!(events.len(), 2);
    }

    #[tokio::test]
    async fn an_unknown_kind_is_refused_before_any_spawn_or_event() {
        // Cross-platform: the vetting runs before any env building, so it
        // needs no sandbox mechanism at all.
        let env = TestEnv::new();
        let out = run_headless(
            &env,
            &req("cowboy", "yeehaw"),
            env.workspace.path(),
            Duration::from_secs(30),
        )
        .await;
        assert_eq!(
            out.unwrap_err(),
            "cannot run 'cowboy' headless — not a headless-capable agent CLI"
        );
        assert!(
            env.spawns.lock().unwrap().is_empty(),
            "a refused kind must never reach a spawn"
        );
        assert!(
            env.events.lock().unwrap().is_empty(),
            "a refused kind must emit no lifecycle events"
        );
    }

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
