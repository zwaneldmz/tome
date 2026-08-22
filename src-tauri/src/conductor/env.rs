//! The injected environment seam the tool loop and tool dispatch are driven
//! through — same rationale and shape as `flow::runner::env::RunnerEnv`
//! (see that module's doc comment): a direct translation of `conductor.js`'s
//! `init(opts)` module-level closures (`send`, `canOpenFile`, `logEvent`,
//! `getRoots`) plus one Rust-only addition (`write_pty`, standing in for
//! JS's direct `ptys.get(id).write(...)` closure-captured `ptys` Map) and
//! one net-new capability JS never had to inject at all (`stream_chat` —
//! JS's `runChat` always calls the real `chat-client.js` `streamChat`
//! directly; this crate needs an injection seam here so
//! `test/conductor-security.test.js`'s abort scenario — and this port's own
//! token-budget/loop-limit scenarios — can run against a fake wire instead
//! of a real network call or a hand-rolled local HTTP server).
//!
//! [`production_env`] builds the real one per `chat_send` call (mirroring
//! `flow_env::production_env`'s own "cheap to call per-command, no
//! boot-time `init()` step" shape); [`super::tests`] builds fakes.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager};

use crate::chat::registry::ResolvedProvider;
use crate::chat::sse::{self, ChatError, NormalizedResponse};
use crate::{confine, skills, state::AppState};

pub type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;
/// One stream call's text-delta sink — `impl FnMut(&str) + Send` boxed so
/// it can travel through the `Arc<dyn Fn>` seam below (a bare `impl Trait`
/// parameter can't appear in a trait-object type).
pub type OnText = Box<dyn FnMut(&str) + Send>;

/// Every seam the tool loop ([`super::chat::run_chat`]) and tool dispatch
/// ([`super::tools::run_tool`]) read instead of touching Tauri/the network
/// directly. See the module doc comment.
#[derive(Clone)]
pub struct ConductorEnv {
    /// Pushes one event to the renderer — `send(channel, payload)`.
    pub send: Arc<dyn Fn(&str, Value) + Send + Sync>,
    /// Persistent audit log — `logEvent(kind, fields)`.
    pub log_event: Arc<dyn Fn(&str, Vec<(String, Value)>) + Send + Sync>,
    /// `open_file`'s confinement check — `canOpenFile(file)`. Production
    /// wires the STRONGER, realpath-resolving `confine::confined_real_path`
    /// rather than the lexical-only `isConfinedPath` JS's `canOpenFile` is
    /// literally bound to: `confine.rs`'s own doc comment names conductor's
    /// `open_file` tool as an intended caller of the realpath-resolving
    /// version once it landed, and a stronger, TOCTOU-safe check is never a
    /// security regression versus the lexical one — only ever stricter.
    pub can_open_file: Arc<dyn Fn(&Path) -> bool + Send + Sync>,
    /// `type_in_terminal`'s write — `ptys.get(id)?.write(text) ...
    /// !!p`. Returns whether a live pane was found, exactly like
    /// `pty::Registry::write`'s own return value (reused directly in
    /// production — see [`production_env`]).
    pub write_pty: Arc<dyn Fn(&str, &str) -> bool + Send + Sync>,
    /// `getRoots()` — open workspace folders, `[]` until `ws:sync` lands.
    pub roots: Arc<dyn Fn() -> Vec<String> + Send + Sync>,
    /// The ACTIVE workspace root the renderer synced via `conductor:cwd` —
    /// the folder the assistant's workspace-relative tools should operate
    /// at, preferred over `roots`' first entry (which is the first folder of
    /// the FIRST workspace, not the one you are looking at). `None` until
    /// the renderer's first sync.
    pub cwd: Arc<dyn Fn() -> Option<String> + Send + Sync>,
    /// One `(system, messages, tools) -> streamed reply` call — the
    /// wire-agnostic shape `chat::sse::stream_chat` already normalizes to,
    /// with the resolved provider/betas/fallbacks baked in by
    /// [`production_env`] at construction time (they don't change turn to
    /// turn within one `chat:send`). `on_text` is threaded in per call
    /// (rather than captured) so [`super::chat::run_chat`] — the only
    /// caller — stays the one place that knows how a delta becomes a
    /// `chat:delta` event.
    pub stream_chat: Arc<
        dyn Fn(
                Option<String>,
                Vec<Value>,
                Vec<Value>,
                OnText,
            ) -> BoxFuture<Result<NormalizedResponse, ChatError>>
            + Send
            + Sync,
    >,
    /// `read_file`/`run_command(cwd)`'s confinement resolver for EXISTING
    /// paths — `confine::confined_real_path` (realpath + double-check,
    /// requires the path to exist).
    pub resolve_path: Arc<dyn Fn(&Path) -> Result<PathBuf, String> + Send + Sync>,
    /// `write_file`'s confinement resolver for NEW (not-yet-existing) files
    /// — `confine::confined_write_path` (parent-dir check so a fresh file
    /// can be created inside a confined root).
    pub resolve_write: Arc<dyn Fn(&Path) -> Result<PathBuf, String> + Send + Sync>,
    /// `list_skills`/`read_skill`'s skills root — `skills::default_root(app)`.
    pub skills_root: Arc<dyn Fn() -> Option<PathBuf> + Send + Sync>,
    /// `run_command`'s backend — `(cwd, cmd) -> combined stdout+stderr`
    /// (capped, with a timeout), or an `Err` describing the failure.
    pub run_command: Arc<dyn Fn(&str, &str) -> BoxFuture<Result<String, String>> + Send + Sync>,
    /// `run_agent`'s backend — one headless agent run (sandboxed + gapped,
    /// bounded by the backend's own timeout), or an `Err` describing the
    /// refusal/failure. See `crate::agent_run` for the containment story.
    pub run_agent:
        Arc<dyn Fn(crate::agent_run::RunAgentRequest, PathBuf) -> BoxFuture<Result<String, String>> + Send + Sync>,
    /// `gate_question`'s backend — registers a comprehension gate, emits
    /// `mentor:check`, and awaits the user's `mentor_answer` (or times out).
    /// Returns the serialized answer value the tool loop appends as the
    /// tool result; `Err` carries a same-shaped fallback string.
    pub gate_question: Arc<dyn Fn(Value) -> BoxFuture<Result<String, String>> + Send + Sync>,
}

/// `run_command`'s timeout — the process is SIGKILLed via `kill_on_drop`
/// when this expires, matching the kill-on-timeout discipline `git.rs`/
/// `login_env.rs` already apply.
const RUN_TIMEOUT: Duration = Duration::from_secs(120);
/// `run_command`'s combined-output cap, in bytes. Output past this is
/// trimmed before being handed back to the model (trimmed from the FRONT,
/// keeping the tail — the most useful part of a long command run — via the
/// same boundary-snap idiom `conductor::state::Conductor::record` uses).
const RUN_OUTPUT_CAP: usize = 50_000;
/// `gate_question`'s wait cap — how long the paused tool loop holds before
/// giving up and reporting a timeout back to the model.
const GATE_TIMEOUT: Duration = Duration::from_secs(600);

/// Runs `sh -c <cmd>` in `cwd`, returning stdout+stderr combined and capped.
/// Spawn failure, timeout, and non-zero exit are all reported as `Err` (the
/// non-zero exit carries its code AND the output so the model can see what
/// went wrong). `kill_on_drop(true)` is what makes the timeout path actually
/// kill the child rather than orphan it — see `git.rs`'s doc comment for the
/// parity rationale.
async fn run_command_impl(cwd: &str, cmd: &str) -> Result<String, String> {
    let child = tokio::process::Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .current_dir(cwd)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| e.to_string())?;
    let out = tokio::time::timeout(RUN_TIMEOUT, child.wait_with_output())
        .await
        .map_err(|_| "run_command timed out".to_string())?
        .map_err(|e| e.to_string())?;

    let mut combined = String::from_utf8_lossy(&out.stdout).into_owned();
    combined.push_str(&String::from_utf8_lossy(&out.stderr));
    if combined.len() > RUN_OUTPUT_CAP {
        let mut cut = combined.len() - RUN_OUTPUT_CAP;
        while cut < combined.len() && !combined.is_char_boundary(cut) {
            cut += 1;
        }
        combined.drain(..cut);
    }

    if out.status.success() {
        Ok(combined)
    } else {
        let code = out.status.code().unwrap_or(-1);
        Err(format!("exit {code}:\n{combined}"))
    }
}

/// `gate_question`'s real backend: mint a gate on `AppState.mentor`, emit
/// `mentor:check` so the renderer can prompt the user, then await the
/// `mentor_answer` that completes it. The tool loop stays paused on this
/// future for up to [`GATE_TIMEOUT`]; on timeout (or a closed gate) it
/// returns a same-shaped JSON string rather than an `Err`, so the model
/// always gets a parsable tool result to react to.
async fn gate_question_impl(app: &AppHandle, payload: Value) -> Result<String, String> {
    let (id, rx) = app.state::<AppState>().mentor.register();
    let _ = app.emit(
        "mentor:check",
        json!({
            "id": id,
            "questions": payload["questions"],
            "test_code": payload["test_code"],
            "summary": payload["summary"],
        }),
    );
    match tokio::time::timeout(GATE_TIMEOUT, rx).await {
        Ok(Ok(v)) => Ok(v.to_string()),
        Ok(Err(_)) => Ok("{\"error\":\"gate closed\"}".to_string()),
        Err(_) => Ok("{\"timed_out\":true}".to_string()),
    }
}

/// Builds the real [`ConductorEnv`] a `chat:send` call drives the tool loop
/// through. `provider`/`betas`/`fallbacks` are `chat_send`'s own already-
/// resolved values (identical to what a single-turn `chat_send` builds
/// today) — baked into the `stream_chat` closure once here rather than
/// re-threaded through every turn.
pub fn production_env(
    app: AppHandle,
    provider: ResolvedProvider,
    betas: Option<Vec<String>>,
    fallbacks: Option<String>,
) -> ConductorEnv {
    ConductorEnv {
        send: {
            let app = app.clone();
            Arc::new(move |channel: &str, payload: Value| {
                let _ = app.emit(channel, payload);
            })
        },
        log_event: {
            let app = app.clone();
            Arc::new(move |kind: &str, fields: Vec<(String, Value)>| {
                crate::events::log_event(&app, kind, fields);
            })
        },
        can_open_file: {
            let app = app.clone();
            Arc::new(move |p: &Path| {
                let state = app.state::<AppState>();
                confine::confined_real_path(&state, p).is_ok()
            })
        },
        write_pty: {
            let app = app.clone();
            Arc::new(move |id: &str, text: &str| app.state::<AppState>().pty.write(id, text))
        },
        roots: {
            let app = app.clone();
            Arc::new(move || {
                let state = app.state::<AppState>();
                let synced = *state
                    .folders_synced
                    .read()
                    .expect("AppState.folders_synced lock poisoned");
                if !synced {
                    return Vec::new();
                }
                // Bound to a local first (rather than tail-expression-returned
                // directly): the `RwLockReadGuard` temporary's scope otherwise
                // extends across `state`'s own drop point when this whole
                // chain is the closure's tail expression, which the borrow
                // checker rejects even though the collected `Vec<String>`
                // itself borrows nothing.
                let folders: Vec<String> = state
                    .open_folders
                    .read()
                    .expect("AppState.open_folders lock poisoned")
                    .iter()
                    .map(|p| p.to_string_lossy().into_owned())
                    .collect();
                folders
            })
        },
        cwd: {
            let app = app.clone();
            Arc::new(move || app.state::<AppState>().conductor.cwd())
        },
        stream_chat: Arc::new(
            move |system: Option<String>,
                  messages: Vec<Value>,
                  tools: Vec<Value>,
                  mut on_text: OnText| {
                let provider = provider.clone();
                let betas = betas.clone();
                let fallbacks = fallbacks.clone();
                Box::pin(async move {
                    let args = sse::StreamChatArgs {
                        system: system.as_deref(),
                        messages: &messages,
                        tools: &tools,
                        betas: betas.as_deref(),
                        fallbacks: fallbacks.as_deref(),
                    };
                    sse::stream_chat(
                        crate::ipc::chat::http_client(),
                        &provider,
                        args,
                        move |t: &str| on_text(t),
                    )
                    .await
                }) as BoxFuture<Result<NormalizedResponse, ChatError>>
            },
        ),
        resolve_path: {
            let app = app.clone();
            Arc::new(move |p: &Path| {
                let state = app.state::<AppState>();
                confine::confined_real_path(&state, p)
            })
        },
        resolve_write: {
            let app = app.clone();
            Arc::new(move |p: &Path| {
                let state = app.state::<AppState>();
                confine::confined_write_path(&state, p)
            })
        },
        skills_root: {
            let app = app.clone();
            Arc::new(move || skills::default_root(&app))
        },
        run_command: Arc::new(move |cwd: &str, cmd: &str| {
            let cwd = cwd.to_string();
            let cmd = cmd.to_string();
            Box::pin(async move { run_command_impl(&cwd, &cmd).await })
                as BoxFuture<Result<String, String>>
        }),
        run_agent: {
            let app = app.clone();
            Arc::new(move |req: crate::agent_run::RunAgentRequest, cwd: PathBuf| {
                let app = app.clone();
                Box::pin(async move { crate::agent_run::run_headless_agent(&app, &req, &cwd).await })
                    as BoxFuture<Result<String, String>>
            })
        },
        gate_question: {
            let app = app.clone();
            Arc::new(move |payload: Value| {
                let app = app.clone();
                Box::pin(async move { gate_question_impl(&app, payload).await })
                    as BoxFuture<Result<String, String>>
            })
        },
    }
}
