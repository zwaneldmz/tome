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
//! `flow::runner::env::production_env`'s own "cheap to call per-command, no
//! boot-time `init()` step" shape); [`super::tests`] builds fakes.

use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;

use serde_json::Value;
use tauri::{AppHandle, Emitter, Manager};

use crate::chat::providers::ResolvedProvider;
use crate::chat::sse::{self, ChatError, NormalizedResponse};
use crate::{confine, state::AppState};

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
    /// One `(system, messages, tools) -> streamed reply` call — the
    /// wire-agnostic shape `chat::sse::stream_chat` already normalizes to,
    /// with the resolved provider/betas/fallbacks baked in by
    /// [`production_env`] at construction time (they don't change turn to
    /// turn within one `chat:send`). `on_text` is threaded in per call
    /// (rather than captured) so [`super::chat::run_chat`] — the only
    /// caller — stays the one place that knows how a delta becomes a
    /// `chat:delta` event.
    pub stream_chat: Arc<
        dyn Fn(Option<String>, Vec<Value>, Vec<Value>, OnText) -> BoxFuture<Result<NormalizedResponse, ChatError>>
            + Send
            + Sync,
    >,
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
                let synced = *state.folders_synced.read().expect("AppState.folders_synced lock poisoned");
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
        stream_chat: Arc::new(move |system: Option<String>, messages: Vec<Value>, tools: Vec<Value>, mut on_text: OnText| {
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
                sse::stream_chat(crate::ipc::chat::http_client(), &provider, args, move |t: &str| on_text(t)).await
            }) as BoxFuture<Result<NormalizedResponse, ChatError>>
        }),
    }
}
