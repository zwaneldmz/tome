//! Chat send/abort/providers. Ports `src/main/index.js`'s `chat:send`/
//! `chat:providers`/`chat:abort` handler BODIES (not `conductor.runChat`,
//! which those handlers call into for the real multi-turn work — see the
//! scope note below). OpenAI wire ports `streamOpenAI` SSE directly;
//! Anthropic goes through a hand-rolled `/v1/messages` SSE client on
//! `reqwest` rather than the SDK. Deltas stream via the event bus (see the
//! transport note below); abort via a `CancellationToken` held per chat id.
//!
//! ## Transport: event bus, not a Channel — a deliberate, noted deviation
//! from this phase's own default
//!
//! Every OTHER high-rate stream this rewrite ports (`pty:data`) uses a
//! Tauri `Channel` instead of `app.emit`, and that was this task's
//! starting assumption for `chat:delta` too. But `tome-ipc.js` — the
//! ALREADY-COMMITTED renderer contract this phase must not break — wires
//! `chat.onDelta`/`onDone`/`onTool` as plain `listen('chat:*', cb)` event
//! subscriptions, with its own comment on those three lines: "Rust side
//! emits these as plain events until the real chat command lands (plan
//! §Chat) — same wire names, same shim, no special-casing." Switching to a
//! Channel here would require a `tome-ipc.js` edit (a different slice's
//! file — out of this task's ownership) to hand `chat_send` a `Channel`
//! the way `pty.create` does, so this keeps `app.emit("chat:delta", ...)`
//! for now, matching what the shim already expects. Revisit alongside a
//! `tome-ipc.js` edit if chat delta volume ever proves the event bus
//! insufficient (it hasn't — chat deltas are far lower-rate than pty output:
//! word-at-a-time text, not a `cat` of a large file).
//!
//! ## Scope boundary: no multi-turn tool loop here (phase 5b's job)
//!
//! `index.js`'s real `chat:send` handler is a thin wrapper that resolves
//! the provider, builds `system` from `conductor.SYSTEM` (+ brain vault
//! context when `brainWs` is given), and hands off to `conductor.runChat`
//! — an 8-turn loop that streams text, executes tool calls against live
//! ptys (`chat:tool` events), and re-sends the transcript with tool
//! results appended. `conductor.js` is a phase 5b module (it depends on
//! chat, per the rewrite plan's explicit phase split) and does not exist
//! in this tree yet — no `src-tauri/src/conductor.rs`. This file therefore
//! implements the layer chat-client.js itself actually owns: ONE
//! `chat::sse::stream_chat` call per `chat:send`, streaming `chat:delta`
//! text as it arrives and firing exactly one `chat:done` at the end.
//! Concretely, relative to the full JS behavior:
//! - `system` is `None` — `conductor.SYSTEM` doesn't exist yet, and
//!   `brainWs`-driven vault context (`brain.contextFor`) belongs to the
//!   sibling `brain` slice, which this file must not depend on (each
//!   phase-5a leaf subsystem is independently ported). `brain_ws` is still
//!   accepted as a parameter — so a real renderer payload deserializes
//!   cleanly and the wire contract stays documented — but is otherwise
//!   unused this phase.
//! - `tools` is always empty, so the model has nothing to call and a
//!   `stop_reason` of `tool_use` should not arise in practice yet; if it
//!   ever does (a provider offering its own implicit tools, say), this
//!   still ends the turn cleanly (same `chat:done` as a plain `end`) rather
//!   than inventing a partial tool-loop — no `chat:tool` event is emitted
//!   anywhere in this file. Full tool-loop parity (including `chat:tool`)
//!   arrives with conductor in phase 5b.
//! - The renderer already resends its full transcript on every `chat.send`
//!   call (`panels/chat.js`'s `tome.chat.send(this.chatId, this.history,
//!   brainWs)`), so plain multi-turn TEXT conversations work correctly
//!   even without server-side history — only agentic tool use is deferred.

use std::collections::HashMap;
use std::sync::Mutex;

use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager, State};
use tokio_util::sync::CancellationToken;

use crate::chat::{providers, sse};
use crate::state::AppState;
use crate::{lock_gate, login_env, store};

/// In-flight `chat:send` calls keyed by chat id — the Rust analogue of
/// `conductor.js`'s module-level `inflight` Map (`chatId ->
/// AbortController`). A domain-local static rather than an `AppState`
/// field: `state.rs`'s own doc comment explicitly invites this ("later
/// slices extend THEIR OWN modules... rather than adding fields here, so
/// this file stays a rare merge-conflict site across parallel agents") —
/// this task's ownership is `src-tauri/src/chat/`, `ipc/chat.rs`, and one
/// line of `lib.rs`, not `state.rs`. Same pattern `login_env.rs` already
/// uses for its own module-local cache.
static INFLIGHT: std::sync::OnceLock<Mutex<HashMap<String, CancellationToken>>> =
    std::sync::OnceLock::new();

fn inflight() -> &'static Mutex<HashMap<String, CancellationToken>> {
    INFLIGHT.get_or_init(|| Mutex::new(HashMap::new()))
}

/// One shared `reqwest::Client` for connection reuse across sends — same
/// module-local-static rationale as `inflight()` above. `reqwest::Client`
/// is `Clone + Send + Sync` and documented as cheap to share via a
/// long-lived reference; building a fresh one per `chat:send` would throw
/// connection pooling away for no benefit.
static HTTP_CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();

fn http_client() -> &'static reqwest::Client {
    HTTP_CLIENT.get_or_init(reqwest::Client::new)
}

/// The beta flag `chat:send` attaches whenever a resolved provider's
/// `beta` bool is set — verbatim from `index.js`'s `chat:send` handler:
/// `if (provider.beta) { provider.betas = ['server-side-fallback-2026-07-01'];
/// provider.fallbacks = 'default' }`. Applied uniformly regardless of which
/// `resolve_chat_provider` branch produced the provider (only the `claude`
/// store-backed branch sets `beta: true` today; env-override and Requesty
/// both force it off — see `chat::providers`'s doc comment).
const SERVER_SIDE_FALLBACK_BETA: &str = "server-side-fallback-2026-07-01";

/// `chat:providers` (`ipcMain.handle('chat:providers', ...)`,
/// `index.js` ~1310-1325). Provider list for Preferences: names, models,
/// and a `keySet` boolean derived from the login-shell secrets — the key
/// ITSELF never crosses IPC.
#[tauri::command]
pub async fn chat_providers(app: AppHandle, state: State<'_, AppState>) -> Result<Value, String> {
    lock_gate::guard(&state, "chat:providers")?;
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let locked = *state.locked.read().expect("AppState.locked lock poisoned");

    let login = login_env::login_env().await;
    let env: HashMap<String, String> = std::env::vars().collect();
    let stored = {
        let dir = dir.clone();
        tokio::task::spawn_blocking(move || store::get(&dir, "chat-provider", locked))
            .await
            .map_err(|e| e.to_string())?
    };
    let active = providers::active_provider_id(stored.as_str());

    let list: Vec<Value> = providers::CHAT_PROVIDERS
        .iter()
        .map(|p| {
            json!({
                "id": p.id,
                "label": p.label,
                "model": p.model,
                "keyEnv": p.key_env,
                "keySet": providers::key_is_set(&login.secrets, &env, p.key_env),
            })
        })
        .collect();

    Ok(json!({ "providers": list, "active": active }))
}

/// `chat:send` (`index.js` ~1271-1305). See this file's module doc comment
/// for the exact scope this implements versus the full JS behavior
/// (`conductor.runChat`'s tool loop is phase 5b).
#[tauri::command]
#[allow(unused_variables)] // brain_ws: accepted for wire-shape completeness, unused this phase — see module doc comment
pub async fn chat_send(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
    messages: Vec<Value>,
    brain_ws: Option<String>,
) -> Result<Value, String> {
    lock_gate::guard(&state, "chat:send")?;

    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let locked = *state.locked.read().expect("AppState.locked lock poisoned");
    let login = login_env::login_env().await;
    let env: HashMap<String, String> = std::env::vars().collect();
    let (stored_provider, stored_model) = {
        let dir = dir.clone();
        tokio::task::spawn_blocking(move || {
            (
                store::get(&dir, "chat-provider", locked),
                store::get(&dir, "chat-model", locked),
            )
        })
        .await
        .map_err(|e| e.to_string())?
    };

    let provider = match providers::resolve_chat_provider(
        &env,
        &login.secrets,
        stored_provider.as_str(),
        stored_model.as_str(),
    ) {
        providers::ProviderResolution::KeyMissing { entry, .. } => {
            let message = format!(
                "{} needs {} — export it in your shell and restart, or pick another provider in \u{2318}, \u{2192} Assistant.",
                entry.label, entry.key_env
            );
            emit_done(&app, &id, false, Some(message));
            return Ok(json!({}));
        }
        providers::ProviderResolution::Ready(p) => p,
    };

    let (betas, fallbacks): (Option<Vec<String>>, Option<String>) = if provider.beta {
        (
            Some(vec![SERVER_SIDE_FALLBACK_BETA.to_string()]),
            Some("default".to_string()),
        )
    } else {
        (None, None)
    };

    let token = CancellationToken::new();
    inflight()
        .lock()
        .expect("chat inflight lock poisoned")
        .insert(id.clone(), token.clone());

    let emit_app = app.clone();
    let emit_id = id.clone();
    let on_text = move |text: &str| {
        let _ = emit_app.emit("chat:delta", json!({ "id": emit_id, "text": text }));
    };

    let args = sse::StreamChatArgs {
        // conductor.SYSTEM + brain vault context land in phase 5b — see
        // module doc comment.
        system: None,
        messages: &messages,
        tools: &[],
        betas: betas.as_deref(),
        fallbacks: fallbacks.as_deref(),
    };

    // Races the stream against chat_abort's cancellation — dropping the
    // stream_chat future (the losing branch) drops its in-flight reqwest
    // request, which cancels the underlying connection the same way a JS
    // AbortController.abort() feeding fetch's `signal` would. See this
    // file's module doc comment for why chat::sse itself takes no signal
    // parameter at all.
    let outcome = tokio::select! {
        result = sse::stream_chat(http_client(), &provider, args, on_text) => Outcome::Finished(result),
        _ = token.cancelled() => Outcome::Aborted,
    };

    inflight()
        .lock()
        .expect("chat inflight lock poisoned")
        .remove(&id);

    match outcome {
        Outcome::Aborted => emit_done(&app, &id, true, Some("Stopped.".to_string())),
        Outcome::Finished(Ok(resp)) if resp.stop_reason == "refusal" => emit_done(
            &app,
            &id,
            false,
            Some("Request declined by safety classifiers.".to_string()),
        ),
        // Covers both 'end' and 'tool_use' — see module doc comment on why
        // tool_use is not specially handled without conductor's loop.
        Outcome::Finished(Ok(_)) => emit_done(&app, &id, false, None),
        Outcome::Finished(Err(err)) => {
            let msg = err.message();
            let authy = err.status() == Some(401) || is_authy_message(&msg);
            let friendly = if authy {
                "Chat credentials rejected. Check the provider key (MOONSHOT_API_KEY / ZHIPU_API_KEY / ANTHROPIC_API_KEY / REQUESTY_API_KEY) in your shell and restart Tome.".to_string()
            } else {
                msg
            };
            emit_done(&app, &id, false, Some(friendly));
        }
    }
    Ok(json!({}))
}

/// `chat:abort` (`ipcMain.on('chat:abort', (e, id) => conductor.abortChat(id))`).
/// A no-op for an unknown/already-finished chat id — same optional-chaining
/// tolerance as `conductor.abortChat`'s `inflight.get(id)?.abort()`.
#[tauri::command]
pub async fn chat_abort(state: State<'_, AppState>, id: String) -> Result<Value, String> {
    lock_gate::guard(&state, "chat:abort")?;
    if let Some(token) = inflight()
        .lock()
        .expect("chat inflight lock poisoned")
        .get(&id)
    {
        token.cancel();
    }
    Ok(json!({}))
}

/// `chat_send`'s two race outcomes — see its own doc comment on the
/// `tokio::select!` this backs.
enum Outcome {
    Finished(Result<sse::NormalizedResponse, sse::ChatError>),
    Aborted,
}

/// Emits `chat:done` — see this file's module doc comment on why every
/// call here includes both keys explicitly (`aborted: false, error: null`)
/// where the JS original sometimes sends a bare `{ id }`: the renderer's
/// own consumption (`({ id, error, aborted }) => ...`, `if (error)`) treats
/// a missing key and an explicit falsy value identically, so this is a
/// harmless normalization, not a behavioral change.
fn emit_done(app: &AppHandle, id: &str, aborted: bool, error: Option<String>) {
    let _ = app.emit(
        "chat:done",
        json!({ "id": id, "aborted": aborted, "error": error }),
    );
}

/// `err?.status === 401 || /api.key|auth/i.test(msg)` — the second half.
/// `regex` is already a `Cargo.toml` dependency (this task's brief lists it
/// as present), reused here for exact fidelity to the JS pattern (`.`
/// matches any single character, not a literal dot — so this matches "api
/// key", "api-key", "apikey", etc., same as the source regex, not just the
/// literal string "api.key").
fn is_authy_message(msg: &str) -> bool {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| regex::Regex::new("(?i)api.key|auth").expect("static pattern is valid"))
        .is_match(msg)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ================= is_authy_message — ports the JS catch block's
    // regex half of `authy` (the `err?.status === 401` half is exercised
    // through chat::sse::ChatError::status directly, in that module's own
    // tests) =================

    #[test]
    fn is_authy_message_matches_api_key_variants_case_insensitively() {
        assert!(is_authy_message("Invalid API key provided"));
        assert!(is_authy_message("bad api-key"));
        assert!(is_authy_message("APIXKEY rejected")); // '.' matches any single char, verbatim JS regex semantics
    }

    #[test]
    fn is_authy_message_matches_auth_as_a_substring() {
        assert!(is_authy_message("Unauthorized"));
        assert!(is_authy_message("authentication failed"));
    }

    #[test]
    fn is_authy_message_false_for_an_unrelated_message() {
        assert!(!is_authy_message("connection reset by peer"));
    }
}
